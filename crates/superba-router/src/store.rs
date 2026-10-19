//! The request log on disk (`<data dir>/router/usage.sqlite3`): written only by
//! the running router's recorder thread, read read-only by Superba (GUI, `spb`)
//! through [`Reader`], so history stays visible while the router is stopped.
//!
//! Schema (version 3): one row per proxied model request (Codex Responses,
//! Claude Code Messages), the columns of
//! [`RequestRow`] plus the detail fields (timing marks, attempts, log lines).
//! Never stored: prompts, outputs, request or response headers and bodies, keys,
//! tokens.
//!
//! [`clear`] deletes through the running router (its writer stays the only
//! writer) and directly when the router is stopped.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params_from_iter};
use serde::Serialize;
use serde::de::DeserializeOwned;
use superba_core::{
    ClearReport, ClearRequest, Error, ProfileId, RequestAttempt, RequestClient, RequestDetail,
    RequestGroup, RequestGrouping, RequestHarness, RequestPage, RequestQuery, RequestRow,
    RequestSortKey, RequestStats, RequestStatus, RequestStatusFilter, RequestTiming, RequestTotals,
    RequestTransport, Result, StatsBucket, StatsGroupBy, StatsQuery, Timestamp, Usd,
    provider_line_key, split_provider_key,
};

use crate::paths::RouterPaths;

/// Schema version written to `PRAGMA user_version`. A log of another version
/// (an earlier build's) is not read and is recreated when the router opens it
/// for writing: nothing has been released, so there is no migration.
pub const SCHEMA_VERSION: i64 = 3;

/// Rows a query returns at most (a larger `limit` is clamped).
pub const MAX_QUERY_ROWS: u32 = 10_000;

/// Groups a query returns at most.
pub const MAX_GROUPS: usize = 500;

pub(crate) const USAGE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS requests (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  started_ms INTEGER NOT NULL,
  ttfb_ms INTEGER, first_event_ms INTEGER, first_token_ms INTEGER, duration_ms INTEGER,
  member_id TEXT NOT NULL, member_name TEXT NOT NULL, line_id TEXT, line_name TEXT,
  upstream_host TEXT, failovers INTEGER NOT NULL DEFAULT 0,
  transport TEXT NOT NULL, originator TEXT, client TEXT NOT NULL,
  request_kind TEXT, thread_source TEXT, thread_id TEXT, turn_id TEXT, prompt_cache_key TEXT,
  model TEXT, upstream_model TEXT, response_model TEXT,
  service_tier TEXT, response_service_tier TEXT,
  status_kind TEXT NOT NULL, status_code TEXT, status_http INTEGER, http_status INTEGER, error TEXT,
  input_tokens INTEGER, cached_tokens INTEGER, cache_write_tokens INTEGER,
  output_tokens INTEGER, reasoning_tokens INTEGER,
  tps_milli INTEGER, cost_micros INTEGER, price_rev TEXT,
  injected INTEGER NOT NULL DEFAULT 0, stripped INTEGER NOT NULL DEFAULT 0,
  upstream_request_id TEXT, attempts TEXT, log TEXT,
  sent_ms INTEGER, last_delta_ms INTEGER, codex_host TEXT,
  harness TEXT NOT NULL DEFAULT 'codex', harness_version TEXT,
  protocol TEXT NOT NULL DEFAULT 'responses',
  requested_model TEXT, session_id TEXT, agent_kind TEXT, response_id TEXT
);
CREATE INDEX IF NOT EXISTS requests_time ON requests(started_ms);
CREATE INDEX IF NOT EXISTS requests_member_time ON requests(member_id, started_ms);
CREATE INDEX IF NOT EXISTS requests_model_time ON requests(model, started_ms);
CREATE INDEX IF NOT EXISTS requests_turn ON requests(turn_id);
CREATE INDEX IF NOT EXISTS requests_session ON requests(COALESCE(prompt_cache_key, thread_id), started_ms);
CREATE INDEX IF NOT EXISTS requests_host ON requests(upstream_host, started_ms);
";

/// Columns of [`RequestRow`] in [`row_from_sql`] order.
pub(crate) const ROW_COLUMNS: &str = "id, started_ms, prompt_cache_key, thread_id, turn_id, \
     member_id, member_name, line_id, line_name, upstream_host, failovers, transport, originator, \
     client, request_kind, thread_source, model, upstream_model, response_model, service_tier, \
     response_service_tier, status_kind, status_code, status_http, http_status, error, \
     input_tokens, cached_tokens, cache_write_tokens, output_tokens, reasoning_tokens, ttfb_ms, \
     first_token_ms, duration_ms, tps_milli, cost_micros, price_rev, injected, stripped, \
     upstream_request_id, codex_host, harness, harness_version, protocol, requested_model, \
     session_id, agent_kind, response_id";

