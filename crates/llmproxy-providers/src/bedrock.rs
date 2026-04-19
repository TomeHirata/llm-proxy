use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sigv4::{
    http_request::{sign, SignableBody, SignableRequest, SigningSettings},
    sign::v4,
};
use bytes::Bytes;
use futures::stream::BoxStream;
use llmproxy_core::{
    error::ProxyError,
    openai_types::{ChatMessage, ChatRequest, ChatResponse, Choice, MessageContent, Usage},
    provider::{Credential, Provider},
};
use serde_json::{json, Value};

pub struct BedrockProvider {
    client: reqwest::Client,
}

impl Default for BedrockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BedrockProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
        }
    }

    fn url(region: &str, model_id: &str) -> String {
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse",
            region, model_id
        )
    }

    pub(crate) fn to_converse_body(req: &ChatRequest) -> Value {
        let system: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| json!({ "text": m.content.as_text() }))
            .collect();

        let messages: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| {
                json!({
                    "role": m.role,
                    "content": [{ "text": m.content.as_text() }],
                })
            })
            .collect();

        let mut body = json!({ "messages": messages });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        let mut ic = serde_json::Map::new();
        if let Some(t) = req.max_tokens {
            ic.insert("maxTokens".into(), json!(t));
        }
        if let Some(t) = req.temperature {
            ic.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.top_p {
            ic.insert("topP".into(), json!(p));
        }
        if let Some(stop) = &req.stop {
            let seqs = match stop {
                Value::String(s) => json!([s]),
                other => other.clone(),
            };
            ic.insert("stopSequences".into(), seqs);
        }
        if !ic.is_empty() {
            body["inferenceConfig"] = Value::Object(ic);
        }
        body
    }

    pub(crate) fn from_converse(resp: Value, model_id: &str) -> ChatResponse {
        let text = resp["output"]["message"]["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let input = resp["usage"]["inputTokens"].as_u64().unwrap_or(0) as u32;
        let output = resp["usage"]["outputTokens"].as_u64().unwrap_or(0) as u32;

        let finish = match resp["stopReason"].as_str() {
            Some("end_turn") => Some("stop".into()),
            Some("max_tokens") => Some("length".into()),
            Some("tool_use") => Some("tool_calls".into()),
            Some("stop_sequence") => Some("stop".into()),
            Some("content_filtered") => Some("content_filter".into()),
            Some(other) => Some(other.to_string()),
            None => None,
        };

        ChatResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp() as u64,
            model: model_id.to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: MessageContent::Text(text),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: finish,
            }],
            usage: Some(Usage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
            }),
        }
    }

    fn signed_request(
        &self,
        url: &str,
        body: Vec<u8>,
        cred: &Credential,
    ) -> Result<reqwest::Request, ProxyError> {
        let (access, secret, session, region) = match cred {
            Credential::AwsSigV4 {
                access_key_id,
                secret_access_key,
                session_token,
                region,
            } => (
                access_key_id.as_str(),
                secret_access_key.as_str(),
                session_token.as_deref(),
                region.as_str(),
            ),
            Credential::BearerToken(_) => {
                return Err(ProxyError::Config(
                    "bedrock requires AWS SigV4 credentials".into(),
                ));
            }
        };

        let credentials =
            Credentials::new(access, secret, session.map(String::from), None, "llmproxy");
        let identity = credentials.into();

        let signing_settings = SigningSettings::default();
        let signing_params = v4::SigningParams::builder()
            .identity(&identity)
            .region(region)
            .name("bedrock")
            .time(SystemTime::now())
            .settings(signing_settings)
            .build()
            .map_err(|e| ProxyError::Aws(e.to_string()))?
            .into();

        let headers = [("content-type", "application/json")];
        let signable = SignableRequest::new(
            "POST",
            url,
            headers.iter().copied(),
            SignableBody::Bytes(&body),
        )
        .map_err(|e| ProxyError::Aws(e.to_string()))?;

        let (instructions, _sig) = sign(signable, &signing_params)
            .map_err(|e| ProxyError::Aws(e.to_string()))?
            .into_parts();

        let mut rb = self
            .client
            .post(url)
            .header("content-type", "application/json");

        let (signed_headers, signed_query) = instructions.into_parts();
        for header in signed_headers {
            let name = header.name().to_string();
            let value = header.value().to_string();
            rb = rb.header(name, value);
        }
        if !signed_query.is_empty() {
            let q: Vec<(String, String)> = signed_query
                .into_iter()
                .map(|(name, value)| (name.to_string(), value.into_owned()))
                .collect();
            rb = rb.query(&q);
        }

        rb.body(body).build().map_err(ProxyError::Http)
    }
}

#[async_trait]
impl Provider for BedrockProvider {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let region = match cred {
            Credential::AwsSigV4 { region, .. } => region.clone(),
            Credential::BearerToken(_) => {
                return Err(ProxyError::Config(
                    "bedrock requires AWS SigV4 credentials".into(),
                ));
            }
        };

        let url = Self::url(&region, model_id);
        let body = serde_json::to_vec(&Self::to_converse_body(&req))?;
        let request = self.signed_request(&url, body, cred)?;
        let resp = self.client.execute(request).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }

        let value: Value = resp.json().await?;
        Ok(Self::from_converse(value, model_id))
    }

    async fn chat_stream(
        &self,
        _req: ChatRequest,
        _model_id: &str,
        _cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        Err(ProxyError::NotImplemented(
            "Bedrock streaming is not implemented in v0.1 (deferred to v0.2)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmproxy_core::openai_types::{ChatMessage, MessageContent};

    fn req() -> ChatRequest {
        ChatRequest {
            model: "bedrock/amazon.nova-pro-v1:0".into(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: MessageContent::Text("be helpful".into()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".into(),
                    content: MessageContent::Text("hi".into()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            stream: None,
            temperature: Some(0.5),
            max_tokens: Some(128),
            top_p: None,
            stop: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn converse_body_shape() {
        let body = BedrockProvider::to_converse_body(&req());
        assert_eq!(body["system"][0]["text"], "be helpful");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 128);
        assert_eq!(body["inferenceConfig"]["temperature"], 0.5);
    }

    #[test]
    fn from_converse_maps_fields() {
        let raw = json!({
            "output": { "message": { "role": "assistant", "content": [{"text": "hello"}] } },
            "usage": { "inputTokens": 5, "outputTokens": 7, "totalTokens": 12 },
            "stopReason": "end_turn",
        });
        let resp = BedrockProvider::from_converse(raw, "amazon.nova-pro-v1:0");
        assert_eq!(resp.choices[0].message.content.as_text(), "hello");
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        let u = resp.usage.unwrap();
        assert_eq!(u.prompt_tokens, 5);
        assert_eq!(u.completion_tokens, 7);
    }

    #[test]
    fn url_format() {
        assert_eq!(
            BedrockProvider::url("us-east-1", "amazon.nova-pro-v1:0"),
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/amazon.nova-pro-v1:0/converse"
        );
    }
}
