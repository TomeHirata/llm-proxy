use std::{convert::Infallible, sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use chrono::Utc;
use futures::StreamExt;
use llmproxy_core::{error::ProxyError, openai_types::ChatRequest};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::{
    registry::ProviderRegistry,
    usage_log::{self, UsageEntry, UsageStore},
};

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<ProviderRegistry>,
    pub usage_store: Option<UsageStore>,
    pub http: reqwest::Client,
    /// Per-entry cap on captured request/response bodies. Bodies longer than
    /// this are truncated with a `… [truncated N bytes]` marker so one huge
    /// response can't OOM the server.
    pub max_body_bytes: usize,
}

/// Truncate an already-UTF-8 string to at most `limit` bytes, appending a
/// marker if anything was dropped. Falls back to `String::from_utf8_lossy` if
/// the cut lands mid-codepoint.
fn truncate_body(s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let dropped = s.len() - limit;
    let mut head = s.into_bytes();
    head.truncate(limit);
    let head = String::from_utf8(head)
        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned());
    format!("{head}… [truncated {dropped} bytes]")
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/models", get(models_handler))
        .route("/health", get(health_handler))
        // Native provider passthroughs — no translation, body forwarded as-is.
        .route("/openai/v1/responses", post(openai_responses_handler))
        .route("/anthropic/v1/messages", post(anthropic_messages_handler))
        .route(
            "/gemini/v1beta/models/:model_id/generateContent",
            post(gemini_generate_content_handler),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health_handler() -> &'static str {
    "ok"
}

async fn chat_handler(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let started = Instant::now();

    let req: ChatRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            let err = ProxyError::Serde(e);
            return proxy_error_to_response(&err);
        }
    };
    let raw_request = String::from_utf8_lossy(&body).into_owned();

    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let (provider, model_id, cred) = match state.registry.resolve(&req.model, auth.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            record_error(&state, &e, &req, "", started, false, &raw_request);
            return proxy_error_to_response(&e);
        }
    };

    let want_stream = req.stream.unwrap_or(false);
    let stream_flag = want_stream;
    let model_for_log = model_id.clone();

    if want_stream {
        let req_for_provider = req.clone();
        match provider
            .chat_stream(req_for_provider, &model_id, &cred)
            .await
        {
            Ok(stream) => {
                let byte_stream = stream.map(|item| match item {
                    Ok(b) => Ok::<_, Infallible>(b),
                    Err(e) => {
                        let msg = e.to_string();
                        let err_chunk = format!(
                            "data: {}\n\n",
                            json!({ "error": { "message": msg, "type": "proxy_error" } })
                        );
                        Ok(bytes::Bytes::from(err_chunk.into_bytes()))
                    }
                });
                let provider_name = req
                    .model
                    .split_once('/')
                    .map(|(p, _)| p.to_string())
                    .unwrap_or_default();
                let finalizer = StreamFinalizer {
                    store: state.usage_store.clone(),
                    provider: provider_name,
                    model_id: model_for_log,
                    request_body: truncate_body(raw_request, state.max_body_bytes),
                    started,
                    max_body_bytes: state.max_body_bytes,
                    sse_format: SseFormat::OpenAI,
                };
                let body = Body::from_stream(FinalizedStream::new(byte_stream, finalizer));
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .header("connection", "keep-alive")
                    .body(body)
                    .unwrap()
            }
            Err(e) => {
                record_error(
                    &state,
                    &e,
                    &req,
                    &model_for_log,
                    started,
                    true,
                    &raw_request,
                );
                proxy_error_to_response(&e)
            }
        }
    } else {
        match provider.chat(req.clone(), &model_id, &cred).await {
            Ok(resp) => {
                let body = serde_json::to_string(&resp).unwrap_or_default();
                let (pt, ct, tt) = usage_log::extract_tokens(&body);
                record_entry(
                    &state,
                    &req,
                    &model_for_log,
                    200,
                    started,
                    stream_flag,
                    &raw_request,
                    &body,
                    pt,
                    ct,
                    tt,
                    None,
                );
                (StatusCode::OK, [("content-type", "application/json")], body).into_response()
            }
            Err(e) => {
                record_error(
                    &state,
                    &e,
                    &req,
                    &model_for_log,
                    started,
                    stream_flag,
                    &raw_request,
                );
                proxy_error_to_response(&e)
            }
        }
    }
}

