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

    // Na Linuxie mozna uzyc natywnych powiadomien D-Bus
    #[cfg(target_os = "linux")]
    if settings.use_native_notifications {
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
    let safe_volume = (volume * settings.sound_volume).clamp(0.0, 1.0);

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

#[tauri::command]
fn show_main_window(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
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
    #[cfg(target_os = "linux")]
    std::env::set_var("GDK_BACKEND", "x11");

    let inject_script = r#"
        window.addEventListener('DOMContentLoaded', () => {
            if (window.__ISM_CHAT_INIT) return;
            window.__ISM_CHAT_INIT = true;

            let trayAlertActive = false;
            let lastCount = 0;

            console.log('[ISM-Chat] Skrypt wstrzykniety');

            window.open = function(url, name, features) {
                // Linki do Google - otwieraj wewnatrz, reszta w przegladarce
                try {
                    const u = new URL(url, window.location.href);
                    if (u.hostname.endsWith('google.com')) {
                        window.location.href = url;
                        return null;
                    }
                } catch(e) {}
                // Zewnetrzny link -> otwieramy w przegladarce systemowej
                if (window.__TAURI__ && window.__TAURI__.core) {
                    window.__TAURI__.core.invoke('plugin:shell|open', { path: url }).catch(() => {});
                }
                return null;
            };

            document.addEventListener('click', (e) => {
                const a = e.target.closest('a');
                if (!a || !a.href) return;
                try {
                    const u = new URL(a.href);
                    if (u.hostname.endsWith('google.com')) {
                        // Linki Google - zostaja wewnatrz
                        if (a.target === '_blank') a.target = '_self';
                    } else {
                        // Linki zewnetrzne - otwieramy w przegladarce
                        e.preventDefault();
                        if (window.__TAURI__ && window.__TAURI__.core) {
                            window.__TAURI__.core.invoke('plugin:shell|open', { path: a.href }).catch(() => {});
                        }
                    }
                } catch(err) {}
            }, true);

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
        });
    "#;

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
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
            open_settings_window
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
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.unminimize();
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    } else if event.id() == "settings" {
                        let _ = open_settings_window(app.clone());
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.unminimize();
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            let visible = !settings.start_minimized;

            let _ = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::External("https://chat.google.com".parse().unwrap())
                )
                .title("Google Chat by ism")
                .inner_size(1280.0, 800.0)
                .visible(visible)
                .initialization_script(inject_script)
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
