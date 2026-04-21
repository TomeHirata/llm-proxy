//! Persistent request/response log backed by SQLite.
//!
//! Stores one row per chat completion request (successful or failed) with raw
//! payloads, latency, token usage, and HTTP status. A background retention
//! task deletes rows older than the configured retention window.
//!
//! Opt-in: nothing is written unless `usage_log.enabled = true` in the config.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone)]
pub struct UsageEntry {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub provider: String,
    pub model_id: String,
    pub status: u16,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub stream: bool,
    pub request_body: String,
    pub response_body: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SummaryRow {
    pub provider: String,
    pub model_id: String,
    pub count: i64,
    pub success_count: i64,
    pub avg_latency_ms: f64,
    pub p50_latency_ms: i64,
    pub p95_latency_ms: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct SummaryTotals {
    pub count: i64,
    pub success_count: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

/// Handle to the usage store. Cheap to clone; internally wraps a shared
/// connection and an mpsc sender to a writer task so callers never block.
#[derive(Clone)]
pub struct UsageStore {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    conn: Mutex<Connection>,
    writer: mpsc::Sender<UsageEntry>,
}

impl UsageStore {
    /// Open (or create) the database at `path`, initialize the schema, and
    /// spawn the background writer thread.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating usage log directory {}", parent.display()))?;
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .with_context(|| format!("opening usage log at {}", path.display()))?;
        init_schema(&conn)?;

        let (tx, mut rx) = mpsc::channel::<UsageEntry>(1024);
        let writer_path = path.clone();
        // Run the writer on a dedicated OS thread (not a tokio worker) — the
        // rusqlite API is synchronous, and running a blocking writer loop on
        // a tokio worker thread under sustained load would stall request
        // handling. `blocking_recv` is explicitly designed for this.
        if let Err(e) = std::thread::Builder::new()
            .name("usage-log-writer".to_string())
            .spawn(move || {
                let mut writer_conn = match Connection::open(&writer_path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("usage_log: cannot open writer conn: {e}");
                        return;
                    }
                };
                if let Err(e) = init_schema(&writer_conn) {
                    tracing::error!("usage_log: writer schema init failed: {e}");
                    return;
                }
                while let Some(entry) = rx.blocking_recv() {
                    if let Err(e) = insert_entry(&mut writer_conn, &entry) {
                        tracing::error!("usage_log: insert failed: {e}");
                    }
                }
            })
        {
            tracing::error!("usage_log: failed to spawn writer thread: {e}");
        }

        Ok(Self {
            inner: Arc::new(Inner {
                path,
                conn: Mutex::new(conn),
                writer: tx,
            }),
        })
    }

    /// Non-blocking — drops the entry if the writer queue is full.
    pub fn record(&self, entry: UsageEntry) {
        if let Err(e) = self.inner.writer.try_send(entry) {
            tracing::warn!("usage_log: dropped entry (writer busy): {e}");
        }
    }

    /// Delete rows older than `retention`. Returns number of rows deleted.
    pub async fn prune(&self, retention: Duration) -> Result<usize> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(retention).unwrap_or_else(|_| chrono::Duration::days(30));
        let cutoff_s = cutoff.to_rfc3339();
        let conn = self.inner.conn.lock().await;
        let n = conn.execute(
            "DELETE FROM usage_log WHERE created_at < ?1",
            params![cutoff_s],
        )?;
        Ok(n)
    }