async fn models_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let models: Vec<_> = state
        .registry
        .provider_names()
        .into_iter()
        .map(|name| {
            json!({
                "id": name,
                "object": "model",
                "owned_by": "llmproxy",
                "note": format!("use '{}/{{model_id}}' as the model field", name),
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": models }))
}

/// Headers from the caller that are safe to forward to upstream providers.
/// Auth and hop-by-hop headers are excluded; provider-specific beta/org
/// headers are included so callers can use native provider features.
fn should_forward_request_header(name: &axum::http::header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "accept"
            | "content-type"
            | "anthropic-beta"
            | "anthropic-version"
            | "openai-beta"
            | "openai-organization"
            | "openai-project"
            | "x-request-id"
    )
}

/// Merge forwarded caller headers with injected auth/provider headers.
/// Injected headers take precedence over forwarded ones.
fn build_upstream_headers(
    incoming: &HeaderMap,
    injected: reqwest::header::HeaderMap,
) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in incoming {
        if should_forward_request_header(name) {
            if let (Ok(n), Ok(v)) = (
                reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
                reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
            ) {
                out.append(n, v);
            }
        }
    }
    out.extend(injected);
    if !out.contains_key(reqwest::header::CONTENT_TYPE) {
        out.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
    }
    out
}

/// Forward a request body to `url` as-is and stream the upstream response back
/// verbatim. `injected` headers (auth etc.) are layered on top of any
/// forwarded caller headers.
async fn native_forward(
    client: &reqwest::Client,
    url: reqwest::Url,
    incoming: &HeaderMap,
    injected: reqwest::header::HeaderMap,
    body: Bytes,
) -> Response {
    let upstream_headers = build_upstream_headers(incoming, injected);
    let upstream = match client
        .post(url)
        .headers(upstream_headers)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Avoid including the full URL in the message — it may contain an
            // API key as a query parameter (e.g. Gemini ?key=...).
            let msg = format!("upstream request failed: {}", e.without_url());
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"error": {"message": msg, "type": "proxy_error"}}).to_string(),
                ))
                .unwrap();
        }
    };

    let status = upstream.status().as_u16();
    let content_type = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    let byte_stream = upstream.bytes_stream().map(|r| {
        r.map_err(|e| {
            tracing::warn!("stream error: {e}");
            e
        })
    });

    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Body::from_stream(byte_stream))
        .unwrap()
}

fn make_header_value(s: &str) -> Result<reqwest::header::HeaderValue, Box<Response>> {
    reqwest::header::HeaderValue::from_str(s).map_err(|_| {
        Box::new(proxy_error_to_response(&ProxyError::Config(
            "credential contains invalid header characters".into(),
        )))
    })
}

async fn openai_responses_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let cred = match state.registry.credential_for("openai", auth.as_deref()) {
        Ok(c) => c,
        Err(e) => return proxy_error_to_response(&e),
    };
    let token = match cred {
        llmproxy_core::provider::Credential::BearerToken(t) => t,
        _ => {
            return proxy_error_to_response(&ProxyError::Config(
                "openai requires a bearer token".into(),
            ))
        }
    };
    let bearer = match make_header_value(&format!("Bearer {token}")) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let mut injected = reqwest::header::HeaderMap::new();
    injected.insert(reqwest::header::AUTHORIZATION, bearer);
    let url = reqwest::Url::parse("https://api.openai.com/v1/responses").unwrap();
    native_forward(&state.http, url, &headers, injected, body).await
}

