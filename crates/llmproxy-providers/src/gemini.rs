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

const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GeminiProvider {
    client: reqwest::Client,
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client"),
        }
    }

    fn api_key(cred: &Credential) -> Result<&str, ProxyError> {
        match cred {
            Credential::BearerToken(s) => Ok(s.as_str()),
            Credential::AwsSigV4 { .. } => Err(ProxyError::Config(
                "gemini provider requires an API key".into(),
            )),
        }
    }

    fn url(model_id: &str, stream: bool) -> String {
        if stream {
            format!("{GEMINI_BASE}/models/{model_id}:streamGenerateContent?alt=sse")
        } else {
            format!("{GEMINI_BASE}/models/{model_id}:generateContent")
        }
    }

    pub(crate) fn to_gemini(req: &ChatRequest) -> Value {
        let system_text = req
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_text().to_string());

        let raw: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| {
                let role = if m.role == "assistant" {
                    "model"
                } else {
                    "user"
                };
                json!({
                    "role": role,
                    "parts": [{ "text": m.content.as_text() }],
                })
            })
            .collect();

        let contents = merge_consecutive_roles(raw);

        let mut body = json!({ "contents": contents });
        if let Some(sys) = system_text {
            body["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
        }
        let mut gen_config = serde_json::Map::new();
        if let Some(t) = req.temperature {
            gen_config.insert("temperature".into(), json!(t));
        }
        if let Some(m) = req.max_tokens {
            gen_config.insert("maxOutputTokens".into(), json!(m));
        }
        if let Some(p) = req.top_p {
            gen_config.insert("topP".into(), json!(p));
        }
        if let Some(stop) = &req.stop {
            let seqs = match stop {
                Value::String(s) => json!([s]),
                other => other.clone(),
            };
            gen_config.insert("stopSequences".into(), seqs);
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
        }
        body
    }

    pub(crate) fn from_gemini(resp: Value, model_id: &str) -> ChatResponse {
        let text = resp["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let prompt_tokens = resp["usageMetadata"]["promptTokenCount"]
            .as_u64()
            .unwrap_or(0) as u32;
        let output_tokens = resp["usageMetadata"]["candidatesTokenCount"]
            .as_u64()
            .unwrap_or(0) as u32;

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
                finish_reason: map_finish_reason(resp["candidates"][0]["finishReason"].as_str()),
            }],
            usage: Some(Usage {
                prompt_tokens,
                completion_tokens: output_tokens,
                total_tokens: prompt_tokens + output_tokens,
            }),
        }
    }
}

fn map_finish_reason(r: Option<&str>) -> Option<String> {
    match r {
        Some("STOP") => Some("stop".into()),
        Some("MAX_TOKENS") => Some("length".into()),
        Some("SAFETY") | Some("RECITATION") => Some("content_filter".into()),
        Some(other) => Some(other.to_string()),
        None => None,
    }
}

pub(crate) fn merge_consecutive_roles(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        if let Some(last) = out.last_mut() {
            if last.get("role") == item.get("role") {
                if let (Some(last_parts), Some(new_parts)) = (
                    last.get_mut("parts").and_then(|p| p.as_array_mut()),
                    item.get("parts").and_then(|p| p.as_array()),
                ) {
                    for p in new_parts {
                        last_parts.push(p.clone());
                    }
                    continue;
                }
            }
        }
        out.push(item);
    }
    out
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<ChatResponse, ProxyError> {
        let key = Self::api_key(cred)?;
        let body = Self::to_gemini(&req);
        let resp = self
            .client
            .post(Self::url(model_id, false))
            .query(&[("key", key)])
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }
        let value: Value = resp.json().await?;
        Ok(Self::from_gemini(value, model_id))
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let key = Self::api_key(cred)?.to_string();
        let body = Self::to_gemini(&req);
        let model_id = model_id.to_string();

        let resp = self
            .client
            .post(Self::url(&model_id, true))
            .query(&[("key", &key)])
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

                while let Some((pos, sep_len)) = find_event_boundary(&buf) {
                    let raw = buf.drain(..pos + sep_len).collect::<Vec<u8>>();
                    let event = std::str::from_utf8(&raw[..raw.len().saturating_sub(sep_len)])
                        .map_err(|e| ProxyError::Stream(e.to_string()))?;
                    for out_chunk in translate_gemini_event(event, &chat_id, created, &model_id, &mut first_delta) {
                        yield out_chunk;
                    }
                }
            }
            yield Bytes::from_static(b"data: [DONE]\n\n");
        };

        Ok(Box::pin(out))
    }
}

