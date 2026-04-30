//! Shared on-disk OAuth token store at `~/.config/llmproxy/oauth_tokens.json`.
//! Both the Tauri app (writes after auth) and the proxy binary (reads at startup)
//! use this file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OAuthStore {
    #[serde(default)]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot: Option<crate::copilot_auth::CopilotCreds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<crate::codex_oauth::CodexCreds>,
}

/// `dir` is the config directory (e.g. `~/.config/llmproxy`).
/// The store is always at `dir/oauth_tokens.json`.
pub fn load_store(dir: &std::path::Path) -> OAuthStore {
    let path = dir.join("oauth_tokens.json");
    if !path.exists() {
        return OAuthStore::default();
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_store(dir: &std::path::Path, store: &OAuthStore) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join("oauth_tokens.json");
    let content = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;

    // Atomic write via temp file
    let tmp = dir.join("oauth_tokens.json.tmp");
    std::fs::write(&tmp, &content).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }

    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}
