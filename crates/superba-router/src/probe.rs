//! Active health probes: an unauthenticated GET of each line's probe URL (Krill
//! answers 401 quickly; any answer other than 502-504 or an edge interception
//! means the line is reachable). All lines are probed at start; afterwards a line
//! is probed when its open breaker turns half-open, so a dead line is retried with
//! the breaker's exponential backoff even when no request arrives.
//!
//! A probe only proves a line dead when it cannot connect (DNS, TCP, TLS), times
//! out, or gets 502-504 / an edge interception. A peer that accepts the
//! connection and then closes it without an answer is reachable; some relays
//! drop unauthenticated requests that way (seen on a test relay), so that result
//! is inconclusive and leaves the breaker alone, as does a probe that could not
//! open a socket because this process ran out of descriptors. Real requests
//! still feed the passive breaker.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures_util::future::join_all;

use crate::forward::{describe_error, failover_status};
use crate::limits::{Place, file_limit_detail, file_limit_in};
use crate::state::{Generation, Shared, lock};
use crate::status::RouteEventKind;

/// How often the background task looks for half-open lines.
const TICK: Duration = Duration::from_secs(1);

/// Probe one line and update its health.
pub(crate) async fn probe_line(
    shared: &Shared,
    generation: &Generation,
    member: usize,
    line: usize,
) {
    let m = &generation.members[member];
    let l = &m.lines[line];
    if l.probing.swap(true, Ordering::AcqRel) {
        return;
    }
    let url = l.cfg.effective_probe_url();
    let outcome = match generation.probe.get(url).send().await {
        Ok(resp) => match failover_status(resp.status(), resp.headers()) {
            None => Some(Ok(())),
            Some(reason) => Some(Err(format!("probe: {reason}"))),
        },
        // Out of descriptors here says nothing about the line.
        Err(e) if file_limit_in(&e).is_some() => {
            shared.local_file_limit(
                Place::Probe(generation.label(member, line)),
                &file_limit_detail(&e),
            );
            None
        }
        Err(e) if e.is_connect() || e.is_timeout() => {
            Some(Err(format!("probe: {}", describe_error(&e))))
        }
        Err(e) => {
            tracing::debug!(
                member = %m.cfg.id,
                line = %l.cfg.id,
                error = %describe_error(&e),
                "inconclusive probe"
            );
            None
        }
    };
    let Some(outcome) = outcome else {
        l.probing.store(false, Ordering::Release);
        return;
    };
    let (alive, detail) = match outcome {
        Ok(()) => (true, None),
        Err(d) => (false, Some(d)),
    };
    let changed = lock(&l.health).on_probe(Instant::now(), alive, detail.clone());
    l.probing.store(false, Ordering::Release);
    if changed {
        let kind = if alive {
            RouteEventKind::LineUp
        } else {
            RouteEventKind::LineDown
        };
        shared.event(kind, Some(&m.cfg.id), Some(&l.cfg.id), detail);
    }
}

/// Probe every line of `generation` concurrently.
pub(crate) async fn probe_all(shared: &Shared, generation: &Arc<Generation>) {
    let mut jobs = Vec::new();
    for (mi, m) in generation.members.iter().enumerate() {
        for li in 0..m.lines.len() {
            jobs.push(probe_line(shared, generation, mi, li));
        }
    }
    join_all(jobs).await;
}

/// Background loop: probe all lines at start, then half-open lines as their
/// backoff ends. Stops when the server shuts down.
pub(crate) async fn health_loop(shared: Arc<Shared>) {
    let mut stop = shared.shutdown.subscribe();
    if shared.current().config.tuning.active_probes {
        probe_all(&shared, &shared.current()).await;
    }
    loop {
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = stop.changed() => return,
        }
        shared.retry_stats();
        let generation = shared.current();
        if !generation.config.tuning.active_probes {
            continue;
        }
        let now = Instant::now();
        for (mi, m) in generation.members.iter().enumerate() {
            for (li, l) in m.lines.iter().enumerate() {
                let due = lock(&l.health).open_until().is_some_and(|u| u <= now);
                if due && !l.probing.load(Ordering::Acquire) {
                    let s = shared.clone();
                    let g = generation.clone();
                    tokio::spawn(async move { probe_line(&s, &g, mi, li).await });
                }
            }
        }
    }
}