    pub async fn summary(&self, since: DateTime<Utc>) -> Result<(Vec<SummaryRow>, SummaryTotals)> {
        let conn = self.inner.conn.lock().await;
        let since_s = since.to_rfc3339();

        // Single query with two window functions to compute p50/p95 per group —
        // avoids an N+1 of 3 extra queries per (provider, model_id).
        let mut stmt = conn.prepare(
            "WITH filtered AS (
                 SELECT provider, model_id, status, latency_ms,
                        prompt_tokens, completion_tokens
                 FROM usage_log
                 WHERE created_at >= ?1
             ),
             ranked AS (
                 SELECT provider, model_id, latency_ms,
                        ROW_NUMBER() OVER (
                            PARTITION BY provider, model_id ORDER BY latency_ms
                        ) AS rn,
                        COUNT(*) OVER (PARTITION BY provider, model_id) AS cnt
                 FROM filtered
             ),
             aggregated AS (
                 SELECT provider, model_id,
                        COUNT(*)                                              AS count,
                        SUM(CASE WHEN status BETWEEN 200 AND 299 THEN 1 ELSE 0 END) AS ok,
                        COALESCE(AVG(latency_ms), 0.0)                        AS avg_lat,
                        COALESCE(SUM(prompt_tokens), 0)                       AS pt,
                        COALESCE(SUM(completion_tokens), 0)                   AS ct
                 FROM filtered
                 GROUP BY provider, model_id
             ),
             pct AS (
                 SELECT provider, model_id,
                        MIN(CASE WHEN rn >= ((cnt + 1) / 2)           THEN latency_ms END) AS p50,
                        MIN(CASE WHEN rn >= ((cnt * 95 + 99) / 100)   THEN latency_ms END) AS p95
                 FROM ranked
                 GROUP BY provider, model_id
             )
             SELECT a.provider, a.model_id, a.count, a.ok, a.avg_lat,
                    COALESCE(p.p50, 0), COALESCE(p.p95, 0), a.pt, a.ct
             FROM aggregated a
             LEFT JOIN pct p
               ON p.provider = a.provider AND p.model_id = a.model_id
             ORDER BY a.count DESC",
        )?;
        let out: Vec<SummaryRow> = stmt
            .query_map(params![since_s], |r| {
                Ok(SummaryRow {
                    provider: r.get::<_, String>(0)?,
                    model_id: r.get::<_, String>(1)?,
                    count: r.get::<_, i64>(2)?,
                    success_count: r.get::<_, i64>(3)?,
                    avg_latency_ms: r.get::<_, f64>(4)?,
                    p50_latency_ms: r.get::<_, i64>(5)?,
                    p95_latency_ms: r.get::<_, i64>(6)?,
                    prompt_tokens: r.get::<_, i64>(7)?,
                    completion_tokens: r.get::<_, i64>(8)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        let totals = conn.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN status BETWEEN 200 AND 299 THEN 1 ELSE 0 END),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0)
             FROM usage_log WHERE created_at >= ?1",
            params![since_s],
            |r| {
                Ok(SummaryTotals {
                    count: r.get::<_, i64>(0)?,
                    success_count: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    prompt_tokens: r.get::<_, i64>(2)?,
                    completion_tokens: r.get::<_, i64>(3)?,
                })
            },
        )?;

        Ok((out, totals))
    }

