# llmproxy — MVP Design & Implementation Guide

> **For coding agents:** This document is the authoritative spec. Implement strictly
> in the order described in §13 (Implementation Order). Each section includes the
> exact API endpoints, wire formats, and known gotchas needed to write correct code
> without additional research. Do not add features not listed here for v0.1.

Localhost-focused Rust binary that exposes an **OpenAI-compatible API**
(`/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`) and proxies to
every provider MLflow AI Gateway supports natively.

No custom unified schema. Clients use the standard OpenAI SDK pointed at
`http://localhost:8080` — zero code changes required.

---

## 1. MVP Provider Scope (mirrors MLflow AI Gateway)

Drawn directly from `mlflow.gateway.config.Provider`:

| Provider key         | Auth mechanism                          | Priority |
|----------------------|-----------------------------------------|----------|
| `openai`             | `OPENAI_API_KEY`                        | MVP      |
| `anthropic`          | `ANTHROPIC_API_KEY`                     | MVP      |
| `gemini`             | `GEMINI_API_KEY`                        | MVP      |
| `bedrock`            | AWS SigV4 signed via `reqwest` + `aws-sigv4` | MVP |
| `azure`              | `AZURE_OPENAI_API_KEY` + endpoint       | MVP      |
| `mistral`            | `MISTRAL_API_KEY`                       | MVP      |
| `cohere`             | `COHERE_API_KEY`                        | v0.2     |
| `togetherai`         | `TOGETHERAI_API_KEY`                    | v0.2     |
| `huggingface-tgi`    | `HF_API_KEY` + server URL              | v0.2     |
| `ai21labs`           | `AI21LABS_API_KEY`                      | v0.3     |
| `mlflow-model-serving` | MLflow server URL                    | v0.3     |

**Gemini** is the key addition vs. the previous design. Unlike the others it uses
the Google AI SDK REST format, not OpenAI-compat, so it needs a real translation layer.

---

## 2. Simplified Architecture (no unified schema)

Since the API surface is purely OpenAI-compatible, the proxy's job is:

```
Client (OpenAI SDK, curl, etc.)
  │
  │  POST /v1/chat/completions   { "model": "anthropic/claude-sonnet-4-5", ... }
  │  Authorization: Bearer <api_key>   ← optional; overrides config if present
  ▼
┌──────────────────────────────────┐
│  Axum HTTP server                │
│  deserialize → OpenAIChatRequest │
│  extract Authorization header    │
└──────────────┬───────────────────┘
               │  parse "provider/model" from req.model
               ▼
┌──────────────────────────────────┐
│  model field split               │
│  "anthropic/claude-sonnet-4-5"   │
│    → provider="anthropic"        │
│    → model_id="claude-sonnet-4-5"│
│                                  │
│  credential resolution:          │
│  1. Authorization header (first) │
│  2. config file (fallback)       │
└──────────────┬───────────────────┘
               │  look up provider in registry by name
               ▼
       ┌───────┴──────────────────┐
       │  Provider::forward()     │
       │                          │
       │  OpenAI / Azure / Mistral│ ← passthrough (already OpenAI-compat)
       │  Anthropic               │ ← translate req + resp
       │  Gemini                  │ ← translate req + resp
       │  Bedrock                 │ ← SigV4-signed reqwest (no AWS SDK)
       │  Cohere / TogetherAI     │ ← translate req + resp
       └──────────────────────────┘
               │
               ▼
       OpenAIChatResponse → client
```

For **passthrough providers** (OpenAI, Azure, Mistral, TogetherAI — all already
speak OpenAI format), the proxy simply:
1. Swaps out the base URL and injects the API key header
2. Streams the response bytes back unchanged

For **translation providers** (Anthropic, Gemini, Bedrock, Cohere), the proxy
deserializes the OpenAI request, converts to the native format, calls the API,
and converts the response back to OpenAI format — including SSE streaming.

---

## 3. Project Layout

```
llmproxy/
├── Cargo.toml                    # workspace root (no [package], only [workspace])
├── config.example.yaml
├── .github/
│   └── workflows/
│       ├── ci.yml
│       └── release.yml
└── crates/
    ├── llmproxy-core/            # OpenAI types + Provider trait; zero I/O
    │   └── src/
    │       ├── lib.rs
    │       ├── openai_types.rs   # ChatRequest, ChatResponse, StreamChunk
    │       ├── provider.rs       # Provider trait
    │       └── error.rs          # ProxyError (thiserror)
    ├── llmproxy-providers/       # one module per provider; depends on core only
    │   └── src/
    │       ├── lib.rs
    │       ├── passthrough.rs    # OpenAI / Azure / Mistral / TogetherAI
    │       ├── anthropic.rs
    │       ├── gemini.rs
    │       ├── bedrock.rs
    │       └── cohere.rs
    └── llmproxy-server/          # Axum server + config + CLI; depends on both
        └── src/
            ├── main.rs
            ├── server.rs
            ├── config.rs
            └── registry.rs       # provider name → Arc<dyn Provider> (no aliases)
```

**Dependency direction:** `server` → `providers` → `core`. Core has no dependency on the others.

### Workspace Cargo.toml

```toml
# Cargo.toml (workspace root)
[workspace]
members = [
    "crates/llmproxy-core",
    "crates/llmproxy-providers",
    "crates/llmproxy-server",
]
resolver = "2"

[workspace.dependencies]
# Pin versions here; crates reference them with { workspace = true }
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
tokio       = { version = "1", features = ["full"] }
reqwest     = { version = "0.12", features = ["json", "stream"] }
thiserror   = "1"
async-trait = "0.1"
tracing     = "0.1"
bytes       = "1"
futures     = "0.3"
uuid        = { version = "1", features = ["v4"] }
chrono      = { version = "0.4", features = ["serde"] }
```

### crates/llmproxy-core/Cargo.toml

```toml
[package]
name    = "llmproxy-core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde       = { workspace = true }
serde_json  = { workspace = true }
thiserror   = { workspace = true }
async-trait = { workspace = true }
bytes       = { workspace = true }
futures     = { workspace = true }
```

### crates/llmproxy-providers/Cargo.toml

```toml
[package]
name    = "llmproxy-providers"
version = "0.1.0"
edition = "2021"

[dependencies]
llmproxy-core = { path = "../llmproxy-core" }
serde         = { workspace = true }
serde_json    = { workspace = true }
reqwest       = { workspace = true }
tokio         = { workspace = true }
async-trait   = { workspace = true }
bytes         = { workspace = true }
futures       = { workspace = true }
tracing       = { workspace = true }
uuid          = { workspace = true }
chrono        = { workspace = true }
async-stream  = "0.3"
# Bedrock SigV4 signing — much lighter than the full AWS SDK
aws-sigv4     = "1"
aws-credential-types = "1"
```

### crates/llmproxy-server/Cargo.toml

```toml
[package]
name    = "llmproxy-server"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "llmproxy"
path = "src/main.rs"

[dependencies]
llmproxy-core      = { path = "../llmproxy-core" }
llmproxy-providers = { path = "../llmproxy-providers" }
serde       = { workspace = true }
serde_json  = { workspace = true }
tokio       = { workspace = true }
tracing     = { workspace = true }
axum        = { version = "0.7", features = ["macros"] }
tower-http  = { version = "0.5", features = ["trace", "cors"] }
clap        = { version = "4", features = ["derive", "env"] }
config      = "0.14"    # layered config with env var interpolation
dotenvy     = "0.15"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[package.metadata.deb]
assets = [
    ["target/release/llmproxy", "usr/bin/", "755"],
    ["config.example.yaml", "etc/llmproxy/config.yaml", "644"],
]
```

---

## 4. Core Types (llmproxy-core)

Only OpenAI request/response types are needed — no custom schema.

### error.rs

