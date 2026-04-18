use std::time::Duration;

use async_stream::try_stream;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream::BoxStream, StreamExt};
use llmproxy_core::{
    error::ProxyError,
    openai_types::{ChatMessage, ChatRequest, ChatResponse, Choice, MessageContent, Usage},
    provider::{Credential, Provider},
};
use serde_json::{json, Value};

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: reqwest::Client,
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
        }
    }

    fn bearer(cred: &Credential) -> Result<&str, ProxyError> {
        match cred {
            Credential::BearerToken(s) => Ok(s.as_str()),
            Credential::AwsSigV4 { .. } => Err(ProxyError::Config(
                "anthropic provider requires an API key".into(),
            )),
        }
    }

    pub(crate) fn to_anthropic(req: &ChatRequest, model_id: &str, stream: bool) -> Value {
        let system = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_text().to_string());

        let messages: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| {
                json!({
                    "role": m.role,
                    "content": m.content.as_text(),
                })
            })
            .collect();

        let mut body = json!({
            "model": model_id,
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "messages": messages,
        });
        if let Some(sys) = system {
            body["system"] = json!(sys);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(stop) = &req.stop {
            body["stop_sequences"] = stop.clone();
        }
        if stream {
            body["stream"] = json!(true);
        }
        body
    }

    pub(crate) fn from_anthropic(resp: Value) -> ChatResponse {
        let text = resp["content"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find_map(|p| p.get("text").and_then(|t| t.as_str()))
            })
            .unwrap_or("")
            .to_string();
        let input_tokens = resp["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
        let output_tokens = resp["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;

        ChatResponse {
            id: resp["id"].as_str().unwrap_or("").to_string(),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: resp["model"].as_str().unwrap_or("").to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text(text),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: map_stop_reason(resp["stop_reason"].as_str()),
            }],
            usage: Some(Usage {
                prompt_tokens: input_tokens,
                completion_tokens: output_tokens,
                total_tokens: input_tokens + output_tokens,
            }),
        }
    }
}

fn map_stop_reason(r: Option<&str>) -> Option<String> {
    match r {
        Some("end_turn") => Some("stop".into()),
        Some("max_tokens") => Some("length".into()),
        Some("tool_use") => Some("tool_calls".into()),
        Some("stop_sequence") => Some("stop".into()),
        Some(other) => Some(other.into()),
        None => None,
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let token = Self::bearer(cred)?;
        let body = Self::to_anthropic(&req, model_id, false);
        let resp = self
            .client
            .post(ANTHROPIC_URL)
            .header("x-api-key", token)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }
        let value: Value = resp.json().await?;
        Ok(Self::from_anthropic(value))
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let token = Self::bearer(cred)?.to_string();
        let model_id = model_id.to_string();
        let body = Self::to_anthropic(&req, &model_id, true);

        let resp = self
            .client
            .post(ANTHROPIC_URL)
            .header("x-api-key", &token)
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
                    // Drop the trailing blank-line separator.
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

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn translate_anthropic_event(
    event: &str,
    chat_id: &str,
    created: u64,
    model_id: &str,
    first_delta: &mut bool,
) -> Vec<Bytes> {
    let mut event_type = "";
    let mut data_line = "";
    for line in event.split('\n') {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_line = rest.trim();
        }
    }
    if data_line.is_empty() {
        return Vec::new();
    }
    let value: Value = match serde_json::from_str(data_line) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    // Fall back to value["type"] if the `event:` line was absent.
    let ty = if event_type.is_empty() {
        value.get("type").and_then(|v| v.as_str()).unwrap_or("")
    } else {
        event_type
    };

    match ty {
        "content_block_delta" => {
            let text = value["delta"]["text"].as_str().unwrap_or("");
            if text.is_empty() {
                return Vec::new();
            }
            let mut delta = json!({ "content": text });
            if *first_delta {
                delta["role"] = json!("assistant");
                *first_delta = false;
            }
            let chunk = json!({
                "id": chat_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_id,
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": Value::Null,
                }]
            });
            vec![format_sse(&chunk)]
        }
        "message_delta" => {
            let reason = map_stop_reason(value["delta"]["stop_reason"].as_str());
            let chunk = json!({
                "id": chat_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_id,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": reason,
                }]
            });
            vec![format_sse(&chunk)]
        }
        _ => Vec::new(),
    }
}

fn format_sse(v: &Value) -> Bytes {
    let s = format!("data: {}\n\n", v);
    Bytes::from(s.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmproxy_core::openai_types::{ChatMessage, MessageContent};

    fn req() -> ChatRequest {
        ChatRequest {
            model: "anthropic/claude-sonnet-4-5".into(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: MessageContent::Text("be terse".into()),
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
    fn to_anthropic_maps_system_and_messages() {
        let body = AnthropicProvider::to_anthropic(&req(), "claude-sonnet-4-5", false);
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["system"], "be terse");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn from_anthropic_maps_content_and_usage() {
        let raw = json!({
            "id": "msg_01",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "hello"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 2},
        });
        let resp = AnthropicProvider::from_anthropic(raw);
        assert_eq!(resp.choices[0].message.content.as_text(), "hello");
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        let u = resp.usage.unwrap();
        assert_eq!(u.prompt_tokens, 3);
        assert_eq!(u.completion_tokens, 2);
        assert_eq!(u.total_tokens, 5);
    }

    #[test]
    fn translate_content_block_delta_emits_chunk() {
        let event = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}";
        let mut first = true;
        let out = translate_anthropic_event(event, "chatcmpl-abc", 1, "claude", &mut first);
        assert_eq!(out.len(), 1);
        let body = std::str::from_utf8(&out[0]).unwrap();
        assert!(body.starts_with("data: "));
        assert!(body.contains("\"content\":\"Hello\""));
        assert!(body.contains("\"role\":\"assistant\""));
        assert!(!first);
    }

    #[test]
    fn translate_message_delta_emits_finish_reason() {
        let event = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}";
        let mut first = false;
        let out = translate_anthropic_event(event, "chatcmpl-abc", 1, "claude", &mut first);
        assert_eq!(out.len(), 1);
        let body = std::str::from_utf8(&out[0]).unwrap();
        assert!(body.contains("\"finish_reason\":\"stop\""));
    }

    #[test]
    fn translate_ignores_ping_and_message_start() {
        let mut first = true;
        let ping = "event: ping\ndata: {\"type\":\"ping\"}";
        assert!(translate_anthropic_event(ping, "x", 0, "m", &mut first).is_empty());
        let start = "event: message_start\ndata: {\"type\":\"message_start\"}";
        assert!(translate_anthropic_event(start, "x", 0, "m", &mut first).is_empty());
    }
}
