use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream::BoxStream, StreamExt};
use llmproxy_core::{
    error::ProxyError,
    openai_types::{ChatRequest, ChatResponse},
    provider::{Credential, Provider},
};

/// How to send the API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthHeader {
    /// `Authorization: Bearer <token>` — OpenAI, Mistral, TogetherAI.
    Bearer,
    /// `api-key: <token>` — Azure OpenAI.
    ApiKey,
}

/// Passthrough provider for upstreams that already speak OpenAI format.
///
/// Non-streaming requests are relayed as-is; streaming responses are relayed
/// byte-for-byte because each SSE chunk already matches the OpenAI wire format.
pub struct PassthroughProvider {
    client: reqwest::Client,
    base_url: String,
    auth_header: AuthHeader,
    /// OpenAI deprecated `max_tokens` in favour of `max_completion_tokens`.
    /// Set true for OpenAI; leave false for other providers that still use max_tokens.
    use_max_completion_tokens: bool,
}

impl PassthroughProvider {
    pub fn new(base_url: impl Into<String>, auth_header: AuthHeader) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("reqwest client");
        Self {
            client,
            base_url: base_url.into(),
            auth_header,
            use_max_completion_tokens: false,
        }
    }

    pub fn openai() -> Self {
        Self {
            use_max_completion_tokens: true,
            ..Self::new("https://api.openai.com/v1", AuthHeader::Bearer)
        }
    }

    pub fn mistral() -> Self {
        Self::new("https://api.mistral.ai/v1", AuthHeader::Bearer)
    }

    pub fn togetherai() -> Self {
        Self::new("https://api.together.xyz/v1", AuthHeader::Bearer)
    }

    /// Databricks Model Serving — endpoint is the workspace URL
    /// (e.g. `https://my-workspace.azuredatabricks.net`). The chat completions
    /// path is appended automatically.
    pub fn databricks(workspace_url: impl Into<String>) -> Self {
        let url: String = workspace_url.into();
        let url = url.trim_end_matches('/');
        Self::new(format!("{url}/serving-endpoints/v1"), AuthHeader::Bearer)
    }

    /// Azure OpenAI — endpoint like `https://my-resource.openai.azure.com` with
    /// a specific `api_version` query param. The model field in the request is
    /// treated as the deployment name.
    pub fn azure(endpoint: impl Into<String>, api_version: impl Into<String>) -> AzurePassthrough {
        AzurePassthrough {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
            endpoint: endpoint.into(),
            api_version: api_version.into(),
        }
    }

    fn bearer(cred: &Credential) -> Result<&str, ProxyError> {
        match cred {
            Credential::BearerToken(s) => Ok(s.as_str()),
            Credential::AwsSigV4 { .. } => Err(ProxyError::Config(
                "passthrough provider requires a Bearer token credential".into(),
            )),
        }
    }

    fn apply_auth(&self, rb: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        match self.auth_header {
            AuthHeader::Bearer => rb.bearer_auth(token),
            AuthHeader::ApiKey => rb.header("api-key", token),
        }
    }

    fn build_body(&self, req: &ChatRequest, model_id: &str, stream: bool) -> serde_json::Value {
        let mut body = serde_json::to_value(req).unwrap_or_else(|_| serde_json::json!({}));
        body["model"] = serde_json::json!(model_id);
        if stream {
            body["stream"] = serde_json::json!(true);
            // Request a trailing usage chunk so token counts are logged.
            body["stream_options"] = serde_json::json!({"include_usage": true});
        } else {
            // Clear any client-sent "stream: true" since the caller asked for non-streaming.
            if let Some(obj) = body.as_object_mut() {
                obj.remove("stream");
            }
        }
        // OpenAI deprecated max_tokens in favour of max_completion_tokens.
        if self.use_max_completion_tokens {
            if let Some(obj) = body.as_object_mut() {
                if let Some(v) = obj.remove("max_tokens") {
                    if !v.is_null() {
                        obj.insert("max_completion_tokens".into(), v);
                    }
                }
            }
        }
        body
    }
}

