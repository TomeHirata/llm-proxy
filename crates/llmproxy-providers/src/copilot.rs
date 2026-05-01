//! GitHub Copilot provider — forwards OpenAI-format requests to
//! `api.githubcopilot.com` using a short-lived Copilot token derived
//! from the user's long-lived GitHub OAuth token.

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

const COPILOT_API_BASE: &str = "https://api.githubcopilot.com";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const EDITOR_VERSION: &str = "vscode/1.93.0";
const INTEGRATION_ID: &str = "vscode-chat";

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

pub struct CopilotProvider {
    client: reqwest::Client,
    github_token: String,
    cache: Arc<RwLock<Option<CachedToken>>>,
}

impl CopilotProvider {
    pub fn new(github_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
            github_token: github_token.into(),
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

        let fresh = self.fetch_copilot_token().await?;
        let token_str = fresh.token.clone();
        *cache = Some(fresh);
        Ok(token_str)
    }

    async fn fetch_copilot_token(&self) -> Result<CachedToken, ProxyError> {
        let resp = self
            .client
            .get(COPILOT_TOKEN_URL)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "llmproxy")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    ProxyError::Config(format!(
                        "Copilot token fetch failed ({}): {body}",
                        status.as_u16()
                    ))
                }
                _ => ProxyError::Upstream {
                    status: status.as_u16(),
                    body,
                },
            });
        }

        // GitHub returns expires_at as a Unix timestamp integer.
        #[derive(serde::Deserialize)]
        struct Resp {
            token: String,
            expires_at: i64,
        }
        let body: Resp = resp
            .json()
            .await
            .map_err(|e| ProxyError::Config(format!("Failed to parse Copilot token: {e}")))?;

        let expires_at = body.expires_at;

        Ok(CachedToken {
            token: body.token,
            expires_at,
        })
    }

    fn add_copilot_headers(rb: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        rb.header("Authorization", format!("Bearer {token}"))
            .header("Editor-Version", EDITOR_VERSION)
            .header("Copilot-Integration-Id", INTEGRATION_ID)
            .header("X-GitHub-Api-Version", "2022-11-28")
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
        body
    }
}

#[async_trait]
impl Provider for CopilotProvider {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        _cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let token = self.get_token().await?;
        let body = Self::build_body(&req, model_id, false);
        let rb = self
            .client
            .post(format!("{COPILOT_API_BASE}/chat/completions"))
            .json(&body);
        let resp = Self::add_copilot_headers(rb, &token).send().await?;
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
        let rb = self
            .client
            .post(format!("{COPILOT_API_BASE}/chat/completions"))
            .json(&body);
        let resp = Self::add_copilot_headers(rb, &token).send().await?;
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
            model: "copilot/gpt-4o".into(),
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
    fn build_body_sets_model() {
        let req = simple_req();
        let body = CopilotProvider::build_body(&req, "gpt-4o", false);
        assert_eq!(body["model"], serde_json::json!("gpt-4o"));
    }

    #[test]
    fn build_body_stream_adds_stream_options() {
        let req = simple_req();
        let body = CopilotProvider::build_body(&req, "gpt-4o", true);
        assert_eq!(body["stream"], serde_json::json!(true));
        assert!(body["stream_options"].is_object());
    }

    #[test]
    fn build_body_non_stream_removes_stream_key() {
        let mut req = simple_req();
        req.stream = Some(true);
        let body = CopilotProvider::build_body(&req, "gpt-4o", false);
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
