//! Fake service, area "sessions" (owner B6).
//! Sessions page: sessions per host.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here, so the area's
//! owner edits only this file.
//!
//! This computer answers with the core fake's scan and history; a remote host
//! answers with a read-only rollout count (devbox: 236 under `openai`, 12 hidden
//! under `krill`; other hosts: everything under `openai`) unless a test made it
//! fail.

use std::collections::{BTreeMap, BTreeSet};

use superba_core::*;

use super::FakeService;

/// Fake state of the area (kept for the life of the fake world).
#[derive(Debug, Default)]
pub(super) struct State {
    /// Pause one local snapshot after reading it, to order concurrent scans.
    local_gate: Option<(
        std::sync::mpsc::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
    /// Hosts whose scan fails ("ssh: connect timed out").
    failing: BTreeSet<TargetId>,
    /// Hosts scanned so far, in order.
    scans: Vec<TargetId>,
    /// Hosts whose scan does not answer until released (a slow host).
    held: BTreeSet<TargetId>,
    /// Hosts answering with these (provider, sessions) counts and no migrate
    /// command of the service's own.
    providers: BTreeMap<TargetId, Vec<(String, u32)>>,
}

impl FakeService {
    /// Start with these local counts and no migration history.
    pub fn set_session_counts(&self, counts: &[(&str, u32)]) {
        let mut s = self.state();
        s.sessions.by_provider = counts
            .iter()
            .map(|(provider, count)| {
                (
                    (*provider).to_owned(),
                    ProviderSessions {
                        threads: *count,
                        rollouts: *count,
                        distinct: *count,
                        ..Default::default()
                    },
                )
            })
            .collect();
        s.history.clear();
    }

    /// Signal when the next local snapshot is captured and hold its delivery
    /// until the returned sender is released. Later scans can finish first.
    pub fn gate_next_session_scan(
        &self,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let old = self
            .sessions_area()
            .local_gate
            .replace((started_tx, release_rx));
        assert!(old.is_none(), "a session scan is already gated");
        (started_rx, release_tx)
    }

    fn sessions_area(&self) -> std::sync::MutexGuard<'_, State> {
        self.areas
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Make the scan of `target` fail (or succeed again).
    pub fn fail_host_sessions(&self, target: &TargetId, fail: bool) {
        let mut a = self.sessions_area();
        if fail {
            a.failing.insert(target.clone());
        } else {
            a.failing.remove(target);
        }
    }

    /// Answer the scan of `target` with `counts` (provider, sessions) and no
    /// migrate command (the page builds its own).
    pub fn set_host_providers(&self, target: &TargetId, counts: &[(&str, u32)]) {
        self.sessions_area().providers.insert(
            target.clone(),
            counts.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect(),
        );
    }

    /// Hold the scans of `target` until released (`false`): a slow host.
    pub fn hold_host_sessions(&self, target: &TargetId, hold: bool) {
        let mut a = self.sessions_area();
        if hold {
            a.held.insert(target.clone());
        } else {
            a.held.remove(target);
        }
    }

