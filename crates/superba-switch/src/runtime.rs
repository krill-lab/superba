//! "Written" versus "in effect" (GH#4) and stopping Codex processes for the
//! user (GH#2), on top of the component observations.
//!
//! Codex reads its configuration only when a process starts, so a running
//! consumer uses the generation that was current at its start. [`build`]
//! turns the process list of the local home, the applied state and the
//! generation history into a [`RuntimeView`]: one row per consumer (the
//! desktop app with its Codex core, every codex CLI session, `codex exec`,
//! the daemon, every IDE extension's app-server), what it runs, whether a turn
//! runs in it (rollout session headers say which kind of client runs a turn:
//! [`attribute_busy`]) and how it picks the new configuration up. Pure, so
//! every rule is unit-tested; the service gathers the facts.

use std::path::Path;

use superba_core::codex::{ORIGINATOR_DESKTOP, ORIGINATOR_EXEC, ORIGINATOR_TUI};
use superba_core::{
    ActiveState, ActivitySignal, BusyState, CodexProcessKind, ConsumerRole, ConsumerRuntime,
    GenerationRecord, HotSwitchStatus, ManualEdit, ProcessInfo, RestartAdvice, RunningGeneration,
    RuntimeEffect, RuntimeView, StopOutcome, StopReport, ThreadActivity, Timestamp, UnknownReason,
    process_role,
};

use crate::Switcher;
use crate::rules::PROCESS_START_SLACK_MS;

/// Facts [`build`] works from.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeFacts<'a> {
    /// Codex processes of the local home ([`superba_core::AppController::processes`]).
    pub processes: &'a [ProcessInfo],
    /// The configuration Superba wrote last.
    pub active: Option<&'a ActiveState>,
    /// Generation history of the home (ascending).
    pub generations: &'a [GenerationRecord],
    /// The last save of the file editor in the home (GitHub issue 9), when it
    /// came after the configuration Superba wrote last.
    pub edited: Option<&'a ManualEdit>,
    /// Whether the running app can be hot-restarted.
    pub hot: Option<&'a HotSwitchStatus>,
    /// Busy threads of the local target (`None`: not checked).
    pub busy: Option<&'a [ThreadActivity]>,
    /// The controller can stop processes for the user.
    pub can_stop: bool,
    /// Now.
    pub now: Timestamp,
}

/// The generation that was current when a process started at `started`
/// (`generation: None` = before Superba's first write).
pub fn generation_at(generations: &[GenerationRecord], started: Timestamp) -> RunningGeneration {
    let at = started.as_millis().saturating_add(PROCESS_START_SLACK_MS);
    match generations
        .iter()
        .filter(|g| g.applied_at.as_millis() <= at)
        .max_by_key(|g| (g.applied_at, g.generation))
    {
        Some(g) => RunningGeneration {
            generation: Some(g.generation),
            profile_id: g.profile_id.clone(),
            site_kind: g.site_kind,
            applied_at: Some(g.applied_at),
        },
        None => RunningGeneration {
            generation: None,
            profile_id: None,
            site_kind: None,
            applied_at: None,
        },
    }
}

/// Which configuration a process that started at `started` runs. Within
/// [`PROCESS_START_SLACK_MS`] of the write it counts as current (the app-server
/// the switch itself restarted; plan 7.7 "only when certain").
pub fn effect(
    started: Option<Timestamp>,
    active: Option<&ActiveState>,
    generations: &[GenerationRecord],
) -> RuntimeEffect {
    effect_with_edit(started, active, generations, None)
}

/// [`effect`] when the file editor saved a file of the home at `edited` (a
/// process that started before the save runs the file from before it, also
/// when Superba never wrote the home).
pub fn effect_with_edit(
    started: Option<Timestamp>,
    active: Option<&ActiveState>,
    generations: &[GenerationRecord],
    edited: Option<&ManualEdit>,
) -> RuntimeEffect {
    let written_at = match (active.map(|a| a.applied_at), edited.map(|e| e.at)) {
        (Some(a), Some(e)) => a.max(e),
        (Some(t), None) | (None, Some(t)) => t,
        (None, None) => {
            return RuntimeEffect::Unknown {
                reason: UnknownReason::NothingWritten,
            };
        }
    };
    let Some(started) = started else {
        return RuntimeEffect::Unknown {
            reason: UnknownReason::NoStartTime,
        };
    };
    if started.as_millis().saturating_add(PROCESS_START_SLACK_MS) < written_at.as_millis() {
        RuntimeEffect::Previous {
            running: generation_at(generations, started),
        }
    } else {
        RuntimeEffect::Current
    }
}

