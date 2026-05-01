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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub databricks: Option<crate::databricks_oauth::DatabricksCreds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<crate::anthropic_oauth::AnthropicCreds>,
}

/// `dir` is the config directory (e.g. `~/.config/llmproxy`).
/// Returns an error if the file exists but cannot be read or parsed, so
/// callers can avoid overwriting valid credentials on a transient I/O failure.
pub fn load_store(dir: &std::path::Path) -> Result<OAuthStore, String> {
    let path = dir.join("oauth_tokens.json");
    if !path.exists() {
        return Ok(OAuthStore::default());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read OAuth store {}: {}", path.display(), e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse OAuth store {}: {}", path.display(), e))
}

pub fn save_store(dir: &std::path::Path, store: &OAuthStore) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join("oauth_tokens.json");
    let content = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;

    // Atomic write via temp file.
    let tmp = dir.join("oauth_tokens.json.tmp");
    std::fs::write(&tmp, &content).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }

    // On Windows rename fails if the destination exists; remove it first.
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }

    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}
