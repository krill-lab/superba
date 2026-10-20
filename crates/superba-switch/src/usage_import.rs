//! Usage history imported from cc-switch and AI Toolbox
//! ([`superba_core::SwitchService::import_usage`], GitHub issue 1).
//!
//! `superba-import` reads the tool's database (read-only); this module decides
//! what Superba adds and keeps it in its own `<data dir>/imported-usage.sqlite3`
//! (Superba's file; the tools' files are never written):
//!
//! - **Requests** keyed by (tool, the tool's `request_id`): a second import adds
//!   only what is new ("already imported" otherwise), so importing is
//!   idempotent; costs are kept as the tool recorded them (no cost when it
//!   recorded none), error texts are never copied.
//! - **Daily totals** of days the tool keeps only as totals (older than its
//!   detail retention), keyed by the tool's own row key; they count in the
//!   statistics, not in the request list.
//! - Rows the tool read from **Codex's own session files** are imported only
//!   for conversations Superba cannot read itself (their rollout files are
//!   gone), and session totals only for days before the oldest rollout
//!   Superba has; otherwise they are "covered" and skipped, so nothing is
//!   counted twice.
//!
//! The Usage page lists imported rows with their source ("Imported from …")
//! and can filter by it; `remove` takes a tool's rows out again.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use superba_core::{
    Error, ImportSource, ProfileId, RequestRow, RequestSource, RequestStatus, Result, Timestamp,
    UsageImportReport, UsageImportRequest, Usd,
};
use superba_import::usage::{ForeignDaily, ForeignRequest, UsageRead};
use superba_import::{Os, ScanEnv};

use crate::Switcher;

/// File name under the data dir.
pub const DB_FILE: &str = "imported-usage.sqlite3";
/// Rows of one query at most (newest first beyond it are left out).
pub const MAX_ROWS: usize = 200_000;
/// Ids of imported rows lie below this (rollout rows are `-2^62 ..= -1`,
/// router rows positive).
pub const ID_LIMIT: i64 = -(1 << 62);

/// The row id of an imported request (stable, below [`ID_LIMIT`]).
pub fn row_id(source: ImportSource, source_id: &str) -> i64 {
    let d = Sha256::digest(format!("{}\u{1f}{source_id}", source.id()).as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[..8]);
    let h = u64::from_le_bytes(b) & 0x3fff_ffff_ffff_ffff;
    i64::MIN + h as i64
}

/// The member id imported rows of `provider` are grouped under.
pub fn member_id(source: ImportSource, provider: &str) -> ProfileId {
    ProfileId::new(format!("import:{}:{provider}", source.id()))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS requests (
    source TEXT NOT NULL,
    source_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    model TEXT,
    input INTEGER NOT NULL,
    cached INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cost_micros INTEGER,
    status INTEGER,
    outcome TEXT,
    first_token_ms INTEGER,
    duration_ms INTEGER,
    thread_id TEXT,
    service_tier TEXT,
    from_sessions INTEGER NOT NULL,
    imported_at_ms INTEGER NOT NULL,
    PRIMARY KEY (source, source_id)
);
CREATE INDEX IF NOT EXISTS requests_started ON requests (started_ms);
CREATE INDEX IF NOT EXISTS requests_id ON requests (id);
CREATE TABLE IF NOT EXISTS daily (
    source TEXT NOT NULL,
    day TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model TEXT NOT NULL,
    variant TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    requests INTEGER NOT NULL,
    ok INTEGER NOT NULL,
    input INTEGER NOT NULL,
    cached INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cost_micros INTEGER,
    from_sessions INTEGER NOT NULL,
    imported_at_ms INTEGER NOT NULL,
    PRIMARY KEY (source, day, provider_id, model, variant)
);
";

/// One imported day's totals, as the statistics use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedDay {
    /// The tool.
    pub source: ImportSource,
    /// Local date (`YYYY-MM-DD`) in the tool's time zone.
    pub day: String,
    /// Member id ([`member_id`]).
    pub member_id: ProfileId,
    /// Provider name.
    pub member_name: String,
    /// Model.
    pub model: String,
    /// Requests.
    pub requests: u64,
    /// Successful requests.
    pub ok: u64,
    /// Input including cached and cache-write tokens.
    pub input: u64,
    /// Cache reads.
    pub cached: u64,
    /// Cache writes.
    pub cache_write: u64,
    /// Output.
    pub output: u64,
    /// Recorded cost.
    pub cost: Option<Usd>,
}