fn advice(
    role: &ConsumerRole,
    effect: &RuntimeEffect,
    hot: Option<&HotSwitchStatus>,
) -> RestartAdvice {
    if !effect.is_previous() {
        return RestartAdvice::None;
    }
    match role {
        ConsumerRole::DesktopApp => match hot {
            Some(HotSwitchStatus::Ready) => RestartAdvice::HotRestart,
            _ => RestartAdvice::FullRestart,
        },
        ConsumerRole::Terminal => RestartAdvice::Reopen,
        ConsumerRole::Exec => RestartAdvice::Rerun,
        ConsumerRole::Daemon => RestartAdvice::RestartDaemon,
        ConsumerRole::IdeExtension { ide } => RestartAdvice::ReloadIde { ide: *ide },
        _ => RestartAdvice::RestartOwner,
    }
}

/// The view (see the module docs). Remote links are left out (remote hosts
/// have their own view); app-servers of the desktop app are folded into its
/// row (the core is what reads the configuration).
pub fn build(f: &RuntimeFacts<'_>) -> RuntimeView {
    let mut rows: Vec<ConsumerRuntime> = Vec::new();
    let mains: Vec<&ProcessInfo> = f
        .processes
        .iter()
        .filter(|p| p.kind == CodexProcessKind::AppMain)
        .collect();
    let cores: Vec<&ProcessInfo> = f
        .processes
        .iter()
        .filter(|p| p.kind == CodexProcessKind::AppServer)
        .collect();
    for main in &mains {
        // The newest app-server child: an old one may still be exiting.
        let core = cores
            .iter()
            .filter(|c| c.ppid == Some(main.pid))
            .max_by_key(|c| c.started_at)
            .copied();
        let effect = match core {
            Some(c) => effect_with_edit(c.started_at, f.active, f.generations, f.edited),
            None => RuntimeEffect::Unknown {
                reason: UnknownReason::NoCore,
            },
        };
        let role = ConsumerRole::DesktopApp;
        rows.push(ConsumerRuntime {
            advice: advice(&role, &effect, f.hot),
            role,
            pid: main.pid,
            core_pid: core.map(|c| c.pid),
            started_at: core.and_then(|c| c.started_at),
            codex_home: core
                .and_then(|c| c.codex_home.clone())
                .or_else(|| main.codex_home.clone()),
            cwd: None,
            exe: main.exe.clone(),
            cmdline: Vec::new(),
            effect,
            busy: BusyState::NotChecked,
            can_stop: false,
        });
    }
    // An app-server whose app main process is not visible (it is the app's
    // Codex core all the same).
    for core in cores
        .iter()
        .filter(|c| !mains.iter().any(|m| c.ppid == Some(m.pid)))
    {
        let effect = effect_with_edit(core.started_at, f.active, f.generations, f.edited);
        let role = ConsumerRole::DesktopApp;
        rows.push(ConsumerRuntime {
            advice: advice(&role, &effect, f.hot),
            role,
            pid: core.pid,
            core_pid: Some(core.pid),
            started_at: core.started_at,
            codex_home: core.codex_home.clone(),
            cwd: None,
            exe: core.exe.clone(),
            cmdline: Vec::new(),
            effect,
            busy: BusyState::NotChecked,
            can_stop: false,
        });
    }
    for p in f.processes.iter().filter(|p| {
        !matches!(
            p.kind,
            CodexProcessKind::AppMain
                | CodexProcessKind::AppServer
                | CodexProcessKind::RemoteLink { .. }
        )
    }) {
        let role = process_role(p);
        let effect = effect_with_edit(p.started_at, f.active, f.generations, f.edited);
        rows.push(ConsumerRuntime {
            advice: advice(&role, &effect, f.hot),
            role,
            pid: p.pid,
            core_pid: None,
            started_at: p.started_at,
            codex_home: p.codex_home.clone(),
            cwd: p.cwd.clone(),
            exe: p.exe.clone(),
            cmdline: p.cmdline.clone(),
            effect,
            busy: BusyState::NotChecked,
            can_stop: f.can_stop && crate_stoppable(&p.kind),
        });
    }
    if let Some(busy) = f.busy {
        attribute_busy(&mut rows, busy);
    }
    RuntimeView {
        checked_at: f.now,
        written: f.active.cloned(),
        consumers: rows,
        busy_checked: f.busy.is_some(),
        edited: f.edited.copied(),
    }
}