```rust
// crates/llmproxy-core/src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("AWS SDK error: {0}")]
    Aws(String),   // Box the SDK errors as strings to avoid generic complexity

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Provider config error: {0}")]
    Config(String),

    #[error("Upstream error {status}: {body}")]
    Upstream { status: u16, body: String },

    #[error("Stream error: {0}")]
    Stream(String),
}
```

### openai_types.rs

```rust
// crates/llmproxy-core/src/openai_types.rs

/// Inbound from client — standard OpenAI chat/completions shape
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: Option<bool>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f32>,
    pub stop: Option<serde_json::Value>,           // string or [string]
    pub tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,
    pub response_format: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>, // forward unknowns to passthroughs
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<serde_json::Value>),  // [{type:"text",text:...},{type:"image_url",...}]
}

impl MessageContent {
    /// Flatten all text parts to a single string. Used by translation providers
    /// that don't support multimodal (Anthropic handles images natively, but
    /// Bedrock Converse and Gemini need special handling — use this for simplicity
    /// in MVP, and add image support in v0.2).
    pub fn as_text(&self) -> &str {
        match self {
            MessageContent::Text(s) => s.as_str(),
            MessageContent::Parts(parts) => {
                // Return first text part found, or empty string
                parts.iter()
                    .find_map(|p| p["text"].as_str())
                    .unwrap_or("")
            }
        }
    }
}

/// Outbound to client — standard OpenAI chat.completion shape
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,     // always "chat.completion"
    pub created: u64,       // unix timestamp seconds
    pub model: String,      // echo the model_id used (not the alias)
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,  // "stop" | "length" | "tool_calls" | "content_filter"
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
```

### SSE streaming wire format

For streaming, providers return `BoxStream<'static, Result<Bytes, ProxyError>>`.
Each `Bytes` item must be a **complete SSE event** already in OpenAI format,
ready to write to the socket:

```
data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}\n\n
```

The final item before the stream closes:
```
data: {"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n
```

Followed by:
```
data: [DONE]\n\n
```

**Passthrough providers** relay the upstream bytes verbatim — they already arrive in this format.
**Translation providers** must convert their native SSE chunks into this format before yielding.

### registry.rs (in llmproxy-server)

The registry is a simple `HashMap<String, Arc<dyn Provider>>` keyed by provider
name. No aliases. The `model` field in every request is parsed as `"provider/model_id"`.

```rust
// crates/llmproxy-server/src/registry.rs
use std::{collections::HashMap, sync::Arc};
use llmproxy_core::{error::ProxyError, provider::Provider};

pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    /// Parse "provider/model_id" and return the provider + native model id.
    /// Returns an error if the format is wrong or the provider is not configured.
    pub fn resolve(&self, model_field: &str) -> Result<(Arc<dyn Provider>, String), ProxyError> {
        let (provider_name, model_id) = model_field
            .split_once('/')
            .ok_or_else(|| ProxyError::ModelNotFound(
                format!("model must be in 'provider/model_id' format, got: '{}'", model_field)
            ))?;

        let provider = self.providers.get(provider_name)
            .ok_or_else(|| ProxyError::ModelNotFound(
                format!("provider '{}' is not configured", provider_name)
            ))?;

        Ok((Arc::clone(provider), model_id.to_string()))
    }

    /// List all configured provider names (used by GET /v1/models).
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
}
```

**Routing examples:**

| `model` field in request         | Resolved provider | model_id passed to provider |
|----------------------------------|-------------------|-----------------------------|
| `"openai/gpt-4o"`                | OpenAI            | `"gpt-4o"`                  |
| `"anthropic/claude-sonnet-4-5"`  | Anthropic         | `"claude-sonnet-4-5"`       |
| `"gemini/gemini-2.5-flash"`      | Gemini            | `"gemini-2.5-flash"`        |
| `"bedrock/amazon.nova-pro-v1:0"` | Bedrock           | `"amazon.nova-pro-v1:0"`    |
| `"azure/my-gpt4-deployment"`     | Azure             | `"my-gpt4-deployment"`      |
| `"mistral/mistral-large-latest"` | Mistral           | `"mistral-large-latest"`    |

**Error response** when format is invalid or provider not configured:
```json
{ "error": { "message": "provider 'xyz' is not configured", "type": "proxy_error", "code": "model_not_found" } }
```
HTTP status: `404`.

---

## 5. Config File

The config file only contains **provider credentials** — no model aliases, no daemon flag.
The `--daemon` flag is a CLI argument only (see §16).

The config file is **optional entirely** — if a client passes an API key in the
`Authorization: Bearer <key>` header, the proxy uses it directly for that request
without needing a config entry for that provider (see §5a).

```yaml
# ~/.config/llmproxy/config.yaml

server:
  host: 127.0.0.1
  port: 8080

providers:
  openai:
    api_key: ${OPENAI_API_KEY}

  anthropic:
    api_key: ${ANTHROPIC_API_KEY}

  gemini:
    api_key: ${GEMINI_API_KEY}

  bedrock:
    region: us-east-1
    # credentials from env: AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY
    # or profile: profile_name: myprofile

  azure:
    api_key: ${AZURE_OPENAI_API_KEY}
    endpoint: https://my-resource.openai.azure.com
    api_version: "2024-02-01"

  mistral:
    api_key: ${MISTRAL_API_KEY}
```

**No `models:` block.** Clients pass `"provider/model_id"` as the model field:

```python
# Python example — zero config on the client side
from openai import OpenAI
client = OpenAI(base_url="http://localhost:8080/v1", api_key="unused")
resp = client.chat.completions.create(
    model="anthropic/claude-sonnet-4-5",
    messages=[{"role": "user", "content": "hello"}]
)
```

### 5a. Auth header passthrough (config-free mode)

Any provider that uses a simple Bearer token (OpenAI, Anthropic, Gemini, Mistral,
Cohere, TogetherAI) can be used **without a config file** by passing the API key
directly in the `Authorization` header. The proxy extracts it and uses it for
that request only.

```bash
# No config file needed — key passed per-request
curl http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer sk-ant-..." \
  -H "Content-Type: application/json" \
  -d '{"model": "anthropic/claude-sonnet-4-5", "messages": [...]}'
```

**Credential resolution order** (per request):
1. `Authorization: Bearer <token>` header from the incoming request
2. Provider entry in `~/.config/llmproxy/config.yaml`
3. Standard environment variable for that provider (e.g. `ANTHROPIC_API_KEY`)
4. Error: `401 Unauthorized` — `{"error": {"message": "no credentials for provider 'anthropic'"}}`

**Bedrock** is the exception: it requires AWS SigV4 signing which needs
`AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` + `AWS_REGION`. These are read
from environment variables or `~/.aws/credentials` — there is no single-token
header equivalent. The `Authorization` header override does not apply to Bedrock.

**Provider trait change** — the credential is passed per-call rather than baked
into the provider struct at startup:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,       // resolved per-request
    ) -> Result<ChatResponse, ProxyError>;

    async fn chat_stream(
        &self,
        req: ChatRequest,
        model_id: &str,
        cred: &Credential,
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError>;
}

/// Resolved credential for a single request
pub enum Credential {
    BearerToken(String),        // all non-AWS providers
    AwsSigV4 {                  // bedrock only
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
        region: String,
    },
}
```

**ProviderRegistry update** — `resolve()` now also returns a `Credential`:

```rust
pub fn resolve(
    &self,
    model_field: &str,
    auth_header: Option<&str>,  // value of Authorization header, if present
) -> Result<(Arc<dyn Provider>, String, Credential), ProxyError> {
    let (provider_name, model_id) = model_field
        .split_once('/')
        .ok_or_else(|| ProxyError::ModelNotFound(
            format!("model must be 'provider/model_id', got '{}'", model_field)
        ))?;

    let provider = self.providers.get(provider_name)
        .ok_or_else(|| ProxyError::ModelNotFound(
            format!("provider '{}' is not configured", provider_name)
        ))?;

    let cred = self.resolve_credential(provider_name, auth_header)?;
    Ok((Arc::clone(provider), model_id.to_string(), cred))
}