async fn anthropic_messages_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let request_body_str = String::from_utf8_lossy(&body).into_owned();
    let model_id = serde_json::from_str::<serde_json::Value>(&request_body_str)
        .ok()
        .and_then(|v| v["model"].as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let cred = match state.registry.credential_for("anthropic", auth.as_deref()) {
        Ok(c) => c,
        Err(e) => return proxy_error_to_response(&e),
    };
    let token = match cred {
        llmproxy_core::provider::Credential::BearerToken(t) => t,
        _ => {
            return proxy_error_to_response(&ProxyError::Config(
                "anthropic requires a bearer token".into(),
            ))
        }
    };
    let api_key = match make_header_value(&token) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let mut injected = reqwest::header::HeaderMap::new();
    injected.insert("x-api-key", api_key);
    if !headers.contains_key("anthropic-version") {
        injected.insert(
            "anthropic-version",
            reqwest::header::HeaderValue::from_static("2023-06-01"),
        );
    }

    let upstream_headers = build_upstream_headers(&headers, injected);
    let url = reqwest::Url::parse("https://api.anthropic.com/v1/messages").unwrap();
    let upstream = match state
        .http
        .post(url)
        .headers(upstream_headers)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("upstream request failed: {}", e.without_url());
            record_raw(
                &state,
                "anthropic",
                &model_id,
                502,
                started,
                false,
                &request_body_str,
                &msg,
                None,
                None,
                None,
                Some(msg.clone()),
            );
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"error": {"message": msg, "type": "proxy_error"}}).to_string(),
                ))
                .unwrap();
        }
    };

    let status = upstream.status().as_u16();
    let content_type = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    if content_type.contains("text/event-stream") {
        let byte_stream = upstream.bytes_stream().map(|r| match r {
            Ok(b) => Ok::<_, Infallible>(b),
            Err(e) => {
                let payload =
                    json!({"type": "error", "error": {"message": e.to_string()}}).to_string();
                Ok(bytes::Bytes::from(
                    format!("data: {payload}\n\n").into_bytes(),
                ))
            }
        });
        let finalizer = StreamFinalizer {
            store: state.usage_store.clone(),
            provider: "anthropic".to_string(),
            model_id,
            request_body: truncate_body(request_body_str, state.max_body_bytes),
            started,
            max_body_bytes: state.max_body_bytes,
            sse_format: SseFormat::Anthropic,
        };
        Response::builder()
            .status(status)
            .header("content-type", content_type)
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive")
            .body(Body::from_stream(FinalizedStream::new(
                byte_stream,
                finalizer,
            )))
            .unwrap()
    } else {
        // Stream the response to the client while teeing into a capped buffer
        // for usage logging, avoiding buffering the full body in memory.
        let max_body_bytes = state.max_body_bytes;
        let usage_state = state.clone();
        let usage_model_id = model_id.clone();
        let usage_request_body = truncate_body(request_body_str, max_body_bytes);
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(8);

        tokio::spawn(async move {
            let mut byte_stream = upstream.bytes_stream();
            let mut resp_buf: Vec<u8> = Vec::new();
            let mut read_error: Option<String> = None;

            while let Some(item) = byte_stream.next().await {
                match item {
                    Ok(chunk) => {
                        let room = max_body_bytes.saturating_sub(resp_buf.len());
                        if room > 0 {
                            resp_buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
                        }
                        if tx.send(Ok(chunk)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        read_error = Some(format!("failed to read response: {}", e.without_url()));
                        break;
                    }
                }
            }

            let resp_str = String::from_utf8_lossy(&resp_buf).into_owned();
            let (pt, ct, tt) = usage_log::extract_tokens_anthropic(&resp_str);
            let error = match read_error {
                Some(msg) => Some(msg),
                None if status >= 400 => Some(resp_str.clone()),
                None => None,
            };
            record_raw(
                &usage_state,
                "anthropic",
                &usage_model_id,
                status,
                started,
                false,
                &usage_request_body,
                &resp_str,
                pt,
                ct,
                tt,
                error,
            );
        });

        let body_stream =
            futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|v| (v, rx)) });
        Response::builder()
            .status(status)
            .header("content-type", content_type)
            .body(Body::from_stream(body_stream))
            .unwrap()
    }
}

