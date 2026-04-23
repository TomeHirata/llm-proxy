use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder,
};

const PROXY_BASE: &str = "http://127.0.0.1:8080";

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // Hide the dock icon on macOS — this is a menu-bar-only app.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            build_tray(app)?;

            // Pre-create the main window (hidden). It will be shown on tray click.
            let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("llmproxy")
                .inner_size(920.0, 640.0)
                .min_inner_size(700.0, 480.0)
                .center()
                .visible(false)
                .build()?;

            // macOS: clicking the red close button hides rather than destroys.
            #[cfg(target_os = "macos")]
            let _ = win.set_closable(true);

            let handle = app.handle().clone();
            win.on_window_event(move |e| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    api.prevent_close();
                    if let Some(w) = handle.get_webview_window("main") {
                        let _ = w.hide();
                    }
                    // Revert to Accessory so the dock icon disappears again.
                    #[cfg(target_os = "macos")]
                    let _ = handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_proxy,
            stop_proxy,
            proxy_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running llmproxy app");
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Dashboard", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let start = MenuItem::with_id(app, "start", "Start Proxy", true, None::<&str>)?;
    let stop_item = MenuItem::with_id(app, "stop", "Stop Proxy", true, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&open, &sep, &start, &stop_item, &sep2, &quit])?;

    TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .tooltip("llmproxy")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_window(app),
            "start" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = do_start_proxy(&app).await;
                });
            }
            "stop" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = do_stop_proxy(&app).await;
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
        #[cfg(target_os = "macos")]
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    }
}

/// Tauri command: start the proxy daemon.
#[tauri::command]
async fn start_proxy(app: tauri::AppHandle) -> Result<String, String> {
    do_start_proxy(&app).await
}

fn llmproxy_bin() -> std::path::PathBuf {
    // In the app bundle: sidecar sits next to the main binary in Contents/MacOS/
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bin = dir.join("llmproxy");
            if bin.exists() {
                return bin;
            }
        }
    }
    // Dev fallback: rely on PATH
    std::path::PathBuf::from("llmproxy")
}

async fn do_start_proxy(_app: &tauri::AppHandle) -> Result<String, String> {
    let out = tokio::process::Command::new(llmproxy_bin())
        .args(["serve", "--daemon"])
        .output()
        .await
        .map_err(|e| format!("failed to run llmproxy: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if stderr.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            stderr
        };
        return Err(if msg.is_empty() {
            format!("llmproxy exited with {}", out.status)
        } else {
            msg
        });
    }
    Ok("started".into())
}

/// Tauri command: stop the proxy daemon.
#[tauri::command]
async fn stop_proxy(app: tauri::AppHandle) -> Result<String, String> {
    do_stop_proxy(&app).await
}

async fn do_stop_proxy(_app: &tauri::AppHandle) -> Result<String, String> {
    let out = tokio::process::Command::new(llmproxy_bin())
        .arg("stop")
        .output()
        .await
        .map_err(|e| format!("failed to run llmproxy: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if stderr.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            stderr
        };
        return Err(if msg.is_empty() {
            format!("llmproxy exited with {}", out.status)
        } else {
            msg
        });
    }
    Ok("stopped".into())
}

/// Tauri command: check whether the proxy is reachable.
#[tauri::command]
async fn proxy_status() -> bool {
    reqwest::get(format!("{PROXY_BASE}/health"))
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}
