//! Claude Code's local transcripts in the request log
//! ([`superba_sessions::claude_usage`]): Claude Code requests that did not go
//! through the local router still count on the Usage page and in the Overview.
//!
//! - Rows are attributed to a synthetic `claude:anthropic` provider
//!   ("Anthropic") and priced from the price table like rollout rows (no
//!   price: cost `None`, never 0).
//! - A transcript row is dropped when the router recorded a Claude Code row
//!   with the same response id (`message.id`), or a Claude Code row of the
//!   same session that started within [`MATCH_BEFORE`] before the row's start
//!   and [`MATCH_AFTER`] after its end: the router's row is richer.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use superba_core::{
    AgentKind, ProfileId, RequestClient, RequestDetail, RequestHarness, RequestKind,
    RequestProtocol, RequestRow, RequestSource, RequestStatus, RequestTiming, Result, Timestamp,
};
use superba_sessions::claude_usage::{ClaudeRequest, ClaudeUsageIndex};
use superba_sessions::usage::RolloutUsage;

use crate::Switcher;
use crate::usage::{Attribution, MATCH_AFTER, MATCH_BEFORE};

/// Shared transcript index of the local Claude Code configuration directory.
pub(crate) type SharedClaudeIndex = Arc<Mutex<ClaudeUsageIndex>>;

/// Member id of transcript rows.
pub const MEMBER_ID: &str = "claude:anthropic";
/// Member name of transcript rows.
pub const MEMBER_NAME: &str = "Anthropic";

fn end_of(row: &RequestRow) -> Timestamp {
    row.started_at
        .saturating_add(Duration::from_millis(row.duration_ms.unwrap_or(0)))
}

/// The log row of a transcript request, priced with `a`.
pub fn row(a: &Attribution, r: &ClaudeRequest) -> RequestRow {
    // Price through the rollout path (same lookup, ratio and basis).
    let member_id = ProfileId::new(MEMBER_ID);
    let priced = a.price(
        &member_id,
        Some(r.model.as_str()),
        Some(r.usage),
        None,
        r.started_at,
    );
    let (cost, price_rev) = match priced {
        Some((c, basis)) => (Some(c), basis),
        None => (None, None),
    };
    let u: RolloutUsage = r.usage;
    RequestRow {
        id: r.id,
        started_at: r.started_at,
        prompt_cache_key: None,
        thread_id: None,
        turn_id: None,
        member_id,
        member_name: MEMBER_NAME.to_owned(),
        line_id: None,
        line_name: None,
        upstream_host: None,
        failovers: 0,
        attempts: Vec::new(),
        transport: Default::default(),
        originator: None,
        client: RequestClient::Other,
        request_kind: Some(RequestKind::Turn),
        thread_source: None,
        model: Some(r.model.clone()),
        upstream_model: None,
        response_model: None,
        service_tier_requested: None,
        service_tier_answered: None,
        status: RequestStatus::Ok,
        http_status: None,
        error: None,
        input_tokens: Some(u.input),
        cached_tokens: Some(u.cached),
        cache_write_tokens: Some(u.cache_write),
        output_tokens: Some(u.output),
        reasoning_tokens: None,
        ttfb_ms: None,
        first_token_ms: None,
        duration_ms: Some(r.duration_ms()),
        tps_milli: None,
        price_rev,
        cost,
        injected: false,
        stripped: 0,
        upstream_request_id: r.request_id.clone(),
        source: RequestSource::ClaudeTranscript,
        codex_host: None,
        harness: RequestHarness::ClaudeCode,
        harness_version: None,
        response_id: Some(r.message_id.clone()),
        protocol: RequestProtocol::Messages,
        requested_model: Some(r.model.clone()),
        session_id: r.session_id.clone(),
        agent_kind: Some(if r.sidechain {
            AgentKind::Subagent
        } else {
            AgentKind::Main
        }),
    }
}

/// The transcript rows no router row describes: no Claude Code router row
/// with the same response id, and none of the same session that started in
/// `[start - MATCH_BEFORE, end + MATCH_AFTER]`.
pub fn dedupe(rows: Vec<RequestRow>, router: &[RequestRow]) -> Vec<RequestRow> {
    let router: Vec<&RequestRow> = router
        .iter()
        .filter(|x| x.harness == RequestHarness::ClaudeCode && x.source == RequestSource::Router)
        .collect();
    let responses: HashSet<&str> = router
        .iter()
        .filter_map(|x| x.response_id.as_deref())
        .collect();
    rows.into_iter()
        .filter(|r| {
            if r.response_id
                .as_deref()
                .is_some_and(|id| responses.contains(id))
            {
                return false;
            }
            let Some(session) = r.session_id.as_deref() else {
                return true;
            };
            let lo = r.started_at.saturating_sub(MATCH_BEFORE);
            let hi = end_of(r).saturating_add(MATCH_AFTER);
            !router.iter().any(|x| {
                x.session_id.as_deref() == Some(session) && x.started_at >= lo && x.started_at <= hi
            })
        })
        .collect()
}

impl Switcher {
    /// Claude Code transcript rows started in `[from, to)` that the router
    /// did not record, priced; `true` when the row cap was reached.
    pub(crate) async fn claude_rows(
        &self,
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    ) -> Result<(Vec<RequestRow>, bool)> {
        let index = self.claude_usage.clone();
        let page = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .requests(from, to)
        })
        .await
        .map_err(|e| superba_core::Error::other(format!("claude usage: {e}")))?;
        if page.rows.is_empty() {
            return Ok((Vec::new(), page.truncated));
        }
        let a = self.attribution().await;
        let rows: Vec<RequestRow> = page.rows.iter().map(|r| row(&a, r)).collect();
        let sessions: BTreeSet<String> = rows.iter().filter_map(|r| r.session_id.clone()).collect();
        let responses: Vec<String> = rows.iter().filter_map(|r| r.response_id.clone()).collect();
        let lo = rows
            .iter()
            .map(|r| r.started_at)
            .min()
            .map(|t| t.saturating_sub(MATCH_BEFORE));
        let hi = rows
            .iter()
            .map(end_of)
            .max()
            .map(|t| t.saturating_add(MATCH_AFTER + Duration::from_millis(1)));
        let router = self
            .deps
            .router
            .claude_requests_for(lo, hi, sessions.into_iter().collect(), responses)
            .await
            .unwrap_or_default();
        Ok((dedupe(rows, &router), page.truncated))
    }

    /// The detail of a transcript row (id in the transcript range).
    pub(crate) async fn claude_detail(&self, id: i64) -> Result<RequestDetail> {
        let index = self.claude_usage.clone();
        let found = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .row(id)
        })
        .await
        .map_err(|e| superba_core::Error::other(format!("claude usage: {e}")))?;
        let Some(r) = found else {
            return Err(superba_core::Error::NotFound {
                what: format!("request {id}"),
            });
        };
        let row = row(&self.attribution().await, &r);
        Ok(RequestDetail {
            timing: RequestTiming {
                end_ms: row.duration_ms,
                ..RequestTiming::default()
            },
            row,
            attempts: Vec::new(),
            log: Vec::new(),
        })
    }
}
