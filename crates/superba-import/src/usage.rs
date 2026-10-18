//! Usage history of cc-switch and AI Toolbox (GitHub issue 1), read-only.
//!
//! Both tools keep it in the same two tables of their SQLite database (AI
//! Toolbox's gateway started from cc-switch's design):
//!
//! - `proxy_request_logs`: one row per request with a stable `request_id`,
//!   `app_type` (`codex`, `claude`, `gemini`, …), `provider_id`, `model`, token
//!   counts, the cost the tool computed (`total_cost_usd`, a decimal string),
//!   `status_code`, timings, `session_id` and `created_at` (Unix seconds when
//!   the request ended). `data_source` says where the row came from: `proxy`
//!   (the tool's own local proxy) or the tool's reading of Codex's session
//!   files (cc-switch `codex_session`, provider `_codex_session`; AI Toolbox
//!   `session`, provider `session`).
//! - `usage_daily_rollups`: per local day, app, provider and model totals of
//!   the detail rows the tool pruned (older than its retention).
//!
//! Token semantics differ: cc-switch writes `input_token_semantics` (0 legacy
//! = input includes cache reads, 1 = includes reads and writes, 2 = fresh
//! input only); AI Toolbox stores fresh input (cache reads and writes
//! separate). Rows are normalised to OpenAI semantics (input includes cached
//! and cache-write tokens), as Superba's own rows are.
//!
//! Columns are detected per database (`PRAGMA table_info`), so every schema
//! version with these tables reads the same way; only Codex rows are
//! returned, others are counted. Nothing is written to the tool's files (see
//! [`crate::sqlite`]). Prompts and error texts are never read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use superba_core::{ImportSource, ImportStore, LocalizedText, StoreStatus, Usd};

use crate::{ScanEnv, aitoolbox, ccs, sqlite, text};

/// Request table of both tools.
pub const REQUESTS_TABLE: &str = "proxy_request_logs";
/// Daily totals table of both tools.
pub const DAILY_TABLE: &str = "usage_daily_rollups";
/// `app_type` of Codex rows.
pub const CODEX_APP: &str = "codex";

/// One request of the other tool's history, normalised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignRequest {
    /// The tool's `request_id` (stable; the import key).
    pub source_id: String,
    /// When it started (end minus its duration), Unix ms.
    pub started_ms: i64,
    /// When it ended (`created_at`), Unix ms.
    pub ended_ms: i64,
    /// The tool's provider id.
    pub provider_id: String,
    /// Model as recorded (the upstream model, else the requested one).
    pub model: Option<String>,
    /// Input tokens including cached and cache-write tokens.
    pub input: u64,
    /// Cache reads.
    pub cached: u64,
    /// Cache writes.
    pub cache_write: u64,
    /// Output tokens.
    pub output: u64,
    /// The cost the tool recorded (`None`: not recorded, or zero for a row
    /// with tokens, which the tool writes when it had no price).
    pub cost: Option<Usd>,
    /// HTTP status (`None`: not recorded).
    pub status: Option<u16>,
    /// How a stream ended (`completed`, `incomplete`, `failed`, `canceled`).
    pub outcome: Option<String>,
    /// Time to the first token.
    pub first_token_ms: Option<u64>,
    /// Duration.
    pub duration_ms: Option<u64>,
    /// The session (Codex thread) id, when recorded.
    pub thread_id: Option<String>,
    /// Requested service tier, when recorded.
    pub service_tier: Option<String>,
    /// Read by the tool from Codex's own session files (not its proxy).
    pub from_sessions: bool,
}