/// Find an SSE event boundary (`\n\n` or `\r\n\r\n`).
/// Returns `(position of boundary start, boundary byte length)`.
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let len = buf.len();
    for i in 0..len {
        if i + 3 < len && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        if i + 1 < len && buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
    }
    None
}

fn translate_gemini_event(
    event: &str,
    chat_id: &str,
    created: u64,
    model_id: &str,
    first_delta: &mut bool,
) -> Vec<Bytes> {
    let mut data_line = String::new();
    for line in event.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            if !data_line.is_empty() {
                data_line.push('\n');
            }
            data_line.push_str(rest.trim_start());
        }
    }
    let data_line = data_line.trim();
    if data_line.is_empty() {
        return Vec::new();
    }
    let value: Value = match serde_json::from_str(data_line) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let text = value["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| !p.get("thought").and_then(|v| v.as_bool()).unwrap_or(false))
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let finish = map_finish_reason(value["candidates"][0]["finishReason"].as_str());

    let mut out = Vec::new();
    if !text.is_empty() {
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
        out.push(format_sse(&chunk));
    }
    if finish.is_some() {
        let pt = value["usageMetadata"]["promptTokenCount"].as_i64();
        let ct = value["usageMetadata"]["candidatesTokenCount"].as_i64();
        let tt = pt
            .zip(ct)
            .map(|(p, c)| p + c)
            .or_else(|| value["usageMetadata"]["totalTokenCount"].as_i64());
        let mut chunk = json!({
            "id": chat_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model_id,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish,
            }]
        });
        if pt.is_some() || ct.is_some() {
            chunk["usage"] = json!({
                "prompt_tokens": pt,
                "completion_tokens": ct,
                "total_tokens": tt,
            });
        }
        out.push(format_sse(&chunk));
    }
    out
}

fn format_sse(v: &Value) -> Bytes {
    Bytes::from(format!("data: {}\n\n", v).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmproxy_core::openai_types::{ChatMessage, MessageContent};

    fn req_with_roles(roles: &[&str]) -> ChatRequest {
        ChatRequest {
            model: "gemini/gemini-2.5-flash".into(),
            messages: roles
                .iter()
                .map(|r| ChatMessage {
                    role: (*r).into(),
                    content: MessageContent::Text(format!("{r}-msg")),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                })
                .collect(),
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
    fn merges_consecutive_user_messages() {
        let body = GeminiProvider::to_gemini(&req_with_roles(&["user", "user", "assistant"]));
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(contents[1]["role"], "model");
    }

    #[test]
    fn extracts_system_instruction() {
        let body = GeminiProvider::to_gemini(&req_with_roles(&["system", "user"]));
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "system-msg");
        assert_eq!(body["contents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn translate_emits_content_and_finish() {
        let event = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}]}";
        let mut first = true;
        let out = translate_gemini_event(event, "id", 1, "gemini-x", &mut first);
        assert_eq!(out.len(), 2);
        assert!(std::str::from_utf8(&out[0])
            .unwrap()
            .contains("\"content\":\"hi\""));
        assert!(std::str::from_utf8(&out[1])
            .unwrap()
            .contains("\"finish_reason\":\"stop\""));
    }
}
