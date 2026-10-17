//! The recorder: a dedicated thread owning the SQLite connections. Rows arrive
//! through a bounded channel ([`QUEUE`] entries; a full queue drops the row and
//! counts it, the proxy never waits), are inserted in one transaction per
//! [`BATCH_WINDOW`] or [`BATCH_ROWS`] rows, and retention runs at start and
//! hourly. Numbers only: no headers or bodies are ever stored.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};
use superba_core::{
    ClearReport, ClearRequest, RequestAttempt, RequestRow, RequestTiming, Result, Timestamp,
};

use crate::paths::RouterPaths;
use crate::store::{USAGE_SCHEMA, clear_on, enum_name, open_write, sql_err, status_columns};

/// Queue length between the proxy and the writer.
pub(crate) const QUEUE: usize = 4096;
/// Longest wait before pending rows are written.
pub(crate) const BATCH_WINDOW: Duration = Duration::from_millis(500);
/// Rows per transaction at most.
pub(crate) const BATCH_ROWS: usize = 128;
/// Retention period.
const RETENTION_EVERY: Duration = Duration::from_secs(3600);

/// Retention limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Retention {
    pub usage_days: u32,
    pub max_rows: u32,
}

/// A row with its detail fields.
#[derive(Debug)]
pub(crate) struct StoredRow {
    pub row: RequestRow,
    /// Every timing mark (normalized); the row repeats headers, first token and
    /// end.
    pub timing: RequestTiming,
    pub attempts: Vec<RequestAttempt>,
    pub log: Vec<String>,
}

enum Cmd {
    Row(Box<StoredRow>),
    Retention(Retention),
    Clear(ClearRequest, std::sync::mpsc::Sender<Result<ClearReport>>),
    Shutdown,
}

/// Handle of the writer thread.
pub(crate) struct Recorder {
    tx: SyncSender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recorder")
    }
}

impl Recorder {
    /// Open the request log under `paths` and start the writer. Returns the
    /// recorder and the largest row id ever used (new ids continue after it).
    pub fn start(
        paths: &RouterPaths,
        retention: Retention,
        written: Arc<AtomicU64>,
    ) -> Result<(Recorder, i64)> {
        let conn = open_write(&paths.usage_db(), USAGE_SCHEMA)?;
        let max_id: i64 = conn
            .query_row(
                "SELECT MAX(COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'requests'), 0), \
                 COALESCE((SELECT MAX(id) FROM requests), 0))",
                [],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        let (tx, rx) = sync_channel(QUEUE);
        let thread = std::thread::Builder::new()
            .name("superba-recorder".into())
            .spawn(move || {
                Writer {
                    usage: conn,
                    retention,
                    written,
                }
                .run(&rx)
            })
            .map_err(|e| superba_core::Error::other(format!("request log thread: {e}")))?;
        Ok((
            Recorder {
                tx,
                thread: Some(thread),
            },
            max_id,
        ))
    }

    /// Queue a row; `false` when the queue is full (the row is dropped).
    pub fn record(&self, row: StoredRow) -> bool {
        !matches!(
            self.tx.try_send(Cmd::Row(Box::new(row))),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_))
        )
    }

    /// New retention limits.
    pub fn set_retention(&self, retention: Retention) {
        let _ = self.tx.try_send(Cmd::Retention(retention));
    }

    /// Delete rows after everything queued so far is written.
    pub fn clear(&self, request: ClearRequest) -> Result<ClearReport> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.tx
            .send(Cmd::Clear(request, reply))
            .map_err(|_| superba_core::Error::other("the request log writer stopped"))?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| superba_core::Error::Timeout {
                what: "clearing the request log".into(),
            })?
    }

    /// Write what is queued and stop the thread.
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.tx.try_send(Cmd::Shutdown);
    }
}

struct Writer {
    usage: Connection,
    retention: Retention,
    written: Arc<AtomicU64>,
}

