use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder,
};

const PROXY_BASE: &str = "http://127.0.0.1:8080";

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // Start as menu-bar-only; tray must register before any window appears.
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            build_tray(app)?;

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
                    #[cfg(target_os = "macos")]
                    let _ = handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            });

            // Open dashboard on first launch.
            show_window(app.handle());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_proxy,
            stop_proxy,
            proxy_status,
            read_agent_configs,
            apply_agent_config,
            reset_agent_config,
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

// ── Agent config helpers ────────────────────────────────────────────────────

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
}

#[derive(serde::Serialize)]
struct AgentStatus {
    config_path: String,
    config_exists: bool,
    active: bool,
    model: String,
}

fn read_claude_code_status() -> AgentStatus {
    let path = home_dir().join(".claude").join("settings.json");
    let config_path = path.to_string_lossy().to_string();
    if !path.exists() {
        return AgentStatus { config_path, config_exists: false, active: false, model: String::new() };
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    let active = json["env"]["ANTHROPIC_BASE_URL"].as_str()
        == Some("http://localhost:8080/anthropic");
    let model = json["model"].as_str().unwrap_or("").to_string();
    AgentStatus { config_path, config_exists: true, active, model }
}

fn read_codex_status() -> AgentStatus {
    let path = home_dir().join(".codex").join("config.toml");
    let config_path = path.to_string_lossy().to_string();
    if !path.exists() {
        return AgentStatus { config_path, config_exists: false, active: false, model: String::new() };
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let val: toml::Value = toml::from_str(&raw)
        .unwrap_or(toml::Value::Table(toml::map::Map::new()));
    let active = val
        .get("model_providers").and_then(|mp| mp.get("llmproxy"))
        .and_then(|p| p.get("base_url")).and_then(|v| v.as_str())
        == Some("http://localhost:8080/openai/v1");
    let model = val.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    AgentStatus { config_path, config_exists: true, active, model }
}

fn read_gemini_status() -> AgentStatus {
    let path = home_dir().join(".gemini").join("settings.json");
    let config_path = path.to_string_lossy().to_string();
    if !path.exists() {
        return AgentStatus { config_path, config_exists: false, active: false, model: String::new() };
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    let active = json["apiEndpoint"].as_str() == Some("http://localhost:8080/gemini");
    let model = json["model"].as_str()
        .or_else(|| json["model"]["name"].as_str())
        .unwrap_or("").to_string();
    AgentStatus { config_path, config_exists: true, active, model }
}

#[tauri::command]
fn read_agent_configs() -> serde_json::Value {
    serde_json::json!({
        "claude_code": read_claude_code_status(),
        "codex":       read_codex_status(),
        "gemini":      read_gemini_status(),
    })
}

#[tauri::command]
fn apply_agent_config(agent: String, model: String) -> Result<(), String> {
    match agent.as_str() {
        "claude_code" => apply_claude_code(&model),
        "codex"       => apply_codex(&model),
        "gemini"      => apply_gemini(&model),
        _             => Err(format!("unknown agent: {agent}")),
    }
}

fn apply_claude_code(model: &str) -> Result<(), String> {
    let path = home_dir().join(".claude").join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    let mut json: serde_json::Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let env = json["env"].take();
    let mut env_map = if let serde_json::Value::Object(m) = env {
        m
    } else {
        serde_json::Map::new()
    };
    env_map.insert("ANTHROPIC_BASE_URL".into(),
        serde_json::Value::String("http://localhost:8080/anthropic".into()));
    env_map.insert("ANTHROPIC_API_KEY".into(),
        serde_json::Value::String("llmproxy".into()));
    json["env"] = serde_json::Value::Object(env_map);
    json["model"] = serde_json::Value::String(model.into());

    std::fs::write(&path, serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn apply_codex(model: &str) -> Result<(), String> {
    let path = home_dir().join(".codex").join("config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    let mut table: toml::map::Map<String, toml::Value> = if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        match toml::from_str::<toml::Value>(&raw) {
            Ok(toml::Value::Table(t)) => t,
            _ => toml::map::Map::new(),
        }
    } else {
        toml::map::Map::new()
    };

    table.insert("model_provider".into(), toml::Value::String("llmproxy".into()));
    table.insert("model".into(), toml::Value::String(model.into()));
    table.insert("api_key".into(), toml::Value::String("llmproxy".into()));
    table.insert("disable_response_storage".into(), toml::Value::Boolean(true));

    // [model_providers.llmproxy]
    let mut provider = toml::map::Map::new();
    provider.insert("name".into(), toml::Value::String("llmproxy".into()));
    provider.insert("base_url".into(),
        toml::Value::String("http://localhost:8080/openai/v1".into()));
    provider.insert("wire_api".into(), toml::Value::String("responses".into()));
    provider.insert("requires_openai_auth".into(), toml::Value::Boolean(true));

    let mut model_providers = toml::map::Map::new();
    model_providers.insert("llmproxy".into(), toml::Value::Table(provider));
    table.insert("model_providers".into(), toml::Value::Table(model_providers));

    std::fs::write(&path,
        toml::to_string(&toml::Value::Table(table)).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn apply_gemini(model: &str) -> Result<(), String> {
    let path = home_dir().join(".gemini").join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;

    let mut json: serde_json::Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    json["apiEndpoint"] = serde_json::Value::String("http://localhost:8080/gemini".into());
    json["model"] = serde_json::Value::String(model.into());

    std::fs::write(&path, serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reset_agent_config(agent: String) -> Result<(), String> {
    match agent.as_str() {
        "claude_code" => reset_claude_code(),
        "codex"       => reset_codex(),
        "gemini"      => reset_gemini(),
        _             => Err(format!("unknown agent: {agent}")),
    }
}

fn reset_claude_code() -> Result<(), String> {
    let path = home_dir().join(".claude").join("settings.json");
    if !path.exists() { return Ok(()); }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|_| serde_json::json!({}));
    if let Some(env) = json["env"].as_object_mut() {
        env.remove("ANTHROPIC_BASE_URL");
        env.remove("ANTHROPIC_API_KEY");
    }
    json.as_object_mut().map(|o| o.remove("model"));
    std::fs::write(&path, serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn reset_codex() -> Result<(), String> {
    let path = home_dir().join(".codex").join("config.toml");
    if !path.exists() { return Ok(()); }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut table: toml::map::Map<String, toml::Value> =
        match toml::from_str::<toml::Value>(&raw) {
            Ok(toml::Value::Table(t)) => t,
            _ => return Ok(()),
        };
    table.remove("model_provider");
    table.remove("model");
    table.remove("api_key");
    table.remove("disable_response_storage");
    if let Some(toml::Value::Table(ref mut mp)) = table.get_mut("model_providers") {
        mp.remove("llmproxy");
        if mp.is_empty() {
            table.remove("model_providers");
        }
    }
    std::fs::write(&path,
        toml::to_string(&toml::Value::Table(table)).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

fn reset_gemini() -> Result<(), String> {
    let path = home_dir().join(".gemini").join("settings.json");
    if !path.exists() { return Ok(()); }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|_| serde_json::json!({}));
    if let Some(o) = json.as_object_mut() {
        o.remove("apiEndpoint");
        o.remove("model");
    }
    std::fs::write(&path, serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