/// Number of [`ROW_COLUMNS`] (the detail fields follow them).
fn row_column_count() -> usize {
    ROW_COLUMNS.split(',').count()
}

/// [`ROW_COLUMNS`] (kept as a function for the readers).
pub(crate) fn row_columns(_conn: &Connection) -> String {
    ROW_COLUMNS.to_owned()
}

/// The schema version of an open log.
fn schema_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0)
}

/// The session key of a row (SQL).
const HARNESS_KEY: &str = "CASE harness WHEN 'claude_code' THEN 'claude-code' ELSE harness END";
const HOST_KEY: &str = "COALESCE(NULLIF(codex_host, ''), 'local')";
const SESSION_KEY: &str = "(CASE harness WHEN 'claude_code' THEN 'claude-code' ELSE harness END) || char(31) || COALESCE(NULLIF(session_id, ''), CASE WHEN harness = 'codex' THEN COALESCE(prompt_cache_key, thread_id) END)";
const HARNESS_MODEL_KEY: &str = "(CASE harness WHEN 'claude_code' THEN 'claude-code' ELSE harness END) || char(31) || COALESCE(upstream_model, model, response_model, '')";

/// The provider key of a row (SQL, `RequestRow::provider_key`).
const PROVIDER_KEY: &str = "NULLIF(member_id, '')";

/// The line label of a row (SQL, `RequestRow::line_label`).
const LINE_LABEL: &str = "COALESCE(NULLIF(line_name, ''), NULLIF(line_id, ''))";

pub(crate) fn sql_err(e: rusqlite::Error) -> Error {
    Error::other(format!("request log: {e}"))
}

/// The serde name of a unit enum value (`"web_socket"`, `"desktop"`).
pub(crate) fn enum_name<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn enum_parse<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

/// `(status_kind, status_code, status_http)` of a status.
pub(crate) fn status_columns(s: &RequestStatus) -> (&'static str, Option<String>, Option<u16>) {
    match s {
        RequestStatus::Http { status, code } => ("http", code.clone(), Some(*status)),
        RequestStatus::Failed { code } => ("failed", code.clone(), None),
        RequestStatus::Incomplete { reason } => ("incomplete", reason.clone(), None),
        RequestStatus::Broken => ("broken", None, None),
        RequestStatus::Cancelled => ("cancelled", None, None),
        _ => ("ok", None, None),
    }
}

fn status_from(kind: &str, code: Option<String>, http: Option<u16>) -> RequestStatus {
    match kind {
        "http" => RequestStatus::Http {
            status: http.unwrap_or(0),
            code,
        },
        "failed" => RequestStatus::Failed { code },
        "incomplete" => RequestStatus::Incomplete { reason: code },
        "broken" => RequestStatus::Broken,
        "cancelled" => RequestStatus::Cancelled,
        _ => RequestStatus::Ok,
    }
}

fn u64_of(v: Option<i64>) -> Option<u64> {
    v.and_then(|v| u64::try_from(v).ok())
}