/// Superba's store of imported usage.
#[derive(Debug)]
pub struct ImportStore {
    conn: Connection,
}

fn sql_err(e: rusqlite::Error) -> Error {
    Error::other(format!("imported usage: {e}"))
}

fn source_of(id: &str) -> Option<ImportSource> {
    ImportSource::parse(id)
}

impl ImportStore {
    /// Open (and create) the store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
        }
        let conn = Connection::open(path).map_err(sql_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_err)?;
        conn.execute_batch(SCHEMA).map_err(sql_err)?;
        Ok(Self { conn })
    }

    /// Open the store at `path` for reading, `None` when it does not exist.
    pub fn open_existing(path: &Path) -> Result<Option<Self>> {
        if !path.is_file() {
            return Ok(None);
        }
        Self::open(path).map(Some)
    }

    fn has_request(&self, source: ImportSource, id: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM requests WHERE source = ?1 AND source_id = ?2",
                params![source.id(), id],
                |_| Ok(()),
            )
            .optional()
            .map(|r| r.is_some())
            .map_err(sql_err)
    }

    fn has_day(&self, source: ImportSource, d: &ForeignDaily) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM daily WHERE source = ?1 AND day = ?2 AND provider_id = ?3 \
                 AND model = ?4 AND variant = ?5",
                params![source.id(), d.day, d.provider_id, d.model, d.variant],
                |_| Ok(()),
            )
            .optional()
            .map(|r| r.is_some())
            .map_err(sql_err)
    }

    /// Add `requests` and `days` of `read` (already filtered); returns how many
    /// of each were new. With `dry_run` nothing is written.
    pub fn add(
        &mut self,
        read: &UsageRead,
        requests: &[&ForeignRequest],
        days: &[&ForeignDaily],
        now_ms: i64,
        dry_run: bool,
    ) -> Result<(Vec<bool>, Vec<bool>)> {
        let source = read.source;
        let mut new_requests = Vec::with_capacity(requests.len());
        let mut new_days = Vec::with_capacity(days.len());
        if dry_run {
            for r in requests {
                new_requests.push(!self.has_request(source, &r.source_id)?);
            }
            for d in days {
                new_days.push(!self.has_day(source, d)?);
            }
            return Ok((new_requests, new_days));
        }
        let tx = self.conn.transaction().map_err(sql_err)?;
        {
            let mut ins = tx
                .prepare(
                    "INSERT OR IGNORE INTO requests (source, source_id, id, started_ms, ended_ms, \
                     provider_id, provider_name, model, input, cached, cache_write, output, \
                     cost_micros, status, outcome, first_token_ms, duration_ms, thread_id, \
                     service_tier, from_sessions, imported_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
                     ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                )
                .map_err(sql_err)?;
            for r in requests {
                let n = ins
                    .execute(params![
                        source.id(),
                        r.source_id,
                        row_id(source, &r.source_id),
                        r.started_ms,
                        r.ended_ms,
                        r.provider_id,
                        read.provider_name(&r.provider_id),
                        r.model,
                        r.input as i64,
                        r.cached as i64,
                        r.cache_write as i64,
                        r.output as i64,
                        r.cost.map(Usd::micros),
                        r.status,
                        r.outcome,
                        r.first_token_ms.map(|v| v as i64),
                        r.duration_ms.map(|v| v as i64),
                        r.thread_id,
                        r.service_tier,
                        r.from_sessions,
                        now_ms,
                    ])
                    .map_err(sql_err)?;
                new_requests.push(n > 0);
            }
            let mut ins = tx
                .prepare(
                    "INSERT OR IGNORE INTO daily (source, day, provider_id, model, variant, \
                     provider_name, requests, ok, input, cached, cache_write, output, cost_micros, \
                     from_sessions, imported_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, \
                     ?10, ?11, ?12, ?13, ?14, ?15)",
                )
                .map_err(sql_err)?;
            for d in days {
                let n = ins
                    .execute(params![
                        source.id(),
                        d.day,
                        d.provider_id,
                        d.model,
                        d.variant,
                        read.provider_name(&d.provider_id),
                        d.requests as i64,
                        d.ok as i64,
                        d.input as i64,
                        d.cached as i64,
                        d.cache_write as i64,
                        d.output as i64,
                        d.cost.map(Usd::micros),
                        d.from_sessions,
                        now_ms,
                    ])
                    .map_err(sql_err)?;
                new_days.push(n > 0);
            }
        }
        tx.commit().map_err(sql_err)?;
        Ok((new_requests, new_days))
    }

    /// Remove every row of `source`: (requests, days).
    pub fn remove(&mut self, source: ImportSource) -> Result<(u64, u64)> {
        let tx = self.conn.transaction().map_err(sql_err)?;
        let r = tx
            .execute("DELETE FROM requests WHERE source = ?1", [source.id()])
            .map_err(sql_err)?;
        let d = tx
            .execute("DELETE FROM daily WHERE source = ?1", [source.id()])
            .map_err(sql_err)?;
        tx.commit().map_err(sql_err)?;
        Ok((r as u64, d as u64))
    }

    /// Imported requests started in `[from, to)`, newest first, at most
    /// [`MAX_ROWS`]; `true` when the cap was reached.
    pub fn rows(
        &self,
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    ) -> Result<(Vec<RequestRow>, bool)> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source, id, started_ms, ended_ms, provider_id, provider_name, model, input, \
                 cached, cache_write, output, cost_micros, status, outcome, first_token_ms, \
                 duration_ms, thread_id, service_tier FROM requests \
                 WHERE started_ms >= ?1 AND started_ms < ?2 ORDER BY started_ms DESC LIMIT ?3",
            )
            .map_err(sql_err)?;
        let lo = from.map_or(i64::MIN, Timestamp::as_millis);
        let hi = to.map_or(i64::MAX, Timestamp::as_millis);
        let mut out = Vec::new();
        let mut rows = stmt
            .query(params![lo, hi, (MAX_ROWS + 1) as i64])
            .map_err(sql_err)?;
        while let Some(r) = rows.next().map_err(sql_err)? {
            let Some(source) = source_of(&r.get::<_, String>(0).map_err(sql_err)?) else {
                continue;
            };
            out.push(request_row(source, r).map_err(sql_err)?);
        }
        let truncated = out.len() > MAX_ROWS;
        out.truncate(MAX_ROWS);
        Ok((out, truncated))
    }

    /// One imported request by its row id.
    pub fn row(&self, id: i64) -> Result<Option<RequestRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source, id, started_ms, ended_ms, provider_id, provider_name, model, input, \
                 cached, cache_write, output, cost_micros, status, outcome, first_token_ms, \
                 duration_ms, thread_id, service_tier FROM requests WHERE id = ?1",
            )
            .map_err(sql_err)?;
        let mut rows = stmt.query([id]).map_err(sql_err)?;
        match rows.next().map_err(sql_err)? {
            Some(r) => {
                let Some(source) = source_of(&r.get::<_, String>(0).map_err(sql_err)?) else {
                    return Ok(None);
                };
                request_row(source, r).map(Some).map_err(sql_err)
            }
            None => Ok(None),
        }
    }

    /// Every imported day (statistics filter them by range).
    pub fn days(&self) -> Result<Vec<ImportedDay>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source, day, provider_id, provider_name, model, requests, ok, input, \
                 cached, cache_write, output, cost_micros FROM daily ORDER BY day",
            )
            .map_err(sql_err)?;
        let mut out = Vec::new();
        let mut rows = stmt.query([]).map_err(sql_err)?;
        while let Some(r) = rows.next().map_err(sql_err)? {
            let Some(source) = source_of(&r.get::<_, String>(0).map_err(sql_err)?) else {
                continue;
            };
            let provider: String = r.get(2).map_err(sql_err)?;
            let u = |i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
            };
            out.push(ImportedDay {
                source,
                day: r.get(1).map_err(sql_err)?,
                member_id: member_id(source, &provider),
                member_name: r.get(3).map_err(sql_err)?,
                model: r.get(4).map_err(sql_err)?,
                requests: u(5).map_err(sql_err)?,
                ok: u(6).map_err(sql_err)?,
                input: u(7).map_err(sql_err)?,
                cached: u(8).map_err(sql_err)?,
                cache_write: u(9).map_err(sql_err)?,
                output: u(10).map_err(sql_err)?,
                cost: r
                    .get::<_, Option<i64>>(11)
                    .map_err(sql_err)?
                    .map(Usd::from_micros),
            });
        }
        Ok(out)
    }
}

