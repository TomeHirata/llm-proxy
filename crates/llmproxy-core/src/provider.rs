use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;

use crate::error::ProxyError;
use crate::openai_types::{ChatRequest, ChatResponse};

/// Resolved credential for a single request.
#[derive(Debug, Clone)]
pub enum Credential {
    /// Simple Bearer token — OpenAI, Anthropic, Gemini, Mistral, Cohere,
    /// TogetherAI, Azure (mapped to `api-key` header internally).
    BearerToken(String),
    /// AWS SigV4 signing material — Bedrock only.
    AwsSigV4 {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
        region: String,
    },
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<ChatResponse, ProxyError>;

    async fn chat_stream(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError>;

    /// Return a live bearer token for this provider. The default implementation
    /// extracts the token from `cred`; OAuth providers override this to return a
    /// freshly-refreshed access token (ignoring `cred`).
    async fn fetch_token(&self, cred: &Credential) -> Result<String, ProxyError> {
        match cred {
            Credential::BearerToken(t) => Ok(t.clone()),
            Credential::AwsSigV4 { .. } => Err(ProxyError::Config(
                "AWS SigV4 providers do not use bearer tokens".into(),
            )),
        }
    }
}