/// A row from a `SELECT ROW_COLUMNS`.
pub(crate) fn row_from_sql(r: &Row<'_>) -> rusqlite::Result<RequestRow> {
    let status_kind: String = r.get(21)?;
    let status_code: Option<String> = r.get(22)?;
    let status_http: Option<i64> = r.get(23)?;
    let transport: String = r.get(11)?;
    let client: String = r.get(13)?;
    let request_kind: Option<String> = r.get(14)?;
    let harness: String = r.get(41)?;
    let protocol: String = r.get(43)?;
    let agent_kind: Option<String> = r.get(46)?;
    Ok(RequestRow {
        id: r.get(0)?,
        started_at: Timestamp::from_millis(r.get(1)?),
        prompt_cache_key: r.get(2)?,
        thread_id: r.get(3)?,
        turn_id: r.get(4)?,
        member_id: ProfileId::new(r.get::<_, String>(5)?),
        member_name: r.get(6)?,
        line_id: r.get(7)?,
        line_name: r.get(8)?,
        upstream_host: r.get(9)?,
        failovers: u32::try_from(r.get::<_, i64>(10)?).unwrap_or(0),
        transport: enum_parse(&transport).unwrap_or(RequestTransport::Http),
        originator: r.get(12)?,
        client: enum_parse(&client).unwrap_or(RequestClient::Other),
        request_kind: request_kind.as_deref().and_then(enum_parse),
        thread_source: r.get(15)?,
        model: r.get(16)?,
        upstream_model: r.get(17)?,
        response_model: r.get(18)?,
        service_tier_requested: r.get(19)?,
        service_tier_answered: r.get(20)?,
        status: status_from(
            &status_kind,
            status_code,
            status_http.and_then(|s| u16::try_from(s).ok()),
        ),
        http_status: r
            .get::<_, Option<i64>>(24)?
            .and_then(|s| u16::try_from(s).ok()),
        error: r.get(25)?,
        input_tokens: u64_of(r.get(26)?),
        cached_tokens: u64_of(r.get(27)?),
        cache_write_tokens: u64_of(r.get(28)?),
        output_tokens: u64_of(r.get(29)?),
        reasoning_tokens: u64_of(r.get(30)?),
        ttfb_ms: u64_of(r.get(31)?),
        first_token_ms: u64_of(r.get(32)?),
        duration_ms: u64_of(r.get(33)?),
        tps_milli: u64_of(r.get(34)?),
        cost: r.get::<_, Option<i64>>(35)?.map(Usd::from_micros),
        price_rev: r.get(36)?,
        injected: r.get::<_, i64>(37)? != 0,
        stripped: u32::try_from(r.get::<_, i64>(38)?).unwrap_or(0),
        upstream_request_id: r.get(39)?,
        source: superba_core::RequestSource::Router,
        codex_host: r.get(40)?,
        harness: enum_parse(&harness).unwrap_or(RequestHarness::Unknown),
        harness_version: r.get(42)?,
        protocol: enum_parse(&protocol).unwrap_or_default(),
        requested_model: r.get(44)?,
        session_id: r.get(45)?,
        agent_kind: agent_kind.as_deref().and_then(enum_parse),
        response_id: r.get(47)?,
    })
}

/// Open a database read-only; `None` when it does not exist.
fn open_read_only(path: &Path) -> Result<Option<Connection>> {
    if !path.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql_err)?;
    conn.busy_timeout(Duration::from_secs(3)).map_err(sql_err)?;
    // A log of an earlier build's format holds nothing this build reads (the
    // router starts it again when it next records).
    if schema_version(&conn) != SCHEMA_VERSION {
        return Ok(None);
    }
    Ok(Some(conn))
}

/// Open (creating with mode 0600) a database for writing.
pub(crate) fn open_write(path: &Path, schema: &str) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        crate::config::create_private_dir(dir)?;
    }
    // Created with mode 0600 when missing. Opening it here first also reports
    // the OS error behind a failure ("too many open files" included), where
    // SQLite would only say it cannot open the file.
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    drop(opts.open(path).map_err(|e| Error::io(path, &e))?);
    let conn = Connection::open(path).map_err(sql_err)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(sql_err)?;
    // auto_vacuum must be chosen before the first table exists.
    conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")
        .map_err(sql_err)?;
    let _: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .map_err(sql_err)?;
    conn.execute_batch("PRAGMA synchronous = NORMAL;")
        .map_err(sql_err)?;
    // A log of an earlier build's format starts again (no migrations before
    // the first release).
    let version = schema_version(&conn);
    if version != 0 && version != SCHEMA_VERSION {
        tracing::info!(
            version,
            "request log of an earlier format; starting a new one"
        );
        conn.execute_batch("DROP TABLE IF EXISTS requests;")
            .map_err(sql_err)?;
    }
    conn.execute_batch(schema).map_err(sql_err)?;
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
        .map_err(sql_err)?;
    Ok(conn)
}

/// Delete rows per `request` on an open write connection.
pub(crate) fn clear_on(usage: Option<&Connection>, request: &ClearRequest) -> Result<ClearReport> {
    let before = request.before.map(Timestamp::as_millis).unwrap_or(i64::MAX);
    let mut report = ClearReport::default();
    if let Some(u) = usage {
        report.rows = u
            .execute("DELETE FROM requests WHERE started_ms < ?1", [before])
            .map_err(sql_err)? as u64;
        let _ = u.execute_batch("PRAGMA incremental_vacuum;");
    }
    Ok(report)
}