/// Kinds Superba stops for the user (never the desktop app here).
fn crate_stoppable(kind: &CodexProcessKind) -> bool {
    matches!(
        kind,
        CodexProcessKind::CliTui
            | CodexProcessKind::CliExec
            | CodexProcessKind::CliDaemon
            | CodexProcessKind::IdeAppServer
            | CodexProcessKind::Other
    )
}

/// Which kind of consumer runs a turn, from its session's `originator` (the
/// renderer's own observations are the desktop app's).
fn originator_role(t: &ThreadActivity) -> Option<fn(&ConsumerRole) -> bool> {
    if matches!(
        t.signal,
        ActivitySignal::RendererDom | ActivitySignal::AppSetup
    ) {
        return Some(|r| matches!(r, ConsumerRole::DesktopApp));
    }
    match t.originator.as_deref().map(str::trim) {
        Some(ORIGINATOR_DESKTOP) => Some(|r| matches!(r, ConsumerRole::DesktopApp)),
        Some(ORIGINATOR_TUI) => {
            Some(|r| matches!(r, ConsumerRole::Terminal | ConsumerRole::Daemon))
        }
        Some(ORIGINATOR_EXEC) => Some(|r| matches!(r, ConsumerRole::Exec)),
        Some(o) if o.to_ascii_lowercase().contains("vscode") => {
            Some(|r| matches!(r, ConsumerRole::IdeExtension { .. }))
        }
        _ => None,
    }
}

fn same_dir(a: &Path, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s
        }
    };
    norm(&a.to_string_lossy()) == norm(b)
}

/// Put each busy thread on the rows that may run it: the rows of its
/// session's client kind (a terminal turn on a terminal row, preferring the one
/// in the session's working directory; with no terminal row, the daemon that
/// serves the TUI). A turn of a known client kind without a row is nobody's
/// here; a turn whose client is unknown marks every row: better one warning
/// too many than an interrupted turn nobody was told about. Rows without a
/// thread become [`BusyState::Idle`].
pub fn attribute_busy(rows: &mut [ConsumerRuntime], busy: &[ThreadActivity]) {
    let mut found: Vec<Vec<ThreadActivity>> = vec![Vec::new(); rows.len()];
    for t in busy.iter().filter(|t| t.target_id.is_local()) {
        // A known client kind without a row here (the app's turn while only
        // CLI processes are listed) belongs to none of them.
        let mut idx: Vec<usize> = match originator_role(t) {
            Some(matches) => rows
                .iter()
                .enumerate()
                .filter(|(_, r)| matches(&r.role))
                .map(|(i, _)| i)
                .collect(),
            None => (0..rows.len()).collect(),
        };
        // A TUI turn runs in the daemon only when no terminal row exists.
        if idx.len() > 1
            && idx
                .iter()
                .any(|i| matches!(rows[*i].role, ConsumerRole::Terminal))
        {
            idx.retain(|i| !matches!(rows[*i].role, ConsumerRole::Daemon));
        }
        if idx.len() > 1
            && let Some(cwd) = t.cwd.as_deref()
        {
            let narrowed: Vec<usize> = idx
                .iter()
                .copied()
                .filter(|i| rows[*i].cwd.as_deref().is_some_and(|c| same_dir(c, cwd)))
                .collect();
            if !narrowed.is_empty() {
                idx = narrowed;
            }
        }
        for i in idx {
            found[i].push(t.clone());
        }
    }
    for (row, threads) in rows.iter_mut().zip(found) {
        row.busy = if threads.is_empty() {
            BusyState::Idle
        } else {
            BusyState::Running { threads }
        };
    }
}