impl Writer {
    fn run(mut self, rx: &Receiver<Cmd>) {
        let mut rows: Vec<StoredRow> = Vec::new();
        let mut first_pending: Option<Instant> = None;
        self.retention_pass();
        let mut last_retention = Instant::now();
        loop {
            let wait = match first_pending {
                Some(t) => BATCH_WINDOW.saturating_sub(t.elapsed()),
                None => Duration::from_secs(60),
            };
            let stop = match rx.recv_timeout(wait) {
                Ok(Cmd::Row(r)) => {
                    rows.push(*r);
                    first_pending.get_or_insert_with(Instant::now);
                    false
                }
                Ok(Cmd::Retention(r)) => {
                    self.retention = r;
                    false
                }
                Ok(Cmd::Clear(req, reply)) => {
                    self.flush(&mut rows);
                    first_pending = None;
                    let _ = reply.send(clear_on(Some(&self.usage), &req));
                    false
                }
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => true,
                Err(RecvTimeoutError::Timeout) => false,
            };
            let due = first_pending.is_some_and(|t| t.elapsed() >= BATCH_WINDOW);
            if stop || due || rows.len() >= BATCH_ROWS {
                self.flush(&mut rows);
                first_pending = None;
            }
            if stop {
                break;
            }
            if last_retention.elapsed() >= RETENTION_EVERY {
                self.retention_pass();
                last_retention = Instant::now();
            }
        }
    }

    fn flush(&mut self, rows: &mut Vec<StoredRow>) {
        if !rows.is_empty() {
            let n = rows.len() as u64;
            match insert_rows(&mut self.usage, rows) {
                Ok(()) => {
                    self.written.fetch_add(n, Ordering::Relaxed);
                }
                Err(e) => tracing::warn!(error = %e, rows = n, "request log write failed"),
            }
            rows.clear();
        }
    }

    fn retention_pass(&mut self) {
        let now = Timestamp::now().as_millis();
        let day = 86_400_000i64;
        let r = self.retention;
        let cutoff = now - i64::from(r.usage_days.max(1)) * day;
        let res = self
            .usage
            .execute("DELETE FROM requests WHERE started_ms < ?1", [cutoff])
            .and_then(|_| {
                self.usage.execute(
                    "DELETE FROM requests WHERE id <= (SELECT id FROM requests ORDER BY id DESC \
                     LIMIT 1 OFFSET ?1)",
                    [i64::from(r.max_rows.max(1))],
                )
            })
            .and_then(|_| self.usage.execute_batch("PRAGMA incremental_vacuum;"));
        if let Err(e) = res {
            tracing::warn!(error = %e, "request log retention failed");
        }
    }
}

fn opt_i64(v: Option<u64>) -> Option<i64> {
    v.and_then(|v| i64::try_from(v).ok())
}

fn insert_rows(conn: &mut Connection, rows: &[StoredRow]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO requests (id, started_ms, ttfb_ms, first_event_ms, \
             first_token_ms, duration_ms, member_id, member_name, line_id, line_name, \
             upstream_host, failovers, transport, originator, client, request_kind, \
             thread_source, thread_id, turn_id, prompt_cache_key, model, upstream_model, \
             response_model, service_tier, response_service_tier, status_kind, status_code, \
             status_http, http_status, error, input_tokens, cached_tokens, cache_write_tokens, \
             output_tokens, reasoning_tokens, tps_milli, cost_micros, price_rev, injected, \
             stripped, upstream_request_id, attempts, log, sent_ms, last_delta_ms) VALUES (?1, \
             ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, \
             ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, \
             ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45)",
        )?;
        for s in rows {
            let r = &s.row;
            let (kind, code, http) = status_columns(&r.status);
            let attempts = serde_json::to_string(&s.attempts).ok();
            let log = serde_json::to_string(&s.log).ok();
            stmt.execute(params![
                r.id,
                r.started_at.as_millis(),
                opt_i64(r.ttfb_ms),
                opt_i64(s.timing.first_event_ms),
                opt_i64(r.first_token_ms),
                opt_i64(r.duration_ms),
                r.member_id.as_str(),
                r.member_name,
                r.line_id,
                r.line_name,
                r.upstream_host,
                i64::from(r.failovers),
                enum_name(&r.transport),
                r.originator,
                enum_name(&r.client),
                r.request_kind.as_ref().map(enum_name),
                r.thread_source,
                r.thread_id,
                r.turn_id,
                r.prompt_cache_key,
                r.model,
                r.upstream_model,
                r.response_model,
                r.service_tier_requested,
                r.service_tier_answered,
                kind,
                code,
                http.map(i64::from),
                r.http_status.map(i64::from),
                r.error,
                opt_i64(r.input_tokens),
                opt_i64(r.cached_tokens),
                opt_i64(r.cache_write_tokens),
                opt_i64(r.output_tokens),
                opt_i64(r.reasoning_tokens),
                opt_i64(r.tps_milli),
                r.cost.map(|c| c.micros()),
                r.price_rev,
                i64::from(r.injected),
                i64::from(r.stripped),
                r.upstream_request_id,
                attempts,
                log,
                opt_i64(s.timing.sent_ms),
                opt_i64(s.timing.last_delta_ms),
            ])?;
        }
    }
    tx.commit()
}
