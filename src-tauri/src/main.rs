// Prevents additional console window on Windows in release
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(not(target_os = "linux"))]
use tauri::{AppHandle, WebviewWindowBuilder, WebviewUrl};
#[cfg(target_os = "linux")]
use tauri::AppHandle;

use rodio::{Decoder, OutputStream, Sink};
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

// We need a lazy static mutex for audio to prevent issues with dropped streams,
// but lazy_static isn't added. We can just instantiate OutputStream inside the thread.

#[tauri::command]
async fn create_notification_window(app: AppHandle, title: String, body: String) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = body; // Unused for now in webview mode
        let id = WINDOW_COUNTER.fetch_add(1, Ordering::SeqCst);
        let label = format!("notif_{}", id);
        
        let window = WebviewWindowBuilder::new(
            &app,
            label,
            WebviewUrl::App("notification.html".into())
        )
        .title(title)
        .inner_size(320.0, 100.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .build()
        .map_err(|e| e.to_string())?;

        if let Ok(Some(monitor)) = window.primary_monitor() {
            let scale_factor = monitor.scale_factor();
            let size = window.outer_size().unwrap_or(tauri::PhysicalSize::new(
                (320.0 * scale_factor) as u32,
                (100.0 * scale_factor) as u32,
            ));
            let monitor_size = monitor.size();
            let monitor_pos = monitor.position();

            let margin_x = (20.0 * scale_factor) as i32;
            let margin_y = (60.0 * scale_factor) as i32;

            let x = monitor_pos.x + monitor_size.width as i32 - size.width as i32 - margin_x;
            let y = monitor_pos.y + monitor_size.height as i32 - size.height as i32 - margin_y;

            let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = app;
        notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .appname("Google Chat")
            .show()
            .map_err(|e| format!("Błąd wysyłania powiadomienia D-Bus: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
fn play_notification_sound(volume: f32) -> Result<(), String> {
    let safe_volume = volume.clamp(0.0, 1.0);
    
    thread::spawn(move || {
        // Obtaining an output stream in rodio v0.19
        if let Ok((_stream, stream_handle)) = OutputStream::try_default() {
            if let Ok(sink) = Sink::try_new(&stream_handle) {
                sink.set_volume(safe_volume);

                // TODO: Dostarczyć zasób notif.mp3 do folderu assets/
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

    // Asynchroniczne potwierdzenie otwarcia pliku
    let mut _file = async_fs::File::open(&file_path)
        .await
        .map_err(|e| format!("Błąd otwarcia pliku: {}", e))?;

    // Zamockowanie operacji
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

fn main() {
    #[cfg(target_os = "linux")]
    std::env::set_var("GDK_BACKEND", "x11");

    let inject_script = r#"
        window.addEventListener('DOMContentLoaded', () => {
            let hasNotified = false;

            // Wymuszamy aby wyskakujace okna logowania otwieraly sie w naszej aplikacji Tauri (blokowanie target="_blank")
            window.open = function(url, name, features) {
                window.location.href = url;
                return null;
            };

            document.addEventListener('click', (e) => {
                const a = e.target.closest('a');
                if (a && a.target === '_blank') {
                    // Przechwytywacz zmusza Google do przejscia dalej wewnatrz glownego okna zamiast gubic akcje
                    a.target = '_self';
                }
            }, true);

            const invokeTauri = (cmd, args) => {
                if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
                    window.__TAURI__.core.invoke(cmd, args).catch(console.error);
                }
            };
            
            const titleEl = document.querySelector('title');
            if (titleEl) {
                const observer = new MutationObserver((mutations) => {
                    const title = document.title;
                    if (title.match(/^\(\d+\)/) || title.includes("napisa")) {
                        if (!hasNotified) {
                            hasNotified = true;
                            let safeTitle = "Nowa wiadomość";
                            if (title.includes("napisa")) {
                                safeTitle = title.split(" -")[0] || title;
                            }
                            invokeTauri('play_notification_sound', { volume: 0.5 });
                            invokeTauri('create_notification_window', { 
                                title: safeTitle, 
                                body: "Sprawdź zakładkę z aplikacją Google Chat" 
                            });
                        }
                    } else if (title === "Google Chat" || title === "Chat") {
                        if (hasNotified) {
                            hasNotified = false;
                            invokeTauri('close_notification_window', {});
                        }
                    }
                });
                observer.observe(titleEl, { subtree: true, characterData: true, childList: true });
            }
            
            window.addEventListener('focus', () => { 
                if (hasNotified) {
                    hasNotified = false; 
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
            close_notification_window
        ])
        .setup(move |app| {
            let show_menu = MenuItem::with_id(app, "show", "Pokaż", true, None::<&str>)?;
            let quit_menu = MenuItem::with_id(app, "quit", "Zakończ", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_menu, &quit_menu])?;
            
            let _tray = TrayIconBuilder::new()
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
                // Linux: WebKitGTK requires similar builder
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
