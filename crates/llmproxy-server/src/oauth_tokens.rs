//! Reads `~/.config/llmproxy/oauth_tokens.json` to obtain long-lived
//! credentials (GitHub token for Copilot, refresh token for Codex).

use std::path::Path;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
struct OAuthStore {
    #[serde(default)]
    copilot: Option<CopilotCreds>,
    #[serde(default)]
    codex: Option<CodexCreds>,
}

#[derive(Debug, Clone, Deserialize)]
struct CopilotCreds {
    github_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexCreds {
    refresh_token: String,
}

#[derive(Debug, Clone, Default)]
pub struct OAuthTokens {
    /// Long-lived GitHub OAuth token; used to mint short-lived Copilot tokens.
    pub copilot_github_token: Option<String>,
    /// Long-lived OpenAI refresh token; used to refresh short-lived access tokens.
    pub codex_refresh_token: Option<String>,
}

impl OAuthTokens {
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join("oauth_tokens.json");
        if !path.exists() {
            return Self::default();
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let store: OAuthStore = serde_json::from_str(&content).unwrap_or_default();
        Self {
            copilot_github_token: store.copilot.map(|c| c.github_token),
            codex_refresh_token: store.codex.map(|c| c.refresh_token),
        }
    }
}