/// Delete directly in the files (the router is not running).
pub(crate) fn clear_files(paths: &RouterPaths, request: &ClearRequest) -> Result<ClearReport> {
    let usage = paths
        .usage_db()
        .is_file()
        .then(|| open_write(&paths.usage_db(), USAGE_SCHEMA))
        .transpose()?;
    clear_on(usage.as_ref(), request)
}

/// Delete recorded rows: through the running router's admin API
/// when it runs (its writer stays the only writer), directly in the files when
/// it is stopped.
pub async fn clear(paths: &RouterPaths, request: &ClearRequest) -> Result<ClearReport> {
    if let Ok(client) = crate::client::AdminClient::from_runtime(paths)
        && client.health().await.is_ok()
    {
        return client.clear(request).await;
    }
    clear_files(paths, request)
}

/// Read-only access to the request log.
#[derive(Clone, Debug)]
pub struct Reader {
    paths: RouterPaths,
}

/// WHERE clause and its parameters.
#[derive(Clone)]
struct Filter {
    clauses: Vec<String>,
    params: Vec<SqlValue>,
}

impl Filter {
    fn new() -> Self {
        Self {
            clauses: Vec::new(),
            params: Vec::new(),
        }
    }

    fn push(&mut self, clause: &str, value: SqlValue) {
        self.params.push(value);
        self.clauses
            .push(clause.replace('?', &format!("?{}", self.params.len())));
    }

    fn raw(&mut self, clause: &str) {
        self.clauses.push(clause.to_owned());
    }

    fn sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.clauses.join(" AND "))
        }
    }
}

fn text(s: &str) -> SqlValue {
    SqlValue::Text(s.to_owned())
}

fn like_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('%');
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

const TOTALS: &str = "COUNT(*), COALESCE(SUM(status_kind = 'ok'), 0), \
     COALESCE(SUM(status_kind != 'ok'), 0), COALESCE(SUM(input_tokens), 0), \
     COALESCE(SUM(cached_tokens), 0), COALESCE(SUM(cache_write_tokens), 0), \
     COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cost_micros), 0), \
     COALESCE(SUM(input_tokens IS NOT NULL AND cost_micros IS NULL), 0), \
     COUNT(first_token_ms), COALESCE(SUM(first_token_ms), 0), \
     COALESCE(SUM(CASE WHEN tps_milli IS NOT NULL THEN output_tokens END), 0), \
     COALESCE(SUM(CASE WHEN tps_milli IS NOT NULL THEN output_tokens * tps_milli END), 0)";

fn totals_from(r: &Row<'_>, at: usize) -> rusqlite::Result<RequestTotals> {
    let n = |i: usize| -> rusqlite::Result<u64> {
        Ok(u64::try_from(r.get::<_, i64>(at + i)?).unwrap_or(0))
    };
    Ok(RequestTotals {
        requests: n(0)?,
        ok: n(1)?,
        errors: n(2)?,
        input_tokens: n(3)?,
        cached_tokens: n(4)?,
        cache_write_tokens: n(5)?,
        output_tokens: n(6)?,
        cost: Usd::from_micros(r.get(at + 7)?),
        unpriced: n(8)?,
        first_token_rows: n(9)?,
        first_token_ms_sum: n(10)?,
        tps_output_tokens: n(11)?,
        tps_weighted_milli: n(12)?,
    })
}

fn sort_sql(query: &RequestQuery) -> String {
    let col = match query.sort.key {
        RequestSortKey::Harness => HARNESS_KEY,
        RequestSortKey::Model => "COALESCE(upstream_model, model, response_model)",
        RequestSortKey::CacheKey => "prompt_cache_key",
        RequestSortKey::Session => SESSION_KEY,
        RequestSortKey::Status => "status_kind",
        RequestSortKey::Provider => "member_name, line_name",
        RequestSortKey::Input => {
            "(input_tokens - COALESCE(cached_tokens, 0) - COALESCE(cache_write_tokens, 0))"
        }
        RequestSortKey::Output => "output_tokens",
        RequestSortKey::CacheRead => "cached_tokens",
        RequestSortKey::CacheWrite => "cache_write_tokens",
        RequestSortKey::FirstToken => "first_token_ms",
        RequestSortKey::Duration => "duration_ms",
        RequestSortKey::Tps => "tps_milli",
        RequestSortKey::Cost => "cost_micros",
        _ => "started_ms",
    };
    let dir = if query.sort.descending { "DESC" } else { "ASC" };
    let columns = if query.sort.key == RequestSortKey::Provider {
        vec!["member_name", "line_name"]
    } else {
        vec![col]
    };
    let cols: Vec<String> = columns
        .into_iter()
        .map(|c| format!("{c} {dir}"))
        .chain(std::iter::once(format!("id {dir}")))
        .collect();
    cols.join(", ")
}

