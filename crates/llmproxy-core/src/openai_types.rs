use serde::{Deserialize, Serialize};

/// Inbound from client — standard OpenAI chat/completions shape.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<serde_json::Value>),
}

impl MessageContent {
    /// Flatten to a single text string. For multimodal `Parts`, returns the
    /// first `text` part found, or empty string. Translation providers use
    /// this for MVP; multimodal support is deferred to v0.2.
    pub fn as_text(&self) -> &str {
        match self {
            MessageContent::Text(s) => s.as_str(),
            MessageContent::Parts(parts) => parts
                .iter()
                .find_map(|p| p.get("text").and_then(|v| v.as_str()))
                .unwrap_or(""),
        }
    }
}

/// Outbound to client — standard OpenAI chat.completion shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_content_text_flattens_text() {
        let c = MessageContent::Text("hello".into());
        assert_eq!(c.as_text(), "hello");
    }

    #[test]
    fn message_content_parts_picks_first_text() {
        let c = MessageContent::Parts(vec![
            serde_json::json!({"type": "text", "text": "hi"}),
            serde_json::json!({"type": "text", "text": "there"}),
        ]);
        assert_eq!(c.as_text(), "hi");
    }

    #[test]
    fn message_content_parts_empty_returns_empty() {
        let c = MessageContent::Parts(vec![
            serde_json::json!({"type": "image_url", "image_url": {"url": "..."}}),
        ]);
        assert_eq!(c.as_text(), "");
    }

    #[test]
    fn chat_request_deserializes_extras() {
        let raw = r#"{"model":"x","messages":[],"user":"alice"}"#;
        let req: ChatRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.model, "x");
        assert_eq!(req.extra.get("user").unwrap().as_str(), Some("alice"));
    }
}