fn request_row(source: ImportSource, r: &rusqlite::Row<'_>) -> rusqlite::Result<RequestRow> {
    let u =
        |i: usize| -> rusqlite::Result<u64> { Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0)) };
    let opt_u = |i: usize| -> rusqlite::Result<Option<u64>> {
        Ok(r.get::<_, Option<i64>>(i)?
            .and_then(|v| u64::try_from(v).ok()))
    };
    let provider: String = r.get(4)?;
    let status_code: Option<u16> = r
        .get::<_, Option<i64>>(12)?
        .and_then(|s| u16::try_from(s).ok());
    let outcome: Option<String> = r.get(13)?;
    let status = match outcome.as_deref() {
        Some("failed") => RequestStatus::Failed { code: None },
        Some("incomplete") => RequestStatus::Incomplete { reason: None },
        Some("canceled" | "cancelled") => RequestStatus::Cancelled,
        _ => match status_code {
            Some(s) if !(200..300).contains(&s) => RequestStatus::Http {
                status: s,
                code: None,
            },
            _ => RequestStatus::Ok,
        },
    };
    let started: i64 = r.get(2)?;
    let ended: i64 = r.get(3)?;
    Ok(RequestRow {
        id: r.get(1)?,
        started_at: Timestamp::from_millis(started),
        prompt_cache_key: None,
        thread_id: r.get(16)?,
        turn_id: None,
        member_id: member_id(source, &provider),
        member_name: r.get(5)?,
        line_id: None,
        line_name: None,
        upstream_host: None,
        failovers: 0,
        transport: Default::default(),
        originator: None,
        client: superba_core::RequestClient::from_originator(None),
        request_kind: None,
        thread_source: None,
        model: r.get(6)?,
        upstream_model: None,
        response_model: None,
        service_tier_requested: r.get(17)?,
        service_tier_answered: None,
        status,
        http_status: status_code,
        error: None,
        input_tokens: Some(u(7)?),
        cached_tokens: Some(u(8)?),
        cache_write_tokens: Some(u(9)?),
        output_tokens: Some(u(10)?),
        reasoning_tokens: None,
        ttfb_ms: None,
        first_token_ms: opt_u(14)?,
        duration_ms: opt_u(15)?.or_else(|| u64::try_from(ended - started).ok().filter(|d| *d > 0)),
        tps_milli: None,
        cost: r.get::<_, Option<i64>>(11)?.map(Usd::from_micros),
        price_rev: None,
        injected: false,
        stripped: 0,
        upstream_request_id: None,
        source: RequestSource::of_import(source),
        codex_host: None,
        harness: superba_core::RequestHarness::Codex,
        harness_version: None,
        response_id: None,
        protocol: superba_core::RequestProtocol::Responses,
        requested_model: None,
        session_id: None,
        agent_kind: None,
        attempts: Vec::new(),
    })
}