async fn gemini_generate_content_handler(
    State(state): State<AppState>,
    Path(model_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let cred = match state.registry.credential_for("gemini", auth.as_deref()) {
        Ok(c) => c,
        Err(e) => return proxy_error_to_response(&e),
    };
    let token = match cred {
        llmproxy_core::provider::Credential::BearerToken(t) => t,
        _ => {
            return proxy_error_to_response(&ProxyError::Config(
                "gemini requires a bearer token".into(),
            ))
        }
    };
    // Build URL with percent-encoded path segments and query param so that
    // model IDs or keys with special characters are handled correctly, and
    // the key is never interpolated into a string that could appear in logs.
    let mut url =
        reqwest::Url::parse("https://generativelanguage.googleapis.com/v1beta/models/").unwrap();
    url.path_segments_mut()
        .unwrap()
        .push(&format!("{model_id}:generateContent"));
    url.query_pairs_mut().append_pair("key", &token);
    native_forward(
        &state.http,
        url,
        &headers,
        reqwest::header::HeaderMap::new(),
        body,
    )
    .await
}

fn proxy_error_to_response(err: &ProxyError) -> Response {
    let (status, code) = match err {
        ProxyError::ModelNotFound(_) => (StatusCode::NOT_FOUND, "model_not_found"),
        ProxyError::Config(_) => (StatusCode::UNAUTHORIZED, "invalid_auth"),
        ProxyError::Upstream { status, .. } => (
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
            "upstream_error",
        ),
        ProxyError::NotImplemented(_) => (StatusCode::NOT_IMPLEMENTED, "not_implemented"),
        ProxyError::Http(_) | ProxyError::Stream(_) | ProxyError::Aws(_) => {
            (StatusCode::BAD_GATEWAY, "upstream_error")
        }
        ProxyError::Serde(_) => (StatusCode::BAD_REQUEST, "bad_request"),
    };
    let body = match err {
        ProxyError::Upstream { body, .. } if !body.is_empty() => body.clone(),
        _ => json!({
            "error": { "message": err.to_string(), "type": "proxy_error", "code": code }
        })
        .to_string(),
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn record_entry(
    state: &AppState,
    req: &ChatRequest,
    model_id: &str,
    status: u16,
    started: Instant,
    stream: bool,
    request_body: &str,
    response_body: &str,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    error: Option<String>,
) {
    let Some(store) = state.usage_store.as_ref() else {
        return;
    };
    let provider = req
        .model
        .split_once('/')
        .map(|(p, _)| p.to_string())
        .unwrap_or_default();
    store.record(UsageEntry {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: Utc::now(),
        provider,
        model_id: model_id.to_string(),
        status,
        latency_ms: started.elapsed().as_millis() as i64,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        stream,
        request_body: truncate_body(request_body.to_string(), state.max_body_bytes),
        response_body: truncate_body(response_body.to_string(), state.max_body_bytes),
        error,
    });
}

fn record_error(
    state: &AppState,
    err: &ProxyError,
    req: &ChatRequest,
    model_id: &str,
    started: Instant,
    stream: bool,
    request_body: &str,
) {
    let status = match err {
        ProxyError::ModelNotFound(_) => 404,
        ProxyError::Config(_) => 401,
        ProxyError::Upstream { status, .. } => *status,
        ProxyError::NotImplemented(_) => 501,
        ProxyError::Serde(_) => 400,
        _ => 502,
    };
    let response_body = match err {
        ProxyError::Upstream { body, .. } => body.clone(),
        _ => String::new(),
    };
    record_entry(
        state,
        req,
        model_id,
        status,
        started,
        stream,
        request_body,
        &response_body,
        None,
        None,
        None,
        Some(err.to_string()),
    );
}

/// Which SSE dialect the upstream speaks — controls the terminator marker and
/// the token-extraction strategy used by `FinalizedStream`.
#[derive(Clone, Copy)]
enum SseFormat {
    /// OpenAI: stream ends with `data: [DONE]`, tokens in `usage` object.
    OpenAI,
    /// Anthropic: stream ends with `"type":"message_stop"`, tokens split
    /// across `message_start` (input) and `message_delta` (output) events.
    Anthropic,
}

/// Captures latency + assembled SSE bytes when a streaming response finishes.
struct StreamFinalizer {
    store: Option<UsageStore>,
    provider: String,
    model_id: String,
    request_body: String,
    started: Instant,
    max_body_bytes: usize,
    sse_format: SseFormat,
}

/// A stream adapter that buffers every chunk it yields and, when the inner
/// stream ends, writes a usage log entry.
///
/// Axum doesn't expose a "response finished" hook for streaming bodies, so we
/// wrap the body stream itself and record from `Drop`. Dropping on the stream
/// (not the response) means we also log early-terminated connections.
///
/// The buffer is capped at `max_body_bytes`; further bytes are accounted for
/// via `dropped_bytes` and only a truncation marker is persisted.
struct FinalizedStream<S> {
    inner: S,
    buf: Vec<u8>,
    dropped_bytes: usize,
    /// True if the server ever emitted a synthesized error chunk for this
    /// stream (upstream yielded `Err`).
    saw_error: bool,
    /// True if we saw the terminating `data: [DONE]` marker.
    saw_done: bool,
    finalizer: Option<StreamFinalizer>,
}

impl<S> FinalizedStream<S> {
    fn new(inner: S, finalizer: StreamFinalizer) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            dropped_bytes: 0,
            saw_error: false,
            saw_done: false,
            finalizer: Some(finalizer),
        }
    }

    fn absorb(&mut self, chunk: &[u8]) {
        let cap = self
            .finalizer
            .as_ref()
            .map(|f| f.max_body_bytes)
            .unwrap_or(0);
        let room = cap.saturating_sub(self.buf.len());
        if room > 0 {
            let take = chunk.len().min(room);
            self.buf.extend_from_slice(&chunk[..take]);
            self.dropped_bytes = self.dropped_bytes.saturating_add(chunk.len() - take);
        } else {
            self.dropped_bytes = self.dropped_bytes.saturating_add(chunk.len());
        }
    }
}

impl<S> Drop for FinalizedStream<S> {
    fn drop(&mut self) {
        let Some(f) = self.finalizer.take() else {
            return;
        };
        let Some(store) = f.store else {
            return;
        };
        let mut assembled = String::from_utf8_lossy(&self.buf).into_owned();
        if self.dropped_bytes > 0 {
            assembled.push_str(&format!("… [truncated {} bytes]", self.dropped_bytes));
        }
        let tokens = match f.sse_format {
            SseFormat::OpenAI => last_usage_from_sse(&assembled),
            SseFormat::Anthropic => last_usage_from_anthropic_sse(&assembled),
        };
        let done_marker = match f.sse_format {
            SseFormat::OpenAI => "data: [DONE]",
            SseFormat::Anthropic => "message_stop event",
        };

        let (status, error) = if self.saw_error {
            (502, Some("upstream stream error".into()))
        } else if !self.saw_done {
            (
                499,
                Some(format!("client disconnected before {done_marker}")),
            )
        } else {
            (200, None)
        };

        store.record(UsageEntry {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            provider: f.provider,
            model_id: f.model_id,
            status,
            latency_ms: f.started.elapsed().as_millis() as i64,
            prompt_tokens: tokens.0,
            completion_tokens: tokens.1,
            total_tokens: tokens.2,
            stream: true,
            request_body: f.request_body,
            response_body: assembled,
            error,
        });
    }
}

impl<S> futures::Stream for FinalizedStream<S>
where
    S: futures::Stream<Item = Result<Bytes, Infallible>> + Unpin,
{
    type Item = Result<Bytes, Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let poll = std::pin::Pin::new(&mut self.inner).poll_next(cx);
        if let std::task::Poll::Ready(Some(Ok(b))) = &poll {
            // Track error/terminator markers before buffering so we don't
            // miss them when the body is large enough to hit the cap.
            let s = std::str::from_utf8(b).unwrap_or("");
            if s.contains("\"error\":") {
                self.saw_error = true;
            }
            let saw_done = match self.finalizer.as_ref().map(|f| f.sse_format) {
                Some(SseFormat::Anthropic) => s.lines().any(|line| {
                    let Some(rest) = line.strip_prefix("data:") else {
                        return false;
                    };
                    serde_json::from_str::<serde_json::Value>(rest.trim())
                        .ok()
                        .and_then(|v| v["type"].as_str().map(|t| t == "message_stop"))
                        .unwrap_or(false)
                }),
                _ => s.contains("data: [DONE]"),
            };
            if saw_done {
                self.saw_done = true;
            }
            let chunk = b.clone();
            self.absorb(&chunk);
        }
        poll
    }
}

