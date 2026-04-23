use std::time::{Duration, SystemTime};

use async_stream::try_stream;
use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sigv4::{
    http_request::{sign, SignableBody, SignableRequest, SigningSettings},
    sign::v4,
};
use bytes::Bytes;
use futures::{stream::BoxStream, StreamExt};
use llmproxy_core::{
    error::ProxyError,
    openai_types::{ChatMessage, ChatRequest, ChatResponse, Choice, MessageContent, Usage},
    provider::{Credential, Provider},
};
use serde_json::{json, Value};

/// Convert a `MessageContent` to Bedrock Converse content blocks.
/// Supports text and data-URL images; audio is not supported.
/// Returns `Err(ProxyError::NotImplemented)` when a non-empty `Parts` input
/// produces no translatable blocks, so callers never send `content: []`.
fn content_to_converse_parts(content: &MessageContent) -> Result<Vec<Value>, ProxyError> {
    match content {
        MessageContent::Text(s) => Ok(vec![json!({"text": s})]),
        MessageContent::Parts(parts) => {
            let translated: Vec<Value> = parts
                .iter()
                .filter_map(|p| match p.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        Some(json!({"text": text}))
                    }
                    Some("image_url") => {
                        let url = p.get("image_url")?.get("url")?.as_str()?;
                        let (mime, data) = crate::util::parse_data_url(url)?;
                        let format = match mime.split('/').nth(1).unwrap_or("jpeg") {
                            "jpeg" | "jpg" => "jpeg",
                            "png" => "png",
                            "gif" => "gif",
                            "webp" => "webp",
                            _ => "jpeg",
                        };
                        Some(json!({"image": {"format": format, "source": {"bytes": data}}}))
                    }
                    _ => None,
                })
                .collect();

            if !parts.is_empty() && translated.is_empty() {
                return Err(ProxyError::NotImplemented(
                    "Bedrock Converse only supports text and image_url (data URL) parts; \
                     audio and other media types are not yet supported"
                        .into(),
                ));
            }

            Ok(translated)
        }
    }
}

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

    fn stream_url(region: &str, model_id: &str) -> String {
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse-stream",
            region, model_id
        )
    }

    pub(crate) fn to_converse_body(req: &ChatRequest) -> Result<Value, ProxyError> {
        let system: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| json!({ "text": m.content.as_text() }))
            .collect();

        let mut messages: Vec<Value> = Vec::new();
        for m in req.messages.iter().filter(|m| m.role != "system") {
            messages.push(json!({
                "role": m.role,
                "content": content_to_converse_parts(&m.content)?,
            }));
        }

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
        Ok(body)
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

/// Advance `pos` past one AWS Event Stream header value based on its type byte.
///
/// Value type encodings:
///   0/1 → bool (0 bytes)    2 → i8 (1 byte)       3 → i16 (2 bytes)
///   4   → i32 (4 bytes)     5 → i64 (8 bytes)      6 → bytes (u16 len + data)
///   7   → string (u16 len + UTF-8 data)             8 → timestamp (8 bytes)
///   9   → UUID (16 bytes)
///
/// Returns `(string_value_if_type_7, new_pos)` or `None` if the buffer is too short.
fn skip_header_value(
    frame: &[u8],
    pos: usize,
    value_type: u8,
    limit: usize,
) -> Option<(Option<&str>, usize)> {
    match value_type {
        0 | 1 => Some((None, pos)),
        2 => (pos < limit).then_some((None, pos + 1)),
        3 => (pos + 1 < limit).then_some((None, pos + 2)),
        4 => (pos + 3 < limit).then_some((None, pos + 4)),
        5 | 8 => (pos + 7 < limit).then_some((None, pos + 8)),
        6 | 7 => {
            let len_end = pos + 2;
            if len_end > limit {
                return None;
            }
            let value_len = u16::from_be_bytes(frame[pos..len_end].try_into().ok()?) as usize;
            let value_end = len_end + value_len;
            if value_end > limit {
                return None;
            }
            let s = if value_type == 7 {
                std::str::from_utf8(&frame[len_end..value_end]).ok()
            } else {
                None
            };
            Some((s, value_end))
        }
        9 => (pos + 16 <= limit).then_some((None, pos + 16)),
        _ => None,
    }
}

