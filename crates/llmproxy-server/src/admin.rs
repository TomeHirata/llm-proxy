/// `/admin/*` management REST API.
///
/// All handlers receive the same `AppState` as the proxy routes. CORS is
/// opened to all origins — the server only binds to 127.0.0.1, so anything
/// that can reach it is already on the same machine.
use axum::{
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::{config::redacted, server::AppState, usage_log::parse_since};

/// Returns the admin sub-router (no state — it shares `AppState` with the
/// main router via `.merge()`).
pub fn admin_routes() -> Router<AppState> {
    // Allow only localhost origins — the admin API must not be reachable from
    // arbitrary websites even when the server is bound to a non-loopback address.
    let localhost_origins = AllowOrigin::predicate(|origin: &HeaderValue, _| {
        origin
            .to_str()
            .map(|s| {
                s.starts_with("http://localhost")
                    || s.starts_with("http://127.0.0.1")
                    || s.starts_with("tauri://localhost")
                    || s == "null" // file:// / Tauri custom protocol
            })
            .unwrap_or(false)
    });
    let cors = CorsLayer::new()
        .allow_origin(localhost_origins)
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    Router::new()
        .route("/admin/status", get(status_handler))
        .route("/admin/usage/summary", get(usage_summary_handler))
        .route("/admin/usage/recent", get(usage_recent_handler))
        .route("/admin/config", get(config_get_handler))
        .route("/admin/config/provider/:name", put(config_put_provider))
        .route("/admin/models/:provider", get(provider_models_handler))
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

/// Fetch the live model list from an upstream provider using configured credentials.
/// Returns `{"models": ["model-id", ...]}` with IDs suitable for use as
/// `provider/model-id` in chat requests.
async fn provider_models_handler(
    State(s): State<AppState>,
    Path(provider): Path<String>,
) -> impl IntoResponse {
    let cred = match s.registry.credential_for(&provider, None) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let token = match cred {
        llmproxy_core::provider::Credential::BearerToken(t) => t,
        llmproxy_core::provider::Credential::AwsSigV4 { .. } => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "bedrock model listing not supported"})),
            )
                .into_response()
        }
    };

    let result: Result<Vec<String>, String> = match provider.as_str() {
        "openai" => fetch_openai_models(&s.http, &token).await,
        "anthropic" => fetch_anthropic_models(&s.http, &token).await,
        "gemini" => fetch_gemini_models(&s.http, &token).await,
        "mistral" => {
            fetch_openai_compat_models(&s.http, &token, "https://api.mistral.ai/v1/models").await
        }
        "togetherai" => {
            fetch_openai_compat_models(&s.http, &token, "https://api.together.xyz/v1/models").await
        }
        "databricks" => {
            let workspace_url = s
                .cfg
                .providers
                .get("databricks")
                .and_then(|p| p.endpoint.as_deref())
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_string();
            if workspace_url.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "databricks endpoint not configured"})),
                )
                    .into_response();
            }
            fetch_databricks_models(&s.http, &token, &workspace_url).await
        }
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("model listing not supported for {provider}")})),
            )
                .into_response()
        }
    };

    match result {
        Ok(models) => Json(json!({"models": models})).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": e}))).into_response(),
    }
}

async fn fetch_openai_models(client: &reqwest::Client, token: &str) -> Result<Vec<String>, String> {
    let resp = client
        .get("https://api.openai.com/v1/models")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| body.clone());
        return Err(format!("OpenAI API error {status}: {msg}"));
    }
    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("parse error: {e}"))?;
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["id"].as_str())
                .filter(|id| is_chat_model_openai(id))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}

fn is_chat_model_openai(id: &str) -> bool {
    let keep = ["gpt-", "o1", "o3", "o4", "chatgpt"];
    let drop = [
        "instruct",
        "embedding",
        "whisper",
        "tts",
        "dall-e",
        "davinci",
        "babbage",
        "text-",
        "audio",
        "image",
        "search",
        "similarity",
        "code-",
    ];
    keep.iter().any(|p| id.starts_with(p)) && !drop.iter().any(|p| id.contains(p))
}

async fn fetch_anthropic_models(
    client: &reqwest::Client,
    token: &str,
) -> Result<Vec<String>, String> {
    let resp = client
        .get("https://api.anthropic.com/v1/models")
        .header("x-api-key", token)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| body.clone());
        return Err(format!("Anthropic API error {status}: {msg}"));
    }
    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("parse error: {e}"))?;
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["id"].as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort_by(|a, b| b.cmp(a)); // newest first
    Ok(ids)
}

async fn fetch_gemini_models(client: &reqwest::Client, token: &str) -> Result<Vec<String>, String> {
    let mut url =
        reqwest::Url::parse("https://generativelanguage.googleapis.com/v1beta/models").unwrap();
    url.query_pairs_mut().append_pair("key", token);
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| body.clone());
        return Err(format!("Gemini API error {status}: {msg}"));
    }
    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("parse error: {e}"))?;
    let mut ids = Vec::new();
    if let Some(arr) = body["models"].as_array() {
        for m in arr {
            let supports_generate = m["supportedGenerationMethods"]
                .as_array()
                .map(|a| a.iter().any(|v| v.as_str() == Some("generateContent")))
                .unwrap_or(false);
            if !supports_generate {
                continue;
            }
            if let Some(id) = m["name"].as_str().and_then(|n| n.strip_prefix("models/")) {
                if !id.contains("embedding") && !id.contains("aqa") {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids.sort();
    Ok(ids)
}

async fn fetch_databricks_models(
    client: &reqwest::Client,
    token: &str,
    workspace_url: &str,
) -> Result<Vec<String>, String> {
    let url = format!("{workspace_url}/api/2.0/serving-endpoints");
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Databricks API error {status}: {body}"));
    }
    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("parse error: {e}"))?;
    let mut ids: Vec<String> = body["endpoints"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["name"].as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}

async fn fetch_openai_compat_models(
    client: &reqwest::Client,
    token: &str,
    url: &str,
) -> Result<Vec<String>, String> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| body.clone());
        return Err(format!("API error {status}: {msg}"));
    }
    let body = resp
        .json::<Value>()
        .await
        .map_err(|e| format!("parse error: {e}"))?;
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["id"].as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}
