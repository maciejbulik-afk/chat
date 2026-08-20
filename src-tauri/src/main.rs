#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use tauri::{AppHandle, WebviewWindowBuilder, WebviewUrl};

use tauri::image::Image as TauriImage;
use image::GenericImageView;
use rodio::{Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::BufReader;
use std::io::Cursor;
use std::thread;
use tokio::fs as async_fs;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;
use tauri::webview::DownloadEvent;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_shell::ShellExt;

use std::sync::atomic::{AtomicUsize, Ordering};

static WINDOW_COUNTER: AtomicUsize = AtomicUsize::new(0);

static ICON_NORMAL: &[u8] = include_bytes!("../icons/32x32.png");
static ICON_ALERT: &[u8] = include_bytes!("../icons/32x32-alert.png");
static NOTIF_SOUND: &[u8] = include_bytes!("../assets/notif.mp3");

fn png_to_tauri_icon(png_bytes: &[u8]) -> TauriImage<'static> {
    let img = image::load_from_memory(png_bytes).expect("Nie udalo sie zdekodowac ikony PNG");
    let rgba = img.to_rgba8();
    let (w, h) = img.dimensions();
    TauriImage::new_owned(rgba.into_raw(), w, h)
}

// === Ustawienia ===

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppSettings {
    theme: String,
    custom_theme: String,
    background: String,
    custom_background: String,
    notification_height: f64,
    autostart: bool,
    start_minimized: bool,
    sound_volume: f32,
    sound_enabled: bool,
    use_native_notifications: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "red".into(),
            custom_theme: "#e74c3c".into(),
            background: "dark".into(),
            custom_background: "#1a1a2e".into(),
            notification_height: 80.0,
            autostart: false,
            start_minimized: false,
            sound_volume: 0.5,
            sound_enabled: true,
            use_native_notifications: false,
        }
    }
}

fn settings_path(app: &AppHandle) -> std::path::PathBuf {
    let config_dir = app.path().app_config_dir().unwrap();
    fs::create_dir_all(&config_dir).ok();
    config_dir.join("settings.json")
}

fn load_settings(app: &AppHandle) -> AppSettings {
    let path = settings_path(app);
    if let Ok(data) = fs::read_to_string(&path) {
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        AppSettings::default()
    }
}

#[tauri::command]
fn get_settings(app: AppHandle) -> AppSettings {
    load_settings(&app)
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    let path = settings_path(&app);
    let data = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    fs::write(&path, data).map_err(|e| e.to_string())?;

    // Aktualizuj autostart
    let autolaunch = app.autolaunch();
    if settings.autostart {
        autolaunch.enable().map_err(|e| e.to_string())?;
    } else {
        autolaunch.disable().map_err(|e| e.to_string())?;
    }

    Ok(())
}

// === Linki zewnetrzne ===

/// Czy to wrapper Google na link zewnetrzny (https://www.google.com/url?q=...).
fn is_google_redirect(url: &tauri::Url) -> bool {
    let host = url.host_str().unwrap_or("");
    (host == "www.google.com" || host == "google.com") && url.path() == "/url"
}

/// Hosty obslugiwane WEWNATRZ okna aplikacji.
///
/// UWAGA: na Linuksie WebKitGTK wola on_navigation TAKZE dla iframe'ow, a Chat
/// laduje w ramkach widgety z roznych hostow Google (ogs.google.com - przelacznik
/// aplikacji, contacts.google.com - wizytowki kontaktow, apis.google.com).
/// Dlatego ta lista musi obejmowac cale *.google.com - inaczej widgety wyskakuja
/// w przegladarce przy starcie. Wyjatkiem jest wrapper /url?q=, ktory z definicji
/// prowadzi na zewnatrz. Rozroznianie "docs/drive do przegladarki, reszta w oknie"
/// robi warstwa JS, ktora widzi klikniecie uzytkownika i dziala tylko w glownej ramce.
fn is_internal_url(url: &tauri::Url) -> bool {
    match url.scheme() {
        "http" | "https" => {
            if is_google_redirect(url) {
                return false;
            }
            let host = url.host_str().unwrap_or("");
            host == "google.com"
                || host.ends_with(".google.com")
                || host.ends_with(".googleusercontent.com")
                || host.ends_with(".gstatic.com")
                || host == "accounts.youtube.com"
        }
        // tauri://, about:blank, blob:, data: - wewnetrzne okna aplikacji
        _ => true,
    }
}

