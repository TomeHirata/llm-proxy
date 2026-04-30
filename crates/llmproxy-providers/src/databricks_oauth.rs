//! Databricks OAuth provider — uses a stored refresh token to obtain short-lived
//! access tokens and routes requests to the Databricks serving-endpoints API.

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

const CLIENT_ID: &str = "databricks-cli";

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

pub struct DatabricksOAuthProvider {
    client: reqwest::Client,
    workspace_url: String,
    refresh_token: String,
    cache: Arc<RwLock<Option<CachedToken>>>,
}

impl DatabricksOAuthProvider {
    pub fn new(workspace_url: impl Into<String>, refresh_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
            workspace_url: workspace_url.into().trim_end_matches('/').to_string(),
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
        let token_url = format!("{}/oidc/v1/token", self.workspace_url);
        let resp = self
            .client
            .post(&token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh_token),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    ProxyError::Config(format!(
                        "Databricks token refresh failed ({}): {body}\n\
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
            .map_err(|e| ProxyError::Config(format!("Failed to parse Databricks token: {e}")))?;

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
        body
    }
}

#[async_trait]
impl Provider for DatabricksOAuthProvider {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        _cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let token = self.get_token().await?;
        let body = Self::build_body(&req, model_id, false);
        let url = format!(
            "{}/serving-endpoints/v1/chat/completions",
            self.workspace_url
        );
        let resp = self
            .client
            .post(&url)
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
        let url = format!(
            "{}/serving-endpoints/v1/chat/completions",
            self.workspace_url
        );
        let resp = self
            .client
            .post(&url)
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
            model: "databricks/databricks-meta-llama-3-1-70b-instruct".into(),
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
        let body = DatabricksOAuthProvider::build_body(
            &req,
            "databricks-meta-llama-3-1-70b-instruct",
            false,
        );
        assert_eq!(
            body["model"],
            serde_json::json!("databricks-meta-llama-3-1-70b-instruct")
        );
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn build_body_stream_adds_stream_options() {
        let req = simple_req();
        let body = DatabricksOAuthProvider::build_body(&req, "meta-llama", true);
        assert_eq!(body["stream"], serde_json::json!(true));
        assert!(body["stream_options"].is_object());
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
