// Prevents additional console window on Windows in release
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(not(target_os = "linux"))]
use tauri::{AppHandle, WebviewWindowBuilder, WebviewUrl};
#[cfg(target_os = "linux")]
use tauri::AppHandle;

use tauri::image::Image as TauriImage;
use image::GenericImageView;
use rodio::{Decoder, OutputStream, Sink};

fn png_to_tauri_icon(png_bytes: &[u8]) -> TauriImage<'static> {
    let img = image::load_from_memory(png_bytes).expect("Nie udalo sie zdekodowac ikony PNG");
    let rgba = img.to_rgba8();
    let (w, h) = img.dimensions();
    TauriImage::new_owned(rgba.into_raw(), w, h)
}
use std::fs::File;
use std::thread;
use std::io::BufReader;
use tokio::fs as async_fs;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};

use tauri::Manager;

#[cfg(not(target_os = "linux"))]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(target_os = "linux"))]
static WINDOW_COUNTER: AtomicUsize = AtomicUsize::new(0);

static ICON_NORMAL: &[u8] = include_bytes!("../icons/32x32.png");
static ICON_ALERT: &[u8] = include_bytes!("../icons/32x32-alert.png");

#[tauri::command]
async fn create_notification_window(app: AppHandle, title: String, body: String) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = body;
        let id = WINDOW_COUNTER.fetch_add(1, Ordering::SeqCst);
        let label = format!("notif_{}", id);
        
        let window = WebviewWindowBuilder::new(
            &app,
            label,
            WebviewUrl::App("notification.html".into())
        )
        .title(title)
        .inner_size(360.0, 80.0)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;

        if let Ok(Some(monitor)) = window.primary_monitor() {
            let scale_factor = monitor.scale_factor();
            let size = window.outer_size().unwrap_or(tauri::PhysicalSize::new(
                (360.0 * scale_factor) as u32,
                (80.0 * scale_factor) as u32,
            ));
            let monitor_size = monitor.size();
            let monitor_pos = monitor.position();

            let margin_x = (12.0 * scale_factor) as i32;
            let margin_y = (50.0 * scale_factor) as i32;

            let x = monitor_pos.x + monitor_size.width as i32 - size.width as i32 - margin_x;
            let y = monitor_pos.y + monitor_size.height as i32 - size.height as i32 - margin_y;

            let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        }

        let _ = window.show();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = app;
        notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .appname("Google Chat")
            .icon("mail-message-new")
            .timeout(notify_rust::Timeout::Milliseconds(8000))
            .show()
            .map_err(|e| format!("Błąd wysyłania powiadomienia D-Bus: {}", e))?;
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
fn play_notification_sound(volume: f32) -> Result<(), String> {
    let safe_volume = volume.clamp(0.0, 1.0);
    
    thread::spawn(move || {
        if let Ok((_stream, stream_handle)) = OutputStream::try_default() {
            if let Ok(sink) = Sink::try_new(&stream_handle) {
                sink.set_volume(safe_volume);
                if let Ok(file) = File::open("assets/notif.mp3") {
                    if let Ok(source) = Decoder::new(BufReader::new(file)) {
                        sink.append(source);
                        sink.sleep_until_end();
                    }
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

    println!("=== ROZPOCZĘTO SYMULACJĘ UPLOADU ===");
    println!("Nazwa pliku: {}", file_name);
    println!("Rozmiar pliku: {} bajtów", size);
    println!("======================================");

    Ok(format!("Zakończono odczyt pliku ({}) o rozmiarze {} bajtów.", file_name, size))
}

#[tauri::command]
fn close_notification_window(#[allow(unused_variables)] app: tauri::AppHandle) {
    #[cfg(not(target_os = "linux"))]
    {
        for (label, window) in app.webview_windows() {
            if label.starts_with("notif_") {
                let _ = window.close();
            }
        }
    }
}

#[tauri::command]
fn show_main_window(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    std::env::set_var("GDK_BACKEND", "x11");

    let inject_script = r#"
        window.addEventListener('DOMContentLoaded', () => {
            let hasNotified = false;
            let activeNotifications = 0;

            console.log('[ISM-Chat] Skrypt wstrzykniety');

            window.open = function(url, name, features) {
                window.location.href = url;
                return null;
            };

            document.addEventListener('click', (e) => {
                const a = e.target.closest('a');
                if (a && a.target === '_blank') {
                    a.target = '_self';
                }
            }, true);

            const invokeTauri = (cmd, args) => {
                if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
                    console.log('[ISM-Chat] invoke:', cmd);
                    window.__TAURI__.core.invoke(cmd, args).catch(e => console.error('[ISM-Chat] invoke BLAD:', cmd, e));
                }
            };

            // === 1. Przechwycenie window.Notification ===
            // Google Chat wywoluje new Notification() przy nowej wiadomosci
            const NativeNotification = window.Notification;

            const ProxyNotification = function(title, options) {
                const instance = new NativeNotification(title, options);
                activeNotifications++;
                console.log('[ISM-Chat] Notification przechwycony:', title, 'aktywne:', activeNotifications);

                if (!hasNotified) {
                    hasNotified = true;
                    invokeTauri('play_notification_sound', { volume: 0.5 });
                    invokeTauri('create_notification_window', {
                        title: title || "Nowa wiadomość",
                        body: (options && options.body) || "Sprawdź Google Chat"
                    });
                    invokeTauri('set_tray_alert', { alert: true });
                }

                instance.addEventListener('close', () => {
                    activeNotifications = Math.max(0, activeNotifications - 1);
                    console.log('[ISM-Chat] Notification zamkniety, aktywne:', activeNotifications);
                });

                return instance;
            };

            Object.assign(ProxyNotification, NativeNotification);
            ProxyNotification.prototype = NativeNotification.prototype;
            window.Notification = ProxyNotification;

            // === 2. Szukanie nieprzeczytanych w DOM (polski UI) ===
            const searchAriaLabel = (root, text) => {
                if (root.nodeType === Node.ELEMENT_NODE) {
                    const label = root.getAttribute && root.getAttribute('aria-label');
                    if (label && label.includes(text)) return root;
                    for (const child of root.childNodes) {
                        const found = searchAriaLabel(child, text);
                        if (found) return found;
                    }
                }
                return null;
            };

            const getUnreadCount = () => {
                // Szukaj po polsku i angielsku
                const el = searchAriaLabel(document.documentElement, "nieprzeczytan")
                        || searchAriaLabel(document.documentElement, "unread");
                if (el && el.textContent) {
                    const m = el.textContent.match(/\d+/);
                    if (m) return parseInt(m[0], 10);
                }
                return 0;
            };

            // === 3. Polling co 3s - sprawdza czy wiadomosci odczytane ===
            setInterval(() => {
                const count = getUnreadCount();
                if (count === 0 && hasNotified && activeNotifications === 0) {
                    hasNotified = false;
                    console.log('[ISM-Chat] Wszystko odczytane');
                    invokeTauri('close_notification_window', {});
                    invokeTauri('set_tray_alert', { alert: false });
                }
            }, 3000);

            window.addEventListener('focus', () => {
                if (hasNotified) {
                    invokeTauri('close_notification_window', {});
                }
            });
        });
    "#;

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            create_notification_window,
            play_notification_sound,
            upload_file_stream,
            close_notification_window,
            show_main_window,
            set_tray_alert
        ])
        .setup(move |app| {
            let show_menu = MenuItem::with_id(app, "show", "Pokaż", true, None::<&str>)?;
            let quit_menu = MenuItem::with_id(app, "quit", "Zakończ", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_menu, &quit_menu])?;
            
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

            #[cfg(not(target_os = "linux"))]
            {
                let _ = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::External("https://chat.google.com".parse().unwrap())
                )
                .title("Google Chat Native")
                .inner_size(1280.0, 800.0)
                .initialization_script(inject_script)
                .build()?;
            }
            #[cfg(target_os = "linux")]
            {
                let _ = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::External("https://chat.google.com".parse().unwrap())
                )
                .title("Google Chat Native")
                .inner_size(1280.0, 800.0)
                .initialization_script(inject_script)
                .build()?;
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
