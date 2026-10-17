//! Shared statistics state of a running router: the recorder, the live ring of
//! recent rows, requests in flight, the per-turn totals of the stats line, and
//! the per-request [`Tracker`] that turns one proxied request into a row.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::HeaderMap;
use superba_core::{
    ClearReport, ClearRequest, InflightPhase, InflightRequest, ProfileId, RequestAttempt,
    RequestKind, RequestRow, RequestStatus, RequestTiming, RequestTransport, Result,
    RouterStatsStatus, Timestamp, Usd,
};

use super::meta::RequestMeta;
use super::store::{Recorder, Retention, StoredRow};
use super::tap::{LineSetup, Outcome, TapSummary, TurnSoFar, line_allowed};
use super::{StatsConfig, price};
use crate::paths::RouterPaths;
use crate::select::Candidate;
use crate::state::{Generation, lock, unix_ms};

/// Rows kept for the admin API's live view.
pub(crate) const LIVE_RING: usize = 256;
/// Turns kept for the stats line.
const TURN_CAPACITY: usize = 4096;
/// Idle time after which a turn's totals are forgotten.
const TURN_TTL: Duration = Duration::from_secs(3600);

/// Totals of one turn so far.
#[derive(Clone, Copy, Debug)]
struct TurnAgg {
    so_far: TurnSoFar,
    last_seen: Instant,
}

/// One request in flight.
#[derive(Debug)]
pub(crate) struct Inflight {
    id: u64,
    started_ms: i64,
    t0: Instant,
    info: Mutex<InflightInfo>,
    phase: AtomicU8,
    deltas: AtomicU64,
    /// Ms after start of the first output, as the response tap measured it
    /// (`u64::MAX` = none yet).
    first_token_ms: AtomicU64,
}

#[derive(Clone, Debug, Default)]
struct InflightInfo {
    member_id: String,
    member_name: String,
    line_name: Option<String>,
    model: Option<String>,
    prompt_cache_key: Option<String>,
    client: superba_core::RequestClient,
    transport: superba_core::RequestTransport,
}

