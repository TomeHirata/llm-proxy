//! Codex OAuth provider — wraps the OpenAI passthrough provider but
//! automatically refreshes its access token using a stored refresh token.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream::BoxStream, StreamExt};
use llmproxy_core::{
    error::ProxyError,
    openai_types::{ChatRequest, ChatResponse},
    provider::{Credential, Provider},
};
use tokio::sync::RwLock;

const OPENAI_BASE: &str = "https://api.openai.com/v1";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: i64,
}

impl CachedToken {
    fn is_expiring_soon(&self) -> bool {
        chrono::Utc::now().timestamp() > self.expires_at - 120
    }
}

pub struct CodexOAuthProvider {
    client: reqwest::Client,
    /// Mutable so we can rotate the refresh token on each use.
    refresh_token: Arc<RwLock<String>>,
    cache: Arc<RwLock<Option<CachedToken>>>,
    /// Called with the new refresh token whenever it rotates.
    token_saver: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl CodexOAuthProvider {
    pub fn new(refresh_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
            refresh_token: Arc::new(RwLock::new(refresh_token.into())),
            cache: Arc::new(RwLock::new(None)),
            token_saver: None,
        }
    }

    pub fn with_token_saver(mut self, saver: impl Fn(String) + Send + Sync + 'static) -> Self {
        self.token_saver = Some(Arc::new(saver));
        self
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
        let current_refresh = self.refresh_token.read().await.clone();
        let resp = self
            .client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", current_refresh.as_str()),
                ("client_id", CLIENT_ID),
                ("scope", "openid profile email offline_access model.request"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    ProxyError::Config(format!(
                        "Codex token refresh failed ({}): {body}\n\
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
            refresh_token: Option<String>,
            #[serde(default)]
            expires_in: Option<i64>,
        }
        let body: Resp = resp
            .json()
            .await
            .map_err(|e| ProxyError::Config(format!("Failed to parse Codex token: {e}")))?;

        // Rotate refresh token if a new one was returned.
        if let Some(new_rt) = body.refresh_token {
            *self.refresh_token.write().await = new_rt.clone();
            if let Some(ref saver) = self.token_saver {
                saver(new_rt);
            }
        }

        let expires_at = chrono::Utc::now().timestamp() + body.expires_in.unwrap_or(3600);
        Ok(CachedToken {
            token: body.access_token,
            expires_at,
        })
    }

    fn build_body(req: &ChatRequest, model_id: &str, stream: bool) -> serde_json::Value {
        let mut body = serde_json::to_value(req).unwrap_or_else(|_| serde_json::json!({}));
        body["model"] = serde_json::json!(model_id);
        if stream {
            body["stream"] = serde_json::json!(true);
            body["stream_options"] = serde_json::json!({"include_usage": true});
        } else if let Some(obj) = body.as_object_mut() {
            obj.remove("stream");
        }
        // OpenAI's Responses API uses max_completion_tokens.
        if let Some(obj) = body.as_object_mut() {
            if let Some(v) = obj.remove("max_tokens") {
                if !v.is_null() {
                    obj.insert("max_completion_tokens".into(), v);
                }
            }
        }
        body
    }
}

#[async_trait]
impl Provider for CodexOAuthProvider {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        _cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let token = self.get_token().await?;
        let body = Self::build_body(&req, model_id, false);
        let resp = self
            .client
            .post(format!("{OPENAI_BASE}/chat/completions"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }
        Ok(resp.json::<ChatResponse>().await?)
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        model_id: &str,
        _cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let token = self.get_token().await?;
        let body = Self::build_body(&req, model_id, true);
        let resp = self
            .client
            .post(format!("{OPENAI_BASE}/chat/completions"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }
        let stream = resp.bytes_stream().map(|r| r.map_err(ProxyError::from));
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmproxy_core::openai_types::{ChatMessage, MessageContent};

    fn simple_req() -> ChatRequest {
        ChatRequest {
            model: "codex_oauth/gpt-4o".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: MessageContent::Text("hi".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: None,
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn build_body_translates_max_tokens() {
        let mut req = simple_req();
        req.max_tokens = Some(512);
        let body = CodexOAuthProvider::build_body(&req, "gpt-4o", false);
        assert_eq!(body["max_completion_tokens"], serde_json::json!(512));
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn build_body_stream_options_included() {
        let req = simple_req();
        let body = CodexOAuthProvider::build_body(&req, "gpt-4o", true);
        assert_eq!(body["stream"], serde_json::json!(true));
        assert!(body["stream_options"].is_object());
    }

    #[test]
    fn build_body_non_stream_removes_stream_key() {
        let mut req = simple_req();
        req.stream = Some(true);
        let body = CodexOAuthProvider::build_body(&req, "gpt-4o", false);
        assert!(body.get("stream").is_none());
    }

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