fn resolve_credential(
    &self,
    provider_name: &str,
    auth_header: Option<&str>,
) -> Result<Credential, ProxyError> {
    // 1. Authorization header (strip "Bearer " prefix)
    if let Some(h) = auth_header {
        let token = h.trim_start_matches("Bearer ").trim().to_string();
        if !token.is_empty() && provider_name != "bedrock" {
            return Ok(Credential::BearerToken(token));
        }
    }

    // 2. Config file value (already loaded at startup into self.credentials)
    if let Some(cred) = self.credentials.get(provider_name) {
        return Ok(cred.clone());
    }

    // 3. Well-known env vars
    let env_key = match provider_name {
        "openai"     => "OPENAI_API_KEY",
        "anthropic"  => "ANTHROPIC_API_KEY",
        "gemini"     => "GEMINI_API_KEY",
        "mistral"    => "MISTRAL_API_KEY",
        "cohere"     => "COHERE_API_KEY",
        "togetherai" => "TOGETHERAI_API_KEY",
        _ => return Err(ProxyError::Config(
            format!("no credentials for provider '{}'", provider_name)
        )),
    };
    std::env::var(env_key)
        .map(|v| Credential::BearerToken(v))
        .map_err(|_| ProxyError::Config(
            format!("no credentials for provider '{}' (set {} or pass Authorization header)", provider_name, env_key)
        ))
}
```

---

## 6. Provider Implementations

### 6a. Passthrough (OpenAI / Azure / Mistral / TogetherAI)

These already speak OpenAI format. The proxy just relays bytes.

**API references:**
- OpenAI: `https://api.openai.com/v1` — Auth: `Authorization: Bearer $OPENAI_API_KEY`
- Azure: `https://{resource}.openai.azure.com/openai/deployments/{deployment}/chat/completions?api-version=2024-02-01` — Auth: `api-key: $AZURE_OPENAI_API_KEY` (not Bearer)
- Mistral: `https://api.mistral.ai/v1` — Auth: `Authorization: Bearer $MISTRAL_API_KEY`
- TogetherAI: `https://api.together.xyz/v1` — Auth: `Authorization: Bearer $TOGETHERAI_API_KEY`

**Azure gotcha:** Azure uses a different URL structure and `api-key` header instead
of `Authorization: Bearer`. Also the model field in the request body is ignored by
Azure — the deployment name in the URL is what selects the model. Still send `model`
in the body to avoid errors from strict request validation.

```rust
// crates/llmproxy-providers/src/passthrough.rs

pub struct PassthroughProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    auth_header: AuthHeader,  // Bearer or ApiKey (Azure)
}

pub enum AuthHeader {
    Bearer,
    ApiKey,  // Azure uses "api-key" header, not "Authorization: Bearer"
}

#[async_trait]
impl Provider for PassthroughProvider {
    async fn chat(&self, req: ChatRequest, model_id: &str) -> Result<ChatResponse, ProxyError> {
        let mut body = serde_json::to_value(&req)?;
        body["model"] = json!(model_id);

        let resp = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send().await?;

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
    ) -> Result<BoxStream<'static, Result<Bytes, ProxyError>>, ProxyError> {
        let mut body = serde_json::to_value(&req)?;
        body["model"] = json!(model_id);
        body["stream"] = json!(true);

        let stream = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send().await?
            .bytes_stream()
            .map_err(ProxyError::from);

        Ok(Box::pin(stream))   // SSE bytes relayed verbatim
    }
}
```

### 6b. Anthropic (translation required)

**API reference:** https://docs.anthropic.com/en/api/messages

**Endpoint:** `POST https://api.anthropic.com/v1/messages`
**Auth headers:**
```
x-api-key: $ANTHROPIC_API_KEY
anthropic-version: 2023-06-01
Content-Type: application/json
```

Key differences from OpenAI:
- `system` is a top-level string field, not a message with `role: "system"`
- `max_tokens` is **required** — Anthropic has no default. Use `4096` if not provided.
- Response: `content[].text` instead of `choices[0].message.content`
- `stop_reason` instead of `finish_reason`. Mapping: `"end_turn"` → `"stop"`, `"max_tokens"` → `"length"`, `"tool_use"` → `"tool_calls"`
- Usage: `input_tokens` / `output_tokens` instead of `prompt_tokens` / `completion_tokens`

**SSE streaming — Anthropic event types to handle:**
```
event: message_start       → ignore (metadata)
event: content_block_start → ignore
event: ping                → ignore
event: content_block_delta → extract delta.text, emit OpenAI chunk
event: content_block_stop  → ignore
event: message_delta       → contains stop_reason, emit final OpenAI chunk
event: message_stop        → emit data: [DONE]
event: error               → propagate as ProxyError::Upstream
```

Each SSE event from Anthropic arrives as two lines: `event: <type>\ndata: <json>`.
Only `content_block_delta` and `message_delta` produce output chunks.

```rust
// crates/llmproxy-providers/src/anthropic.rs

impl AnthropicProvider {
    fn to_anthropic(&self, req: &ChatRequest, model_id: &str) -> serde_json::Value {
        let system = req.messages.iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_text());

        let messages: Vec<_> = req.messages.iter()
            .filter(|m| m.role != "system")
            .map(|m| json!({ "role": m.role, "content": m.content }))
            .collect();

        let mut body = json!({
            "model": model_id,
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "messages": messages,
        });
        if let Some(sys) = system { body["system"] = json!(sys); }
        if let Some(t) = req.temperature { body["temperature"] = json!(t); }
        if let Some(tools) = &req.tools { body["tools"] = json!(tools); }
        body
    }

    fn from_anthropic(resp: serde_json::Value) -> ChatResponse {
        let text = resp["content"][0]["text"].as_str().unwrap_or("").to_string();
        let input_tokens = resp["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
        let output_tokens = resp["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;

        let finish_reason = match resp["stop_reason"].as_str() {
            Some("end_turn")    => Some("stop".to_string()),
            Some("max_tokens")  => Some("length".to_string()),
            Some("tool_use")    => Some("tool_calls".to_string()),
            other               => other.map(String::from),
        };

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
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                finish_reason,
            }],
            usage: Some(Usage {
                prompt_tokens: input_tokens,
                completion_tokens: output_tokens,
                total_tokens: input_tokens + output_tokens,
            }),
        }
    }
}
```

### 6c. Gemini (translation required)

**API reference:** https://ai.google.dev/api/generate-content

**Endpoints:**
- Non-streaming: `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}`
- Streaming: `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?alt=sse&key={api_key}`

**Auth:** API key as query param `?key=...` — no Authorization header needed.

**Gotchas:**
- Gemini rejects requests if there are two consecutive messages with the same `role`. OpenAI allows multiple user/assistant turns but Gemini requires strict alternation. If you receive consecutive same-role messages, merge their content before sending.
- `role` in Gemini is `"user"` or `"model"` (not `"assistant"`).
- `system` message maps to a top-level `systemInstruction` field, not in `contents`.
- Generation params go in a nested `generationConfig` object (not top-level).
- Gemini SSE: each chunk is a complete JSON object wrapped in `data: {...}`. Each chunk has the full `candidates[0].content.parts` — it is **not** a delta. To convert to OpenAI delta format, just emit the text from each chunk as `delta.content`.
- `finishReason` values: `"STOP"` → `"stop"`, `"MAX_TOKENS"` → `"length"`, `"SAFETY"` → `"content_filter"`.

Gemini's REST API (`generativelanguage.googleapis.com`) uses a completely different
shape. Key mappings:

