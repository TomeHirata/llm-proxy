// Translation helpers for routing native-format requests from coding agents
// (Claude Code /anthropic/v1/messages, Gemini CLI /gemini/v1beta/...) through
// the unified provider pipeline when a non-native provider/model is requested.

use bytes::Bytes;
use serde_json::{json, Value};

use llmproxy_core::{
    error::ProxyError,
    openai_types::{ChatMessage, ChatRequest, ChatResponse, MessageContent},
};

// ─── Anthropic ↔ OpenAI ────────────────────────────────────────────────────

/// Parse an Anthropic `/v1/messages` request body into a `ChatRequest`.
pub fn anthropic_to_chat_request(body: &[u8]) -> Result<ChatRequest, ProxyError> {
    let v: Value = serde_json::from_slice(body).map_err(ProxyError::Serde)?;

    let model = v["model"].as_str().unwrap_or("").to_string();
    let mut messages = Vec::new();

    if let Some(sys) = v["system"].as_str() {
        if !sys.is_empty() {
            messages.push(msg("system", sys));
        }
    }

    for m in v["messages"].as_array().unwrap_or(&vec![]) {
        let role = m["role"].as_str().unwrap_or("user");
        let content = match &m["content"] {
            Value::String(s) => MessageContent::Text(s.clone()),
            Value::Array(parts) => MessageContent::Parts(
                parts
                    .iter()
                    .map(|p| match p["type"].as_str() {
                        Some("text") => json!({"type":"text","text":p["text"]}),
                        Some("image") => {
                            let url = match p["source"]["type"].as_str() {
                                Some("base64") => format!(
                                    "data:{};base64,{}",
                                    p["source"]["media_type"].as_str().unwrap_or("image/jpeg"),
                                    p["source"]["data"].as_str().unwrap_or("")
                                ),
                                Some("url") => {
                                    p["source"]["url"].as_str().unwrap_or("").to_string()
                                }
                                _ => String::new(),
                            };
                            json!({"type":"image_url","image_url":{"url":url}})
                        }
                        _ => p.clone(),
                    })
                    .collect(),
            ),
            _ => MessageContent::Text(String::new()),
        };
        messages.push(ChatMessage {
            role: role.to_string(),
            content,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }

    Ok(ChatRequest {
        model,
        messages,
        stream: v["stream"].as_bool(),
        temperature: v["temperature"].as_f64().map(|f| f as f32),
        max_tokens: v["max_tokens"].as_u64().map(|n| n as u32),
        top_p: v["top_p"].as_f64().map(|f| f as f32),
        stop: v.get("stop_sequences").cloned(),
        tools: None,
        tool_choice: None,
        response_format: None,
        extra: Default::default(),
    })
}

/// Encode a `ChatResponse` as an Anthropic `/v1/messages` response body.
pub fn chat_response_to_anthropic(resp: &ChatResponse) -> Value {
    let choice = resp.choices.first();
    let text = choice
        .map(|c| c.message.content.as_text())
        .unwrap_or("")
        .to_string();
    let stop_reason = choice
        .and_then(|c| c.finish_reason.as_deref())
        .map(|r| match r {
            "length" => "max_tokens",
            _ => "end_turn",
        })
        .unwrap_or("end_turn");

    json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "model": resp.model,
        "content": [{"type":"text","text":text}],
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": resp.usage.as_ref().map(|u| json!({
            "input_tokens":  u.prompt_tokens,
            "output_tokens": u.completion_tokens,
        })).unwrap_or(json!({"input_tokens":0,"output_tokens":0})),
    })
}

/// Stateful adapter that converts an OpenAI SSE byte stream into Anthropic SSE
/// format consumed by Claude Code.
pub struct AnthropicStreamAdapter {
    buf: String,
    initialized: bool,
    model: String,
    msg_id: String,
    output_tokens: u32,
    finished: bool,
}

impl AnthropicStreamAdapter {
    pub fn new(model: &str) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        Self {
            buf: String::new(),
            initialized: false,
            model: model.to_string(),
            msg_id: format!("msg_{nonce:08x}"),
            output_tokens: 0,
            finished: false,
        }
    }

    /// Feed a chunk from the OpenAI SSE stream; returns Anthropic SSE bytes.
    pub fn process(&mut self, chunk: Bytes) -> Vec<Bytes> {
        self.buf.push_str(&String::from_utf8_lossy(&chunk));
        let mut out = Vec::new();
        while let Some(end) = self.buf.find("\n\n") {
            let event = self.buf[..end].to_string();
            self.buf = self.buf[end + 2..].to_string();
            out.extend(self.handle_event(&event));
        }
        out
    }

    fn handle_event(&mut self, event: &str) -> Vec<Bytes> {
        let mut out = Vec::new();
        for line in event.lines() {
            if !line.starts_with("data: ") {
                continue;
            }
            let data = line[6..].trim();
            if data == "[DONE]" {
                if !self.finished {
                    self.finished = true;
                    if !self.initialized {
                        self.initialized = true;
                        out.push(self.msg_start());
                        out.push(self.block_start());
                    }
                    out.extend(self.close("end_turn"));
                }
                return out;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let delta = v["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or("");
            let finish = v["choices"][0]["finish_reason"]
                .as_str()
                .filter(|s| !s.is_empty() && *s != "null");
            if let Some(ct) = v["usage"]["completion_tokens"].as_u64() {
                self.output_tokens = ct as u32;
            }

            if !delta.is_empty() {
                if !self.initialized {
                    self.initialized = true;
                    out.push(self.msg_start());
                    out.push(self.block_start());
                }
                self.output_tokens = self.output_tokens.saturating_add(1);
                out.push(self.content_delta(delta));
            }

            if let Some(reason) = finish {
                if !self.finished {
                    self.finished = true;
                    if !self.initialized {
                        self.initialized = true;
                        out.push(self.msg_start());
                        out.push(self.block_start());
                    }
                    let stop = if reason == "length" { "max_tokens" } else { "end_turn" };
                    out.extend(self.close(stop));
                }
            }
        }
        out
    }

    fn sse(event_type: &str, data: Value) -> Bytes {
        Bytes::from(format!("event: {event_type}\ndata: {data}\n\n"))
    }

    fn msg_start(&self) -> Bytes {
        Self::sse(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens":0,"output_tokens":0},
                }
            }),
        )
    }

    fn block_start(&self) -> Bytes {
        Self::sse(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type":"text","text":""},
            }),
        )
    }

    fn content_delta(&self, text: &str) -> Bytes {
        Self::sse(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type":"text_delta","text":text},
            }),
        )
    }

    fn close(&self, stop_reason: &str) -> Vec<Bytes> {
        vec![
            Self::sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            Self::sse(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason":stop_reason,"stop_sequence":null},
                    "usage": {"output_tokens":self.output_tokens},
                }),
            ),
            Self::sse("message_stop", json!({"type":"message_stop"})),
        ]
    }
}