/// One day's totals of the other tool (requests it pruned).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignDaily {
    /// The tool's local date (`YYYY-MM-DD`).
    pub day: String,
    /// The tool's provider id.
    pub provider_id: String,
    /// Model.
    pub model: String,
    /// The rest of the tool's key for the row (request model, pricing model,
    /// project), so every row has a stable id.
    pub variant: String,
    /// Requests.
    pub requests: u64,
    /// Successful requests.
    pub ok: u64,
    /// Input tokens including cached and cache-write tokens.
    pub input: u64,
    /// Cache reads.
    pub cached: u64,
    /// Cache writes.
    pub cache_write: u64,
    /// Output.
    pub output: u64,
    /// Cost as recorded (`None`: zero with tokens).
    pub cost: Option<Usd>,
    /// Totals of rows read from Codex's own session files.
    pub from_sessions: bool,
}

/// What reading one tool's usage produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageRead {
    /// The tool.
    pub source: ImportSource,
    /// The database looked at.
    pub stores: Vec<ImportStore>,
    /// Codex requests, oldest first.
    pub requests: Vec<ForeignRequest>,
    /// Codex daily totals, oldest first.
    pub daily: Vec<ForeignDaily>,
    /// Provider names by the tool's provider id.
    pub provider_names: BTreeMap<String, String>,
    /// Rows (requests and days) of other coding tools.
    pub other_apps: u64,
    /// Remarks.
    pub notes: Vec<LocalizedText>,
}

impl UsageRead {
    fn empty(source: ImportSource) -> Self {
        Self {
            source,
            stores: Vec::new(),
            requests: Vec::new(),
            daily: Vec::new(),
            provider_names: BTreeMap::new(),
            other_apps: 0,
            notes: Vec::new(),
        }
    }

    /// The display name of a provider id (its name in the tool, else the id;
    /// the tools' session readers get a readable name).
    pub fn provider_name(&self, id: &str) -> String {
        if let Some(n) = self.provider_names.get(id) {
            return n.clone();
        }
        match id {
            "_codex_session" | "session" => "Codex sessions".to_owned(),
            "" | "unknown" => "Unknown provider".to_owned(),
            other => other.to_owned(),
        }
    }
}

/// The tool's database path.
pub fn database(source: ImportSource, env: &ScanEnv<'_>) -> PathBuf {
    match source {
        ImportSource::Ccs => ccs::data_dir(env).join("cc-switch.db"),
        _ => aitoolbox::data_dir(env).0.join(aitoolbox::DB_FILE),
    }
}

fn tool(source: ImportSource) -> &'static str {
    source.name()
}

/// Read the usage history of `source` (blocking file I/O).
pub fn read_usage(source: ImportSource, env: &ScanEnv<'_>) -> UsageRead {
    let db = database(source, env);
    let mut out = UsageRead::empty(source);
    let label = format!("{} database", tool(source));
    if !db.is_file() {
        out.stores
            .push(env.store(&db, label, StoreStatus::Missing, None));
        return out;
    }
    let snapshot = match sqlite::open(&db, env.scratch) {
        Ok(s) => s,
        Err(e) => {
            out.stores.push(
                env.store(
                    &db,
                    label,
                    StoreStatus::Skipped,
                    Some(
                        text(
                            "import.store.unreadable",
                            "It could not be read: {error}. Close {tool} and try again.",
                        )
                        .arg("error", e)
                        .arg("tool", tool(source)),
                    ),
                ),
            );
            return out;
        }
    };
    match read_conn(source, snapshot.conn(), &mut out) {
        Ok(format) => {
            let has_any = !out.requests.is_empty() || !out.daily.is_empty();
            let note = (!has_any && out.other_apps == 0)
                .then(|| text("import.usage.none", "It holds no usage history for Codex."));
            out.stores
                .push(env.store(&db, format, StoreStatus::Read, note));
        }
        Err(e) => out.stores.push(
            env.store(
                &db,
                label,
                StoreStatus::Skipped,
                Some(
                    text(
                        "import.store.unreadable",
                        "It could not be read: {error}. Close {tool} and try again.",
                    )
                    .arg("error", e)
                    .arg("tool", tool(source)),
                ),
            ),
        ),
    }
    out
}