| OpenAI                        | Gemini                                      |
|-------------------------------|---------------------------------------------|
| `messages[].role = "user"`    | `contents[].role = "user"`                 |
| `messages[].role = "assistant"`| `contents[].role = "model"`               |
| `messages[].content = "text"` | `contents[].parts[0].text = "text"`        |
| `system` message              | `systemInstruction.parts[0].text`           |
| `tools[].function`            | `tools[].functionDeclarations[]`            |
| `choices[0].message.content`  | `candidates[0].content.parts[0].text`       |
| SSE `choices[0].delta.content`| SSE `candidates[0].content.parts[0].text`  |
| `temperature`                 | `generationConfig.temperature`              |
| `max_tokens`                  | `generationConfig.maxOutputTokens`          |
| `stop`                        | `generationConfig.stopSequences`            |
| `usage.prompt_tokens`         | `usageMetadata.promptTokenCount`            |
| `usage.completion_tokens`     | `usageMetadata.candidatesTokenCount`        |

Endpoint: `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}`
Streaming: `:streamGenerateContent?alt=sse&key={api_key}`

```rust
// crates/llmproxy-providers/src/gemini.rs

impl GeminiProvider {
    fn to_gemini(&self, req: &ChatRequest, model_id: &str) -> (String, serde_json::Value) {
        let stream = req.stream.unwrap_or(false);
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:{}?key={}",
            model_id,
            if stream { "streamGenerateContent?alt=sse" } else { "generateContent" },
            self.api_key
        );

        let system_text = req.messages.iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_text());

        // Merge consecutive same-role messages to satisfy Gemini's strict alternation rule
        let contents = merge_consecutive_roles(
            req.messages.iter()
                .filter(|m| m.role != "system")
                .map(|m| {
                    let role = if m.role == "assistant" { "model" } else { "user" };
                    json!({
                        "role": role,
                        "parts": [{ "text": m.content.as_text() }]
                    })
                })
                .collect()
        );

        let mut body = json!({ "contents": contents });
        if let Some(sys) = system_text {
            body["systemInstruction"] = json!({
                "parts": [{ "text": sys }]
            });
        }
        let mut gen_config = serde_json::Map::new();
        if let Some(t) = req.temperature { gen_config.insert("temperature".into(), json!(t)); }
        if let Some(m) = req.max_tokens  { gen_config.insert("maxOutputTokens".into(), json!(m)); }
        if !gen_config.is_empty() { body["generationConfig"] = json!(gen_config); }

        (url, body)
    }

    fn from_gemini(resp: serde_json::Value, model_id: &str) -> ChatResponse {
        let text = resp["candidates"][0]["content"]["parts"][0]["text"]
            .as_str().unwrap_or("").to_string();
        let prompt_tokens = resp["usageMetadata"]["promptTokenCount"]
            .as_u64().unwrap_or(0) as u32;
        let output_tokens = resp["usageMetadata"]["candidatesTokenCount"]
            .as_u64().unwrap_or(0) as u32;

        let finish_reason = match resp["candidates"][0]["finishReason"].as_str() {
            Some("STOP")       => Some("stop".to_string()),
            Some("MAX_TOKENS") => Some("length".to_string()),
            Some("SAFETY")     => Some("content_filter".to_string()),
            other              => other.map(String::from),
        };

        ChatResponse {
            id: uuid::Uuid::new_v4().to_string(),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: model_id.to_string(),
            choices: vec![Choice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text(text),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                finish_reason,
            }],
            usage: Some(Usage {
                prompt_tokens,
                completion_tokens: output_tokens,
                total_tokens: prompt_tokens + output_tokens,
            }),
        }
    }
}
```

### 6d. Bedrock (reqwest + SigV4, no AWS SDK)

**API reference:** https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html

Bedrock uses the **Converse API** which provides one unified request shape for all
model families (Claude, Nova, Titan, Llama). The request is a standard HTTPS POST
signed with AWS SigV4. We use `reqwest` for the HTTP call and the `aws-sigv4` crate
for signing — no `aws-sdk-bedrockruntime` needed.

**Endpoint:** `POST https://bedrock-runtime.{region}.amazonaws.com/model/{model_id}/converse`
**Streaming:** `POST .../converse-stream` (returns an event-stream, not SSE)

**Auth:** AWS SigV4. Sign the request with `service = "bedrock"`, `region` from config.
Credentials from: `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` env vars, or
`~/.aws/credentials`. The `Authorization` header override does not apply.

**Converse request body** (JSON):
```json
{
  "messages": [
    { "role": "user", "content": [{ "text": "hello" }] }
  ],
  "system": [{ "text": "You are helpful." }],
  "inferenceConfig": {
    "maxTokens": 4096,
    "temperature": 0.7
  }
}
```

Note: Bedrock Converse uses `inferenceConfig` (camelCase) not `inference_config`.

**Converse response body:**
```json
{
  "output": { "message": { "role": "assistant", "content": [{ "text": "..." }] } },
  "usage": { "inputTokens": 10, "outputTokens": 20, "totalTokens": 30 },
  "stopReason": "end_turn"
}
```

**Field mapping:**

| OpenAI                      | Bedrock Converse                          |
|-----------------------------|-------------------------------------------|
| `messages[].role = "user"`  | `messages[].role = "user"`               |
| `messages[].role = "assistant"` | `messages[].role = "assistant"`      |
| `messages[].content`        | `messages[].content[0].text`             |
| `system` message            | `system[0].text`                         |
| `max_tokens`                | `inferenceConfig.maxTokens`              |
| `temperature`               | `inferenceConfig.temperature`            |
| `choices[0].message.content`| `output.message.content[0].text`         |
| `usage.prompt_tokens`       | `usage.inputTokens`                      |
| `usage.completion_tokens`   | `usage.outputTokens`                     |
| `finish_reason: "stop"`     | `stopReason: "end_turn"`                 |
| `finish_reason: "length"`   | `stopReason: "max_tokens"`               |

**SigV4 signing with `aws-sigv4`:**

```rust
// crates/llmproxy-providers/src/bedrock.rs
use aws_sigv4::http_request::{sign, SigningSettings, SigningParams};
use aws_credential_types::Credentials;

pub struct BedrockProvider {
    client: reqwest::Client,
    region: String,
}

impl BedrockProvider {
    fn sign_request(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        cred: &AwsCred,
    ) -> Result<reqwest::Request, ProxyError> {
        let credentials = Credentials::new(
            &cred.access_key_id,
            &cred.secret_access_key,
            cred.session_token.clone(),
            None,
            "llmproxy",
        );

        let mut request = http::Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json")
            .body(body.to_vec())
            .map_err(|e| ProxyError::Config(e.to_string()))?;

        let signing_params = SigningParams::builder()
            .credentials(&credentials)
            .region(&self.region)
            .service_name("bedrock")
            .time(std::time::SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|e| ProxyError::Config(e.to_string()))?;

        sign(&mut request, &signing_params)
            .map_err(|e| ProxyError::Config(e.to_string()))?;

        // Convert http::Request → reqwest::Request
        let (parts, body) = request.into_parts();
        let mut rb = self.client.request(
            reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).unwrap(),
            parts.uri.to_string(),
        );
        for (k, v) in &parts.headers {
            rb = rb.header(k, v);
        }
        rb.body(body).build().map_err(ProxyError::Http)
    }

    fn to_converse_body(req: &ChatRequest, model_id: &str) -> serde_json::Value {
        let system: Vec<_> = req.messages.iter()
            .filter(|m| m.role == "system")
            .map(|m| json!({ "text": m.content.as_text() }))
            .collect();

        let messages: Vec<_> = req.messages.iter()
            .filter(|m| m.role != "system")
            .map(|m| json!({
                "role": m.role,
                "content": [{ "text": m.content.as_text() }]
            }))
            .collect();

        let mut body = json!({ "messages": messages });
        if !system.is_empty() { body["system"] = json!(system); }
        let mut ic = serde_json::Map::new();
        if let Some(t) = req.max_tokens  { ic.insert("maxTokens".into(), json!(t)); }
        if let Some(t) = req.temperature { ic.insert("temperature".into(), json!(t)); }
        if !ic.is_empty() { body["inferenceConfig"] = json!(ic); }
        body
    }
}
```