/// Scan an assembled OpenAI SSE stream for the last `usage` object — some
/// upstreams (OpenAI, translators emitting final `message_delta`) include
/// token counts in a terminal `data: { ... "usage": {...} }` chunk.
fn last_usage_from_sse(body: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let mut out = (None, None, None);
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() || rest == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) else {
            continue;
        };
        let pt = v["usage"]["prompt_tokens"].as_i64();
        let ct = v["usage"]["completion_tokens"].as_i64();
        let tt = v["usage"]["total_tokens"].as_i64();
        if pt.is_some() || ct.is_some() || tt.is_some() {
            out = (pt, ct, tt);
        }
    }
    out
}

/// Scan an assembled Anthropic SSE stream for token counts.
/// `message_start` carries `input_tokens`; `message_delta` carries the final
/// `output_tokens`.
fn last_usage_from_anthropic_sse(body: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let mut input: Option<i64> = None;
    let mut output: Option<i64> = None;
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(rest.trim()) else {
            continue;
        };
        if let Some(n) = v["message"]["usage"]["input_tokens"].as_i64() {
            input = Some(n);
        }
        if let Some(n) = v["usage"]["output_tokens"].as_i64() {
            output = Some(n);
        }
    }
    let total = input.zip(output).map(|(i, o)| i + o);
    (input, output, total)
}

