//! Per-switch WSL progress, cancellation and probe reuse. The scope belongs to
//! one plan/execution future, so concurrent background probes cannot steal a
//! snapshot, reset a deadline or send progress to another switch.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use superba_core::{LocalizedText, RemoteHost, SwitchEvent, SwitchEvents, TargetId};
use tokio::time::Instant;

use crate::probe::Snapshot;

/// Total WSL work per distribution in one plan or execution, including retries.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(180);

struct Context {
    events: Arc<dyn SwitchEvents>,
    snapshots: Mutex<BTreeMap<TargetId, Snapshot>>,
    budgets: Arc<Mutex<BTreeMap<TargetId, Duration>>>,
}

tokio::task_local! {
    static CURRENT: Context;
}

/// Run one plan/execution with its own WSL budget and event sink. Join futures
/// directly within this scope; spawned tasks deliberately do not inherit it.
pub async fn with_events<T>(events: Arc<dyn SwitchEvents>, future: impl Future<Output = T>) -> T {
    CURRENT
        .scope(
            Context {
                events,
                snapshots: Mutex::default(),
                budgets: Arc::default(),
            },
            future,
        )
        .await
}

pub(crate) fn events() -> Option<Arc<dyn SwitchEvents>> {
    CURRENT.try_with(|c| c.events.clone()).ok()
}

pub(crate) struct Budget {
    budgets: Option<Arc<Mutex<BTreeMap<TargetId, Duration>>>>,
    target: TargetId,
    started: Instant,
    pub deadline: Instant,
}

/// Charge only time spent in WSL. A confirmation dialog or Windows app restart
/// between WSL calls must not consume the distribution's remaining allowance.
pub(crate) fn budget(host: &RemoteHost, standalone_limit: Duration) -> Budget {
    let budgets = CURRENT.try_with(|c| c.budgets.clone()).ok();
    let remaining = budgets.as_ref().map_or(standalone_limit, |b| {
        *b.lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(host.target_id.clone())
            .or_insert(TOTAL_TIMEOUT)
    });
    let started = Instant::now();
    Budget {
        budgets,
        target: host.target_id.clone(),
        started,
        deadline: started + remaining,
    }
}

impl Drop for Budget {
    fn drop(&mut self) {
        if let Some(budgets) = &self.budgets {
            let mut budgets = budgets.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(remaining) = budgets.get_mut(&self.target) {
                *remaining = remaining.saturating_sub(self.started.elapsed());
            }
        }
    }
}

pub(crate) fn remember(host: &RemoteHost, snap: &Snapshot) {
    let _ = CURRENT.try_with(|c| {
        c.snapshots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(host.target_id.clone(), snap.clone());
    });
}

pub(crate) fn take(host: &RemoteHost) -> Option<Snapshot> {
    CURRENT
        .try_with(|c| {
            c.snapshots
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&host.target_id)
        })
        .ok()
        .flatten()
}

pub(crate) fn report(
    events: Option<&Arc<dyn SwitchEvents>>,
    host: &RemoteHost,
    stage: Option<&str>,
    attempt: u8,
    can_stop: bool,
) {
    let message = stage.map(|stage| {
        let (key, fallback) = match stage {
            "warmup" => (
                "wsl.progress.warmup",
                "Starting {host} (attempt {attempt}/2)…",
            ),
            "login" => (
                "wsl.progress.login",
                "Reading the login environment on {host} (attempt {attempt}/2)…",
            ),
            "version" => (
                "wsl.progress.version",
                "Checking the Codex version on {host} (attempt {attempt}/2)…",
            ),
            "files" => (
                "wsl.progress.files",
                "Reading configuration on {host} (attempt {attempt}/2)…",
            ),
            "system" => (
                "wsl.progress.system",
                "Checking processes on {host} (attempt {attempt}/2)…",
            ),
            "write" => ("wsl.progress.write", "Writing configuration on {host}…"),
            _ => (
                "wsl.progress.command",
                "Running a command on {host} (attempt {attempt}/2)…",
            ),
        };
        LocalizedText::new(key, fallback)
            .arg_text("host", host.title())
            .arg("attempt", attempt)
    });
    if let Some(events) = events {
        events.emit(SwitchEvent::RemoteProgress {
            target_id: host.target_id.clone(),
            message,
            can_stop,
        });
    }
}