**Streaming note:** The Bedrock converse-stream endpoint returns
`application/vnd.amazon.eventstream` (binary framing), not SSE. For MVP,
implement non-streaming only and return a `501 Not Implemented` for
`stream: true` on Bedrock. Add streaming in v0.2 using the `aws-smithy-eventstream`
crate or by parsing the binary frame format manually.

---

## 7. Streaming Architecture

All SSE streaming uses `axum::response::Sse`.

For **passthrough providers**: bytes are relayed verbatim — no parsing needed.

For **translation providers**: the provider streams native SSE chunks,
the proxy parses each chunk and re-emits as OpenAI SSE format.

```rust
// crates/llmproxy-server/src/server.rs

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<ProviderRegistry>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/models", get(models_handler))
        .route("/health", get(|| async { "ok" }))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn chat_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    // Extract Authorization header — used for per-request credential override
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let (provider, model_id, cred) = match state.registry.resolve(&req.model, auth) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::NOT_FOUND, &e.to_string()),
    };

    if req.stream.unwrap_or(false) {
        match provider.chat_stream(req, &model_id, &cred).await {
            Ok(stream) => {
                let sse_stream = stream.map(|item| {
                    item.map(|b| Event::default().data(
                        std::str::from_utf8(&b).unwrap_or("").trim_start_matches("data: ")
                    ))
                    .map_err(|e| e.to_string())
                });
                Sse::new(sse_stream).into_response()
            }
            Err(e) => error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
        }
    } else {
        match provider.chat(req, &model_id, &cred).await {
            Ok(resp) => Json(resp).into_response(),
            Err(ProxyError::Upstream { status, body }) => {
                (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                 body).into_response()
            }
            Err(e) => error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
        }
    }
}

/// GET /v1/models — lists configured provider names so clients can discover
/// what's available. Does not enumerate every possible model_id (unknowable).
async fn models_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let models: Vec<_> = state.registry.provider_names().into_iter().map(|name| json!({
        "id": name,
        "object": "model",
        "owned_by": "llmproxy",
        "note": format!("use '{}/{{model_id}}' as the model field", name),
    })).collect();
    Json(json!({ "object": "list", "data": models }))
}

fn error_response(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({
        "error": { "message": msg, "type": "proxy_error", "code": "model_not_found" }
    }))).into_response()
}
```

---

## 8. Crate Dependencies

```toml
[dependencies]
# Server
axum             = { version = "0.7", features = ["macros"] }
tokio            = { version = "1",   features = ["full"] }
tower-http       = { version = "0.5", features = ["trace", "cors"] }

# HTTP client (non-AWS providers)
reqwest          = { version = "0.12", features = ["json", "stream"] }

# AWS Bedrock
aws-config                = "1"
aws-sdk-bedrockruntime    = "1"

# Serde
serde            = { version = "1", features = ["derive"] }
serde_json       = "1"

# Config (layered YAML + env var interpolation)
config           = "0.14"
dotenvy          = "0.15"

# Streaming
tokio-stream     = "0.1"
futures          = "0.3"
async-stream     = "0.3"
bytes            = "1"

# CLI
clap             = { version = "4", features = ["derive", "env"] }

# Observability
tracing                = "0.1"
tracing-subscriber     = { version = "0.3", features = ["env-filter"] }

# Utils
uuid             = { version = "1", features = ["v4"] }
chrono           = { version = "0.4", features = ["serde"] }
thiserror        = "1"
async-trait      = "0.1"
```

---

## 9. Cross-Compilation & Packaging

### Targets

| Binary                              | Platform              |
|-------------------------------------|-----------------------|
| `llmproxy-universal-apple-darwin`   | macOS (Intel + Apple Silicon, via `lipo`) |
| `llmproxy-x86_64-linux-gnu`         | Debian/Ubuntu x86_64  |
| `llmproxy-aarch64-linux-gnu`        | Debian/Ubuntu ARM64   |

### GitHub Actions Release

```yaml
# .github/workflows/release.yml
jobs:
  build:
    strategy:
      matrix:
        include:
          - target: x86_64-apple-darwin
            os: macos-latest
          - target: aarch64-apple-darwin
            os: macos-latest
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest

    steps:
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: "${{ matrix.target }}" }

      - name: Cross-linker for aarch64 Linux
        if: matrix.target == 'aarch64-unknown-linux-gnu'
        run: |
          sudo apt-get install -y gcc-aarch64-linux-gnu
          echo '[target.aarch64-unknown-linux-gnu]' >> ~/.cargo/config.toml
          echo 'linker = "aarch64-linux-gnu-gcc"' >> ~/.cargo/config.toml

      - run: cargo build --release --target ${{ matrix.target }}

  macos-universal:
    needs: build
    runs-on: macos-latest
    steps:
      - run: |
          lipo -create \
            artifacts/x86_64-apple-darwin/llmproxy \
            artifacts/aarch64-apple-darwin/llmproxy \
            -output llmproxy-universal-apple-darwin
```

### Debian .deb via cargo-deb

```toml
# Cargo.toml
[package.metadata.deb]
assets = [
  ["target/release/llmproxy", "usr/bin/", "755"],
  ["config.example.yaml", "etc/llmproxy/config.yaml", "644"],
]
```
```bash
cargo install cargo-deb
cargo deb  # → target/debian/llmproxy_0.1.0_amd64.deb
```

### Homebrew

```ruby
class Llmproxy < Formula
  desc "Localhost LLM proxy — OpenAI-compatible, no Python required"
  version "0.1.0"

  on_macos { url "...llmproxy-universal-apple-darwin.tar.gz" }
  on_linux { url "...llmproxy-x86_64-linux-gnu.tar.gz" }

  def install
    bin.install "llmproxy"
  end

  service do
    run [opt_bin/"llmproxy", "serve"]
    keep_alive true
  end
end
```

---

## 10. CLI

```
llmproxy serve              # start proxy in foreground (default: 127.0.0.1:8080)
llmproxy serve --port 9000  # custom port
llmproxy serve --daemon     # start as background daemon (see §16)
llmproxy stop               # stop the running daemon
llmproxy status             # show whether daemon is running + PID + port
llmproxy providers          # list all configured provider names
llmproxy test <provider>    # e.g. "llmproxy test anthropic" — sends a hello ping
llmproxy install            # install as launchd agent (macOS) or systemd unit (Linux)
llmproxy uninstall          # remove the autostart agent/unit
llmproxy config init        # scaffold ~/.config/llmproxy/config.yaml
llmproxy config show        # print resolved config (secrets redacted to ***)
llmproxy usage summary      # aggregate stats from the persistent usage log (see §17)
llmproxy usage recent       # most recent usage log entries
llmproxy usage prune        # one-shot retention cleanup
```

**`llmproxy providers` output example:**
```
Configured providers:
  openai       ✓  (use "openai/<model_id>")
  anthropic    ✓  (use "anthropic/<model_id>")
  gemini       ✓  (use "gemini/<model_id>")
  bedrock      ✓  (use "bedrock/<model_id>")
  azure        ✓  (use "azure/<deployment_name>")
  mistral      ✗  (not configured — add mistral.api_key to config)
```

---

## 11. MVP Phased Roadmap