// ─── Gemini ↔ OpenAI ──────────────────────────────────────────────────────

/// Parse a Gemini `generateContent` request body into a `ChatRequest`.
pub fn gemini_to_chat_request(model: &str, body: &[u8]) -> Result<ChatRequest, ProxyError> {
    let v: Value = serde_json::from_slice(body).map_err(ProxyError::Serde)?;

    let mut messages = Vec::new();

    if let Some(parts) = v["systemInstruction"]["parts"].as_array() {
        let text: String = parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            messages.push(msg("system", &text));
        }
    }

    for c in v["contents"].as_array().unwrap_or(&vec![]) {
        let role = if c["role"].as_str() == Some("model") { "assistant" } else { "user" };
        let text: String = c["parts"]
            .as_array()
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        messages.push(msg(role, &text));
    }

    let gc = &v["generationConfig"];
    Ok(ChatRequest {
        model: model.to_string(),
        messages,
        stream: None,
        temperature: gc["temperature"].as_f64().map(|f| f as f32),
        max_tokens: gc["maxOutputTokens"].as_u64().map(|n| n as u32),
        top_p: gc["topP"].as_f64().map(|f| f as f32),
        stop: gc.get("stopSequences").cloned(),
        tools: None,
        tool_choice: None,
        response_format: None,
        extra: Default::default(),
    })
}

/// Encode a `ChatResponse` as a Gemini `generateContent` response body.
pub fn chat_response_to_gemini(resp: &ChatResponse) -> Value {
    let choice = resp.choices.first();
    let text = choice
        .map(|c| c.message.content.as_text())
        .unwrap_or("")
        .to_string();
    let finish_reason = choice
        .and_then(|c| c.finish_reason.as_deref())
        .map(|r| match r {
            "length" => "MAX_TOKENS",
            _ => "STOP",
        })
        .unwrap_or("STOP");

    json!({
        "candidates": [{
            "content": {"role":"model","parts":[{"text":text}]},
            "finishReason": finish_reason,
            "index": 0,
        }],
        "usageMetadata": resp.usage.as_ref().map(|u| json!({
            "promptTokenCount":     u.prompt_tokens,
            "candidatesTokenCount": u.completion_tokens,
            "totalTokenCount":      u.total_tokens,
        })).unwrap_or(json!({})),
        "modelVersion": resp.model,
    })
}

/// Stateful adapter that converts an OpenAI SSE byte stream into Gemini SSE
/// format consumed by the Gemini CLI.
pub struct GeminiStreamAdapter {
    buf: String,
    model: String,
}

impl GeminiStreamAdapter {
    pub fn new(model: &str) -> Self {
        Self {
            buf: String::new(),
            model: model.to_string(),
        }
    }

    pub fn process(&mut self, chunk: Bytes) -> Vec<Bytes> {
        self.buf.push_str(&String::from_utf8_lossy(&chunk));
        let mut out = Vec::new();
        while let Some(end) = self.buf.find("\n\n") {
            let event = self.buf[..end].to_string();
            self.buf = self.buf[end + 2..].to_string();
            if let Some(b) = self.handle_event(&event) {
                out.push(b);
            }
        }
        out
    }

    fn handle_event(&self, event: &str) -> Option<Bytes> {
        for line in event.lines() {
            if !line.starts_with("data: ") {
                continue;
            }
            let data = line[6..].trim();
            if data == "[DONE]" {
                return None;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let delta = v["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or("");
            let finish = v["choices"][0]["finish_reason"]
                .as_str()
                .filter(|s| !s.is_empty() && *s != "null");
            let finish_str = finish
                .map(|r| if r == "length" { "MAX_TOKENS" } else { "STOP" })
                .unwrap_or("");
            let usage = if finish.is_some() {
                match (
                    v["usage"]["prompt_tokens"].as_u64(),
                    v["usage"]["completion_tokens"].as_u64(),
                ) {
                    (Some(p), Some(c)) => {
                        json!({"promptTokenCount":p,"candidatesTokenCount":c,"totalTokenCount":p+c})
                    }
                    _ => json!({}),
                }
            } else {
                json!({})
            };

            let chunk = json!({
                "candidates": [{
                    "content": {"role":"model","parts":[{"text":delta}]},
                    "finishReason": finish_str,
                    "index": 0,
                }],
                "usageMetadata": usage,
                "modelVersion": self.model,
            });
            return Some(Bytes::from(format!("data: {chunk}\n\n")));
        }
        None
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────

fn msg(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: MessageContent::Text(text.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
    }
}