#[async_trait]
impl Provider for PassthroughProvider {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let token = Self::bearer(cred)?;
        let body = self.build_body(&req, model_id, false);

        let rb = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        let resp = self.apply_auth(rb, token).send().await?;

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
        cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let token = Self::bearer(cred)?;
        let body = self.build_body(&req, model_id, true);

        let rb = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        let resp = self.apply_auth(rb, token).send().await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }

        let stream = resp.bytes_stream().map(|r| r.map_err(ProxyError::from));
        Ok(Box::pin(stream))
    }
}

/// Azure OpenAI passthrough — builds URLs from `endpoint` + deployment name +
/// `api-version` query parameter. Uses the `api-key` header.
pub struct AzurePassthrough {
    client: reqwest::Client,
    endpoint: String,
    api_version: String,
}

impl AzurePassthrough {
    fn url(&self, deployment: &str) -> String {
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            self.endpoint.trim_end_matches('/'),
            deployment,
            self.api_version
        )
    }

    fn bearer(cred: &Credential) -> Result<&str, ProxyError> {
        match cred {
            Credential::BearerToken(s) => Ok(s.as_str()),
            Credential::AwsSigV4 { .. } => Err(ProxyError::Config(
                "azure provider requires an api-key credential".into(),
            )),
        }
    }

    fn build_body(req: &ChatRequest, model_id: &str, stream: bool) -> serde_json::Value {
        // Azure also uses max_completion_tokens for newer deployments.
        let stub = PassthroughProvider {
            client: reqwest::Client::new(),
            base_url: String::new(),
            auth_header: AuthHeader::ApiKey,
            use_max_completion_tokens: true,
        };
        stub.build_body(req, model_id, stream)
    }
}

#[async_trait]
impl Provider for AzurePassthrough {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let token = Self::bearer(cred)?;
        let body = Self::build_body(&req, model_id, false);

        let resp = self
            .client
            .post(self.url(model_id))
            .header("api-key", token)
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
        cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let token = Self::bearer(cred)?;
        let body = Self::build_body(&req, model_id, true);

        let resp = self
            .client
            .post(self.url(model_id))
            .header("api-key", token)
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

    fn dummy_req() -> ChatRequest {
        ChatRequest {
            model: "alias/ignored".into(),
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
    fn build_body_overwrites_model() {
        let p = PassthroughProvider::openai();
        let body = p.build_body(&dummy_req(), "gpt-4o", false);
        assert_eq!(body["model"], "gpt-4o");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn build_body_sets_stream_flag() {
        let p = PassthroughProvider::openai();
        let body = p.build_body(&dummy_req(), "gpt-4o", true);
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn build_body_renames_max_tokens_for_openai() {
        let mut req = dummy_req();
        req.max_tokens = Some(100);
        let p = PassthroughProvider::openai();
        let body = p.build_body(&req, "gpt-4o", false);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 100);
    }

    #[test]
    fn build_body_keeps_max_tokens_for_others() {
        let mut req = dummy_req();
        req.max_tokens = Some(100);
        let p = PassthroughProvider::mistral();
        let body = p.build_body(&req, "mistral-large", false);
        assert_eq!(body["max_tokens"], 100);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn azure_url_format() {
        let az = PassthroughProvider::azure("https://r.openai.azure.com/", "2024-02-01");
        assert_eq!(
            az.url("my-deploy"),
            "https://r.openai.azure.com/openai/deployments/my-deploy/chat/completions?api-version=2024-02-01"
        );
    }

    #[test]
    fn databricks_base_url_no_trailing_slash() {
        let p = PassthroughProvider::databricks(
            "https://my-workspace.azuredatabricks.net",
        );
        assert_eq!(
            p.base_url,
            "https://my-workspace.azuredatabricks.net/serving-endpoints/v1"
        );
    }

    #[test]
    fn databricks_base_url_trailing_slash_stripped() {
        let p = PassthroughProvider::databricks(
            "https://my-workspace.azuredatabricks.net/",
        );
        assert_eq!(
            p.base_url,
            "https://my-workspace.azuredatabricks.net/serving-endpoints/v1"
        );
    }
}