| Phase | Scope |
|-------|-------|
| **v0.1** | OpenAI, Anthropic, Gemini, Bedrock, Azure, Mistral. Chat + streaming. `provider/model` routing. Daemon mode + `install`/`uninstall` for macOS launchd and Linux systemd. `.deb` + Homebrew. |
| **v0.1.x** | **Persistent usage log + metrics** (SQLite-backed, opt-in) and `llmproxy usage {summary,recent,prune}` CLI (see §17). |
| **v0.2** | + Cohere, TogetherAI, HuggingFace TGI. Embeddings endpoint (`/v1/embeddings`). |
| **v0.3** | + MLflow Model Serving, AI21Labs. |
| **v1.0** | Tool call passthrough for all translation providers. |

---

## 12. Key Design Decisions

**`provider/model` routing instead of aliases** — No config required to start using
a new model. Clients specify `"anthropic/claude-sonnet-4-5"` directly. The only
config needed is credentials per provider. This eliminates an entire config concept
and makes the proxy trivially usable out of the box.

The `split_once('/')` approach handles model IDs that themselves contain slashes
(e.g. Bedrock cross-region model ARNs like `us.anthropic.claude-3-5-sonnet-20241022-v2:0`)
correctly — only the first `/` is the separator.

**Daemon mode in MVP** — target users are developers who want the proxy running
permanently on their laptop. Running `llmproxy install` sets up autostart so the
proxy is always available at `localhost:8080` without manual intervention.

**No unified schema** — OpenAI-compat only. Providers that already speak OpenAI
format (OpenAI, Azure, Mistral, TogetherAI) are pure byte-relay passthroughs.

**No AWS SDK** — Bedrock is implemented with plain `reqwest` + `aws-sigv4` crate for SigV4 request signing. This avoids pulling in the entire `aws-sdk-bedrockruntime` dependency tree (~30 transitive crates, significant compile time). Bedrock streaming is deferred to v0.2 because the event-stream binary format requires a separate small parser.

**Auth header passthrough** — the config file is optional. Any provider that accepts a Bearer token can be used without a config entry by passing `Authorization: Bearer <key>` on each request. Credential resolution order: (1) request header, (2) config file, (3) well-known env var. This means the proxy works out-of-the-box with zero configuration for single-user local use.

**`--daemon` is a CLI flag only** — not a config field. Config files are for credentials and server address, not process management. This keeps the config schema minimal and avoids surprising behavior where a config file silently backgrounds the process.

---

## 13. Implementation Order (for coding agents)

Work in this exact sequence to ensure each step compiles before proceeding.
Never skip ahead — later crates depend on earlier ones compiling cleanly.

**Step 1 — Workspace scaffold**
- Create `Cargo.toml` (workspace root, no `[package]`)
- Create the three crate directories with their `Cargo.toml` files
- Verify: `cargo check` passes with empty `lib.rs` / `main.rs` files

**Step 2 — `llmproxy-core`**
- Implement `error.rs` (`ProxyError` with `thiserror`)
- Implement `openai_types.rs` (all structs, `MessageContent::as_text()`)
- Implement `provider.rs` (the `Provider` trait)
- Verify: `cargo test -p llmproxy-core` passes

**Step 3 — `llmproxy-providers`: PassthroughProvider**
- Implement `passthrough.rs` for OpenAI and Mistral (same base URL pattern)
- Add Azure variant (different auth header + URL structure)
- Write unit tests with `mockito` for request/response shape
- Verify: `cargo test -p llmproxy-providers` passes

**Step 4 — `llmproxy-providers`: AnthropicProvider**
- Implement non-streaming `chat()` first
- Implement streaming `chat_stream()` — parse `content_block_delta` events
- Verify SSE event parsing with a hardcoded fixture string in a unit test

**Step 5 — `llmproxy-providers`: GeminiProvider**
- Implement non-streaming `chat()` first
- Handle the consecutive same-role message merge
- Implement streaming `chat_stream()` — each Gemini SSE chunk is a full JSON object
- Verify with fixture tests

**Step 6 — `llmproxy-providers`: BedrockProvider**
- Implement non-streaming `chat()` via `client.converse()`
- Implement streaming `chat_stream()` via `client.converse_stream()`
- Use `#[cfg(test)]` with a mock Bedrock endpoint for unit tests (AWS SDK supports `endpoint_url` override)

**Step 7 — `llmproxy-server`: config + registry**
- Implement `config.rs`: load YAML, interpolate `${ENV_VAR}`, construct `AppConfig`. No `models` field. `ServerConfig` has only `host` and `port`.
- Add `Credential` enum to `llmproxy-core` (`BearerToken(String)` and `AwsSigV4 { ... }`)
- Implement `registry.rs`: `ProviderRegistry` built from `providers` config. `resolve(model_field, auth_header)` splits on first `/`, looks up provider, then calls `resolve_credential()` with the three-step fallback.
- Write tests: `resolve("anthropic/claude-sonnet-4-5", Some("Bearer sk-ant-test"))` returns `Credential::BearerToken("sk-ant-test")`. `resolve("anthropic/x", None)` falls back to env var. `resolve("badformat", None)` returns error.

**Step 8 — `llmproxy-server`: HTTP server**
- Implement `server.rs` with the Axum router
- `chat_handler` extracts `HeaderMap`, passes `Authorization` header value to `registry.resolve()`
- Implement `models_handler`, error shape
- Integration test: spin up server with mock provider, hit `/v1/chat/completions` with `model: "mock/test-model"` both with and without `Authorization` header

**Step 9 — `llmproxy-server`: CLI + daemon**
- Implement `main.rs` with `clap` subcommands: `serve`, `stop`, `status`, `providers`, `test`, `install`, `uninstall`, `config init`, `config show`
- `serve` reads config, builds registry, starts Axum. With `--daemon` flag, daemonize before binding (see §16).
- `providers` reads config and prints which providers are configured vs. missing
- `config show` loads and pretty-prints config with secret values replaced by `***`
- `install` / `uninstall`: write/remove the platform autostart file (see §16)
- `stop`: read PID file and send SIGTERM

**Step 10 — CI and packaging**
- Add `.github/workflows/ci.yml` (cargo test + clippy)
- Add `.github/workflows/release.yml` (cross-compile matrix + lipo + cargo-deb)
- Add `config.example.yaml` with all six MVP providers showing `${ENV_VAR}` placeholders

---

## 14. Config Loading Detail

The `config` crate supports layered sources. Load in this priority (highest first):
1. CLI flag `--config <path>`
2. `$LLMPROXY_CONFIG` env var
3. `~/.config/llmproxy/config.yaml`
4. `./llmproxy.yaml`

The `config` crate does **not** interpolate `${ENV_VAR}` syntax by default.
Pre-process the raw YAML string with a regex pass before passing to `config`:

```rust
// crates/llmproxy-server/src/config.rs
use config::{Config as ConfigLoader, File, Environment};

pub fn load_config(path: Option<&str>) -> Result<AppConfig, config::ConfigError> {
    let mut builder = ConfigLoader::builder();

    if let Some(p) = path {
        builder = builder.add_source(File::with_name(p));
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        builder = builder
            .add_source(File::with_name(&format!("{}/.config/llmproxy/config", home)).required(false))
            .add_source(File::with_name("llmproxy").required(false));
    }

    // Allow env vars to override: LLMPROXY_SERVER__PORT=9000 overrides server.port
    builder = builder.add_source(
        Environment::with_prefix("LLMPROXY").separator("__")
    );

    builder.build()?.try_deserialize::<AppConfig>()
}

#[derive(Debug, serde::Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub providers: ProvidersConfig,
    // NOTE: no models/aliases section — routing is purely "provider/model_id"
    // NOTE: no daemon field — daemon mode is --daemon CLI flag only
}

#[derive(Debug, serde::Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}
fn default_host() -> String { "127.0.0.1".to_string() }
fn default_port() -> u16 { 8080 }
```

For `${ENV_VAR}` interpolation in YAML values, pre-process the raw YAML string
before passing to `config`:
```rust
fn interpolate_env(s: &str) -> String {
    let re = regex::Regex::new(r"\$\{([^}]+)\}").unwrap();
    re.replace_all(s, |caps: &regex::Captures| {
        std::env::var(&caps[1]).unwrap_or_default()
    }).into_owned()
}
```
Add `regex = "1"` to `llmproxy-server/Cargo.toml` dependencies.

