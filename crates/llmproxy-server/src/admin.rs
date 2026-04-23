/// `/admin/*` management REST API.
///
/// All handlers receive the same `AppState` as the proxy routes. CORS is
/// opened to all origins — the server only binds to 127.0.0.1, so anything
/// that can reach it is already on the same machine.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};

use crate::{config::redacted, server::AppState, usage_log::parse_since};

/// Returns the admin sub-router (no state — it shares `AppState` with the
/// main router via `.merge()`).
pub fn admin_routes() -> Router<AppState> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/admin/status", get(status_handler))
        .route("/admin/usage/summary", get(usage_summary_handler))
        .route("/admin/usage/recent", get(usage_recent_handler))
        .route("/admin/config", get(config_get_handler))
        .route("/admin/config/provider/:name", put(config_put_provider))
        .layer(cors)
}

async fn status_handler(State(s): State<AppState>) -> impl IntoResponse {
    let uptime = (chrono::Utc::now() - s.started_at).num_seconds().max(0);
    let configured: Vec<String> = s
        .registry
        .configured_names()
        .into_iter()
        .filter(|(_, ok)| *ok)
        .map(|(name, _)| name)
        .collect();
    Json(json!({
        "running": true,
        "version": s.version,
        "uptime_secs": uptime,
        "usage_log_enabled": s.usage_store.is_some(),
        "configured_providers": configured,
    }))
}

#[derive(Deserialize)]
struct SinceQuery {
    since: Option<String>,
}

async fn usage_summary_handler(
    State(s): State<AppState>,
    Query(q): Query<SinceQuery>,
) -> impl IntoResponse {
    let Some(store) = &s.usage_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "usage log is disabled"})),
        )
            .into_response();
    };
    let since_str = q.since.as_deref().unwrap_or("7d");
    let since = match parse_since(since_str) {
        Ok(dur) => chrono::Utc::now() - dur,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("bad `since` value: {e}")})),
            )
                .into_response()
        }
    };
    match store.summary(since).await {
        Ok((rows, totals)) => {
            let rows_json: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "provider": r.provider,
                        "model_id": r.model_id,
                        "count": r.count,
                        "success_count": r.success_count,
                        "avg_latency_ms": r.avg_latency_ms,
                        "p50_latency_ms": r.p50_latency_ms,
                        "p95_latency_ms": r.p95_latency_ms,
                        "prompt_tokens": r.prompt_tokens,
                        "completion_tokens": r.completion_tokens,
                    })
                })
                .collect();
            Json(json!({
                "since": since_str,
                "totals": {
                    "count": totals.count,
                    "success_count": totals.success_count,
                    "prompt_tokens": totals.prompt_tokens,
                    "completion_tokens": totals.completion_tokens,
                },
                "rows": rows_json,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct RecentQuery {
    limit: Option<usize>,
}

async fn usage_recent_handler(
    State(s): State<AppState>,
    Query(q): Query<RecentQuery>,
) -> impl IntoResponse {
    let Some(store) = &s.usage_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "usage log is disabled"})),
        )
            .into_response();
    };
    let limit = q.limit.unwrap_or(50).min(500);
    match store.recent(limit).await {
        Ok(entries) => {
            let rows: Vec<Value> = entries
                .iter()
                .map(|e| {
                    json!({
                        "id": e.id,
                        "created_at": e.created_at.to_rfc3339(),
                        "provider": e.provider,
                        "model_id": e.model_id,
                        "status": e.status,
                        "latency_ms": e.latency_ms,
                        "prompt_tokens": e.prompt_tokens,
                        "completion_tokens": e.completion_tokens,
                        "total_tokens": e.total_tokens,
                        "stream": e.stream,
                        "error": e.error,
                    })
                })
                .collect();
            Json(json!({ "entries": rows })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn config_get_handler(State(s): State<AppState>) -> impl IntoResponse {
    Json(redacted(&s.cfg))
}

#[derive(serde::Deserialize)]
struct ProviderPatch {
    api_key: Option<String>,
    endpoint: Option<String>,
    api_version: Option<String>,
    region: Option<String>,
}

async fn config_put_provider(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(patch): Json<ProviderPatch>,
) -> impl IntoResponse {
    let path = match &s.cfg_path {
        Some(p) => p.clone(),
        None => match dirs::home_dir() {
            Some(h) => h.join(".config/llmproxy/config.yaml"),
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "cannot determine home directory"})),
                )
                    .into_response()
            }
        },
    };

    match write_provider_to_config(&path, &name, patch) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Atomically patch a single provider entry into the YAML config file.
fn write_provider_to_config(
    path: &std::path::Path,
    provider: &str,
    patch: ProviderPatch,
) -> anyhow::Result<()> {
    use crate::config::AppConfig;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut cfg: AppConfig = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        serde_yaml::from_str(&raw)?
    } else {
        AppConfig::default()
    };

    let entry = cfg.providers.entry(provider.to_string()).or_default();

    if patch.api_key.is_some() {
        entry.api_key = patch.api_key;
    }
    if patch.endpoint.is_some() {
        entry.endpoint = patch.endpoint;
    }
    if patch.api_version.is_some() {
        entry.api_version = patch.api_version;
    }
    if patch.region.is_some() {
        entry.region = patch.region;
    }

    let yaml = serde_yaml::to_string(&cfg)?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml)?;
    std::fs::rename(&tmp, path)?;

    Ok(())
}