/// Parse one AWS Event Stream binary frame and convert it to an OpenAI SSE chunk.
///
/// Frame layout (all lengths big-endian):
///   [0..4]  total_len   — byte length of the entire frame
///   [4..8]  headers_len — byte length of the headers section
///   [8..12] prelude_crc — CRC32 of bytes 0..8 (not validated here)
///   [12..12+headers_len] headers (each: 1-byte name-len, name, 1-byte value-type, then
///                        a type-specific value — see `skip_header_value`)
///   [12+headers_len..total_len-4] JSON payload
///   [total_len-4..total_len] message_crc (not validated here)
fn parse_event_frame_to_sse(
    frame: &[u8],
    id: &str,
    created: u64,
    model_id: &str,
    first_delta: &mut bool,
) -> Option<Bytes> {
    if frame.len() < 16 {
        return None;
    }
    let total_len = u32::from_be_bytes(frame[0..4].try_into().ok()?) as usize;
    let headers_len = u32::from_be_bytes(frame[4..8].try_into().ok()?) as usize;
    if total_len < 16 || frame.len() < total_len {
        return None;
    }

    let headers_end = 12 + headers_len;
    let payload_end = total_len - 4;
    if headers_end > payload_end || payload_end > frame.len() {
        return None;
    }

    // Decode headers to find :event-type and :message-type.
    let mut event_type = "";
    let mut message_type = "";
    let mut pos = 12usize;
    while pos < headers_end {
        let name_len = frame[pos] as usize;
        pos += 1;
        let name_end = pos + name_len;
        if name_end + 1 > headers_end {
            break;
        }
        let name = std::str::from_utf8(&frame[pos..name_end]).unwrap_or("");
        pos = name_end;
        let value_type = frame[pos];
        pos += 1;
        let (value, next_pos) = skip_header_value(frame, pos, value_type, headers_end)?;
        if let Some(v) = value {
            match name {
                ":event-type" => event_type = v,
                ":message-type" => message_type = v,
                _ => {}
            }
        }
        pos = next_pos;
    }

    // Propagate exception frames as a JSON error chunk so callers see them.
    if message_type == "exception" {
        let payload = std::str::from_utf8(&frame[headers_end..payload_end]).unwrap_or("{}");
        let line = format!("data: {{\"error\":{}}}\n\n", payload);
        return Some(Bytes::from(line));
    }

    let payload = &frame[headers_end..payload_end];
    let v: Value = serde_json::from_slice(payload).ok()?;

    let chunk = match event_type {
        "contentBlockDelta" => {
            let text = v["delta"]["text"].as_str()?;
            // The first content chunk also carries role: "assistant" so that
            // clients which require a role field see it on the opening delta.
            let delta = if *first_delta {
                *first_delta = false;
                json!({"role": "assistant", "content": text})
            } else {
                json!({"content": text})
            };
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_id,
                "choices": [{"index": 0, "delta": delta, "finish_reason": null}],
            })
        }
        "messageStop" => {
            let stop_reason = v["stopReason"].as_str().unwrap_or("end_turn");
            let finish = match stop_reason {
                "end_turn" | "stop_sequence" => "stop",
                "max_tokens" => "length",
                "tool_use" => "tool_calls",
                "content_filtered" => "content_filter",
                other => other,
            };
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_id,
                "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
            })
        }
        // messageStart, contentBlockStart, contentBlockStop, metadata — nothing to emit
        _ => return None,
    };

    let line = format!("data: {}\n\n", serde_json::to_string(&chunk).ok()?);
    Some(Bytes::from(line))
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
        let body = serde_json::to_vec(&Self::to_converse_body(&req)?)?;
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
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let region = match cred {
            Credential::AwsSigV4 { region, .. } => region.clone(),
            Credential::BearerToken(_) => {
                return Err(ProxyError::Config(
                    "bedrock requires AWS SigV4 credentials".into(),
                ));
            }
        };

        let url = Self::stream_url(&region, model_id);
        let body = serde_json::to_vec(&Self::to_converse_body(&req)?)?;
        let mut request = self.signed_request(&url, body, cred)?;
        // Accept header for binary event stream (not part of the signed canonical request)
        request.headers_mut().insert(
            "accept",
            "application/vnd.amazon.eventstream".parse().unwrap(),
        );

        let resp = self.client.execute(request).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProxyError::Upstream { status, body });
        }

        let mut upstream = resp.bytes_stream();
        let chat_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let model_id = model_id.to_string();
        let created = chrono::Utc::now().timestamp() as u64;

        let out = try_stream! {
            let mut buf = Vec::<u8>::new();
            let mut first_delta = true;

            while let Some(chunk) = upstream.next().await {
                let chunk = chunk.map_err(ProxyError::from)?;
                buf.extend_from_slice(&chunk);

                // Each iteration must consume at least one complete frame or break.
                while buf.len() >= 16 {
                    let total_len = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
                    // Reject frames that are structurally impossible (< 16) to avoid spin.
                    if total_len < 16 {
                        return;
                    }
                    if buf.len() < total_len {
                        break;
                    }
                    let frame = buf.drain(..total_len).collect::<Vec<u8>>();
                    if let Some(sse) =
                        parse_event_frame_to_sse(&frame, &chat_id, created, &model_id, &mut first_delta)
                    {
                        yield sse;
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
        let body = BedrockProvider::to_converse_body(&req()).unwrap();
        assert_eq!(body["system"][0]["text"], "be helpful");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 128);
        assert_eq!(body["inferenceConfig"]["temperature"], 0.5);
    }

    #[test]
    fn content_to_converse_parts_image() {
        let c = MessageContent::Parts(vec![
            serde_json::json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}),
        ]);
        let parts = content_to_converse_parts(&c).unwrap();
        assert_eq!(parts[0]["image"]["format"], "png");
        assert_eq!(parts[0]["image"]["source"]["bytes"], "abc");
    }

    #[test]
    fn content_to_converse_parts_unsupported_returns_err() {
        let c = MessageContent::Parts(vec![
            serde_json::json!({"type": "input_audio", "input_audio": {"data": "xyz", "format": "mp3"}}),
        ]);
        assert!(matches!(
            content_to_converse_parts(&c),
            Err(ProxyError::NotImplemented(_))
        ));
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

    #[test]
    fn stream_url_format() {
        assert_eq!(
            BedrockProvider::stream_url("us-west-2", "amazon.nova-pro-v1:0"),
            "https://bedrock-runtime.us-west-2.amazonaws.com/model/amazon.nova-pro-v1:0/converse-stream"
        );
    }

    fn make_event_frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
        // Build a minimal AWS Event Stream frame for testing.
        let name = b":event-type";
        let msg_type_name = b":message-type";
        let msg_type_value = b"event";

        // header 1: :event-type = event_type
        let mut headers = Vec::new();
        headers.push(name.len() as u8);
        headers.extend_from_slice(name);
        headers.push(7u8); // string type
        let et_bytes = event_type.as_bytes();
        headers.extend_from_slice(&(et_bytes.len() as u16).to_be_bytes());
        headers.extend_from_slice(et_bytes);

        // header 2: :message-type = event
        headers.push(msg_type_name.len() as u8);
        headers.extend_from_slice(msg_type_name);
        headers.push(7u8);
        headers.extend_from_slice(&(msg_type_value.len() as u16).to_be_bytes());
        headers.extend_from_slice(msg_type_value);

        let headers_len = headers.len() as u32;
        let total_len = (12 + headers.len() + payload.len() + 4) as u32;

        let mut frame = Vec::new();
        frame.extend_from_slice(&total_len.to_be_bytes());
        frame.extend_from_slice(&headers_len.to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes()); // prelude CRC (not validated)
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(payload);
        frame.extend_from_slice(&0u32.to_be_bytes()); // message CRC (not validated)
        frame
    }

    fn parse_frame(frame: &[u8], first: &mut bool) -> Option<Value> {
        let sse = parse_event_frame_to_sse(frame, "chatcmpl-test", 0, "model", first)?;
        let s = std::str::from_utf8(&sse).unwrap().to_string();
        let json_part = &s["data: ".len()..s.len() - 2];
        serde_json::from_str(json_part).ok()
    }

    #[test]
    fn parse_content_block_delta_first_includes_role() {
        let payload = br#"{"contentBlockIndex":0,"delta":{"text":"hello"}}"#;
        let frame = make_event_frame("contentBlockDelta", payload);
        let mut first = true;
        let v = parse_frame(&frame, &mut first).unwrap();
        assert_eq!(v["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(v["choices"][0]["delta"]["content"], "hello");
        assert_eq!(v["choices"][0]["finish_reason"], Value::Null);
        assert!(!first);
    }

    #[test]
    fn parse_content_block_delta_subsequent_omits_role() {
        let payload = br#"{"contentBlockIndex":0,"delta":{"text":"world"}}"#;
        let frame = make_event_frame("contentBlockDelta", payload);
        let mut first = false;
        let v = parse_frame(&frame, &mut first).unwrap();
        assert_eq!(v["choices"][0]["delta"]["role"], Value::Null);
        assert_eq!(v["choices"][0]["delta"]["content"], "world");
    }

    #[test]
    fn parse_message_stop_end_turn() {
        let payload = br#"{"stopReason":"end_turn"}"#;
        let frame = make_event_frame("messageStop", payload);
        let mut first = false;
        let v = parse_frame(&frame, &mut first).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn parse_message_stop_max_tokens() {
        let payload = br#"{"stopReason":"max_tokens"}"#;
        let frame = make_event_frame("messageStop", payload);
        let mut first = false;
        let v = parse_frame(&frame, &mut first).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn parse_message_stop_content_filtered() {
        let payload = br#"{"stopReason":"content_filtered"}"#;
        let frame = make_event_frame("messageStop", payload);
        let mut first = false;
        let v = parse_frame(&frame, &mut first).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "content_filter");
    }

    #[test]
    fn parse_unknown_event_returns_none() {
        let payload = br#"{"role":"assistant"}"#;
        let frame = make_event_frame("messageStart", payload);
        let mut first = true;
        assert!(parse_event_frame_to_sse(&frame, "id", 0, "model", &mut first).is_none());
    }

    #[test]
    fn parse_truncated_frame_returns_none() {
        // A frame that claims total_len = 1000 but only has 16 bytes.
        let mut frame = vec![0u8; 16];
        frame[0..4].copy_from_slice(&1000u32.to_be_bytes());
        let mut first = true;
        assert!(parse_event_frame_to_sse(&frame, "id", 0, "model", &mut first).is_none());
    }
}