/// Rozpakowuje https://www.google.com/url?q=<cel> do samego <cel>.
fn unwrap_google_redirect(url: &tauri::Url) -> String {
    let mut current = url.clone();
    for _ in 0..3 {
        let host = current.host_str().unwrap_or("").to_string();
        let is_wrapper =
            (host == "www.google.com" || host == "google.com") && current.path() == "/url";
        if !is_wrapper {
            break;
        }
        let target = current
            .query_pairs()
            .find(|(k, _)| k == "q" || k == "url")
            .map(|(_, v)| v.into_owned());
        match target.and_then(|t| tauri::Url::parse(&t).ok()) {
            Some(next) => current = next,
            None => break,
        }
    }
    current.to_string()
}

/// Log ze skryptu wstrzykiwanego -> terminal, zeby nie trzeba bylo
/// otwierac narzedzi deweloperskich w oknie czatu.
#[tauri::command]
fn js_log(msg: String) {
    println!("[ISM-Chat][js] {}", msg);
}

/// Odczyt schowka po stronie Rusta - uzywany, gdy WebKit nie udostepni
/// zawartosci schowka stronie.
///
/// Komendy sa `async`, bo plugin ostrzega przed wolaniem read_text/read_image
/// z glownego watku - na Linuksie potrafi to zakleszczyc caly interfejs.
#[tauri::command]
async fn read_clipboard_text(app: AppHandle) -> Result<String, String> {
    app.clipboard().read_text().map_err(|e| e.to_string())
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Obraz ze schowka jako PNG w base64.
///
/// Potrzebne, bo WebKitGTK nie przekazuje obrazu do strony - zdarzenie 'paste'
/// przychodzi calkiem puste (potwierdzone logami: brak typow, 0 plikow).
#[tauri::command]
async fn read_clipboard_image(app: AppHandle) -> Result<String, String> {
    let img = app
        .clipboard()
        .read_image()
        .map_err(|e| format!("Schowek nie zawiera obrazu: {}", e))?;

    let (w, h) = (img.width(), img.height());
    let buf = image::RgbaImage::from_raw(w, h, img.rgba().to_vec())
        .ok_or_else(|| "Niespojne wymiary obrazu ze schowka".to_string())?;

    let mut png: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("Blad kodowania PNG: {}", e))?;

    println!("[ISM-Chat] Obraz ze schowka: {}x{}, {} B", w, h, png.len());
    Ok(base64_encode(&png))
}

// === Powiadomienia ===

fn urlencode(s: &str) -> String {
    s.chars().map(|c| match c {
        '#' => "%23".to_string(),
        ' ' => "%20".to_string(),
        _ => c.to_string(),
    }).collect()
}