---

## 15. CI Workflow

```yaml
# .github/workflows/ci.yml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy -- -D warnings
      - run: cargo test
```

---

## 16. Daemon Mode & Autostart

### Daemon behavior

When started with `--daemon`, the process:
1. Forks to background using the `daemonize` crate
2. Writes its PID to `~/.local/share/llmproxy/llmproxy.pid`
3. Redirects stdout/stderr to `~/.local/share/llmproxy/llmproxy.log`
4. `llmproxy stop` reads the PID file and sends `SIGTERM`
5. `llmproxy status` checks if the PID is alive and prints port

Add to `llmproxy-server/Cargo.toml`:
```toml
daemonize = "0.5"
```

```rust
// crates/llmproxy-server/src/main.rs (serve subcommand, daemon path)
use daemonize::Daemonize;
use std::path::PathBuf;

fn run_daemon(config: &AppConfig) -> anyhow::Result<()> {
    let data_dir = data_dir();  // ~/.local/share/llmproxy/
    std::fs::create_dir_all(&data_dir)?;

    Daemonize::new()
        .pid_file(data_dir.join("llmproxy.pid"))
        .stdout(std::fs::File::create(data_dir.join("llmproxy.log"))?)
        .stderr(std::fs::File::create(data_dir.join("llmproxy.log"))?)
        .start()?;

    // After fork: start the async runtime and server
    tokio::runtime::Runtime::new()?.block_on(run_server(config))
}

fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/llmproxy")
}
```

### macOS — launchd agent (`llmproxy install`)

`llmproxy install` writes a `launchd` plist to
`~/Library/LaunchAgents/com.llmproxy.plist` and loads it immediately.
The server starts at login automatically and is restarted if it crashes.

```rust
// crates/llmproxy-server/src/main.rs (install subcommand, macOS)
fn install_macos(config_path: &str) -> anyhow::Result<()> {
    let binary = std::env::current_exe()?;
    let plist = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.llmproxy</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>serve</string>
        <string>--config</string>
        <string>{config_path}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>"#,
        binary = binary.display(),
        config_path = config_path,
        log = data_dir().join("llmproxy.log").display(),
    );

    let plist_path = dirs::home_dir()
        .unwrap()
        .join("Library/LaunchAgents/com.llmproxy.plist");
    std::fs::write(&plist_path, plist)?;

    // launchctl load -w ~/Library/LaunchAgents/com.llmproxy.plist
    std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .status()?;

    println!("✓ llmproxy will now start automatically at login.");
    println!("  Logs: {}", data_dir().join("llmproxy.log").display());
    Ok(())
}
```

`llmproxy uninstall` (macOS):
```rust
fn uninstall_macos() -> anyhow::Result<()> {
    let plist_path = dirs::home_dir()
        .unwrap()
        .join("Library/LaunchAgents/com.llmproxy.plist");

    std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status()?;
    std::fs::remove_file(&plist_path)?;
    println!("✓ Autostart removed.");
    Ok(())
}
```

Add `dirs = "5"` to `llmproxy-server/Cargo.toml`.

### Linux — systemd user unit (`llmproxy install`)

`llmproxy install` on Linux writes a **systemd user unit** to
`~/.config/systemd/user/llmproxy.service` and enables it.
User units don't require root. The service starts at login for the current user.

```rust
fn install_linux(config_path: &str) -> anyhow::Result<()> {
    let binary = std::env::current_exe()?;
    let unit = format!(
r#"[Unit]
Description=llmproxy — local LLM API proxy
After=network.target

[Service]
ExecStart={binary} serve --config {config_path}
Restart=on-failure
RestartSec=5
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"#,
        binary = binary.display(),
        config_path = config_path,
        log = data_dir().join("llmproxy.log").display(),
    );

    let unit_dir = dirs::home_dir().unwrap().join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir)?;
    let unit_path = unit_dir.join("llmproxy.service");
    std::fs::write(&unit_path, unit)?;

    // systemctl --user daemon-reload
    // systemctl --user enable --now llmproxy
    std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()?;
    std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "llmproxy"])
        .status()?;

    println!("✓ llmproxy enabled as a systemd user service.");
    println!("  View logs: journalctl --user -u llmproxy -f");
    Ok(())
}
```

`llmproxy uninstall` (Linux):
```rust
fn uninstall_linux() -> anyhow::Result<()> {
    std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "llmproxy"])
        .status()?;
    let unit_path = dirs::home_dir()
        .unwrap()
        .join(".config/systemd/user/llmproxy.service");
    std::fs::remove_file(&unit_path)?;
    std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()?;
    println!("✓ Autostart removed.");
    Ok(())
}
```

### Platform detection

```rust
fn install(config_path: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return install_macos(config_path);
    #[cfg(target_os = "linux")]
    return install_linux(config_path);
    #[allow(unreachable_code)]
    anyhow::bail!("autostart not supported on this platform")
}
```

### Homebrew service integration

The Homebrew formula's `service` block (§9) runs `llmproxy serve` directly
(not `--daemon`) because Homebrew manages the process lifecycle itself via
launchd. When installed via Homebrew, users run `brew services start llmproxy`
instead of `llmproxy install`.

---

## 17. Usage Log & Metrics

> **Ships in v0.1.x.** The original roadmap listed a v1.0 "request log to
> JSONL" item — we replaced that with a SQLite-backed log because aggregate
> queries (latency percentiles, per-provider token totals) are the primary
> use case and `jq` over a growing JSONL file does not scale. The log is
> **opt-in** because captured prompts/responses may contain PII/secrets.

### 17.1 Goals

- Persist one row per chat-completion request so users can audit traffic,
  compare providers, and catch regressions.
- Expose simple CLI queries (`summary`, `recent`, `prune`) without requiring
  users to know SQL or install a client.
- Auto-expire rows past a retention window so the file does not grow
  unboundedly on a laptop.
- Never stall request handling on disk I/O.

### 17.2 Non-goals (for this slice)

- No cross-machine aggregation / metrics export to Prometheus. A user can
  always `COPY` rows out of SQLite or `ATTACH` the file.
- No redaction / PII scrubbing. If this matters for a given user they should
  leave the log disabled.
- No streaming mid-flight ingestion: we only write once a request completes
  (or is cancelled).

### 17.3 Config surface

```yaml
usage_log:
  enabled: false              # opt-in
  retention_days: 30          # background prune cutoff
  max_body_bytes: 1048576     # per-entry body cap (default 1 MiB)
  # path: ${HOME}/.local/share/llmproxy/usage.sqlite
```

