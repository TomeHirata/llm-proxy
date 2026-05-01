//! Anthropic OAuth provider — uses a stored refresh token to obtain short-lived
//! access tokens and routes requests to the Anthropic Messages API using Bearer
//! auth instead of x-api-key.

use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream::BoxStream, StreamExt};
use llmproxy_core::{
    error::ProxyError,
    openai_types::{ChatRequest, ChatResponse},
    provider::{Credential, Provider},
};
use tokio::sync::RwLock;

use crate::anthropic::{find_double_newline, translate_anthropic_event, AnthropicProvider};

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/oauth/token";
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-48f7-a536-85d34a2647cf";

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    /// Unix timestamp (seconds) when this token expires.
    expires_at: i64,
}

impl CachedToken {
    fn is_expiring_soon(&self) -> bool {
        chrono::Utc::now().timestamp() > self.expires_at - 120
    }
}

pub struct AnthropicOAuthProvider {
    client: reqwest::Client,
    refresh_token: String,
    cache: Arc<RwLock<Option<CachedToken>>>,
}

impl AnthropicOAuthProvider {
    pub fn new(refresh_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
            refresh_token: refresh_token.into(),
            cache: Arc::new(RwLock::new(None)),
        }
    }

    async fn get_token(&self) -> Result<String, ProxyError> {
        {
            let cache = self.cache.read().await;
            if let Some(ref t) = *cache {
                if !t.is_expiring_soon() {
                    return Ok(t.token.clone());
                }
            }
        }

        let mut cache = self.cache.write().await;
        if let Some(ref t) = *cache {
            if !t.is_expiring_soon() {
                return Ok(t.token.clone());
            }
        }

        let fresh = self.refresh_access_token().await?;
        let token_str = fresh.token.clone();
        *cache = Some(fresh);
        Ok(token_str)
    }

    async fn refresh_access_token(&self) -> Result<CachedToken, ProxyError> {
        let resp = self
            .client
            .post(ANTHROPIC_TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh_token),
                ("client_id", ANTHROPIC_CLIENT_ID),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    ProxyError::Config(format!(
                        "Anthropic token refresh failed ({}): {body}\n\
                         Please re-authenticate via the llmproxy app.",
                        status.as_u16()
                    ))
                }
                _ => ProxyError::Upstream {
                    status: status.as_u16(),
                    body,
                },
            });
        }

        #[derive(serde::Deserialize)]
        struct Resp {
            access_token: String,
            #[serde(default)]
            expires_in: Option<i64>,
        }
        let body: Resp = resp
            .json()
            .await
            .map_err(|e| ProxyError::Config(format!("Failed to parse Anthropic token: {e}")))?;

        let expires_at = chrono::Utc::now().timestamp() + body.expires_in.unwrap_or(3600);
        Ok(CachedToken {
            token: body.access_token,
            expires_at,
        })
    }
}

#[async_trait]
impl Provider for AnthropicOAuthProvider {
    async fn fetch_token(&self, _cred: &Credential) -> Result<String, ProxyError> {
        self.get_token().await
    }

    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        _cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let token = self.get_token().await?;
        let body = AnthropicProvider::to_anthropic(&req, model_id, false);
        let resp = self
            .client
            .post(ANTHROPIC_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }
        let value: serde_json::Value = resp.json().await?;
        Ok(AnthropicProvider::from_anthropic(value))
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        model_id: &str,
        _cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let token = self.get_token().await?;
        let model_id = model_id.to_string();
        let body = AnthropicProvider::to_anthropic(&req, &model_id, true);

        let resp = self
            .client
            .post(ANTHROPIC_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }

        let mut upstream = resp.bytes_stream();
        let chat_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let created = chrono::Utc::now().timestamp() as u64;

        let out = try_stream! {
            let mut buf = Vec::<u8>::new();
            let mut first_delta = true;

            while let Some(chunk) = upstream.next().await {
                let chunk = chunk.map_err(ProxyError::from)?;
                buf.extend_from_slice(&chunk);

                while let Some(pos) = find_double_newline(&buf) {
                    let raw = buf.drain(..pos + 2).collect::<Vec<u8>>();
                    let event = std::str::from_utf8(&raw[..raw.len().saturating_sub(2)])
                        .map_err(|e| ProxyError::Stream(e.to_string()))?;
                    for out_chunk in translate_anthropic_event(event, &chat_id, created, &model_id, &mut first_delta) {
                        yield out_chunk;
                    }
                }
            }
            yield Bytes::from_static(b"data: [DONE]\n\n");
        };

        Ok(Box::pin(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_token_expiry() {
        let future = CachedToken {
            token: "tok".into(),
            expires_at: chrono::Utc::now().timestamp() + 300,
        };
        assert!(!future.is_expiring_soon());

        let soon = CachedToken {
            token: "tok".into(),
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        assert!(soon.is_expiring_soon());
    }
}
