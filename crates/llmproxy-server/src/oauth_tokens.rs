//! Reads `~/.config/llmproxy/oauth_tokens.json` to obtain long-lived
//! credentials (GitHub token for Copilot, refresh token for Codex).

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Default)]
struct OAuthStore {
    #[serde(default)]
    copilot: Option<CopilotCreds>,
    #[serde(default)]
    codex: Option<CodexCreds>,
    #[serde(default)]
    databricks: Option<DatabricksCreds>,
    #[serde(default)]
    anthropic: Option<AnthropicCreds>,
}

#[derive(Debug, Clone, Deserialize)]
struct CopilotCreds {
    github_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexCreds {
    refresh_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DatabricksCreds {
    workspace_url: String,
    refresh_token: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicCreds {
    refresh_token: String,
}

#[derive(Debug, Clone, Default)]
pub struct OAuthTokens {
    /// Long-lived GitHub OAuth token; used to mint short-lived Copilot tokens.
    pub copilot_github_token: Option<String>,
    /// Long-lived OpenAI refresh token; used to refresh short-lived access tokens.
    pub codex_refresh_token: Option<String>,
    /// Databricks workspace URL; required alongside the refresh token.
    pub databricks_workspace_url: Option<String>,
    /// Long-lived Databricks refresh token; used to refresh short-lived access tokens.
    pub databricks_refresh_token: Option<String>,
    /// Long-lived Anthropic refresh token; used to refresh short-lived access tokens.
    pub anthropic_refresh_token: Option<String>,
}

impl OAuthTokens {
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join("oauth_tokens.json");
        if !path.exists() {
            return Self::default();
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to read {}: {e}", path.display());
                return Self::default();
            }
        };
        let store: OAuthStore = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to parse {}: {e}", path.display());
                return Self::default();
            }
        };
        Self {
            copilot_github_token: store.copilot.map(|c| c.github_token),
            codex_refresh_token: store.codex.map(|c| c.refresh_token),
            databricks_workspace_url: store.databricks.as_ref().map(|c| c.workspace_url.clone()),
            databricks_refresh_token: store.databricks.map(|c| c.refresh_token),
            anthropic_refresh_token: store.anthropic.map(|c| c.refresh_token),
        }
    }
}