/// `YYYY-MM-DD` of a day number since the Unix epoch.
pub fn day_label(days: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Source list entries by session, newest activity first.
fn keyed_groups(conn: &Connection, f: &Filter, key: &str) -> Result<Vec<RequestGroup>> {
    let sql = format!(
        "SELECT k, COUNT(*), MAX(started_ms) FROM (SELECT {key} AS k, started_ms \
         FROM requests{}) WHERE k IS NOT NULL GROUP BY k ORDER BY MAX(started_ms) DESC \
         LIMIT {MAX_GROUPS}",
        f.sql()
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    stmt.query_map(params_from_iter(f.params.iter()), |r| {
        let k: String = r.get(0)?;
        Ok(RequestGroup {
            label: k.clone(),
            key: k,
            count: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
            last: Timestamp::from_millis(r.get(2)?),
            upstream_host: None,
            lines: Vec::new(),
        })
    })
    .map_err(sql_err)?
    .collect::<rusqlite::Result<_>>()
    .map_err(sql_err)
}

/// Source list entries by provider, newest activity first, each with its lines
/// (most requests first). Labels and hosts are those of the newest row (SQLite
/// takes bare columns from the row of the single `MAX()`).
fn provider_groups(conn: &Connection, f: &Filter) -> Result<Vec<RequestGroup>> {
    let count = |r: &Row<'_>, i: usize| -> rusqlite::Result<u64> {
        Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
    };
    let sql = format!(
        "SELECT k, COUNT(*), MAX(started_ms), member_name, upstream_host FROM (SELECT \
         {PROVIDER_KEY} AS k, started_ms, member_name, upstream_host FROM requests{}) \
         WHERE k IS NOT NULL GROUP BY k ORDER BY MAX(started_ms) DESC LIMIT {MAX_GROUPS}",
        f.sql()
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let mut groups: Vec<RequestGroup> = stmt
        .query_map(params_from_iter(f.params.iter()), |r| {
            let k: String = r.get(0)?;
            let name: String = r.get(3)?;
            Ok(RequestGroup {
                label: if name.is_empty() { k.clone() } else { name },
                key: k,
                count: count(r, 1)?,
                last: Timestamp::from_millis(r.get(2)?),
                upstream_host: r.get(4)?,
                lines: Vec::new(),
            })
        })
        .map_err(sql_err)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql_err)?;
    let sql = format!(
        "SELECT k, l, COUNT(*), MAX(started_ms), upstream_host FROM (SELECT {PROVIDER_KEY} AS k, \
         {LINE_LABEL} AS l, started_ms, upstream_host FROM requests{}) \
         WHERE k IS NOT NULL AND l IS NOT NULL GROUP BY k, l ORDER BY COUNT(*) DESC, l ASC",
        f.sql()
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let lines = stmt
        .query_map(params_from_iter(f.params.iter()), |r| {
            let k: String = r.get(0)?;
            let l: String = r.get(1)?;
            Ok((
                k.clone(),
                RequestGroup {
                    key: provider_line_key(&k, &l),
                    label: l,
                    count: count(r, 2)?,
                    last: Timestamp::from_millis(r.get(3)?),
                    upstream_host: r.get(4)?,
                    lines: Vec::new(),
                },
            ))
        })
        .map_err(sql_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_err)?;
    for (provider, line) in lines {
        if let Some(g) = groups.iter_mut().find(|g| g.key == provider) {
            g.lines.push(line);
        }
    }
    Ok(groups)
}

impl Reader {
    /// A reader for the router directory `paths` (the databases are opened
    /// read-only per query; a missing database reads as empty).
    pub fn open(paths: &RouterPaths) -> Result<Reader> {
        Ok(Reader {
            paths: paths.clone(),
        })
    }

    /// The router directory.
    pub fn paths(&self) -> &RouterPaths {
        &self.paths
    }

    fn filter(query: &RequestQuery, with_group: bool, with_after: bool) -> Filter {
        let mut f = Filter::new();
        if let Some(from) = query.from {
            f.push("started_ms >= ?", SqlValue::Integer(from.as_millis()));
        }
        if let Some(to) = query.to {
            f.push("started_ms < ?", SqlValue::Integer(to.as_millis()));
        }
        if let Some(m) = &query.member {
            f.push("member_id = ?", text(m.as_str()));
        }
        if let Some(l) = &query.line {
            f.push("line_id = ?", text(l));
        }
        if let Some(m) = &query.model {
            f.params.push(text(m));
            let n = f.params.len();
            f.raw(&format!(
                "(model = ?{n} OR upstream_model = ?{n} OR requested_model = ?{n} OR response_model = ?{n})"
            ));
        }
        if !query.harnesses.is_empty() {
            let slots: Vec<String> = query
                .harnesses
                .iter()
                .map(|h| {
                    f.params.push(text(&enum_name(h)));
                    format!("?{}", f.params.len())
                })
                .collect();
            f.raw(&format!("harness IN ({})", slots.join(",")));
        }
        if let Some(host) = &query.host {
            f.push(&format!("{HOST_KEY} = ?"), text(host));
        }
        if let Some(protocol) = query.protocol {
            f.push("protocol = ?", text(&enum_name(&protocol)));
        }
        match query.status {
            RequestStatusFilter::Ok => f.raw("status_kind = 'ok'"),
            RequestStatusFilter::Errors => f.raw("status_kind != 'ok'"),
            RequestStatusFilter::Failover => f.raw("failovers > 0"),
            _ => {}
        }
        if let Some(c) = query.client {
            f.push("client = ?", text(&enum_name(&c)));
        }
        if with_group && let Some(g) = &query.group {
            match query.group_by {
                RequestGrouping::Provider => {
                    let (provider, line) = split_provider_key(g);
                    f.push(&format!("{PROVIDER_KEY} = ?"), text(provider));
                    match line {
                        // The provider's rows without a known line.
                        Some("") => f.raw(&format!("{LINE_LABEL} IS NULL")),
                        Some(line) => f.push(&format!("{LINE_LABEL} = ?"), text(line)),
                        None => {}
                    }
                }
                RequestGrouping::Harness => f.push(&format!("{HARNESS_KEY} = ?"), text(g)),
                RequestGrouping::HarnessModel => {
                    f.push(&format!("{HARNESS_MODEL_KEY} = ?"), text(g))
                }
                _ => f.push(&format!("{SESSION_KEY} = ?"), text(g)),
            }
        }
        if let Some(s) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let cols = [
                "prompt_cache_key",
                "thread_id",
                "member_name",
                "line_name",
                "upstream_host",
                "model",
                "upstream_model",
                "response_model",
                "requested_model",
                "harness",
                "session_id",
                "turn_id",
                "codex_host",
                "status_kind",
                "status_code",
                "upstream_request_id",
            ];
            f.params.push(text(&like_pattern(s)));
            let n = f.params.len();
            let any: Vec<String> = cols
                .iter()
                .map(|c| format!("COALESCE({c}, '') LIKE ?{n} ESCAPE '\\'"))
                .collect();
            f.clauses.push(format!("({})", any.join(" OR ")));
        }
        if with_after && let Some(a) = query.after_id {
            f.push("id > ?", SqlValue::Integer(a));
        }
        f
    }

    /// Rows, totals and source-list groups.
    pub fn query(&self, query: &RequestQuery) -> Result<RequestPage> {
        let Some(conn) = open_read_only(&self.paths.usage_db())? else {
            return Ok(RequestPage::default());
        };
        let limit = match query.limit {
            0 => RequestQuery::DEFAULT_LIMIT,
            n => n.min(MAX_QUERY_ROWS),
        };
        let f = Self::filter(query, true, true);
        let sql = format!(
            "SELECT {} FROM requests{} ORDER BY {} LIMIT {}",
            row_columns(&conn),
            f.sql(),
            sort_sql(query),
            limit + 1
        );
        let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
        let mut rows: Vec<RequestRow> = stmt
            .query_map(params_from_iter(f.params.iter()), row_from_sql)
            .map_err(sql_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_err)?;
        let truncated = rows.len() > limit as usize;
        rows.truncate(limit as usize);

        let tf = Self::filter(query, true, false);
        let totals = conn
            .query_row(
                &format!("SELECT {TOTALS} FROM requests{}", tf.sql()),
                params_from_iter(tf.params.iter()),
                |r| totals_from(r, 0),
            )
            .map_err(sql_err)?;

        let gf = Self::filter(query, false, false);
        let groups = match query.group_by {
            RequestGrouping::Provider => provider_groups(&conn, &gf)?,
            RequestGrouping::Harness => keyed_groups(&conn, &gf, HARNESS_KEY)?,
            RequestGrouping::HarnessModel => keyed_groups(&conn, &gf, HARNESS_MODEL_KEY)?,
            _ => keyed_groups(&conn, &gf, SESSION_KEY)?,
        };
        Ok(RequestPage {
            rows,
            totals,
            groups,
            truncated,
            recording: false,
        })
    }

    /// One request with timing and attempts.
    pub fn detail(&self, id: i64) -> Result<RequestDetail> {
        let not_found = || Error::NotFound {
            what: format!("request {id} in the request log"),
        };
        let Some(conn) = open_read_only(&self.paths.usage_db())? else {
            return Err(not_found());
        };
        let found = conn
            .query_row(
                &format!(
                    "SELECT {}, first_event_ms, attempts, log, sent_ms, \
                     last_delta_ms FROM requests WHERE id = ?1",
                    row_columns(&conn)
                ),
                [id],
                |r| {
                    let row = row_from_sql(r)?;
                    let n = row_column_count();
                    let first_event: Option<i64> = r.get(n)?;
                    let attempts: Option<String> = r.get(n + 1)?;
                    let log: Option<String> = r.get(n + 2)?;
                    let sent: Option<i64> = r.get(n + 3)?;
                    let last_delta: Option<i64> = r.get(n + 4)?;
                    Ok((row, [first_event, sent, last_delta], attempts, log))
                },
            )
            .optional()
            .map_err(sql_err)?;
        let Some((row, [first_event, sent, last_delta], attempts, log)) = found else {
            return Err(not_found());
        };
        Ok(RequestDetail {
            // The recorder stores the marks normalized. A WebSocket request has
            // no response headers of its own: its `ttfb_ms` is the first frame.
            timing: RequestTiming {
                sent_ms: u64_of(sent),
                headers_ms: row
                    .ttfb_ms
                    .filter(|_| row.transport != RequestTransport::WebSocket),
                first_event_ms: u64_of(first_event),
                first_token_ms: row.first_token_ms,
                last_delta_ms: u64_of(last_delta),
                end_ms: row.duration_ms,
            },
            attempts: attempts
                .and_then(|j| serde_json::from_str::<Vec<RequestAttempt>>(&j).ok())
                .unwrap_or_default(),
            log: log
                .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
                .unwrap_or_default(),
            row,
        })
    }

    /// Grouped aggregates.
    pub fn stats(&self, query: &StatsQuery) -> Result<RequestStats> {
        let Some(conn) = open_read_only(&self.paths.usage_db())? else {
            return Ok(RequestStats::default());
        };
        let f = Self::filter(
            &RequestQuery {
                from: query.from,
                to: query.to,
                member: query.member.clone(),
                model: query.model.clone(),
                harnesses: query.harnesses.clone(),
                host: query.host.clone(),
                protocol: query.protocol,
                ..RequestQuery::default()
            },
            false,
            false,
        );
        let offset_ms = i64::from(query.utc_offset_minutes) * 60_000;
        let day = format!("((started_ms + ({offset_ms})) / 86400000)");
        // For a provider the label is the newest row's name (SQLite takes the
        // bare column from the row of the single MAX()).
        let (key, label) = match query.by {
            StatsGroupBy::Provider => (
                format!("COALESCE({PROVIDER_KEY}, '')"),
                "member_name".to_owned(),
            ),
            StatsGroupBy::Harness => (HARNESS_KEY.to_owned(), "''".to_owned()),
            StatsGroupBy::HarnessModel => (HARNESS_MODEL_KEY.to_owned(), "''".to_owned()),
            StatsGroupBy::Host => (HOST_KEY.to_owned(), "''".to_owned()),
            StatsGroupBy::Day => (day.clone(), "''".to_owned()),
            StatsGroupBy::Session => (format!("COALESCE({SESSION_KEY}, '')"), "''".to_owned()),
            _ => (
                "COALESCE(upstream_model, model, response_model, '')".to_owned(),
                "''".to_owned(),
            ),
        };
        const BY_COST: &str = "COALESCE(SUM(cost_micros), 0) DESC, COUNT(*) DESC, 1 ASC";
        let bucket = |key: &str, label: &str, order: &str| -> Result<Vec<StatsBucket>> {
            let sql = format!(
                "SELECT {key}, {label}, MAX(started_ms), {TOTALS} FROM requests{} GROUP BY 1 \
                 ORDER BY {order}",
                f.sql()
            );
            let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
            let is_day = key.starts_with("((started_ms");
            stmt.query_map(params_from_iter(f.params.iter()), |r| {
                let k = if is_day {
                    day_label(r.get::<_, i64>(0)?)
                } else {
                    r.get::<_, String>(0)?
                };
                let l: String = r.get(1)?;
                Ok(StatsBucket {
                    label: if l.is_empty() { k.clone() } else { l },
                    key: k,
                    totals: totals_from(r, 3)?,
                    lines: Vec::new(),
                })
            })
            .map_err(sql_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql_err)
        };
        let mut buckets = bucket(
            &key,
            &label,
            if query.by == StatsGroupBy::Day {
                "1 ASC"
            } else {
                BY_COST
            },
        )?;
        if query.by == StatsGroupBy::Provider {
            let mut lf = f.clone();
            lf.raw(&format!("{LINE_LABEL} IS NOT NULL"));
            let sql = format!(
                "SELECT {key}, {LINE_LABEL}, {TOTALS} FROM requests{} GROUP BY 1, 2 \
                 ORDER BY COALESCE(SUM(cost_micros), 0) DESC, COUNT(*) DESC, 2 ASC",
                lf.sql()
            );
            let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
            let lines = stmt
                .query_map(params_from_iter(lf.params.iter()), |r| {
                    let k: String = r.get(0)?;
                    let l: String = r.get(1)?;
                    Ok((
                        k.clone(),
                        StatsBucket {
                            key: provider_line_key(&k, &l),
                            label: l,
                            totals: totals_from(r, 2)?,
                            lines: Vec::new(),
                        },
                    ))
                })
                .map_err(sql_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_err)?;
            for (provider, line) in lines {
                if let Some(b) = buckets.iter_mut().find(|b| b.key == provider) {
                    b.lines.push(line);
                }
            }
        }
        let daily = bucket(&day, "''", "1 ASC")?;
        let totals = conn
            .query_row(
                &format!("SELECT {TOTALS} FROM requests{}", f.sql()),
                params_from_iter(f.params.iter()),
                |r| totals_from(r, 0),
            )
            .map_err(sql_err)?;
        Ok(RequestStats {
            buckets,
            daily,
            totals,
        })
    }

    /// Distinct models, members and hosts recorded (filter menus).
    pub fn facets(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let mut out = BTreeMap::new();
        let Some(conn) = open_read_only(&self.paths.usage_db())? else {
            return Ok(out);
        };
        for (name, col) in [
            ("model", "model"),
            ("member", "member_id"),
            ("line", "line_id"),
            ("host", "upstream_host"),
        ] {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT DISTINCT {col} FROM requests WHERE {col} IS NOT NULL ORDER BY 1 LIMIT 500"
                ))
                .map_err(sql_err)?;
            let values = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(sql_err)?
                .collect::<rusqlite::Result<_>>()
                .map_err(sql_err)?;
            out.insert(name.to_owned(), values);
        }
        Ok(out)
    }

    /// Rows of the threads `threads` (thread id or prompt cache key) started in
    /// `[from, to)`, oldest first: what the Usage page matches Codex rollout rows
    /// against, so a request the router recorded is listed once.
    pub fn rows_for_threads(
        &self,
        from: Option<Timestamp>,
        to: Option<Timestamp>,
        threads: &[String],
    ) -> Result<Vec<RequestRow>> {
        let Some(conn) = open_read_only(&self.paths.usage_db())? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for chunk in threads.chunks(400) {
            let mut f = Filter::new();
            if let Some(from) = from {
                f.push("started_ms >= ?", SqlValue::Integer(from.as_millis()));
            }
            if let Some(to) = to {
                f.push("started_ms < ?", SqlValue::Integer(to.as_millis()));
            }
            let base = f.params.len();
            let marks: Vec<String> = (1..=chunk.len())
                .map(|i| format!("?{}", base + i))
                .collect();
            f.params.extend(chunk.iter().map(|t| text(t)));
            f.clauses.push(format!(
                "(thread_id IN ({0}) OR prompt_cache_key IN ({0}))",
                marks.join(", ")
            ));
            let sql = format!(
                "SELECT {} FROM requests{} ORDER BY started_ms ASC, id ASC LIMIT {}",
                row_columns(&conn),
                f.sql(),
                MAX_QUERY_ROWS * 10
            );
            let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
            let rows = stmt
                .query_map(params_from_iter(f.params.iter()), row_from_sql)
                .map_err(sql_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_err)?;
            out.extend(rows);
        }
        out.sort_by_key(|r| (r.started_at, r.id));
        Ok(out)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
