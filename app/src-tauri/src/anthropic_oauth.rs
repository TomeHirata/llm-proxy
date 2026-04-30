//! Anthropic OAuth — browser-based PKCE flow with local callback server.
//!
//! Flow:
//! 1. `start_browser_flow(oauth_path)` → opens system browser, waits for callback
//! 2. Browser redirects to local server with authorization code
//! 3. Exchange code for tokens, save credentials, return AnthropicAccount
//! 4. Credentials stored in `~/.config/llmproxy/oauth_tokens.json`

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};

const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-48f7-a536-85d34a2647cf";
const ANTHROPIC_AUTH_URL: &str = "https://console.anthropic.com/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/oauth/token";
const ANTHROPIC_SCOPES: &str = "openid email profile";
const FLOW_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicCreds {
    pub refresh_token: String,
    pub email: Option<String>,
    pub authenticated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicAccount {
    pub email: Option<String>,
    pub authenticated_at: i64,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[allow(dead_code)]
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct JwtClaims {
    #[serde(default)]
    email: Option<String>,
}

fn generate_pkce() -> (String, String) {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    use sha2::{Digest, Sha256};
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn generate_state() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn parse_jwt_email(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: JwtClaims = serde_json::from_slice(&decoded).ok()?;
    claims.email
}

async fn accept_callback(listener: tokio::net::TcpListener) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| format!("Failed to accept connection: {e}"))?;

    let mut buf = vec![0u8; 8192];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| format!("Failed to read request: {e}"))?;
    let request = std::str::from_utf8(&buf[..n]).unwrap_or("");

    let code = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| path.split('?').nth(1))
        .and_then(|query| query.split('&').find(|p| p.starts_with("code=")))
        .map(|p| p[5..].to_string())
        .ok_or_else(|| "No authorization code in callback".to_string())?;

    let html = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body><h2>Authentication successful!</h2>\
        <p>You can close this tab.</p></body></html>";
    let _ = stream.write_all(html.as_bytes()).await;

    Ok(code)
}

pub async fn start_browser_flow(oauth_path: &std::path::Path) -> Result<AnthropicAccount, String> {
    let (verifier, challenge) = generate_pkce();
    let state = generate_state();

    // Bind to a free port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind local port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    // Build auth URL.
    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256\
         &scope={}&response_type=code&state={}",
        ANTHROPIC_AUTH_URL,
        ANTHROPIC_CLIENT_ID,
        urlencoding::encode(&redirect_uri),
        challenge,
        urlencoding::encode(ANTHROPIC_SCOPES),
        state,
    );

    open::that(&auth_url).map_err(|e| format!("Failed to open browser: {e}"))?;

    // Wait for callback with timeout.
    let code = tokio::time::timeout(
        std::time::Duration::from_secs(FLOW_TIMEOUT_SECS),
        accept_callback(listener),
    )
    .await
    .map_err(|_| "Authentication timed out (5 minutes). Please try again.".to_string())??;

    // Exchange code for tokens.
    let client = Client::new();
    let resp = client
        .post(ANTHROPIC_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("client_id", ANTHROPIC_CLIENT_ID),
            ("code_verifier", &verifier),
        ])
        .send()
        .await
        .map_err(|e| format!("Token exchange request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Token exchange failed ({}): {body}",
            status.as_u16()
        ));
    }

    let tokens: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    let refresh_token = tokens
        .refresh_token
        .ok_or_else(|| "Response missing refresh_token".to_string())?;

    let email = tokens.id_token.as_deref().and_then(parse_jwt_email);

    let creds = AnthropicCreds {
        refresh_token,
        email: email.clone(),
        authenticated_at: chrono::Utc::now().timestamp(),
    };
    save_anthropic_creds(oauth_path, &creds)?;

    Ok(AnthropicAccount {
        email,
        authenticated_at: creds.authenticated_at,
    })
}

fn save_anthropic_creds(
    oauth_path: &std::path::Path,
    creds: &AnthropicCreds,
) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path).unwrap_or_default();
    let updated = crate::oauth_store::OAuthStore {
        anthropic: Some(creds.clone()),
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn clear_anthropic_creds(oauth_path: &std::path::Path) -> Result<(), String> {
    let store = crate::oauth_store::load_store(oauth_path).unwrap_or_default();
    let updated = crate::oauth_store::OAuthStore {
        anthropic: None,
        ..store
    };
    crate::oauth_store::save_store(oauth_path, &updated)
}

pub fn read_anthropic_account(oauth_path: &std::path::Path) -> Option<AnthropicAccount> {
    crate::oauth_store::load_store(oauth_path)
        .ok()?
        .anthropic
        .map(|c| AnthropicAccount {
            email: c.email,
            authenticated_at: c.authenticated_at,
        })
}
