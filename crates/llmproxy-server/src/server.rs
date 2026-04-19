use std::{convert::Infallible, sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::State,
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
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/models", get(models_handler))
        .route("/health", get(health_handler))
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
                    request_body: raw_request,
                    started,
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
        request_body: request_body.to_string(),
        response_body: response_body.to_string(),
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

/// Captures latency + assembled SSE bytes when a streaming response finishes.
struct StreamFinalizer {
    store: Option<UsageStore>,
    provider: String,
    model_id: String,
    request_body: String,
    started: Instant,
}

/// A stream adapter that buffers every chunk it yields and, when the inner
/// stream ends, writes a usage log entry.
///
/// This is how we record streaming requests — axum's `Sse` doesn't expose a
/// "response finished" hook, so we wrap the body stream itself and write
/// the entry from `Drop`. Using `Drop` on the stream (not the response)
/// means we also record early-terminated connections.
struct FinalizedStream<S> {
    inner: S,
    buf: Vec<u8>,
    finalizer: Option<StreamFinalizer>,
}

impl<S> FinalizedStream<S> {
    fn new(inner: S, finalizer: StreamFinalizer) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            finalizer: Some(finalizer),
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
        let assembled = String::from_utf8_lossy(&self.buf).into_owned();
        let tokens = last_usage_from_sse(&assembled);
        store.record(UsageEntry {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            provider: f.provider,
            model_id: f.model_id,
            status: 200,
            latency_ms: f.started.elapsed().as_millis() as i64,
            prompt_tokens: tokens.0,
            completion_tokens: tokens.1,
            total_tokens: tokens.2,
            stream: true,
            request_body: f.request_body,
            response_body: assembled,
            error: None,
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
            self.buf.extend_from_slice(b);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