Only `${ENV_VAR}` interpolation is performed on `path`; `~` is **not**
expanded (a surprise during review on PR #2, fixed by the current docs).

### 17.4 Storage: SQLite

Single file, WAL journal mode, `synchronous = NORMAL`, `busy_timeout = 5s`
on every connection so concurrent writer / reader / prune calls block-and-
retry instead of returning `SQLITE_BUSY`.

Schema:

```sql
CREATE TABLE usage_log (
    id                TEXT PRIMARY KEY,   -- uuid v4
    created_at        TEXT NOT NULL,      -- RFC-3339 UTC
    provider          TEXT NOT NULL,      -- "openai", "anthropic", ...
    model_id          TEXT NOT NULL,      -- native model id after "/" split
    status            INTEGER NOT NULL,   -- HTTP status (200, 401, 502, 499, ...)
    latency_ms        INTEGER NOT NULL,
    prompt_tokens     INTEGER,            -- parsed from response body
    completion_tokens INTEGER,
    total_tokens      INTEGER,
    stream            INTEGER NOT NULL,   -- 0/1
    request_body      TEXT NOT NULL,      -- truncated to max_body_bytes
    response_body     TEXT NOT NULL,      -- same
    error             TEXT                -- ProxyError rendering, if any
);
CREATE INDEX idx_usage_created         ON usage_log (created_at);
CREATE INDEX idx_usage_provider_model  ON usage_log (provider, model_id, created_at);
```

Rows are written via `INSERT OR REPLACE` keyed on `id` so a double-record
attempt (e.g. re-entering `Drop`) is idempotent.

### 17.5 Write path — never block a handler

Writes go through a bounded `tokio::sync::mpsc::Sender<UsageEntry>` with
capacity **1024**. The request handler calls `try_send`: if the channel is
full it **drops** the entry and emits a `tracing::warn!`. Dropping on
back-pressure is the right call for a non-load-bearing log; back-pressuring
every request handler on sqlite I/O is not.

The consumer runs on a **dedicated OS thread** (`std::thread::Builder`,
named `usage-log-writer`) and calls `rx.blocking_recv()` in a loop. This
avoids putting synchronous rusqlite calls on tokio worker threads, which
would stall request handling under load. A second `Connection` dedicated to
the writer prevents contention with reader queries issued by `summary` /
`recent` / `prune`.

```
          ┌─────────────┐      try_send      ┌─────────────────────┐
request → │  handler    │ ─────────────────► │ mpsc::channel(1024) │
          └─────────────┘                    └──────────┬──────────┘
                                                        │ blocking_recv
                                                        ▼
                                           std::thread "usage-log-writer"
                                           rusqlite::Connection (writer)
```

### 17.6 Capturing bodies

#### Non-streaming

The handler has the full response in hand; we `serde_json::to_string(&resp)`,
run `extract_tokens` on it, then record the entry. Bodies longer than
`max_body_bytes` are trimmed via `truncate_body()`, which appends
`… [truncated N bytes]` so the persisted field is obviously incomplete and
the dropped-byte count is recoverable.

#### Streaming

Axum does not expose a "response finished" hook for streaming bodies, so we
wrap the SSE byte stream in a `FinalizedStream<S>` adapter that:

1. Buffers bytes up to `max_body_bytes` (excess is counted but discarded).
2. Scans each chunk for `"error":` and `data: [DONE]` markers to set
   `saw_error` / `saw_done` flags (we look at the raw bytes so the markers
   aren't missed when the buffer is already full).
3. On `Drop`, assembles the buffered bytes, parses `last_usage_from_sse()`
   for token counts, chooses a status:
    - `saw_error` → **502**, `error = "upstream stream error"`
    - `!saw_done` → **499**, `error = "client disconnected before [DONE]"`
    - otherwise → **200**, `error = None`
4. Submits the resulting `UsageEntry` to the writer channel.

Dropping on the stream (rather than the response) means we still log
requests where the client disconnects early.

```rust
fn last_usage_from_sse(body: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    // Scan every `data: {...}` line; the *last* one bearing a `usage` object
    // wins (OpenAI emits it only in the terminal chunk).
}
```

### 17.7 Token extraction

Tokens are parsed directly from the persisted response body — no separate
pipeline — so the extractor handles both native OpenAI payloads and bodies
translated back into OpenAI shape by `AnthropicProvider` / `GeminiProvider`
/ `BedrockProvider`. The extractor is deliberately lenient:

```rust
pub fn extract_tokens(body: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else { return (None, None, None); };
    let g = |k| v["usage"][k].as_i64();
    (g("prompt_tokens"), g("completion_tokens"), g("total_tokens"))
}
```

For SSE the same shape is expected inside any terminal `data: {...}` chunk.

### 17.8 Retention

A background tokio task runs `prune(retention)` on an hourly interval with
`MissedTickBehavior::Delay` so a long-stalled system doesn't do a thundering
sweep. The prune is a single `DELETE FROM usage_log WHERE created_at < ?1`.
Users can run `llmproxy usage prune` on demand.

### 17.9 Query API — `llmproxy usage summary`

Per-(provider, model) aggregation is a **single** SQL statement using
window functions (`ROW_NUMBER() / COUNT() OVER (PARTITION BY ...)`) to
compute p50 and p95 in the same pass as `count / success_count / avg_latency
/ token totals`. This replaced an N+1 helper that issued three extra
queries per group on an earlier draft.

```sql
WITH filtered AS (
    SELECT provider, model_id, status, latency_ms, prompt_tokens, completion_tokens
    FROM usage_log WHERE created_at >= ?1
),
ranked AS (
    SELECT provider, model_id, latency_ms,
           ROW_NUMBER() OVER (PARTITION BY provider, model_id ORDER BY latency_ms) AS rn,
           COUNT(*)     OVER (PARTITION BY provider, model_id) AS cnt
    FROM filtered
),
aggregated AS (
    SELECT provider, model_id,
           COUNT(*)                                                   AS count,
           SUM(CASE WHEN status BETWEEN 200 AND 299 THEN 1 ELSE 0 END) AS ok,
           COALESCE(AVG(latency_ms), 0.0)                             AS avg_lat,
           COALESCE(SUM(prompt_tokens), 0)                            AS pt,
           COALESCE(SUM(completion_tokens), 0)                        AS ct
    FROM filtered GROUP BY provider, model_id
),
pct AS (
    SELECT provider, model_id,
           MIN(CASE WHEN rn >= ((cnt + 1) / 2)         THEN latency_ms END) AS p50,
           MIN(CASE WHEN rn >= ((cnt * 95 + 99) / 100) THEN latency_ms END) AS p95
    FROM ranked GROUP BY provider, model_id
)
SELECT a.provider, a.model_id, a.count, a.ok, a.avg_lat,
       COALESCE(p.p50, 0), COALESCE(p.p95, 0), a.pt, a.ct
FROM aggregated a LEFT JOIN pct p USING (provider, model_id)
ORDER BY a.count DESC;
```

`--since` accepts the shorthand `7d`, `2w` plus anything `humantime`
understands (`10ms`, `2h 30min`, `1hour 5min`, …) — the custom parser only
handles the units `humantime` does not (`d`/`w`), everything else falls
through.

### 17.10 Privacy / safety posture

- **Opt-in.** Default config has `enabled: false`.
- **`Authorization` header is never persisted** — it is only consumed by
  `registry::resolve()` and thrown away.
- **Body cap** bounds a single runaway response's memory use at
  `max_body_bytes`.
- When tokens come out of a cap-truncated body, token fields are `NULL`
  rather than parsed from a partial JSON document.

### 17.11 Dependencies

Net new crates in `llmproxy-server`:

```toml
rusqlite  = { version = "0.31", features = ["bundled"] }   # no system libsqlite needed
humantime = "2"                                            # duration parsing
tempfile  = "3"                                            # dev-dep for tests
chrono    = { workspace = true }
uuid      = { workspace = true }
```

### 17.12 Testing strategy

- `UsageStore::open` + record + summary against a `tempdir()` database
  (async round-trip with a small sleep for the writer thread).
- `prune` on two-row dataset with one row outside retention.
- `parse_since` cases: `7d`, `24h`, `30m`, `2w`, `10ms`, `2h 30min`, garbage.
- `truncate_body` over/under cap, plus SSE helpers:
  `last_usage_from_sse` picks the terminal chunk, returns `None` when
  absent.

### 17.13 Follow-ups (future PRs)

- Expose `max_body_bytes` in CLI output summary so users know why a
  response is truncated.
- Optional PII redaction (regex-based field stripping) as a middleware
  applied before bodies are buffered.
- Export command (`llmproxy usage export --format jsonl` / `csv`) for
  feeding into external pipelines.
- Bedrock streaming support will add a code path whose body-capture shape
  differs (binary event-stream → translated OpenAI SSE); the existing
  `FinalizedStream` already handles assembled OpenAI SSE so the capture
  side should require no changes.