#[allow(clippy::too_many_arguments)]
fn record_raw(
    state: &AppState,
    provider: &str,
    model_id: &str,
    status: u16,
    started: Instant,
    stream: bool,
    request_body: &str,
    response_body: &str,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    error: Option<String>,
) {
    let Some(store) = state.usage_store.as_ref() else {
        return;
    };
    store.record(UsageEntry {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: Utc::now(),
        provider: provider.to_string(),
        model_id: model_id.to_string(),
        status,
        latency_ms: started.elapsed().as_millis() as i64,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        stream,
        request_body: truncate_body(request_body.to_string(), state.max_body_bytes),
        response_body: truncate_body(response_body.to_string(), state.max_body_bytes),
        error,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    /// Minimal AppState wired to a real (but unconfigured) registry so the
    /// credential-missing path returns 401 without hitting any upstream.
    fn state_no_creds() -> AppState {
        use crate::{config::AppConfig, registry::ProviderRegistry};
        AppState {
            registry: std::sync::Arc::new(ProviderRegistry::from_config(&AppConfig::default())),
            usage_store: None,
            http: reqwest::Client::new(),
            max_body_bytes: 1024,
        }
    }

    async fn body_str(body: axum::body::Body) -> String {
        let b = to_bytes(body, usize::MAX).await.unwrap();
        String::from_utf8_lossy(&b).to_string()
    }

    #[tokio::test]
    async fn openai_missing_cred_returns_401() {
        let app = router(state_no_creds());
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/openai/v1/responses")
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn anthropic_missing_cred_returns_401() {
        let app = router(state_no_creds());
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/anthropic/v1/messages")
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn gemini_missing_cred_returns_401() {
        let app = router(state_no_creds());
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/gemini/v1beta/models/gemini-2.0-flash/generateContent")
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn native_forward_mirrors_status_and_content_type() {
        // Spin up a tiny local HTTP server that returns a known status + body.
        let mock = axum::Router::new().route(
            "/echo",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::CREATED,
                    [("content-type", "text/plain")],
                    "hello",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });

        let client = reqwest::Client::new();
        let url = reqwest::Url::parse(&format!("http://{addr}/echo")).unwrap();
        let resp = native_forward(
            &client,
            url,
            &HeaderMap::new(),
            reqwest::header::HeaderMap::new(),
            Bytes::from("{}"),
        )
        .await;

        assert_eq!(resp.status(), 201);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/plain")
        );
        assert_eq!(body_str(resp.into_body()).await, "hello");
    }

    #[tokio::test]
    async fn native_forward_streams_chunked_body() {
        use futures::stream;
        let mock = axum::Router::new().route(
            "/stream",
            axum::routing::post(|| async {
                let chunks: Vec<Result<String, std::convert::Infallible>> =
                    vec![Ok("chunk1".into()), Ok("chunk2".into())];
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from_stream(stream::iter(chunks)))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });

        let client = reqwest::Client::new();
        let url = reqwest::Url::parse(&format!("http://{addr}/stream")).unwrap();
        let resp = native_forward(
            &client,
            url,
            &HeaderMap::new(),
            reqwest::header::HeaderMap::new(),
            Bytes::from("{}"),
        )
        .await;

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(body_str(resp.into_body()).await, "chunk1chunk2");
    }

    #[test]
    fn should_forward_allowlist() {
        use axum::http::header::HeaderName;
        use std::str::FromStr;
        assert!(should_forward_request_header(
            &HeaderName::from_str("anthropic-beta").unwrap()
        ));
        assert!(should_forward_request_header(
            &HeaderName::from_str("openai-organization").unwrap()
        ));
        assert!(!should_forward_request_header(
            &HeaderName::from_str("authorization").unwrap()
        ));
        assert!(!should_forward_request_header(
            &HeaderName::from_str("host").unwrap()
        ));
    }

    #[test]
    fn last_usage_picks_terminal_chunk() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                    data: {\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n\
                    data: [DONE]\n\n";
        assert_eq!(last_usage_from_sse(body), (Some(5), Some(2), Some(7)));
    }

    #[test]
    fn last_usage_empty_when_absent() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                    data: [DONE]\n\n";
        assert_eq!(last_usage_from_sse(body), (None, None, None));
    }

    #[test]
    fn truncate_body_adds_marker_when_over_cap() {
        let out = truncate_body("x".repeat(200), 50);
        assert!(out.starts_with(&"x".repeat(50)));
        assert!(out.contains("[truncated 150 bytes]"));
    }

    #[test]
    fn truncate_body_unchanged_under_cap() {
        let out = truncate_body("hello".into(), 50);
        assert_eq!(out, "hello");
    }

    #[test]
    fn last_usage_from_anthropic_sse_extracts_tokens() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        assert_eq!(
            last_usage_from_anthropic_sse(body),
            (Some(10), Some(5), Some(15))
        );
    }

    #[test]
    fn last_usage_from_anthropic_sse_missing_tokens() {
        let body = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        assert_eq!(last_usage_from_anthropic_sse(body), (None, None, None));
    }

    #[test]
    fn last_usage_from_anthropic_sse_partial_tokens() {
        // Only input tokens present (no message_delta).
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\n",
        );
        assert_eq!(last_usage_from_anthropic_sse(body), (Some(7), None, None));
    }
}