/// Read from an open database; returns the store's format text.
pub fn read_conn(
    source: ImportSource,
    conn: &Connection,
    out: &mut UsageRead,
) -> rusqlite::Result<String> {
    let version = sqlite::user_version(conn)?;
    let format = format!("{} database, schema {version}", tool(source));
    out.provider_names = provider_names(source, conn).unwrap_or_default();
    if sqlite::has_table(conn, REQUESTS_TABLE)? {
        read_requests(source, conn, out)?;
    }
    if sqlite::has_table(conn, DAILY_TABLE)? {
        read_daily(source, conn, out)?;
    }
    Ok(format)
}

/// Codex provider names of the tool.
fn provider_names(
    source: ImportSource,
    conn: &Connection,
) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut names = BTreeMap::new();
    match source {
        ImportSource::Ccs => {
            if !sqlite::has_table(conn, "providers")? {
                return Ok(names);
            }
            let cols = sqlite::columns(conn, "providers")?;
            let filter = if cols.iter().any(|c| c == "app_type") {
                "WHERE app_type = 'codex'"
            } else {
                ""
            };
            let mut stmt = conn.prepare(&format!("SELECT id, name FROM providers {filter}"))?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                let (id, name) = row?;
                if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
                    names.insert(id, n);
                }
            }
        }
        _ => {
            if !sqlite::has_table(conn, "codex_provider")? {
                return Ok(names);
            }
            for (id, data) in aitoolbox::rows(conn, "codex_provider")? {
                if let Some(n) = data
                    .as_ref()
                    .and_then(|d| d.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|n| !n.trim().is_empty())
                {
                    names.insert(id, n.to_owned());
                }
            }
        }
    }
    Ok(names)
}

/// `col` when the table has it, else `fallback` (a literal).
fn col(cols: &[String], name: &str, fallback: &str) -> String {
    if cols.iter().any(|c| c == name) {
        format!("\"{name}\"")
    } else {
        fallback.to_owned()
    }
}

/// A cost text as recorded (`"0.0123"`, rarely `"1.2e-5"`).
pub fn parse_cost(text: Option<&str>) -> Option<Usd> {
    let t = text?.trim();
    if t.is_empty() {
        return None;
    }
    Usd::parse_dollars(t).or_else(|| t.parse::<f64>().ok().and_then(Usd::from_dollars))
}

/// A recorded cost, or `None` when it is zero for a row with tokens (the
/// tools write `0` when they had no price).
fn recorded(cost: Option<Usd>, tokens: u64) -> Option<Usd> {
    cost.filter(|c| !(c.is_zero() && tokens > 0) && c.micros() >= 0)
}

fn non_negative(v: Option<i64>) -> u64 {
    v.and_then(|n| u64::try_from(n).ok()).unwrap_or(0)
}

/// Input including cache reads and writes, from the tool's semantics.
/// `semantics`: cc-switch's `input_token_semantics` (`None` = AI Toolbox,
/// whose input is fresh).
pub fn total_input(
    source: ImportSource,
    semantics: Option<i64>,
    input: u64,
    cached: u64,
    cache_write: u64,
) -> u64 {
    match (source, semantics) {
        // cc-switch: 1 = input includes reads and writes.
        (ImportSource::Ccs, Some(1)) => input,
        // cc-switch 0 (legacy, Codex): input includes cache reads.
        (ImportSource::Ccs, Some(0) | None) => input.saturating_add(cache_write),
        // cc-switch 2 and AI Toolbox: fresh input only.
        _ => input.saturating_add(cached).saturating_add(cache_write),
    }
}