    pub async fn recent(&self, limit: usize) -> Result<Vec<UsageEntry>> {
        let conn = self.inner.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, created_at, provider, model_id, status, latency_ms,
                    prompt_tokens, completion_tokens, total_tokens,
                    stream, request_body, response_body, error
             FROM usage_log
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(UsageEntry {
                    id: r.get(0)?,
                    created_at: r
                        .get::<_, String>(1)?
                        .parse::<DateTime<Utc>>()
                        .unwrap_or_else(|_| Utc::now()),
                    provider: r.get(2)?,
                    model_id: r.get(3)?,
                    status: r.get::<_, i64>(4)? as u16,
                    latency_ms: r.get(5)?,
                    prompt_tokens: r.get(6)?,
                    completion_tokens: r.get(7)?,
                    total_tokens: r.get(8)?,
                    stream: r.get::<_, i64>(9)? != 0,
                    request_body: r.get(10)?,
                    response_body: r.get(11)?,
                    error: r.get(12)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

fn init_schema(conn: &Connection) -> Result<()> {
    // Two connections share the file (reader + writer), plus prune runs
    // concurrently — `busy_timeout` makes `SQLITE_BUSY` block-and-retry
    // instead of returning an error.
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_log (
            id                TEXT PRIMARY KEY,
            created_at        TEXT NOT NULL,
            provider          TEXT NOT NULL,
            model_id          TEXT NOT NULL,
            status            INTEGER NOT NULL,
            latency_ms        INTEGER NOT NULL,
            prompt_tokens     INTEGER,
            completion_tokens INTEGER,
            total_tokens      INTEGER,
            stream            INTEGER NOT NULL,
            request_body      TEXT NOT NULL,
            response_body     TEXT NOT NULL,
            error             TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_usage_created ON usage_log (created_at);
         CREATE INDEX IF NOT EXISTS idx_usage_provider_model
             ON usage_log (provider, model_id, created_at);",
    )?;
    Ok(())
}

fn insert_entry(conn: &mut Connection, e: &UsageEntry) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO usage_log
            (id, created_at, provider, model_id, status, latency_ms,
             prompt_tokens, completion_tokens, total_tokens,
             stream, request_body, response_body, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            e.id,
            e.created_at.to_rfc3339(),
            e.provider,
            e.model_id,
            e.status as i64,
            e.latency_ms,
            e.prompt_tokens,
            e.completion_tokens,
            e.total_tokens,
            e.stream as i64,
            e.request_body,
            e.response_body,
            e.error,
        ],
    )?;
    Ok(())
}

/// Extract `{prompt_tokens, completion_tokens, total_tokens}` from an OpenAI
/// chat completion JSON response body. All three are optional.
pub fn extract_tokens(body: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return (None, None, None);
    };
    let get = |k: &str| v["usage"][k].as_i64();
    (
        get("prompt_tokens"),
        get("completion_tokens"),
        get("total_tokens"),
    )
}

/// Extract token counts from an Anthropic `/v1/messages` response body.
/// Anthropic uses `input_tokens` / `output_tokens` in the `usage` object.
pub fn extract_tokens_anthropic(body: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return (None, None, None);
    };
    let input = v["usage"]["input_tokens"].as_i64();
    let output = v["usage"]["output_tokens"].as_i64();
    let total = input.zip(output).map(|(i, o)| i + o);
    (input, output, total)
}

/// Parse a CLI `--since` value. Handles the shorthand units `d` / `w` that
/// `humantime` does not understand (e.g. `7d`, `2w`); everything else is
/// delegated to `humantime::parse_duration`, so `10ms`, `2h 30min`, etc. all
/// work.
pub fn parse_since(s: &str) -> Result<chrono::Duration> {
    let s = s.trim();
    if let Some((num, unit)) = split_shorthand(s) {
        return Ok(match unit {
            "d" | "day" | "days" => chrono::Duration::days(num as i64),
            "w" | "week" | "weeks" => chrono::Duration::weeks(num as i64),
            _ => unreachable!(),
        });
    }
    let std =
        humantime::parse_duration(s).with_context(|| format!("cannot parse duration '{s}'"))?;
    Ok(chrono::Duration::from_std(std)?)
}