    /// Hosts scanned with `host_sessions`, in order.
    pub fn host_scans(&self) -> Vec<TargetId> {
        self.sessions_area().scans.clone()
    }
}

/// Body of `SwitchService::sessions`, including the optional snapshot gate.
pub(super) async fn sessions(fake: &FakeService) -> Result<(SessionReport, Vec<MigrationReceipt>)> {
    let snapshot = {
        let s = fake.state();
        let mut report = s.sessions.clone();
        report.scanned_at = Timestamp::now();
        (report, s.history.clone())
    };
    let gate = fake.sessions_area().local_gate.take();
    if let Some((started, release)) = gate {
        let _ = started.send(());
        let _ = release.await;
    }
    Ok(snapshot)
}

/// A read-only remote scan of `counts` (provider, sessions).
pub fn remote_report(home: &str, counts: &[(&str, u32)]) -> SessionReport {
    SessionReport {
        codex_home: home.into(),
        scanned_at: Timestamp::now(),
        sqlite_paths: Vec::new(),
        by_provider: counts
            .iter()
            .map(|(p, n)| {
                (
                    (*p).to_owned(),
                    ProviderSessions {
                        threads: 0,
                        rollouts: *n,
                        archived: n / 10,
                        compressed: 0,
                        distinct: *n,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
        inconsistent: 0,
        warnings: Vec::new(),
    }
}

/// Body of `SwitchService::host_sessions`.
pub(super) async fn host_sessions(
    fake: &FakeService,
    target_id: &TargetId,
) -> Result<HostSessions> {
    fake.sessions_area().scans.push(target_id.clone());
    fake.delay(300).await;
    while fake.sessions_area().held.contains(target_id) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    if target_id.is_local() {
        let (report, history) = fake.sessions().await?;
        return Ok(HostSessions {
            target_id: target_id.clone(),
            report,
            history,
            read_only: false,
            migrate_command: None,
        });
    }
    if fake.sessions_area().failing.contains(target_id) {
        return Err(Error::Timeout {
            what: "ssh: connect".into(),
        });
    }
    let providers = fake.sessions_area().providers.get(target_id).cloned();
    if let Some(counts) = providers {
        let counts: Vec<(&str, u32)> = counts.iter().map(|(p, n)| (p.as_str(), *n)).collect();
        return Ok(HostSessions {
            target_id: target_id.clone(),
            report: remote_report("/home/ml/.codex", &counts),
            history: Vec::new(),
            read_only: true,
            migrate_command: None,
        });
    }
    let devbox = target_id.as_str().contains("devbox");
    let report = if devbox {
        remote_report("/home/dev/.codex", &[("openai", 236), ("krill", 12)])
    } else {
        remote_report("/home/ml/.codex", &[("openai", 41)])
    };
    Ok(HostSessions {
        target_id: target_id.clone(),
        report,
        history: Vec::new(),
        read_only: true,
        migrate_command: devbox.then(|| "spb sessions migrate --from krill".to_owned()),
    })
}

/// The session browser's threads: two on this computer, one on the SSH host
/// `bj` (as the app's catalog lists it).
pub(super) async fn session_list(_fake: &FakeService) -> Result<SessionList> {
    let now = Timestamp::now().0;
    let entry =
        |host: &str, kind: &str, id: &str, title: &str, cwd: &str, ago_min: i64| SessionEntry {
            host_id: host.into(),
            host_kind: Some(kind.into()),
            thread_id: id.into(),
            title: title.into(),
            cwd: cwd.into(),
            provider: "openai".into(),
            model: (host == LOCAL_HOST_ID).then(|| "gpt-5.5".into()),
            updated_at: Some(Timestamp(now - ago_min * 60_000)),
            tokens_used: (host == LOCAL_HOST_ID).then_some(123_456),
            ..SessionEntry::default()
        };
    Ok(SessionList {
        entries: vec![
            entry(
                LOCAL_HOST_ID,
                "local",
                "019a2f5b-6a7b-7d46-8e85-4f0a6b8c2d05",
                "Plan the database migration",
                "/home/u/w/superba",
                3,
            ),
            entry(
                "remote-ssh-discovered:bj",
                "ssh",
                "01a158c7-a09e-7cd2-ba08-41459547a695",
                "Fix flaky test",
                "/srv/app",
                60,
            ),
            entry(
                LOCAL_HOST_ID,
                "local",
                "019a2f10-5b1c-7c3e-9d41-0a6b2c4d8e01",
                "Refactor the router",
                "/home/u/w/router",
                600,
            ),
        ],
        truncated: false,
        scanned_at: Timestamp::now(),
        warnings: Vec::new(),
    })
}

/// Markdown of a thread; session formats report a made-up file.
pub(super) async fn session_export(
    _fake: &FakeService,
    thread_id: String,
    format: SessionExportFormat,
) -> Result<SessionExport> {
    Ok(match format {
        SessionExportFormat::Markdown => SessionExport {
            text: format!("# {thread_id}\n"),
            messages: 2,
            ..SessionExport::default()
        },
        _ => SessionExport {
            text: "claude --resume fake".into(),
            path: Some("/tmp/fake.jsonl".into()),
            session_id: Some("fake".into()),
            messages: 2,
        },
    })
}
