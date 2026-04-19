use std::{convert::Infallible, sync::Arc};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures::StreamExt;
use llmproxy_core::{error::ProxyError, openai_types::ChatRequest};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::registry::ProviderRegistry;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<ProviderRegistry>,
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

async fn chat_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let (provider, model_id, cred) = match state.registry.resolve(&req.model, auth.as_deref()) {
        Ok(r) => r,
        Err(e) => return proxy_error_to_response(&e),
    };

    let want_stream = req.stream.unwrap_or(false);

    if want_stream {
        match provider.chat_stream(req, &model_id, &cred).await {
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
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .header("connection", "keep-alive")
                    .body(Body::from_stream(byte_stream))
                    .unwrap()
            }
            Err(e) => proxy_error_to_response(&e),
        }
    } else {
        match provider.chat(req, &model_id, &cred).await {
            Ok(resp) => Json(resp).into_response(),
            Err(e) => proxy_error_to_response(&e),
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