fn read_requests(
    source: ImportSource,
    conn: &Connection,
    out: &mut UsageRead,
) -> rusqlite::Result<()> {
    let cols = sqlite::columns(conn, REQUESTS_TABLE)?;
    let has = |c: &str| cols.iter().any(|x| x == c);
    if !["request_id", "app_type", "created_at"]
        .iter()
        .all(|c| has(c))
    {
        out.notes.push(text(
            "import.usage.unknown_layout",
            "Its request table has an unknown layout; requests were not read.",
        ));
        return Ok(());
    }
    let sql = format!(
        "SELECT request_id, app_type, {provider}, {model}, {request_model}, {input}, {output}, \
         {cache_read}, {cache_creation}, {semantics}, {cost}, {latency}, {first_token}, \
         {duration}, {status}, {session}, created_at, {data_source}, {request_count}, \
         {outcome}, {tier} FROM {REQUESTS_TABLE} ORDER BY created_at, request_id",
        provider = col(&cols, "provider_id", "''"),
        model = col(&cols, "model", "NULL"),
        request_model = col(&cols, "request_model", "NULL"),
        input = col(&cols, "input_tokens", "0"),
        output = col(&cols, "output_tokens", "0"),
        cache_read = col(&cols, "cache_read_tokens", "0"),
        cache_creation = col(&cols, "cache_creation_tokens", "0"),
        semantics = col(&cols, "input_token_semantics", "NULL"),
        cost = col(&cols, "total_cost_usd", "NULL"),
        latency = col(&cols, "latency_ms", "NULL"),
        first_token = col(&cols, "first_token_ms", "NULL"),
        duration = col(&cols, "duration_ms", "NULL"),
        status = col(&cols, "status_code", "NULL"),
        session = col(&cols, "session_id", "NULL"),
        data_source = col(&cols, "data_source", "'proxy'"),
        request_count = col(&cols, "usage_request_count", "1"),
        outcome = col(&cols, "stream_outcome", "NULL"),
        tier = col(&cols, "service_tier", "NULL"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let app: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
        if app != CODEX_APP {
            out.other_apps += 1;
            continue;
        }
        // AI Toolbox records non-model requests (model lists, ...) with a
        // request count of 0.
        if r.get::<_, Option<i64>>(18)?.unwrap_or(1) == 0 {
            continue;
        }
        let Some(source_id) = r.get::<_, Option<String>>(0)?.filter(|s| !s.is_empty()) else {
            continue;
        };
        let text_of = |i: usize| -> rusqlite::Result<Option<String>> {
            Ok(r.get::<_, Option<String>>(i)?
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty() && s != "unknown"))
        };
        let model = text_of(3)?.or(text_of(4)?);
        let fresh_or_total = non_negative(r.get(5)?);
        let output = non_negative(r.get(6)?);
        let cached = non_negative(r.get(7)?);
        let cache_write = non_negative(r.get(8)?);
        let semantics: Option<i64> = match source {
            ImportSource::Ccs => r.get(9)?,
            _ => None,
        };
        let input = total_input(source, semantics, fresh_or_total, cached, cache_write);
        let cost_text: Option<String> = r.get(10).or_else(|_| {
            r.get::<_, Option<f64>>(10)
                .map(|f| f.map(|v| v.to_string()))
        })?;
        let cost = recorded(parse_cost(cost_text.as_deref()), input + output);
        let latency = r
            .get::<_, Option<i64>>(11)?
            .and_then(|n| u64::try_from(n).ok());
        let first_token = r
            .get::<_, Option<i64>>(12)?
            .and_then(|n| u64::try_from(n).ok());
        let duration = r
            .get::<_, Option<i64>>(13)?
            .and_then(|n| u64::try_from(n).ok())
            .or(latency)
            .filter(|d| *d > 0);
        let status = r
            .get::<_, Option<i64>>(14)?
            .and_then(|n| u16::try_from(n).ok())
            .filter(|s| *s > 0);
        let thread_id = text_of(15)?;
        let created: i64 = r.get::<_, Option<i64>>(16)?.unwrap_or(0);
        // Seconds (both tools); a millisecond value is taken as it is.
        let ended_ms = if created > 100_000_000_000 {
            created
        } else {
            created.saturating_mul(1000)
        };
        let data_source = text_of(17)?.unwrap_or_else(|| "proxy".to_owned());
        let provider_id = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        out.requests.push(ForeignRequest {
            source_id,
            started_ms: ended_ms.saturating_sub(duration.map_or(0, |d| d as i64)),
            ended_ms,
            from_sessions: data_source != "proxy",
            provider_id,
            model,
            input,
            cached,
            cache_write,
            output,
            cost,
            status,
            outcome: text_of(19)?,
            first_token_ms: first_token,
            duration_ms: duration,
            thread_id,
            service_tier: text_of(20)?,
        });
    }
    Ok(())
}