#[tauri::command]
async fn create_notification_window(app: AppHandle, title: String, body: String) -> Result<(), String> {
    let settings = load_settings(&app);

    // Na Linuxie mozna uzyc natywnych powiadomien D-Bus.
    // Pod natywnym Wayland klient NIE MOZE ustawiac pozycji wlasnego okna,
    // wiec wlasny popup wyladowalby na srodku ekranu - wtedy zawsze D-Bus.
    #[cfg(target_os = "linux")]
    {
        let on_wayland = std::env::var("WAYLAND_DISPLAY").is_ok()
            && std::env::var("GDK_BACKEND")
                .map(|v| !v.contains("x11"))
                .unwrap_or(true);

        if settings.use_native_notifications || on_wayland {
            notify_rust::Notification::new()
                .summary(&title)
                .body(&body)
                .appname("Google Chat by ism")
                .icon("mail-message-new")
                .timeout(notify_rust::Timeout::Milliseconds(8000))
                .show()
                .map_err(|e| format!("Błąd D-Bus: {}", e))?;
            return Ok(());
        }
    }

    // Popup - identyczny na Windows i Linux
    let _ = body;
    let id = WINDOW_COUNTER.fetch_add(1, Ordering::SeqCst);
    let label = format!("notif_{}", id);
    let h = settings.notification_height.clamp(60.0, 150.0);
    let url = format!("notification.html?theme={}&bg={}&ct={}&cb={}",
        settings.theme, settings.background,
        urlencode(&settings.custom_theme), urlencode(&settings.custom_background));

    let window = WebviewWindowBuilder::new(
        &app,
        label,
        WebviewUrl::App(url.into())
    )
    .title(title)
    .inner_size(360.0, h)
    .min_inner_size(360.0, h)
    .max_inner_size(360.0, h)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .visible(false)
    .build()
    .map_err(|e| e.to_string())?;

    // Pozycjonuj na monitorze okna glownego
    let monitor = app.get_webview_window("main")
        .and_then(|w| w.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());

    if let Some(monitor) = monitor {
        let scale_factor = monitor.scale_factor();
        let monitor_size = monitor.size();
        let monitor_pos = monitor.position();

        let phys_w = (360.0 * scale_factor) as u32;
        let phys_h = (h * scale_factor) as u32;

        let margin_x = (12.0 * scale_factor) as i32;
        let margin_y = (50.0 * scale_factor) as i32;

        let x = monitor_pos.x + monitor_size.width as i32 - phys_w as i32 - margin_x;
        let y = monitor_pos.y + monitor_size.height as i32 - phys_h as i32 - margin_y;

        let _ = window.set_size(tauri::PhysicalSize::new(phys_w, phys_h));
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }

    let _ = window.show();

    // Na Linuxie KWin/Wayland wymuszamy ponownie po opoznieniu
    #[cfg(target_os = "linux")]
    {
        let win_clone = window.clone();
        let monitor2 = app.get_webview_window("main")
            .and_then(|w| w.current_monitor().ok().flatten())
            .or_else(|| win_clone.primary_monitor().ok().flatten());

        if let Some(monitor) = monitor2 {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let scale_factor = monitor.scale_factor();
                let monitor_size = monitor.size();
                let monitor_pos = monitor.position();

                let phys_w = (360.0 * scale_factor) as u32;
                let phys_h = (h * scale_factor) as u32;

                let margin_x = (12.0 * scale_factor) as i32;
                let margin_y = (50.0 * scale_factor) as i32;

                let x = monitor_pos.x + monitor_size.width as i32 - phys_w as i32 - margin_x;
                let y = monitor_pos.y + monitor_size.height as i32 - phys_h as i32 - margin_y;

                let _ = win_clone.set_size(tauri::PhysicalSize::new(phys_w, phys_h));
                let _ = win_clone.set_position(tauri::PhysicalPosition::new(x, y));
            });
        }
    }

    Ok(())
}