/// Returns `Some((n, unit))` only for units `humantime` does not cover —
/// `d`/`day`/`days`/`w`/`week`/`weeks`. Anything else falls through.
fn split_shorthand(s: &str) -> Option<(u64, &str)> {
    let end = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if end == 0 {
        return None;
    }
    let (n, rest) = s.split_at(end);
    let rest = rest.trim();
    let n: u64 = n.parse().ok()?;
    match rest {
        "d" | "day" | "days" | "w" | "week" | "weeks" => Some((n, rest)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_since_accepts_shorthand() {
        assert_eq!(parse_since("7d").unwrap(), chrono::Duration::days(7));
        assert_eq!(parse_since("24h").unwrap(), chrono::Duration::hours(24));
        assert_eq!(parse_since("30m").unwrap(), chrono::Duration::minutes(30));
        assert_eq!(parse_since("2w").unwrap(), chrono::Duration::weeks(2));
    }

    #[test]
    fn parse_since_falls_through_to_humantime() {
        assert_eq!(
            parse_since("10ms").unwrap(),
            chrono::Duration::milliseconds(10)
        );
        assert_eq!(
            parse_since("2h 30min").unwrap(),
            chrono::Duration::hours(2) + chrono::Duration::minutes(30)
        );
    }

    #[test]
    fn parse_since_rejects_garbage() {
        assert!(parse_since("asdf").is_err());
    }

    #[test]
    fn extract_tokens_parses_openai_shape() {
        let body = r#"{"usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}}"#;
        assert_eq!(extract_tokens(body), (Some(12), Some(8), Some(20)));
    }

    #[test]
    fn extract_tokens_missing_usage() {
        assert_eq!(extract_tokens("{}"), (None, None, None));
        assert_eq!(extract_tokens("not-json"), (None, None, None));
    }

    #[test]
    fn extract_tokens_anthropic_parses_usage() {
        let body = r#"{"usage":{"input_tokens":15,"output_tokens":8},"content":[],"stop_reason":"end_turn"}"#;
        assert_eq!(
            extract_tokens_anthropic(body),
            (Some(15), Some(8), Some(23))
        );
    }

    #[test]
    fn extract_tokens_anthropic_missing_usage() {
        assert_eq!(extract_tokens_anthropic("{}"), (None, None, None));
        assert_eq!(extract_tokens_anthropic("not-json"), (None, None, None));
    }

    #[test]
    fn extract_tokens_anthropic_invalid_usage_fields() {
        // usage present but fields are wrong types → fall back to None
        let body = r#"{"usage":{"input_tokens":"bad","output_tokens":null}}"#;
        assert_eq!(extract_tokens_anthropic(body), (None, None, None));
    }

    #[tokio::test]
    async fn round_trip_insert_and_summary() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let store = UsageStore::open(&path).unwrap();

        let now = Utc::now();
        for (provider, model, status, latency, pt, ct) in [
            ("openai", "gpt-4o", 200, 120, 10, 5),
            ("openai", "gpt-4o", 200, 240, 20, 10),
            ("openai", "gpt-4o", 500, 100, 0, 0),
            ("anthropic", "claude", 200, 300, 50, 25),
        ] {
            store.record(UsageEntry {
                id: uuid::Uuid::new_v4().to_string(),
                created_at: now,
                provider: provider.into(),
                model_id: model.into(),
                status,
                latency_ms: latency,
                prompt_tokens: Some(pt),
                completion_tokens: Some(ct),
                total_tokens: Some(pt + ct),
                stream: false,
                request_body: "{}".into(),
                response_body: "{}".into(),
                error: None,
            });
        }

        // Give the background writer a moment to drain the queue.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (_, totals) = store
                .summary(now - chrono::Duration::hours(1))
                .await
                .unwrap();
            if totals.count == 4 {
                break;
            }
        }

        let (rows, totals) = store
            .summary(now - chrono::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(totals.count, 4);
        assert_eq!(totals.success_count, 3);
        assert_eq!(totals.prompt_tokens, 80);
        assert_eq!(totals.completion_tokens, 40);

        let openai = rows.iter().find(|r| r.provider == "openai").unwrap();
        assert_eq!(openai.count, 3);
        assert_eq!(openai.success_count, 2);
        assert_eq!(openai.prompt_tokens, 30);
    }

    #[tokio::test]
    async fn prune_removes_old_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let store = UsageStore::open(&path).unwrap();

        let old = Utc::now() - chrono::Duration::days(60);
        let fresh = Utc::now();
        for created in [old, old, fresh] {
            store.record(UsageEntry {
                id: uuid::Uuid::new_v4().to_string(),
                created_at: created,
                provider: "openai".into(),
                model_id: "gpt-4o".into(),
                status: 200,
                latency_ms: 50,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                stream: false,
                request_body: "{}".into(),
                response_body: "{}".into(),
                error: None,
            });
        }

        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (_, totals) = store
                .summary(Utc::now() - chrono::Duration::days(365))
                .await
                .unwrap();
            if totals.count == 3 {
                break;
            }
        }

        let deleted = store.prune(Duration::from_secs(30 * 86_400)).await.unwrap();
        assert_eq!(deleted, 2);
        let (_, totals) = store
            .summary(Utc::now() - chrono::Duration::days(365))
            .await
            .unwrap();
        assert_eq!(totals.count, 1);
    }
}