fn read_daily(
    source: ImportSource,
    conn: &Connection,
    out: &mut UsageRead,
) -> rusqlite::Result<()> {
    let cols = sqlite::columns(conn, DAILY_TABLE)?;
    let has = |c: &str| cols.iter().any(|x| x == c);
    if !["date", "app_type", "request_count"].iter().all(|c| has(c)) {
        return Ok(());
    }
    // Every key column besides date / app / provider / model.
    let variant_cols: Vec<&str> = ["request_model", "pricing_model", "project_path"]
        .into_iter()
        .filter(|c| has(c))
        .collect();
    let variant = if variant_cols.is_empty() {
        "''".to_owned()
    } else {
        variant_cols
            .iter()
            .map(|c| format!("COALESCE(\"{c}\", '')"))
            .collect::<Vec<_>>()
            .join(" || char(31) || ")
    };
    let sql = format!(
        "SELECT date, app_type, {provider}, {model}, {variant}, request_count, {ok}, {input}, \
         {output}, {cache_read}, {cache_creation}, {semantics}, {cost} FROM {DAILY_TABLE} \
         ORDER BY date",
        provider = col(&cols, "provider_id", "''"),
        model = col(&cols, "model", "''"),
        ok = col(&cols, "success_count", "request_count"),
        input = col(&cols, "input_tokens", "0"),
        output = col(&cols, "output_tokens", "0"),
        cache_read = col(&cols, "cache_read_tokens", "0"),
        cache_creation = col(&cols, "cache_creation_tokens", "0"),
        semantics = col(&cols, "input_token_semantics", "NULL"),
        cost = col(&cols, "total_cost_usd", "NULL"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let app: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
        if app != CODEX_APP {
            out.other_apps += 1;
            continue;
        }
        let requests = non_negative(r.get(5)?);
        if requests == 0 {
            continue;
        }
        let provider_id: String = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        let fresh_or_total = non_negative(r.get(7)?);
        let output = non_negative(r.get(8)?);
        let cached = non_negative(r.get(9)?);
        let cache_write = non_negative(r.get(10)?);
        let semantics: Option<i64> = match source {
            ImportSource::Ccs => r.get(11)?,
            _ => None,
        };
        let input = total_input(source, semantics, fresh_or_total, cached, cache_write);
        let cost_text: Option<String> = r.get(12).or_else(|_| {
            r.get::<_, Option<f64>>(12)
                .map(|f| f.map(|v| v.to_string()))
        })?;
        out.daily.push(ForeignDaily {
            day: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            from_sessions: matches!(provider_id.as_str(), "_codex_session" | "session"),
            provider_id,
            model: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            variant: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            requests,
            ok: non_negative(r.get(6)?).min(requests),
            input,
            cached,
            cache_write,
            output,
            cost: recorded(parse_cost(cost_text.as_deref()), input + output),
        });
    }
    Ok(())
}

/// Read a database file directly (tests, the fixture homes).
pub fn read_file(source: ImportSource, path: &Path) -> rusqlite::Result<UsageRead> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut out = UsageRead::empty(source);
    read_conn(source, &conn, &mut out)?;
    Ok(out)
}
