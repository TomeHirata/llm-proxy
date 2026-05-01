//! Databricks OAuth — device code flow.
//!
//! Flow:
//! 1. `start_device_flow(workspace_url)` → returns user_code + verification_uri
//! 2. User visits verification_uri and enters user_code
//! 3. `poll_device_flow(device_code, workspace_url)` → returns Some(DatabricksAccount) when done
//! 4. Credentials stored in `~/.config/llmproxy/oauth_tokens.json`

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};

const DATABRICKS_CLIENT_ID: &str = "databricks-cli";

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
    Success {
        access_token: String,
        refresh_token: String,
    },
    Pending {
        error: String,
    },
}

#[derive(Debug, Default, Deserialize)]
struct JwtClaims {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabricksCreds {
    pub workspace_url: String,
    pub refresh_token: String,
    pub display_name: Option<String>,
    pub authenticated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabricksAccount {
    pub workspace_url: String,
    pub display_name: Option<String>,
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

/// Normalize a Databricks workspace URL: ensure https://, keep only scheme+host.
fn normalize_workspace_url(raw: &str) -> Result<String, String> {
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    // Parse and keep only scheme + host (strip any path, query, fragment)
    let parsed = reqwest::Url::parse(&with_scheme)
        .map_err(|e| format!("Invalid workspace URL '{raw}': {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("Workspace URL '{raw}' has no host"))?;
    Ok(format!("https://{host}"))
}

/// Fetch the OIDC discovery document and return (device_authorization_endpoint, token_endpoint).
async fn discover_oidc_endpoints(client: &Client, base: &str) -> Option<(String, String)> {
    let discovery_url = format!("{base}/oidc/.well-known/oauth-authorization-server");
    let resp = client.get(&discovery_url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let val: serde_json::Value = resp.json().await.ok()?;
    let device_ep = val
        .get("device_authorization_endpoint")
        .and_then(|v| v.as_str())
        .map(str::to_string)?;
    let token_ep = val
        .get("token_endpoint")
        .and_then(|v| v.as_str())
        .map(str::to_string)?;
    Some((device_ep, token_ep))
}

pub async fn start_device_flow(workspace_url: &str) -> Result<DeviceFlowInfo, String> {
    let base = normalize_workspace_url(workspace_url)?;
    let client = Client::new();

    // Prefer discovered endpoint, fall back to well-known path.
    let device_ep = if let Some((ep, _)) = discover_oidc_endpoints(&client, &base).await {
        ep
    } else {
        format!("{base}/oidc/v1/devicecode")
    };

    let url = device_ep;
    let resp = client
        .post(&url)
        .form(&[
            ("client_id", DATABRICKS_CLIENT_ID),
            ("scopes", "all-apis offline_access"),
        ])
        .send()
        .await
        .map_err(|e| format!("Databricks device code request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Databricks device code request failed ({}): {body}",
            status.as_u16()
        ));
    }

    let dc: DeviceCodeResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse device code response: {e}"))?;

    Ok(DeviceFlowInfo {
        device_code: dc.device_code,
        user_code: dc.user_code,
        verification_uri: dc.verification_uri,
        expires_in: dc.expires_in,
        interval: dc.interval,
    })
}

/// Poll once. Returns `Some(DatabricksAccount)` on success, `None` while pending.
pub async fn poll_device_flow(
    device_code: &str,
    workspace_url: &str,
    oauth_path: &std::path::Path,
) -> Result<Option<DatabricksAccount>, String> {
    let base = normalize_workspace_url(workspace_url)?;
    let client = Client::new();
    let url = if let Some((_, ep)) = discover_oidc_endpoints(&client, &base).await {
        ep
    } else {
        format!("{base}/oidc/v1/token")
    };
    let resp = client
        .post(&url)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", DATABRICKS_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| format!("Databricks token poll failed: {e}"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::BAD_REQUEST {
        // Parse body to check for authorization_pending
        let text = resp.text().await.unwrap_or_default();
        if text.contains("authorization_pending") || text.contains("slow_down") {
            return Ok(None);
        }
        // Try to parse as JSON error
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                if err == "authorization_pending" || err == "slow_down" {
                    return Ok(None);
                }
                return Err(format!("Authorization failed: {err}"));
            }
        }
        return Err(format!("Databricks token poll failed (400): {text}"));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Databricks token poll failed ({}): {body}",
            status.as_u16()
        ));
    }

    let poll: TokenPollResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    match poll {
        TokenPollResponse::Pending { error } => {
            if error == "authorization_pending" || error == "slow_down" {
                return Ok(None);
            }
            Err(format!("Authorization failed: {error}"))
        }
        TokenPollResponse::Success {
            access_token,
            refresh_token,
        } => {
            let display_name = parse_jwt_display_name(&access_token);
            let creds = DatabricksCreds {
                workspace_url: base.clone(),
                refresh_token,
                display_name: display_name.clone(),
                authenticated_at: chrono::Utc::now().timestamp(),
            };
            save_databricks_creds(oauth_path, &creds)?;
            Ok(Some(DatabricksAccount {
                workspace_url: base,
                display_name,
                authenticated_at: creds.authenticated_at,
            }))
        }
    }
}

fn parse_jwt_display_name(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: JwtClaims = serde_json::from_slice(&decoded).ok()?;
    claims.name.or(claims.email).or(claims.preferred_username)
}

fn save_databricks_creds(
    oauth_path: &std::path::Path,
    creds: &DatabricksCreds,
) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path).unwrap_or_default();
    let updated = crate::oauth_store::OAuthStore {
        databricks: Some(creds.clone()),
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn clear_databricks_creds(oauth_path: &std::path::Path) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path).unwrap_or_default();
    let updated = crate::oauth_store::OAuthStore {
        databricks: None,
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn read_databricks_account(oauth_path: &std::path::Path) -> Option<DatabricksAccount> {
    crate::oauth_store::load_store(oauth_path)
        .ok()?
        .databricks
        .map(|c| DatabricksAccount {
            workspace_url: c.workspace_url,
            display_name: c.display_name,
            authenticated_at: c.authenticated_at,
        })
}
