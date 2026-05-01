//! Databricks OAuth — browser-based PKCE flow with local callback server.
//!
//! Flow:
//! 1. `start_browser_flow(workspace_url, oauth_path)` — OIDC discovery, open browser, wait for callback
//! 2. Browser redirects to local server with authorization code
//! 3. Exchange code for tokens, save credentials, return DatabricksAccount

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};

const DATABRICKS_CLIENT_ID: &str = "databricks-cli";
const DATABRICKS_SCOPES: &str = "all-apis offline_access";
const FLOW_TIMEOUT_SECS: u64 = 300;

#[derive(Deserialize)]
struct OidcDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
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
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub display_name: Option<String>,
    pub authenticated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabricksAccount {
    pub workspace_url: String,
    pub display_name: Option<String>,
    pub authenticated_at: i64,
}

/// Normalize workspace URL: add https://, keep only scheme+host.
fn normalize_workspace_url(raw: &str) -> Result<String, String> {
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let parsed = reqwest::Url::parse(&with_scheme)
        .map_err(|e| format!("Invalid workspace URL '{raw}': {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("Workspace URL '{raw}' has no host"))?;
    Ok(format!("https://{host}"))
}

/// Fetch OIDC discovery document.
async fn discover(client: &Client, base: &str) -> Result<OidcDiscovery, String> {
    let candidates = [
        format!("{base}/oidc/.well-known/oauth-authorization-server"),
        format!("{base}/oidc/v1/.well-known/openid-configuration"),
    ];
    let mut last_err = String::new();
    for url in &candidates {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp
                    .json::<OidcDiscovery>()
                    .await
                    .map_err(|e| format!("Failed to parse OIDC discovery at {url}: {e}"));
            }
            Ok(resp) => {
                last_err = format!(
                    "OIDC discovery at {url} returned {}",
                    resp.status().as_u16()
                );
            }
            Err(e) => {
                last_err = format!("OIDC discovery request to {url} failed: {e}");
            }
        }
    }
    Err(format!(
        "Could not find OIDC endpoints for {base}. \
        Make sure the Workspace URL is correct and that OAuth is enabled. \
        Last error: {last_err}"
    ))
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

fn parse_jwt_display_name(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: JwtClaims = serde_json::from_slice(&decoded).ok()?;
    claims.name.or(claims.email).or(claims.preferred_username)
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

pub async fn start_browser_flow(
    workspace_url: &str,
    oauth_path: &std::path::Path,
) -> Result<DatabricksAccount, String> {
    let base = normalize_workspace_url(workspace_url)?;
    let client = Client::new();
    let discovery = discover(&client, &base).await?;

    let (verifier, challenge) = generate_pkce();
    let state = generate_state();

    // Port 8020 is pre-registered for the databricks-cli public client.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8020")
        .await
        .map_err(|_| "Port 8020 is required for Databricks OAuth but is already in use. Please free it and try again.".to_string())?;
    let redirect_uri = "http://localhost:8020".to_string();

    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256\
         &scope={}&response_type=code&state={}",
        discovery.authorization_endpoint,
        DATABRICKS_CLIENT_ID,
        urlencoding::encode(&redirect_uri),
        challenge,
        urlencoding::encode(DATABRICKS_SCOPES),
        state,
    );

    open::that(&auth_url).map_err(|e| format!("Failed to open browser: {e}"))?;

    let code = tokio::time::timeout(
        std::time::Duration::from_secs(FLOW_TIMEOUT_SECS),
        accept_callback(listener),
    )
    .await
    .map_err(|_| "Authentication timed out (5 minutes). Please try again.".to_string())??;

    let resp = client
        .post(&discovery.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("client_id", DATABRICKS_CLIENT_ID),
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

    let display_name = parse_jwt_display_name(&tokens.access_token);
    let now = chrono::Utc::now().timestamp();
    let expires_at = now + tokens.expires_in.unwrap_or(3600) as i64;

    let creds = DatabricksCreds {
        workspace_url: base.clone(),
        access_token: tokens.access_token,
        refresh_token,
        expires_at,
        display_name: display_name.clone(),
        authenticated_at: now,
    };
    save_databricks_creds(oauth_path, &creds)?;

    Ok(DatabricksAccount {
        workspace_url: base,
        display_name,
        authenticated_at: creds.authenticated_at,
    })
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