impl Inflight {
    /// Update the live numbers; `first_token` is the tap's `t0` → first output
    /// (the same value the finished row gets).
    pub fn update(&self, phase: InflightPhase, deltas: u64, first_token: Option<Duration>) {
        let p = match phase {
            InflightPhase::Streaming => 1,
            InflightPhase::Holding => 2,
            _ => 0,
        };
        self.phase.store(p, Ordering::Relaxed);
        self.deltas.store(deltas, Ordering::Relaxed);
        if let Some(d) = first_token {
            let _ = self.first_token_ms.compare_exchange(
                u64::MAX,
                ms(d).min(u64::MAX - 1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    fn view(&self) -> InflightRequest {
        let info = lock(&self.info).clone();
        InflightRequest {
            id: self.id,
            started_at: Timestamp::from_millis(self.started_ms),
            member_id: ProfileId::new(info.member_id),
            member_name: info.member_name,
            line_name: info.line_name,
            model: info.model,
            prompt_cache_key: info.prompt_cache_key,
            client: info.client,
            transport: info.transport,
            phase: match self.phase.load(Ordering::Relaxed) {
                1 => InflightPhase::Streaming,
                2 => InflightPhase::Holding,
                _ => InflightPhase::Waiting,
            },
            output_deltas: self.deltas.load(Ordering::Relaxed),
            first_token_ms: Some(self.first_token_ms.load(Ordering::Relaxed))
                .filter(|ms| *ms != u64::MAX),
            elapsed_ms: u64::try_from(self.t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// Statistics state shared by every request (survives reloads).
pub(crate) struct StatsHub {
    paths: Option<RouterPaths>,
    next_row: AtomicI64,
    next_inflight: AtomicU64,
    turns: Mutex<HashMap<String, TurnAgg>>,
    live: Mutex<VecDeque<RequestRow>>,
    inflight: Mutex<BTreeMap<u64, Arc<Inflight>>>,
    recorder: Mutex<Option<Recorder>>,
    written: Arc<AtomicU64>,
    dropped: AtomicU64,
}

impl std::fmt::Debug for StatsHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StatsHub")
    }
}

impl StatsHub {
    /// A hub recording under `paths` (none: live view only).
    pub fn new(paths: Option<RouterPaths>) -> Self {
        Self {
            paths,
            next_row: AtomicI64::new(0),
            next_inflight: AtomicU64::new(0),
            turns: Mutex::new(HashMap::new()),
            live: Mutex::new(VecDeque::new()),
            inflight: Mutex::new(BTreeMap::new()),
            recorder: Mutex::new(None),
            written: Arc::new(AtomicU64::new(0)),
            dropped: AtomicU64::new(0),
        }
    }

    /// Apply a (re)loaded configuration: start the recorder when recording is
    /// on and a data dir exists, or pass it the new retention. Fails when the
    /// request log cannot be opened (nothing is recorded then).
    pub fn apply(&self, cfg: Option<&StatsConfig>) -> superba_core::Result<()> {
        let Some(cfg) = cfg else { return Ok(()) };
        let retention = Retention {
            usage_days: cfg.usage_days,
            max_rows: cfg.max_rows,
        };
        let mut rec = lock(&self.recorder);
        if let Some(r) = rec.as_ref() {
            r.set_retention(retention);
            return Ok(());
        }
        if !cfg.record {
            return Ok(());
        }
        let Some(paths) = &self.paths else {
            return Ok(());
        };
        let (r, max_id) = Recorder::start(paths, retention, self.written.clone())?;
        self.next_row.fetch_max(max_id, Ordering::SeqCst);
        *rec = Some(r);
        Ok(())
    }

    /// Stop the recorder after writing what is queued.
    pub fn shutdown(&self) {
        let taken = lock(&self.recorder).take();
        if let Some(mut r) = taken {
            r.shutdown();
        }
    }

    /// Recording state for the admin status.
    pub fn status(&self, cfg: Option<&StatsConfig>) -> Option<RouterStatsStatus> {
        let cfg = cfg?;
        let db_bytes = self.paths.as_ref().and_then(|p| {
            let main = std::fs::metadata(p.usage_db()).ok()?.len();
            let wal = std::fs::metadata(p.usage_db().with_extension("sqlite3-wal"))
                .map(|m| m.len())
                .unwrap_or(0);
            Some(main + wal)
        });
        Some(RouterStatsStatus {
            recording: cfg.record && lock(&self.recorder).is_some(),
            rows_written: self.written.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            db_bytes,
        })
    }

    /// Rows of the live ring with an id above `after_id`, oldest first, at most
    /// `limit` (the newest ones).
    pub fn live_rows(&self, after_id: Option<i64>, limit: usize) -> Vec<RequestRow> {
        let ring = lock(&self.live);
        let rows: Vec<RequestRow> = ring
            .iter()
            .filter(|r| after_id.is_none_or(|a| r.id > a))
            .cloned()
            .collect();
        let skip = rows.len().saturating_sub(limit.max(1));
        rows.into_iter().skip(skip).collect()
    }

    /// Requests in flight, oldest first.
    pub fn inflight(&self) -> Vec<InflightRequest> {
        lock(&self.inflight).values().map(|i| i.view()).collect()
    }

    /// Delete recorded rows (through the writer when it runs).
    pub fn clear(&self, request: &ClearRequest) -> Result<ClearReport> {
        if let Some(r) = lock(&self.recorder).as_ref() {
            let report = r.clear(request.clone())?;
            self.clear_live(request);
            return Ok(report);
        }
        let report = match &self.paths {
            Some(p) => crate::store::clear_files(p, request)?,
            None => ClearReport::default(),
        };
        self.clear_live(request);
        Ok(report)
    }

    fn clear_live(&self, request: &ClearRequest) {
        let before = request.before.map(Timestamp::as_millis).unwrap_or(i64::MAX);
        lock(&self.live).retain(|r| r.started_at.as_millis() >= before);
    }

    fn turn_so_far(&self, turn_id: Option<&str>) -> TurnSoFar {
        let Some(t) = turn_id else {
            return TurnSoFar::default();
        };
        let mut turns = lock(&self.turns);
        match turns.get_mut(t) {
            Some(agg) if agg.last_seen.elapsed() < TURN_TTL => {
                agg.last_seen = Instant::now();
                agg.so_far
            }
            _ => TurnSoFar::default(),
        }
    }

    fn add_to_turn(&self, turn_id: &str, input: u64, output: u64, cached: u64, cost: Option<Usd>) {
        let now = Instant::now();
        let mut turns = lock(&self.turns);
        if turns.len() >= TURN_CAPACITY && !turns.contains_key(turn_id) {
            turns.retain(|_, a| now.saturating_duration_since(a.last_seen) < TURN_TTL);
            if turns.len() >= TURN_CAPACITY
                && let Some(oldest) = turns
                    .iter()
                    .min_by_key(|(_, a)| a.last_seen)
                    .map(|(k, _)| k.clone())
            {
                turns.remove(&oldest);
            }
        }
        let agg = turns.entry(turn_id.to_owned()).or_insert(TurnAgg {
            so_far: TurnSoFar::default(),
            last_seen: now,
        });
        if agg.last_seen.elapsed() >= TURN_TTL {
            agg.so_far = TurnSoFar::default();
        }
        agg.last_seen = now;
        let s = &mut agg.so_far;
        s.input = s.input.saturating_add(input);
        s.output = s.output.saturating_add(output);
        s.cached = s.cached.saturating_add(cached);
        s.requests = s.requests.saturating_add(1);
        if let Some(c) = cost {
            s.cost = Some(Usd::from_micros(
                s.cost.map_or(0, Usd::micros).saturating_add(c.micros()),
            ));
        }
    }

    /// Start tracking a request.
    pub fn track(
        self: &Arc<Self>,
        generation: Arc<Generation>,
        meta: RequestMeta,
        t0: Instant,
        started_ms: i64,
    ) -> Tracker {
        let id = self.next_inflight.fetch_add(1, Ordering::Relaxed) + 1;
        let inflight = Arc::new(Inflight {
            id,
            started_ms,
            t0,
            info: Mutex::new(InflightInfo {
                model: meta.model.clone(),
                prompt_cache_key: meta.prompt_cache_key.clone(),
                client: meta.client,
                transport: meta.transport,
                ..InflightInfo::default()
            }),
            phase: AtomicU8::new(0),
            deltas: AtomicU64::new(0),
            first_token_ms: AtomicU64::new(u64::MAX),
        });
        lock(&self.inflight).insert(id, inflight.clone());
        Tracker {
            hub: self.clone(),
            generation,
            meta,
            t0,
            started_ms,
            inflight,
            attempts: Vec::new(),
            log: Vec::new(),
            answered: None,
            failovers: 0,
            http_status: None,
            sent: None,
            ttfb: None,
            upstream_request_id: None,
            compression: superba_core::RequestCompression::None,
            done: false,
        }
    }

    fn finish_row(&self, generation: &Generation, stored: StoredRow) {
        let cfg = generation.config.stats.as_ref();
        let record = cfg.is_some_and(|c| c.record);
        if !record {
            return;
        }
        {
            let mut ring = lock(&self.live);
            if ring.len() >= LIVE_RING {
                ring.pop_front();
            }
            ring.push_back(stored.row.clone());
        }
        let rec = lock(&self.recorder);
        let Some(r) = rec.as_ref() else { return };
        if !r.record(stored) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// How a tracked request ended.
#[derive(Debug, Default)]
pub(crate) struct End {
    /// What the response tap measured (`None`: no tap).
    pub summary: Option<TapSummary>,
    /// A status that overrides the tap's (router errors, broken, cancelled).
    pub status: Option<RequestStatus>,
    /// Redacted short error.
    pub error: Option<String>,
    /// Usage read from a JSON (non-streaming) answer.
    pub json_usage: Option<super::usage::Usage>,
}

/// One request from receipt to its row.
pub(crate) struct Tracker {
    hub: Arc<StatsHub>,
    generation: Arc<Generation>,
    meta: RequestMeta,
    t0: Instant,
    started_ms: i64,
    inflight: Arc<Inflight>,
    attempts: Vec<RequestAttempt>,
    log: Vec<String>,
    answered: Option<Candidate>,
    failovers: u32,
    http_status: Option<u16>,
    /// `t0` → the answering attempt was sent (WebSocket: the frame forwarded).
    sent: Option<Duration>,
    /// `t0` → response headers (WebSocket: the first frame of the answer).
    ttfb: Option<Duration>,
    upstream_request_id: Option<String>,
    /// How the body goes to the line of the current attempt.
    compression: superba_core::RequestCompression,
    done: bool,
}

impl std::fmt::Debug for Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracker")
            .field("inflight", &self.inflight.id)
            .finish_non_exhaustive()
    }
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl Tracker {
    /// `t0`.
    pub fn t0(&self) -> Instant {
        self.t0
    }

    /// The in-flight entry (live numbers).
    pub fn inflight(&self) -> &Arc<Inflight> {
        &self.inflight
    }

    /// Add a log line for the detail pane (never secrets or content).
    pub fn note(&mut self, line: impl Into<String>) {
        if self.log.len() < 64 {
            self.log.push(line.into());
        }
    }

    fn set_member(&self, c: Candidate) {
        let m = &self.generation.members[c.member];
        let mut info = lock(&self.inflight.info);
        info.member_id = m.cfg.id.clone();
        info.member_name = m.cfg.name.clone();
        info.line_name = Some(m.lines[c.line].cfg.name.clone());
    }

    /// A candidate is being tried.
    pub fn trying(&self, c: Candidate) {
        self.set_member(c);
    }

    /// An attempt failed before any byte reached Codex.
    pub fn attempt_failed(
        &mut self,
        c: Candidate,
        status: Option<u16>,
        reason: &str,
        elapsed: Duration,
    ) {
        let m = &self.generation.members[c.member];
        let l = &m.lines[c.line];
        self.attempts.push(RequestAttempt {
            member_id: ProfileId::new(m.cfg.id.clone()),
            line_name: Some(l.cfg.name.clone()),
            upstream_host: l.cfg.base_url.host_str().map(str::to_owned),
            status,
            outcome: reason.to_owned(),
            elapsed_ms: Some(ms(elapsed)),
            compression: self.body_compression(),
        });
        self.failovers += 1;
        let label = self.generation.label(c.member, c.line);
        self.note(format!("{label}: {reason}"));
    }

    /// Headers arrived from `c` at `now` (the request was sent at `sent`) and
    /// are passed on.
    pub fn answered(
        &mut self,
        c: Candidate,
        status: u16,
        headers: &HeaderMap,
        sent: Instant,
        now: Instant,
    ) {
        let m = &self.generation.members[c.member];
        let l = &m.lines[c.line];
        self.set_member(c);
        self.answered = Some(c);
        self.http_status = Some(status);
        self.sent = Some(sent.saturating_duration_since(self.t0));
        let ttfb = now.saturating_duration_since(self.t0);
        self.ttfb = Some(ttfb);
        self.upstream_request_id = headers
            .get("x-request-id")
            .or_else(|| headers.get("x-oai-request-id"))
            .and_then(|v| v.to_str().ok())
            .map(|v| v.chars().take(128).collect());
        self.attempts.push(RequestAttempt {
            member_id: ProfileId::new(m.cfg.id.clone()),
            line_name: Some(l.cfg.name.clone()),
            upstream_host: l.cfg.base_url.host_str().map(str::to_owned),
            status: Some(status),
            outcome: "answered".into(),
            elapsed_ms: Some(ms(ttfb)),
            compression: self.body_compression(),
        });
        let label = self.generation.label(c.member, c.line);
        self.note(format!("{label}: HTTP {status} after {} ms", ms(ttfb)));
        self.inflight.update(InflightPhase::Streaming, 0, None);
    }

    /// How the request body goes to the line of the next attempt (the proxy
    /// sets it per attempt; the answering one is recorded).
    pub fn set_compression(&mut self, compression: superba_core::RequestCompression) {
        self.compression = compression;
    }

    /// `c` refused the router's zstd body with `status` after `elapsed`; the
    /// request goes again uncompressed to the same line (not a failover).
    pub fn zstd_refused(&mut self, c: Candidate, status: u16, elapsed: Duration) {
        let m = &self.generation.members[c.member];
        let l = &m.lines[c.line];
        self.attempts.push(RequestAttempt {
            member_id: ProfileId::new(m.cfg.id.clone()),
            line_name: Some(l.cfg.name.clone()),
            upstream_host: l.cfg.base_url.host_str().map(str::to_owned),
            status: Some(status),
            outcome: "zstd body refused, sent again uncompressed".into(),
            elapsed_ms: Some(ms(elapsed)),
            compression: Some(superba_core::RequestCompression::RouterZstd),
        });
        let label = self.generation.label(c.member, c.line);
        self.note(format!(
            "{label}: zstd body answered HTTP {status}, sent again uncompressed"
        ));
    }

    /// The body compression of an HTTP attempt (`None` for WebSocket frames).
    fn body_compression(&self) -> Option<superba_core::RequestCompression> {
        (self.meta.transport != superba_core::RequestTransport::WebSocket)
            .then_some(self.compression)
    }

    /// A WebSocket `response.create` on a socket pinned to `c` (the handshake
    /// failed over `failovers` times before).
    pub fn ws_pinned(&mut self, c: Candidate, failovers: u32) {
        self.set_member(c);
        self.answered = Some(c);
        self.failovers = failovers;
        let label = self.generation.label(c.member, c.line);
        self.note(format!("{label}: WebSocket response.create"));
    }

    /// The WebSocket request frame was forwarded to the member at `at`.
    pub fn set_sent(&mut self, at: Instant) {
        self.sent = Some(at.saturating_duration_since(self.t0));
    }

    /// The first frame of the answer arrived at `now`.
    pub fn set_ttfb(&mut self, now: Instant) {
        self.ttfb = Some(now.saturating_duration_since(self.t0));
        self.inflight.update(InflightPhase::Streaming, 0, None);
    }

    /// The timing marks of the finished request, in order ([`RequestTiming::
    /// normalized`]); `end` is the end mark.
    fn timing(&self, summary: &TapSummary, end: Duration) -> RequestTiming {
        let websocket = self.meta.transport == RequestTransport::WebSocket;
        RequestTiming {
            sent_ms: self.sent.map(ms),
            // A WebSocket request has no response headers of its own.
            headers_ms: if websocket { None } else { self.ttfb.map(ms) },
            first_event_ms: summary.first_event.map(ms),
            first_token_ms: summary.first_token.map(ms),
            last_delta_ms: summary.last_delta.map(ms),
            end_ms: Some(ms(end)),
        }
        .normalized()
    }

    /// The stats line setup when this request may get the line (gating of RS
    /// 3.3 rules 1-4; the caller checks the transport).
    pub fn line_setup(&self) -> Option<LineSetup> {
        let cfg = self.generation.config.stats.as_ref()?;
        let line = cfg.line.as_ref()?;
        if !line_allowed(line, &self.meta) {
            return None;
        }
        let member = self
            .answered
            .map(|c| self.generation.members[c.member].cfg.id.as_str())
            .unwrap_or_default();
        let models = self.models();
        let model_refs: Vec<&str> = models.iter().map(String::as_str).collect();
        Some(LineSetup {
            template: line.template.clone(),
            style: line.style,
            turn: self.hub.turn_so_far(self.meta.turn_id.as_deref()),
            price: price::lookup(&cfg.prices, member, &model_refs).cloned(),
            ratio: price::ratio(&cfg.prices, member),
        })
    }

    fn upstream_model(&self) -> Option<String> {
        let c = self.answered?;
        let model = self.meta.model.as_deref()?;
        self.generation.members[c.member]
            .cfg
            .model_map
            .get(model)
            .filter(|m| m.as_str() != model)
            .cloned()
    }

    fn models(&self) -> Vec<String> {
        self.meta
            .model
            .iter()
            .cloned()
            .chain(self.upstream_model())
            .collect()
    }

    /// Finish: build the row, update the turn totals and queue it.
    pub fn finish(mut self, end: End) {
        self.finish_inner(end);
    }

    fn finish_inner(&mut self, end: End) {
        if self.done {
            return;
        }
        self.done = true;
        lock(&self.hub.inflight).remove(&self.inflight.id);
        let generation = self.generation.clone();
        let Some(cfg) = generation.config.stats.as_ref() else {
            return;
        };
        let summary = end.summary.unwrap_or_default();
        let usage = summary.usage.or(end.json_usage);
        let status = end.status.unwrap_or_else(|| match &summary.outcome {
            Some(Outcome::Completed) => RequestStatus::Ok,
            Some(Outcome::Failed { code }) => RequestStatus::Failed { code: code.clone() },
            Some(Outcome::Incomplete { reason }) => RequestStatus::Incomplete {
                reason: reason.clone(),
            },
            Some(Outcome::Error { status, code }) => match status {
                Some(s) if *s >= 400 => RequestStatus::Http {
                    status: *s,
                    code: code.clone(),
                },
                _ => RequestStatus::Failed { code: code.clone() },
            },
            None => match self.http_status {
                Some(s) if !(200..300).contains(&s) => RequestStatus::Http {
                    status: s,
                    code: None,
                },
                _ => RequestStatus::Ok,
            },
        });
        let (member_id, member_name, line_id, line_name, host) = match self.answered {
            Some(c) => {
                let m = &generation.members[c.member];
                let l = &m.lines[c.line];
                (
                    m.cfg.id.clone(),
                    m.cfg.name.clone(),
                    Some(l.cfg.id.clone()),
                    Some(l.cfg.name.clone()),
                    l.cfg.base_url.host_str().map(str::to_owned),
                )
            }
            None => {
                let info = lock(&self.inflight.info).clone();
                let line_name = info.line_name.clone();
                let a = self.attempts.last();
                (
                    info.member_id,
                    info.member_name,
                    None,
                    line_name,
                    a.and_then(|a| a.upstream_host.clone()),
                )
            }
        };
        let mut models = self.models();
        if let Some(m) = &summary.response_model {
            models.push(m.clone());
        }
        let model_refs: Vec<&str> = models.iter().map(String::as_str).collect();
        let estimate = usage.and_then(|u| {
            price::estimate(
                &cfg.prices,
                &member_id,
                &model_refs,
                &u,
                summary.response_tier.as_deref(),
            )
        });
        let timing = self.timing(&summary, summary.end.unwrap_or_else(|| self.t0.elapsed()));
        if self.meta.request_kind == Some(RequestKind::Turn)
            && let (Some(turn), Some(u)) = (self.meta.turn_id.as_deref(), usage)
        {
            self.hub.add_to_turn(
                turn,
                u.input,
                u.output,
                u.cached.unwrap_or(0),
                estimate.as_ref().map(|(c, _)| *c),
            );
        }
        match &status {
            RequestStatus::Cancelled => self.note("cancelled by the client"),
            RequestStatus::Broken => self.note("the upstream broke the stream"),
            _ => {}
        }
        if self.meta.stripped > 0 {
            let n = self.meta.stripped;
            self.note(format!("{n} stats line(s) removed from the input"));
        }
        if summary.injected {
            self.note("stats line added");
        }
        let id = self.hub.next_row.fetch_add(1, Ordering::SeqCst) + 1;
        let row = RequestRow {
            id,
            started_at: Timestamp::from_millis(self.started_ms),
            prompt_cache_key: self.meta.prompt_cache_key.clone(),
            thread_id: self.meta.thread_id.clone(),
            turn_id: self.meta.turn_id.clone(),
            member_id: ProfileId::new(member_id),
            member_name,
            line_id,
            line_name,
            upstream_host: host,
            failovers: self.failovers,
            transport: self.meta.transport,
            originator: self.meta.originator.clone(),
            client: self.meta.client,
            request_kind: self.meta.request_kind,
            thread_source: self.meta.thread_source.clone(),
            model: self.meta.model.clone(),
            upstream_model: self.upstream_model(),
            response_model: summary.response_model.clone(),
            service_tier_requested: self.meta.service_tier.clone(),
            service_tier_answered: summary.response_tier.clone(),
            status,
            http_status: self.http_status,
            error: end.error,
            input_tokens: usage.map(|u| u.input),
            cached_tokens: usage.and_then(|u| u.cached),
            cache_write_tokens: usage.and_then(|u| u.cache_write),
            output_tokens: usage.map(|u| u.output),
            reasoning_tokens: usage.and_then(|u| u.reasoning),
            // WebSocket: the first frame (TTFB); HTTP: the response headers.
            ttfb_ms: timing
                .headers_ms
                .or_else(|| self.ttfb.map(ms))
                .map(|t| t.min(timing.end_ms.unwrap_or(u64::MAX))),
            first_token_ms: timing.first_token_ms,
            duration_ms: timing.end_ms,
            // The value the stats line showed (same marks, same function).
            tps_milli: summary.tps_milli(),
            cost: estimate.as_ref().map(|(c, _)| *c),
            price_rev: estimate.and_then(|(_, r)| r),
            injected: summary.injected,
            stripped: self.meta.stripped,
            upstream_request_id: self.upstream_request_id.take(),
            source: superba_core::RequestSource::Router,
        };
        let stored = StoredRow {
            row,
            timing,
            attempts: std::mem::take(&mut self.attempts),
            log: std::mem::take(&mut self.log),
        };
        self.hub.finish_row(&generation, stored);
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        if !self.done {
            self.finish_inner(End {
                status: Some(RequestStatus::Cancelled),
                ..End::default()
            });
        }
    }
}

/// Unix milliseconds now (signed, for rows).
pub(crate) fn now_ms() -> i64 {
    i64::try_from(unix_ms()).unwrap_or(i64::MAX)
}
