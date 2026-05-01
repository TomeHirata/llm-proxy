//! GitHub Copilot OAuth — device code flow for Claude Code authentication.
//!
//! Flow:
//! 1. `copilot_start_device_flow` → returns user_code + verification_uri
//! 2. User visits verification_uri and enters user_code
//! 3. `copilot_poll_device_flow(device_code)` → returns Some(login) when done
//! 4. Token stored in `~/.config/llmproxy/oauth_tokens.json`

use reqwest::Client;
use serde::{Deserialize, Serialize};

const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const USER_URL: &str = "https://api.github.com/user";

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TokenPollResponse {
    Success { access_token: String },
    Pending { error: String },
}

#[derive(Deserialize)]
struct GitHubUser {
    login: String,
    #[serde(default)]
    avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotAccount {
    pub login: String,
    pub avatar_url: Option<String>,
    pub authenticated_at: i64,
}

/// Persisted copilot credentials (github_token is the long-lived OAuth token).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CopilotCreds {
    pub github_token: String,
    pub login: String,
    pub avatar_url: Option<String>,
    pub authenticated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceFlowInfo {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

pub async fn start_device_flow() -> Result<DeviceFlowInfo, String> {
    let client = Client::new();
    let resp = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[("client_id", GITHUB_CLIENT_ID), ("scope", "read:user")])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!(
            "GitHub device code request failed: {}",
            resp.status()
        ));
    }

    let dc: DeviceCodeResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(DeviceFlowInfo {
        device_code: dc.device_code,
        user_code: dc.user_code,
        verification_uri: dc.verification_uri,
        expires_in: dc.expires_in,
        interval: dc.interval,
    })
}

/// Poll once. Returns `Some(CopilotAccount)` on success, `None` while pending,
/// or an error if the flow expired or was denied.
pub async fn poll_device_flow(
    device_code: &str,
    oauth_path: &std::path::Path,
) -> Result<Option<CopilotAccount>, String> {
    let client = Client::new();
    let resp = client
        .post(ACCESS_TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", GITHUB_CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("GitHub token poll failed: {}", resp.status()));
    }

    let poll: TokenPollResponse = resp.json().await.map_err(|e| e.to_string())?;

    match poll {
        TokenPollResponse::Pending { error } => {
            if error == "authorization_pending" || error == "slow_down" {
                return Ok(None);
            }
            Err(format!("Authorization failed: {error}"))
        }
        TokenPollResponse::Success { access_token } => {
            let user = fetch_github_user(&access_token).await?;
            let creds = CopilotCreds {
                github_token: access_token,
                login: user.login.clone(),
                avatar_url: user.avatar_url.clone(),
                authenticated_at: chrono::Utc::now().timestamp(),
            };
            save_copilot_creds(oauth_path, &creds)?;
            Ok(Some(CopilotAccount {
                login: user.login,
                avatar_url: user.avatar_url,
                authenticated_at: creds.authenticated_at,
            }))
        }
    }
}

async fn fetch_github_user(token: &str) -> Result<GitHubUser, String> {
    let client = Client::new();
    let resp = client
        .get(USER_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "llmproxy")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub user fetch failed: {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn save_copilot_creds(oauth_path: &std::path::Path, creds: &CopilotCreds) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path).unwrap_or_default();
    let updated = crate::oauth_store::OAuthStore {
        copilot: Some(creds.clone()),
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn clear_copilot_creds(oauth_path: &std::path::Path) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path).unwrap_or_default();
    let updated = crate::oauth_store::OAuthStore {
        copilot: None,
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn read_copilot_account(oauth_path: &std::path::Path) -> Option<CopilotAccount> {
    crate::oauth_store::load_store(oauth_path)
        .ok()?
        .copilot
        .map(|c| CopilotAccount {
            login: c.login,
            avatar_url: c.avatar_url,
            authenticated_at: c.authenticated_at,
        })
}
