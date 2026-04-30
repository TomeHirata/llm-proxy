//! OpenAI Codex OAuth — device code flow for Codex CLI authentication.
//!
//! Flow:
//! 1. `codex_start_device_flow` → returns user_code + verification_uri
//! 2. User visits verification_uri and enters user_code
//! 3. `codex_poll_device_flow(device_code)` → returns Some(email) when done
//! 4. Refresh token stored in `~/.config/llmproxy/oauth_tokens.json`

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_AUTH_USERCODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_AUTH_TOKEN_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/token";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: Option<serde_json::Value>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct DevicePollSuccess {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct JwtClaims {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(
        default,
        rename = "https://api.openai.com/auth"
    )]
    openai_auth: Option<OpenAiAuthClaim>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiAuthClaim {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAccount {
    pub account_id: String,
    pub email: Option<String>,
    pub authenticated_at: i64,
}

/// Persisted Codex credentials.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexCreds {
    pub refresh_token: String,
    pub account_id: String,
    pub email: Option<String>,
    pub authenticated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceFlowInfo {
    /// Used as `device_code` in subsequent poll calls (maps to `device_auth_id`).
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

pub async fn start_device_flow() -> Result<DeviceFlowInfo, String> {
    let client = Client::new();
    let resp = client
        .post(DEVICE_AUTH_USERCODE_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!(
            "Codex device code request failed: {}",
            resp.status()
        ));
    }

    let dc: DeviceCodeResponse = resp.json().await.map_err(|e| e.to_string())?;
    let interval = parse_interval(dc.interval.as_ref());
    let expires_in = dc.expires_in.unwrap_or(900);

    Ok(DeviceFlowInfo {
        device_code: dc.device_auth_id,
        user_code: dc.user_code,
        verification_uri: VERIFICATION_URI.to_string(),
        expires_in,
        interval,
    })
}

/// Poll once. Returns `Some(CodexAccount)` on success, `None` while pending.
pub async fn poll_device_flow(
    device_code: &str,
    user_code: &str,
    oauth_path: &std::path::Path,
) -> Result<Option<CodexAccount>, String> {
    let client = Client::new();
    let resp = client
        .post(DEVICE_AUTH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "device_auth_id": device_code,
            "user_code": user_code,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None); // still pending
    }
    if status == reqwest::StatusCode::GONE {
        return Err("Device code expired".to_string());
    }
    if !status.is_success() {
        return Err(format!("Codex poll failed: {status}"));
    }

    let success: DevicePollSuccess = resp.json().await.map_err(|e| e.to_string())?;
    let tokens = exchange_code(&success.authorization_code, &success.code_verifier).await?;

    let refresh_token = tokens
        .refresh_token
        .ok_or("Response missing refresh_token")?;

    let (account_id, email) = extract_identity(&tokens.access_token, tokens.id_token.as_deref());
    let account_id = account_id.ok_or("Could not extract account_id from token")?;

    let creds = CodexCreds {
        refresh_token,
        account_id: account_id.clone(),
        email: email.clone(),
        authenticated_at: chrono::Utc::now().timestamp(),
    };
    save_codex_creds(oauth_path, &creds)?;

    Ok(Some(CodexAccount {
        account_id,
        email,
        authenticated_at: creds.authenticated_at,
    }))
}

async fn exchange_code(code: &str, code_verifier: &str) -> Result<OAuthTokenResponse, String> {
    let client = Client::new();
    let resp = client
        .post(OAUTH_TOKEN_URL)
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded",
        )
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("Code exchange failed: {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

fn extract_identity(access_token: &str, id_token: Option<&str>) -> (Option<String>, Option<String>) {
    let mut account_id = None;
    let mut email = None;

    if let Some(id_tok) = id_token {
        if let Some(claims) = parse_jwt::<JwtClaims>(id_tok) {
            account_id = claims
                .chatgpt_account_id
                .or_else(|| claims.openai_auth.and_then(|a| a.chatgpt_account_id));
            email = claims.email;
        }
    }

    if account_id.is_none() {
        if let Some(claims) = parse_jwt::<JwtClaims>(access_token) {
            account_id = claims
                .chatgpt_account_id
                .or_else(|| claims.openai_auth.and_then(|a| a.chatgpt_account_id));
            if email.is_none() {
                email = claims.email;
            }
        }
    }

    (account_id, email)
}

fn parse_jwt<T: for<'de> serde::Deserialize<'de>>(token: &str) -> Option<T> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn parse_interval(value: Option<&serde_json::Value>) -> u64 {
    let raw = match value {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(5),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(5),
        _ => 5,
    };
    raw.max(1) + 3 // safety margin
}

fn save_codex_creds(oauth_path: &std::path::Path, creds: &CodexCreds) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path);
    let updated = crate::oauth_store::OAuthStore {
        codex: Some(creds.clone()),
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn clear_codex_creds(oauth_path: &std::path::Path) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path);
    let updated = crate::oauth_store::OAuthStore {
        codex: None,
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn read_codex_account(oauth_path: &std::path::Path) -> Option<CodexAccount> {
    crate::oauth_store::load_store(oauth_path)
        .codex
        .map(|c| CodexAccount {
            account_id: c.account_id,
            email: c.email,
            authenticated_at: c.authenticated_at,
        })
}