/// The busy threads that may belong to `processes` (for the close/stop
/// questions): [`attribute_busy`] over rows built from the processes alone.
pub fn busy_of(processes: &[ProcessInfo], busy: &[ThreadActivity]) -> Vec<ThreadActivity> {
    let facts = RuntimeFacts {
        processes,
        active: None,
        generations: &[],
        edited: None,
        hot: None,
        busy: Some(busy),
        can_stop: false,
        now: Timestamp::EPOCH,
    };
    let mut out: Vec<ThreadActivity> = Vec::new();
    for row in build(&facts).consumers {
        if let BusyState::Running { threads } = row.busy {
            for t in threads {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
    }
    out
}

impl Switcher {
    /// The hot-switch badge of `session` (the overview's rule).
    pub(crate) async fn hot_switch_status(
        &self,
        installs_empty: bool,
        session: Option<&superba_core::AppSession>,
        safe_mode: bool,
    ) -> HotSwitchStatus {
        match session {
            None if installs_empty => HotSwitchStatus::NotInstalled,
            None => HotSwitchStatus::NotRunning,
            Some(_) if safe_mode => HotSwitchStatus::SafeMode,
            Some(s) => match s.cdp.as_ref().filter(|c| c.owner_verified) {
                None => HotSwitchStatus::Unavailable {
                    reason: superba_core::FullRestartReason::NoCdp,
                },
                Some(_) => match self.deps.app.probe_bridge(s).await {
                    Ok(b) if b.is_ready() => HotSwitchStatus::Ready,
                    _ => HotSwitchStatus::Unavailable {
                        reason: superba_core::FullRestartReason::NoBridge,
                    },
                },
            },
        }
    }

    /// [`build`] for the local home from fresh observations; running turns
    /// only with `check_busy` (rollout scan, renderer probe).
    pub(crate) async fn runtime_view(
        &self,
        processes: &[ProcessInfo],
        session: Option<&superba_core::AppSession>,
        hot: Option<&HotSwitchStatus>,
        active: Option<&ActiveState>,
        check_busy: bool,
    ) -> RuntimeView {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let generations = if active.is_some() {
            d.config.generations(&home).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let busy = if check_busy {
            match d.app.turn_activity(&home, session).await {
                Ok(a) => Some(a.busy),
                Err(e) => {
                    tracing::debug!(error = %e, "turn activity for the runtime view");
                    None
                }
            }
        } else {
            None
        };
        let edited = self.local_edit_after(active);
        let mut view = build(&RuntimeFacts {
            processes,
            active,
            generations: &generations,
            edited: edited.as_ref(),
            hot,
            busy: busy.as_deref(),
            can_stop: true,
            now: d.clock.now(),
        });
        if check_busy && busy.is_none() {
            for c in &mut view.consumers {
                c.busy = BusyState::Unknown;
            }
            view.busy_checked = true;
        }
        view
    }

    /// [`superba_core::SwitchService::runtime`].
    pub(crate) async fn runtime_impl(&self) -> superba_core::Result<RuntimeView> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let set = d.profiles.load().await.unwrap_or_default();
        let active = d.config.read_state(&home).await.ok().and_then(|s| s.active);
        let (session, processes) = d.app.session_and_processes(&home).await;
        let session = session.ok().flatten();
        let processes = processes?;
        let installs_empty = self.monitor.facts().await.installs.is_empty();
        let hot = self
            .hot_switch_status(installs_empty, session.as_ref(), set.settings.safe_mode)
            .await;
        Ok(self
            .runtime_view(
                &processes,
                session.as_ref(),
                Some(&hot),
                active.as_ref(),
                true,
            )
            .await)
    }

    /// [`superba_core::SwitchService::stop_processes`].
    pub(crate) async fn stop_processes_impl(
        &self,
        processes: Vec<ProcessInfo>,
        force: bool,
    ) -> superba_core::Result<Vec<StopReport>> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let listed = d.app.processes(&home).await?;
        let mut out = Vec::new();
        for p in processes {
            let current = listed
                .iter()
                .find(|q| q.pid == p.pid && q.kind == p.kind && q.started_at == p.started_at);
            let report = match current {
                None => StopReport {
                    process: p,
                    outcome: StopOutcome::NotRunning,
                    error: None,
                },
                Some(q) if !crate_stoppable(&q.kind) => StopReport {
                    process: p,
                    outcome: StopOutcome::StillRunning,
                    error: Some("the desktop app is not stopped this way".into()),
                },
                Some(q) => match d.app.stop_process(q, force, STOP_WAIT_MS).await {
                    Ok(outcome) => StopReport {
                        process: q.clone(),
                        outcome,
                        error: None,
                    },
                    Err(e) => StopReport {
                        process: q.clone(),
                        outcome: StopOutcome::StillRunning,
                        error: Some(e.to_string()),
                    },
                },
            };
            tracing::info!(
                pid = report.process.pid,
                kind = ?report.process.kind,
                force,
                outcome = ?report.outcome,
                "stopped a Codex process at the user's request"
            );
            out.push(report);
        }
        Ok(out)
    }
}

/// How long a stop request waits for a process to exit.
pub const STOP_WAIT_MS: u64 = 8_000;