/// What Superba reads itself from Codex's rollouts: the threads it knows and
/// the start of its oldest row.
#[derive(Clone, Debug, Default)]
pub struct RolloutCoverage {
    /// Thread ids with rows in the local rollouts.
    pub threads: BTreeSet<String>,
    /// Start of the oldest rollout row.
    pub since: Option<Timestamp>,
}

/// The requests and days of `read` Superba adds, and how many rows (requests)
/// are covered by its own rollouts.
pub fn select<'a>(
    read: &'a UsageRead,
    coverage: &RolloutCoverage,
) -> (Vec<&'a ForeignRequest>, Vec<&'a ForeignDaily>, u64) {
    let mut covered = 0u64;
    let requests: Vec<&ForeignRequest> = read
        .requests
        .iter()
        .filter(|r| {
            let keep = !r.from_sessions
                || r.thread_id
                    .as_ref()
                    .is_some_and(|t| !coverage.threads.contains(t));
            if !keep {
                covered += 1;
            }
            keep
        })
        .collect();
    // Session totals only for days before Superba's oldest rollout row (a day
    // earlier, for time zones).
    let first_day = coverage
        .since
        .map(|t| superba_router::store::day_label(t.as_millis().div_euclid(86_400_000) - 1));
    let days: Vec<&ForeignDaily> = read
        .daily
        .iter()
        .filter(|d| {
            let keep = !d.from_sessions
                || first_day
                    .as_ref()
                    .is_none_or(|f| d.day.as_str() < f.as_str());
            if !keep {
                covered += d.requests;
            }
            keep
        })
        .collect();
    (requests, days, covered)
}