#[tauri::command]
fn set_tray_alert(app: AppHandle, alert: bool) -> Result<(), String> {
    if let Some(tray) = app.tray_by_id("main-tray") {
        let icon_bytes = if alert { ICON_ALERT } else { ICON_NORMAL };
        let icon = png_to_tauri_icon(icon_bytes);
        tray.set_icon(Some(icon)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn play_notification_sound(app: AppHandle, volume: f32) -> Result<(), String> {
    let settings = load_settings(&app);
    if !settings.sound_enabled {
        return Ok(());
    }
    let _ = volume;
    // Suwak 0-100% mapujemy na rodio 0.0-5.0
    // (1.0 = oryginalna glosnosc pliku, powyzej = wzmocnienie)
    let safe_volume = (settings.sound_volume * 5.0).clamp(0.0, 5.0);

    thread::spawn(move || {
        if let Ok((_stream, stream_handle)) = OutputStream::try_default() {
            if let Ok(sink) = Sink::try_new(&stream_handle) {
                sink.set_volume(safe_volume);
                let cursor = Cursor::new(NOTIF_SOUND);
                if let Ok(source) = Decoder::new(BufReader::new(cursor)) {
                    sink.append(source);
                    sink.sleep_until_end();
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn upload_file_stream(file_path: String) -> Result<String, String> {
    let meta = async_fs::metadata(&file_path)
        .await
        .map_err(|e| format!("Błąd odczytu metadanych: {}", e))?;

    let size = meta.len();
    let path = std::path::Path::new(&file_path);
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");

    let mut _file = async_fs::File::open(&file_path)
        .await
        .map_err(|e| format!("Błąd otwarcia pliku: {}", e))?;

    Ok(format!("Zakończono odczyt pliku ({}) o rozmiarze {} bajtów.", file_name, size))
}

#[tauri::command]
fn close_notification_window(app: AppHandle) {
    for (label, window) in app.webview_windows() {
        if label.starts_with("notif_") {
            let _ = window.close();
        }
    }
}

/// Przywraca okno glowne po schowaniu do traya.
///
/// Kolejnosc ma znaczenie: unminimize() na oknie, ktore jest jeszcze ukryte,
/// zostawia GTK w polowicznym stanie. Najpierw mapujemy okno, dopiero potem
/// je odminimalizowujemy.
fn present_main_window(app: &AppHandle) {
    let window = match app.get_webview_window("main") {
        Some(w) => w,
        None => return,
    };

    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

#[tauri::command]
fn show_main_window(app: AppHandle) {
    present_main_window(&app);
}

#[tauri::command]
fn open_settings_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.set_focus();
        return Ok(());
    }

    WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Ustawienia — Google Chat by ism")
        .inner_size(480.0, 700.0)
        .resizable(false)
        .center()
        .build()
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn main() {
    // Zostajemy na X11 (XWayland).
    //
    // Natywny Wayland naprawia wklejanie, ale psuje zarzadzanie oknem:
    // po hide()/show() kompozytor trzyma stara geometrie dekoracji i przyciski
    // na belce przestaja reagowac (dopiero dwuklik w belke to odblokowuje),
    // a wlasnego okna popupu nie da sie pozycjonowac.
    // Za Ctrl+V odpowiada awaryjne wklejanie w skrypcie wstrzykiwanym nizej.
    //
    // Kto chce sprobowac natywnego Waylandu: ISM_CHAT_FORCE_WAYLAND=1
    #[cfg(target_os = "linux")]
    if std::env::var("ISM_CHAT_FORCE_WAYLAND").is_err() {
        std::env::set_var("GDK_BACKEND", "x11");
    }

    let inject_script = r#"
        window.addEventListener('DOMContentLoaded', () => {
            if (window.__ISM_CHAT_INIT) return;
            window.__ISM_CHAT_INIT = true;

            let trayAlertActive = false;
            let lastCount = 0;

            console.log('[ISM-Chat] Skrypt wstrzykniety');

            // ==========================================================
            // Linki: co zostaje w oknie, a co leci do przegladarki
            //
            // Stary warunek hostname.endsWith('google.com') uznawal za
            // "wewnetrzny" rowniez wrapper https://www.google.com/url?q=<cel>,
            // ktorym Google Chat opakowuje linki z wiadomosci. Dlatego okno
            // nawigowalo do wrappera, ten robil 302 na docelowa strone
            // i zewnetrzny link ladowal w aplikacji.
            // ==========================================================
            const isInternalHost = (h) =>
                h === 'chat.google.com' ||
                h === 'accounts.google.com' ||
                h === 'accounts.youtube.com' ||
                h.endsWith('.googleusercontent.com');

            const unwrapRedirect = (raw) => {
                try {
                    let u = new URL(raw, window.location.href);
                    for (let i = 0; i < 3; i++) {
                        const h = u.hostname;
                        const isWrapper = (h === 'www.google.com' || h === 'google.com')
                            && u.pathname === '/url';
                        if (!isWrapper) break;
                        const t = u.searchParams.get('q') || u.searchParams.get('url');
                        if (!t) break;
                        u = new URL(t, u.href);
                    }
                    return u.href;
                } catch (err) { return String(raw); }
            };

            const isExternal = (raw) => {
                try {
                    const u = new URL(unwrapRedirect(raw), window.location.href);
                    if (u.protocol !== 'http:' && u.protocol !== 'https:') return false;
                    return !isInternalHost(u.hostname);
                } catch (err) { return false; }
            };

            const openExternal = (raw) => {
                const target = unwrapRedirect(raw);
                if (window.__TAURI__ && window.__TAURI__.core) {
                    window.__TAURI__.core.invoke('plugin:shell|open', { path: target })
                        .catch((err) => console.error('[ISM-Chat] shell|open:', err));
                }
            };

            window.open = function (url, name, features) {
                if (!url) return null;
                if (isExternal(url)) openExternal(url);
                else window.location.href = url;
                return null;
            };

            const linkHandler = (e) => {
                if (e.button === 2) return;
                const a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
                if (!a) return;
                if (!isExternal(a.href)) {
                    if (a.target === '_blank') a.target = '_self';
                    return;
                }
                e.preventDefault();
                e.stopPropagation();
                // KLUCZOWE: samo preventDefault nie wystarcza - Google ma wlasny
                // handler kliknięcia, ktory i tak ustawia window.location.
                if (e.stopImmediatePropagation) e.stopImmediatePropagation();
                openExternal(a.href);
            };
            document.addEventListener('click', linkHandler, true);
            document.addEventListener('auxclick', linkHandler, true);

            const invokeTauri = (cmd, args) => {
                if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
                    return window.__TAURI__.core.invoke(cmd, args);
                }
                return Promise.reject('No IPC');
            };

            const getUnreadCount = () => {
                // Polski UI: "nieprzeczytana wiadomość", Angielski: "unread"
                const el = document.querySelector('div[aria-label*="nieprzeczytan"], div[aria-label*="unread"]');
                if (el && el.textContent) {
                    const m = el.textContent.match(/\d+/);
                    if (m) return parseInt(m[0], 10);
                }
                const t = document.title;
                if (t && (t.includes("Masz wiadomo") || t.includes("napisa") || t.includes("says") || t.match(/^\(\d+\)/))) {
                    return 1;
                }
                return 0;
            };

            let lastShowTime = 0;

            const showNotification = () => {
                const now = Date.now();
                if (now - lastShowTime < 3000) return;
                // Nie pokazuj powiadomienia gdy uzytkownik aktywnie uzywa czata
                if (document.hasFocus()) return;
                lastShowTime = now;

                const t = document.title;
                let safeTitle = "Nowa wiadomość";
                if (t.includes("od:")) {
                    safeTitle = t.split(" - ")[0] || safeTitle;
                }
                console.log('[ISM-Chat] Pokazuje powiadomienie:', safeTitle);
                invokeTauri('close_notification_window', {}).catch(() => {});
                invokeTauri('play_notification_sound', { volume: 0.5 }).catch(() => {});
                invokeTauri('create_notification_window', {
                    title: safeTitle,
                    body: "Sprawdź Google Chat"
                }).catch(() => {});
            };

            setInterval(() => {
                const count = getUnreadCount();

                if (count > 0) {
                    if (count > lastCount) {
                        console.log('[ISM-Chat] Nowy rozmowca, count:', count);
                        showNotification();
                    }

                    if (!trayAlertActive) {
                        trayAlertActive = true;
                        invokeTauri('set_tray_alert', { alert: true }).catch(() => {});
                    }
                } else {
                    if (trayAlertActive) {
                        trayAlertActive = false;
                        console.log('[ISM-Chat] Wszystko odczytane');
                        invokeTauri('close_notification_window', {}).catch(() => {});
                        invokeTauri('set_tray_alert', { alert: false }).catch(() => {});
                    }
                }

                lastCount = count;
            }, 2000);

            const NativeNotification = window.Notification;
            const ProxyNotification = function(title, options) {
                const instance = new NativeNotification(title, options);
                console.log('[ISM-Chat] Notification API:', title);
                showNotification();
                if (!trayAlertActive) {
                    trayAlertActive = true;
                    invokeTauri('set_tray_alert', { alert: true }).catch(() => {});
                }
                return instance;
            };
            Object.assign(ProxyNotification, NativeNotification);
            ProxyNotification.prototype = NativeNotification.prototype;
            Object.defineProperty(ProxyNotification, 'permission', {
                get: () => 'granted'
            });
            window.Notification = ProxyNotification;
            ProxyNotification.requestPermission = () => Promise.resolve('granted');

            window.addEventListener('focus', () => {
                invokeTauri('close_notification_window', {}).catch(() => {});
            });

            // ==========================================================
            // Wymuszone wklejanie zwyklym tekstem (Linux / WebKitGTK)
            //
            // Diagnoza z logow: zdarzenie 'paste' DOCHODZI i ma tresc, ale gdy
            // w schowku jest wersja text/html, edytor Chata nie wstawia nic.
            // Ctrl+Shift+V dziala, bo podsuwa mu wylacznie text/plain.
            // Robimy wiec to samo dla zwyklego Ctrl+V: przejmujemy zdarzenie,
            // wyciagamy text/plain i wstawiamy sami.
            //
            // Obrazki i pliki przepuszczamy do Chata bez tykania.
            // Kosztem jest utrata formatowania - tak samo jak przy Ctrl+Shift+V.
            // ==========================================================
            if (navigator.userAgent.indexOf('Linux') !== -1) {
                const plog = (...a) => {
                    const msg = a.map((x) => typeof x === 'string' ? x : String(x)).join(' ');
                    console.log('[ISM-Chat][paste]', msg);
                    invokeTauri('js_log', { msg: '[paste] ' + msg }).catch(() => {});
                };

                const isEditable = (el) => !!el && (
                    el.isContentEditable || el.tagName === 'INPUT' || el.tagName === 'TEXTAREA'
                );

                // Wstawia tekst tak, zeby edytor Chata zobaczyl zmiane.
                const insertText = (text) => {
                    const el = document.activeElement;
                    if (!isEditable(el)) return false;

                    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
                        const a = el.selectionStart != null ? el.selectionStart : el.value.length;
                        const b = el.selectionEnd != null ? el.selectionEnd : el.value.length;
                        el.value = el.value.slice(0, a) + text + el.value.slice(b);
                        el.selectionStart = el.selectionEnd = a + text.length;
                        el.dispatchEvent(new Event('input', { bubbles: true }));
                        plog('wstawione do', el.tagName);
                        return true;
                    }

                    try {
                        if (document.execCommand('insertText', false, text)) {
                            plog('wstawione przez execCommand');
                            return true;
                        }
                    } catch (err) { plog('execCommand wyjatek:', err && err.message); }

                    const sel = window.getSelection();
                    if (!sel || sel.rangeCount === 0) { plog('brak zaznaczenia'); return false; }
                    const range = sel.getRangeAt(0);
                    range.deleteContents();
                    const node = document.createTextNode(text);
                    range.insertNode(node);
                    range.setStartAfter(node);
                    range.setEndAfter(node);
                    sel.removeAllRanges();
                    sel.addRange(range);
                    el.dispatchEvent(new InputEvent('input', {
                        bubbles: true, inputType: 'insertText', data: text
                    }));
                    plog('wstawione przez Range');
                    return true;
                };

                let handledAt = 0;

                // Wstawia obraz ze schowka jako plik. Najpierw probujemy
                // syntetycznego zdarzenia 'paste' z DataTransfer, a gdy nikt go
                // nie obsluzy (brak preventDefault) - sekwencji przeciagniecia,
                // ktora Chat obsluguje przy upuszczaniu plikow na rozmowe.
                const pasteImageFromBackend = async () => {
                    const target = document.activeElement;
                    if (!isEditable(target)) return;

                    let b64 = '';
                    try {
                        b64 = await invokeTauri('read_clipboard_image');
                    } catch (err) {
                        plog('brak obrazu w schowku:', err);
                        return;
                    }
                    if (!b64) return;

                    let file;
                    try {
                        const bin = atob(b64);
                        const bytes = new Uint8Array(bin.length);
                        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
                        file = new File([bytes], 'wklejony-obraz.png', { type: 'image/png' });
                        plog('obraz z backendu,', bytes.length, 'bajtow');
                    } catch (err) {
                        plog('blad dekodowania obrazu:', err && err.message);
                        return;
                    }

                    const dt2 = new DataTransfer();
                    dt2.items.add(file);

                    let handled = false;
                    try {
                        const ev = new ClipboardEvent('paste', {
                            clipboardData: dt2, bubbles: true, cancelable: true
                        });
                        // dispatchEvent zwraca false, gdy ktos zrobil preventDefault
                        handled = !target.dispatchEvent(ev);
                        plog('syntetyczne paste obsluzone:', handled);
                    } catch (err) {
                        plog('syntetyczne paste niedostepne:', err && err.message);
                    }

                    if (handled) return;

                    try {
                        ['dragenter', 'dragover', 'drop'].forEach((type) => {
                            target.dispatchEvent(new DragEvent(type, {
                                bubbles: true, cancelable: true, dataTransfer: dt2
                            }));
                        });
                        plog('wyslana sekwencja drop');
                    } catch (err) {
                        plog('drop niedostepny:', err && err.message);
                    }
                };

                document.addEventListener('paste', (e) => {
                    const dt = e.clipboardData;
                    if (!dt) return;
                    if (!isEditable(document.activeElement)) return;

                    const types = dt.types ? Array.prototype.slice.call(dt.types) : [];
                    plog('typy w schowku:', types.join(', ') || '(brak)',
                         '| files:', dt.files ? dt.files.length : 0,
                         '| items:', dt.items ? dt.items.length : 0);

                    // Cokolwiek innego niz czysty tekst - obrazek, plik, lista
                    // URI po skopiowaniu pliku w menedzerze - oddajemy Chatowi.
                    // dt.files/dt.items bywaja puste w WebKicie, wiec decyduje
                    // dt.types, ktore jest tam wiarygodne.
                    const looksLikeFile =
                        (dt.files && dt.files.length > 0) ||
                        types.some((t) =>
                            t.indexOf('image/') === 0 ||
                            t === 'Files' ||
                            t === 'text/uri-list' ||
                            t === 'application/x-moz-file'
                        ) ||
                        (dt.items && Array.prototype.some.call(
                            dt.items, (i) => i.kind === 'file'
                        ));

                    if (looksLikeFile) {
                        plog('plik/obrazek w schowku - zostawiam Chatowi');
                        handledAt = Date.now();
                        return;
                    }

                    const text = dt.getData('text/plain');
                    if (!text) {
                        // WebKitGTK nie przekazuje obrazu do strony - zdarzenie
                        // przychodzi puste. Bierzemy obraz ze schowka przez
                        // backend i podajemy go Chatowi jako plik.
                        plog('puste zdarzenie paste - probuje obraz przez backend');
                        e.preventDefault();
                        e.stopPropagation();
                        if (e.stopImmediatePropagation) e.stopImmediatePropagation();
                        handledAt = Date.now();
                        pasteImageFromBackend();
                        return;
                    }

                    // Chat nie dostaje juz tego zdarzenia - wstawiamy sami.
                    e.preventDefault();
                    e.stopPropagation();
                    if (e.stopImmediatePropagation) e.stopImmediatePropagation();
                    plog('wymuszam zwykly tekst,', text.length, 'znakow');
                    if (insertText(text)) handledAt = Date.now();
                }, true);

                // Siatka bezpieczenstwa: gdyby zdarzenie 'paste' w ogole nie
                // przyszlo, po 150 ms czytamy schowek przez backend.
                // (navigator.clipboard.readText() jest tu blokowany przez WebKita.)
                document.addEventListener('keydown', (e) => {
                    if (!e.ctrlKey || e.altKey) return;
                    if (e.key !== 'v' && e.key !== 'V') return;
                    if (!isEditable(document.activeElement)) return;

                    const pressedAt = Date.now();
                    setTimeout(async () => {
                        if (handledAt >= pressedAt) return;
                        if (!isEditable(document.activeElement)) return;
                        plog('brak zdarzenia paste - czytam schowek przez backend');
                        let text = '';
                        try {
                            text = await invokeTauri('read_clipboard_text');
                        } catch (err) {
                            plog('backend odmowil:', err);
                            return;
                        }
                        if (!text) { plog('schowek pusty'); return; }
                        insertText(text);
                    }, 150);
                }, true);
            }
        });
    "#;

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            create_notification_window,
            play_notification_sound,
            upload_file_stream,
            close_notification_window,
            show_main_window,
            set_tray_alert,
            get_settings,
            save_settings,
            open_settings_window,
            read_clipboard_text,
            read_clipboard_image,
            js_log
        ])
        .setup(move |app| {
            let settings = load_settings(&app.handle());

            let show_menu = MenuItem::with_id(app, "show", "Pokaż", true, None::<&str>)?;
            let settings_menu = MenuItem::with_id(app, "settings", "Ustawienia", true, None::<&str>)?;
            let quit_menu = MenuItem::with_id(app, "quit", "Zakończ", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_menu, &settings_menu, &quit_menu])?;

            let _tray = TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id() == "quit" {
                        app.exit(0);
                    } else if event.id() == "show" {
                        present_main_window(app);
                    } else if event.id() == "settings" {
                        let _ = open_settings_window(app.clone());
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        present_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            let visible = !settings.start_minimized;

            // Siatka bezpieczenstwa: gdyby link ominal handler w JS
            // (kod Google ustawia location.href, przekierowanie 302 itd.),
            // przechwytujemy nawigacje tutaj i oddajemy ja przegladarce.
            let nav_handle = app.handle().clone();

            let _ = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::External("https://chat.google.com".parse().unwrap())
                )
                .title("Google Chat by ism")
                .inner_size(1280.0, 800.0)
                .visible(visible)
                .initialization_script(inject_script)
                .on_download(|webview, event| {
                    // Webview zapisuje zalacznik po cichu do domyslnego katalogu
                    // pobierania i nie daje zadnego znaku - stad powiadomienie.
                    match event {
                        DownloadEvent::Requested { url, destination } => {
                            println!(
                                "[ISM-Chat] Pobieranie: {} -> {}",
                                url,
                                destination.display()
                            );
                        }
                        DownloadEvent::Finished { url: _, path, success } => {
                            if !success {
                                println!("[ISM-Chat] Pobieranie nie powiodlo sie");
                                return true;
                            }
                            let name = path
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .unwrap_or("plik")
                                .to_string();
                            let full = path
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default();
                            println!("[ISM-Chat] Pobrano: {}", full);

                            let handle = webview.app_handle().clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = create_notification_window(
                                    handle,
                                    format!("Pobrano: {}", name),
                                    full,
                                )
                                .await;
                            });
                        }
                        _ => (),
                    }
                    // true = pozwalamy pobieraniu isc dalej
                    true
                })
                .on_navigation(move |url| {
                    if is_internal_url(url) {
                        return true;
                    }
                    let target = unwrap_google_redirect(url);
                    println!("[ISM-Chat] Nawigacja zewnetrzna -> przegladarka: {}", target);
                    let _ = nav_handle.shell().open(target, None);
                    false
                })
                .build()?;

            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
