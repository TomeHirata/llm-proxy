//! Reads `~/.config/llmproxy/oauth_tokens.json` to obtain long-lived
//! credentials (GitHub token for Copilot, refresh token for Codex,
//! access token for Databricks).

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
    access_token: String,
    #[serde(default)]
    expires_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct OAuthTokens {
    /// Long-lived GitHub OAuth token; used to mint short-lived Copilot tokens.
    pub copilot_github_token: Option<String>,
    /// Long-lived OpenAI refresh token; used to refresh short-lived access tokens.
    pub codex_refresh_token: Option<String>,
    /// Short-lived Databricks access token from browser OAuth flow.
    pub databricks_access_token: Option<String>,
    /// Databricks workspace URL saved at OAuth time (used to register the provider).
    pub databricks_workspace_url: Option<String>,
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

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let (databricks_access_token, databricks_workspace_url) = match store.databricks {
            None => (None, None),
            Some(d) => {
                let workspace_url = Some(d.workspace_url);
                let access_token = if d.expires_at.map(|exp| exp > now).unwrap_or(true) {
                    Some(d.access_token)
                } else {
                    tracing::info!("Databricks OAuth access token has expired; re-authenticate via the llmproxy app");
                    None
                };
                (access_token, workspace_url)
            }
        };

        Self {
            copilot_github_token: store.copilot.map(|c| c.github_token),
            codex_refresh_token: store.codex.map(|c| c.refresh_token),
            databricks_access_token,
            databricks_workspace_url,
        }
    }
}