/// Run `f` on a blocking thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::other(format!("imported usage: {e}")))?
}

/// Remove (or with `dry_run` count) the rows of `source`.
fn remove(path: &Path, source: ImportSource, dry_run: bool) -> Result<UsageImportReport> {
    let mut report = UsageImportReport::new(source, dry_run);
    report.removed = true;
    let Some(mut store) = ImportStore::open_existing(path)? else {
        return Ok(report);
    };
    if dry_run {
        let (rows, _) = store.rows(None, None)?;
        report.requests = rows
            .iter()
            .filter(|r| r.source.imported_from() == Some(source))
            .count() as u64;
        report.days = store.days()?.iter().filter(|d| d.source == source).count() as u64;
    } else {
        let (r, d) = store.remove(source)?;
        report.requests = r;
        report.days = d;
    }
    Ok(report)
}

/// Select what `read` adds (see [`select`]), add it to the store at `path`
/// (with `dry_run` only count) and report.
pub fn apply(
    path: &Path,
    read: &UsageRead,
    coverage: &RolloutCoverage,
    now_ms: i64,
    dry_run: bool,
) -> Result<UsageImportReport> {
    let mut report = UsageImportReport::new(read.source, dry_run);
    report.stores = read.stores.clone();
    report.notes = read.notes.clone();
    report.other_apps = read.other_apps;
    if read.requests.is_empty() && read.daily.is_empty() {
        return Ok(report);
    }
    let (requests, days, covered) = select(read, coverage);
    report.covered = covered;
    let store = if dry_run {
        ImportStore::open_existing(path)?
    } else {
        Some(ImportStore::open(path)?)
    };
    let (new_requests, new_days) = match store {
        Some(mut store) => store.add(read, &requests, &days, now_ms, dry_run)?,
        None => (vec![true; requests.len()], vec![true; days.len()]),
    };
    let mut first: Option<i64> = None;
    let mut last: Option<i64> = None;
    let mut widen = |lo: i64, hi: i64| {
        first = Some(first.map_or(lo, |f| f.min(lo)));
        last = Some(last.map_or(hi, |l| l.max(hi)));
    };
    let mut cost = Usd::ZERO;
    for (r, new) in requests.iter().zip(&new_requests) {
        if !*new {
            report.already += 1;
            continue;
        }
        report.requests += 1;
        widen(r.started_ms, r.ended_ms);
        match r.cost {
            Some(c) => cost = cost.saturating_add(c),
            None => report.unpriced += 1,
        }
    }
    for (d, new) in days.iter().zip(&new_days) {
        if !*new {
            report.already += 1;
            continue;
        }
        report.days += 1;
        report.day_requests += d.requests;
        if let Some(start) = day_start_ms(&d.day, 0) {
            widen(start, start + 86_399_999);
        }
        match d.cost {
            Some(c) => cost = cost.saturating_add(c),
            None => report.unpriced += 1,
        }
    }
    report.cost = cost;
    report.first = first.map(Timestamp::from_millis);
    report.last = last.map(Timestamp::from_millis);
    Ok(report)
}

/// Unix ms of the local midnight of `day` (`YYYY-MM-DD`) at `offset_ms`.
pub fn day_start_ms(day: &str, offset_ms: i64) -> Option<i64> {
    let mut parts = day.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days since 1970-01-01 (civil-from-days inverse, Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400_000 - offset_ms)
}

impl Switcher {
    pub(crate) fn imported_usage_path(&self) -> PathBuf {
        self.deps.ctx.data_dir.root().join(DB_FILE)
    }

    /// What the local rollouts cover (a full read of the index: an explicit
    /// import only).
    async fn rollout_coverage(&self) -> RolloutCoverage {
        let index = self.usage.clone();
        tokio::task::spawn_blocking(move || {
            let page = index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .requests(None, None);
            RolloutCoverage {
                threads: page.rows.iter().map(|r| r.thread_id.clone()).collect(),
                since: page.rows.iter().map(|r| r.started_at).min(),
            }
        })
        .await
        .unwrap_or_default()
    }

    pub(crate) async fn import_usage_impl(
        &self,
        request: UsageImportRequest,
    ) -> Result<UsageImportReport> {
        if !request.dry_run {
            self.refuse_elevated()?;
        }
        let source = request.source;
        let path = self.imported_usage_path();
        if request.remove {
            return blocking(move || remove(&path, source, request.dry_run)).await;
        }
        let home = self.deps.ctx.home_dir.clone();
        let env = self.deps.ctx.env.clone();
        let scratch = self.deps.ctx.data_dir.cache_dir().join("import");
        let read = blocking(move || {
            let scan = ScanEnv {
                home: &home,
                env: env.as_ref(),
                os: Os::current(),
                scratch: &scratch,
            };
            Ok(superba_import::usage::read_usage(source, &scan))
        })
        .await?;
        let coverage = if read.requests.is_empty() && read.daily.is_empty() {
            RolloutCoverage::default()
        } else {
            self.rollout_coverage().await
        };
        let now_ms = self.deps.clock.now().as_millis();
        blocking(move || apply(&path, &read, &coverage, now_ms, request.dry_run)).await
    }

    /// Imported requests started in `[from, to)` (none without the store).
    pub(crate) async fn imported_rows(
        &self,
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    ) -> Result<(Vec<RequestRow>, bool)> {
        let path = self.imported_usage_path();
        blocking(move || match ImportStore::open_existing(&path)? {
            Some(store) => store.rows(from, to),
            None => Ok((Vec::new(), false)),
        })
        .await
    }

    /// Every imported day (none without the store).
    pub(crate) async fn imported_days(&self) -> Result<Vec<ImportedDay>> {
        let path = self.imported_usage_path();
        blocking(move || match ImportStore::open_existing(&path)? {
            Some(store) => store.days(),
            None => Ok(Vec::new()),
        })
        .await
    }

    /// The detail of an imported row.
    pub(crate) async fn imported_detail(&self, id: i64) -> Result<superba_core::RequestDetail> {
        let path = self.imported_usage_path();
        let row = blocking(move || match ImportStore::open_existing(&path)? {
            Some(store) => store.row(id),
            None => Ok(None),
        })
        .await?;
        let Some(row) = row else {
            return Err(Error::NotFound {
                what: format!("request {id}"),
            });
        };
        Ok(superba_core::RequestDetail {
            timing: superba_core::RequestTiming {
                end_ms: row.duration_ms,
                first_token_ms: row.first_token_ms,
                ..superba_core::RequestTiming::default()
            },
            row,
            attempts: Vec::new(),
            log: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_below_rollout_ids_and_differ_per_tool() {
        let a = row_id(ImportSource::Ccs, "req-1");
        assert_eq!(a, row_id(ImportSource::Ccs, "req-1"));
        assert_ne!(a, row_id(ImportSource::AiToolbox, "req-1"));
        assert!(a < ID_LIMIT);
        assert!(row_id(ImportSource::AiToolbox, "x") < ID_LIMIT);
    }

    #[test]
    fn day_starts_follow_the_calendar() {
        assert_eq!(day_start_ms("1970-01-01", 0), Some(0));
        assert_eq!(day_start_ms("2026-10-18", 0), Some(1_792_281_600_000));
        assert_eq!(day_start_ms("2024-02-29", 0), Some(1_709_164_800_000));
        assert_eq!(
            day_start_ms("2026-10-18", 8 * 3_600_000),
            Some(1_792_281_600_000 - 8 * 3_600_000)
        );
        assert_eq!(day_start_ms("2026-13-01", 0), None);
        assert_eq!(day_start_ms("nope", 0), None);
    }
}
