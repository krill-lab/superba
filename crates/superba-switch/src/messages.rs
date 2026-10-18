//! Shared wording of the switch flow (plan 7.3): every state, step, restart kind,
//! decision, question, plan choice, warning and toast as [`LocalizedText`].
//!
//! The English catalog (`locales/en.toml`) is the single source of the English
//! text: [`text`] takes the fallback from it, so code and catalog cannot drift.
//! The GUI and `spb` render everything with [`catalog`] (core keys + these keys,
//! plus their own UI layers on top).

use std::collections::HashMap;
use std::sync::LazyLock;

use superba_core::i18n::flatten_toml;
use superba_core::{
    Catalog, ConsumerKind, Decision, DecisionRequest, Locale, LocalizedText, OutcomeStatus,
    PlanChoice, RestartKind, StepStatus, SwitchPhase, SwitchStep, ThreadActivity,
};

/// English switch catalog source.
pub const SWITCH_EN: &str = include_str!("../locales/en.toml");
/// Simplified Chinese switch catalog source.
pub const SWITCH_ZH_CN: &str = include_str!("../locales/zh-CN.toml");

static ENGLISH: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| flatten_toml(SWITCH_EN).unwrap_or_default());

/// Raw TOML of the switch catalog for `locale` (parity tests in other crates).
pub fn catalog_source(locale: Locale) -> &'static str {
    match locale {
        Locale::En => SWITCH_EN,
        Locale::ZhCn => SWITCH_ZH_CN,
    }
}

/// A catalog for `locale` with the core keys, the texts the planner passes on from
/// superba-config (`config.*`, `env.*`), superba-sessions (`sessions.*`) and
/// superba-catalog (`catalog.*`), and the switch keys. Binaries add their own UI
/// layer with [`Catalog::add_toml`].
pub fn catalog(locale: Locale) -> Catalog {
    let mut c = Catalog::new(locale);
    // All catalogs are compiled in and parsed by the tests; a failure is a bug and
    // only drops that layer (English fallbacks still render).
    for (l, source) in superba_config::catalog_sources() {
        let _ = c.add_toml(l, source);
    }
    for l in [Locale::En, Locale::ZhCn] {
        let _ = c.add_toml(l, superba_sessions::catalog_source(l));
        let _ = c.add_toml(l, superba_catalog::i18n_source(l));
    }
    let _ = c.add_toml(Locale::En, SWITCH_EN);
    let _ = c.add_toml(Locale::ZhCn, SWITCH_ZH_CN);
    c
}

/// Text for a switch catalog key, with the English template as fallback.
pub fn text(key: &str) -> LocalizedText {
    let fallback = ENGLISH.get(key).cloned().unwrap_or_else(|| key.to_owned());
    LocalizedText::new(key, fallback)
}

/// True if `key` exists in the English switch catalog.
pub fn has_key(key: &str) -> bool {
    ENGLISH.contains_key(key)
}

/// Stable snake_case name of a phase (also its catalog key suffix).
pub fn phase_id(phase: SwitchPhase) -> &'static str {
    match phase {
        SwitchPhase::Idle => "idle",
        SwitchPhase::Planning => "planning",
        SwitchPhase::Blocked => "blocked",
        SwitchPhase::Armed => "armed",
        SwitchPhase::ConfirmFull => "confirm_full",
        SwitchPhase::TurnCheck => "turn_check",
        SwitchPhase::WaitingTurn => "waiting_turn",
        SwitchPhase::Quitting => "quitting",
        SwitchPhase::ForceQuitConfirm => "force_quit_confirm",
        SwitchPhase::Applying => "applying",
        SwitchPhase::RollingBack => "rolling_back",
        SwitchPhase::Restarting => "restarting",
        SwitchPhase::FullRestartOffer => "full_restart_offer",
        SwitchPhase::Launching => "launching",
        SwitchPhase::Verifying => "verifying",
        SwitchPhase::NavRestore => "nav_restore",
        SwitchPhase::Recovery => "recovery",
        SwitchPhase::RollbackRestart => "rollback_restart",
        SwitchPhase::Done => "done",
    }
}

/// All phases, in plan 7.2 order.
pub const ALL_PHASES: [SwitchPhase; 19] = [
    SwitchPhase::Idle,
    SwitchPhase::Planning,
    SwitchPhase::Blocked,
    SwitchPhase::Armed,
    SwitchPhase::ConfirmFull,
    SwitchPhase::TurnCheck,
    SwitchPhase::WaitingTurn,
    SwitchPhase::Quitting,
    SwitchPhase::ForceQuitConfirm,
    SwitchPhase::Applying,
    SwitchPhase::RollingBack,
    SwitchPhase::Restarting,
    SwitchPhase::FullRestartOffer,
    SwitchPhase::Launching,
    SwitchPhase::Verifying,
    SwitchPhase::NavRestore,
    SwitchPhase::Recovery,
    SwitchPhase::RollbackRestart,
    SwitchPhase::Done,
];

/// Display name of a state.
pub fn phase(phase: SwitchPhase) -> LocalizedText {
    text(&format!("switch.phase.{}", phase_id(phase)))
}

/// Display name of a progress step.
pub fn step(step: &SwitchStep) -> LocalizedText {
    let key = match step {
        SwitchStep::WriteConfig => "write_config",
        SwitchStep::WriteEnv => "write_env",
        SwitchStep::MigrateSessions => "migrate_sessions",
        SwitchStep::QuitApp => "quit_app",
        SwitchStep::RestartCore => "restart_core",
        SwitchStep::StopProcesses => "stop_processes",
        SwitchStep::LaunchApp => "launch_app",
        SwitchStep::VerifyProcess => "verify_process",
        SwitchStep::VerifyFiles => "verify_files",
        SwitchStep::VerifyAuth => "verify_auth",
        SwitchStep::VerifyLine => "verify_line",
        SwitchStep::VerifyRenderer => "verify_renderer",
        SwitchStep::Remote { target_id } => {
            return text("switch.step.remote").arg("host", target_label(target_id.as_str()));
        }
        _ => "write_config",
    };
    text(&format!("switch.step.{key}"))
}

/// Display text of a step status.
pub fn step_status(status: &StepStatus) -> LocalizedText {
    match status {
        StepStatus::Pending => text("switch.step_status.pending"),
        StepStatus::Running => text("switch.step_status.running"),
        StepStatus::Done => text("switch.step_status.done"),
        StepStatus::Skipped => text("switch.step_status.skipped"),
        StepStatus::Failed { message } => text("switch.step_status.failed").arg("message", message),
    }
}

/// Display text of an outcome status.
pub fn outcome(status: OutcomeStatus) -> LocalizedText {
    let key = match status {
        OutcomeStatus::Switched => "switched",
        OutcomeStatus::WrittenOnly => "written_only",
        OutcomeStatus::Cancelled => "cancelled",
        OutcomeStatus::RolledBack => "rolled_back",
        OutcomeStatus::Partial => "partial",
        OutcomeStatus::Failed => "failed",
    };
    text(&format!("switch.outcome.{key}"))
}

/// Display text of a restart decision.
pub fn restart(kind: &RestartKind) -> LocalizedText {
    match kind {
        RestartKind::Blocked { .. } => text("switch.restart.blocked"),
        RestartKind::None => text("switch.restart.none"),
        RestartKind::AppServerHot { host_id, .. }
            if host_id != superba_core::codex::LOCAL_HOST_ID =>
        {
            text("switch.restart.app_server_hot_remote").arg("host", host_id)
        }
        RestartKind::AppServerHot { .. } => text("switch.restart.app_server_hot"),
        RestartKind::FullAppRestart { .. } => text("switch.restart.full_app_restart"),
        RestartKind::NextLaunch => text("switch.restart.next_launch"),
        RestartKind::DaemonRestart => text("switch.restart.daemon_restart"),
    }
}

/// Display name of a consumer group.
pub fn consumer(kind: &ConsumerKind) -> LocalizedText {
    match kind {
        ConsumerKind::DesktopApp => text("switch.consumer.desktop_app"),
        ConsumerKind::CliProcesses => text("switch.consumer.cli_processes"),
        ConsumerKind::CliDaemon => text("switch.consumer.cli_daemon"),
        ConsumerKind::IdeAppServers => text("switch.consumer.ide_app_servers"),
        ConsumerKind::Remote { target_id } => {
            text("switch.consumer.remote").arg("host", target_label(target_id.as_str()))
        }
        _ => text("switch.consumer.desktop_app"),
    }
}

/// Button label of a decision.
pub fn decision(d: Decision) -> LocalizedText {
    let key = match d {
        Decision::Wait => "wait_turns",
        Decision::Force => "force",
        Decision::Cancel => "cancel",
        Decision::FullRestart => "full_restart",
        Decision::Undo => "undo",
        Decision::KeepWritten => "keep_written",
        Decision::Rollback => "rollback",
        Decision::Continue => "continue",
        Decision::StopProcesses => "stop_processes",
    };
    text(&format!("switch.action.{key}"))
}

/// Body text of a mid-execution question.
pub fn question(request: &DecisionRequest) -> LocalizedText {
    match request {
        DecisionRequest::WaitForTurns { busy, .. } => {
            text("switch.waiting.title").arg("count", busy.len())
        }
        DecisionRequest::ForceQuit { .. } => text("switch.question.force_quit"),
        DecisionRequest::IpcFailed { error } => {
            text("switch.question.ipc_failed").arg_text("error", error.to_text())
        }
        DecisionRequest::VerificationFailed { verification } => {
            text("switch.question.verification_failed")
                .arg("detail", crate::rules::verification_summary(verification))
        }
        DecisionRequest::UnknownRemoteActivity { targets } => {
            let hosts: Vec<String> = targets.iter().map(|t| target_label(t.as_str())).collect();
            text("switch.question.unknown_remote_activity").arg("hosts", hosts.join(", "))
        }
        DecisionRequest::CloseConsumers { processes, .. } => {
            text("switch.question.close_consumers").arg("processes", program_list(processes))
        }
        DecisionRequest::ForceStop { processes, .. } => {
            text("switch.question.force_stop").arg("processes", program_list(processes))
        }
        _ => text("switch.phase.recovery"),
    }
}

/// "codex (pid 1234), Code (pid 99)": program names from the command lines.
fn program_list(processes: &[superba_core::ProcessInfo]) -> String {
    processes
        .iter()
        .map(|p| {
            let name = p
                .cmdline
                .first()
                .map(|a| a.rsplit(['/', '\\']).next().unwrap_or(a).to_owned())
                .unwrap_or_else(|| "codex".to_owned());
            format!("{name} (pid {})", p.pid)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// "codex in a terminal (pid 1234)".
pub fn process_label(p: &superba_core::ProcessInfo) -> LocalizedText {
    text("switch.stop.process")
        .arg_text("role", superba_core::process_role(p).text())
        .arg("pid", p.pid)
}

/// Log line of one stopped process.
pub fn stop_result(
    p: &superba_core::ProcessInfo,
    outcome: superba_core::StopOutcome,
) -> LocalizedText {
    text("switch.stop.result")
        .arg_text("process", process_label(p))
        .arg_text("outcome", outcome.text())
}

/// Log line of a process that could not be stopped.
pub fn stop_failed(p: &superba_core::ProcessInfo, error: &str) -> LocalizedText {
    text("switch.stop.failed")
        .arg_text("process", process_label(p))
        .arg("error", error)
}

/// One line per busy thread for the WaitingTurn dialog.
pub fn busy_thread(t: &ThreadActivity) -> LocalizedText {
    if t.signal == superba_core::ActivitySignal::AppSetup {
        // The app's own first-start setup, not a conversation (user bug 59).
        return text("switch.waiting.app_setup");
    }
    let base = if t.target_id.is_local() {
        text("switch.waiting.thread_local")
    } else {
        text("switch.waiting.thread_remote").arg("host", target_label(t.target_id.as_str()))
    };
    match &t.title {
        Some(title) if !title.trim().is_empty() => base.arg("title", title.trim()),
        _ => base.arg_text("title", text("switch.waiting.untitled")),
    }
}

/// Question text of a plan choice.
pub fn choice(c: &PlanChoice) -> LocalizedText {
    match c {
        PlanChoice::LegacySessions { providers } => {
            let count: u32 = providers.values().sum();
            let names: Vec<&str> = providers.keys().map(String::as_str).collect();
            text("switch.choice.legacy_sessions")
                .arg("count", count)
                .arg("provider", names.join(", "))
        }
        PlanChoice::RestoreProvider {
            baseline_provider,
            created_during,
            migrated_in,
        } => text("switch.choice.restore_provider")
            .arg("provider", baseline_provider)
            .arg("created", created_during)
            .arg("migrated", migrated_in),
        PlanChoice::ReplacementModel {
            current,
            suggestions,
        } => text("switch.choice.replacement_model")
            .arg("model", current)
            .arg("suggestions", suggestions.join(", ")),
        PlanChoice::ForeignEnv { lines } => {
            let shown: Vec<String> = lines
                .iter()
                .map(|l| format!("{}:{} = {}", l.path.display(), l.line_no, l.value))
                .collect();
            text("switch.choice.foreign_env").arg("lines", shown.join("; "))
        }
        PlanChoice::UnknownVersion { consumer } => {
            text("switch.choice.unknown_version").arg("consumer", consumer)
        }
        PlanChoice::OwnedKeyConflict {
            conflicts,
            auth,
            source,
        } => {
            let mut keys: Vec<&str> = conflicts.iter().map(|k| k.path.as_str()).collect();
            if auth.is_some() {
                keys.push(superba_core::AUTH_JSON_PATH);
            }
            text("switch.choice.owned_key_conflict")
                .arg("keys", keys.join(", "))
                .arg_text("source", source.text())
        }
        PlanChoice::SharedHome { targets } => {
            let hosts: Vec<String> = targets.iter().map(|t| target_label(t.as_str())).collect();
            text("switch.choice.shared_home").arg("hosts", hosts.join(", "))
        }
        _ => text("switch.phase.planning"),
    }
}

/// The countdown banner of a hot plan (title + body + bullets), plan 7.3.
pub fn countdown_banner(destination: &LocalizedText, seconds: u64) -> Vec<LocalizedText> {
    vec![
        text("switch.countdown.title")
            .arg("seconds", seconds)
            .arg_text("destination", destination.clone()),
        text("switch.countdown.body"),
        text("switch.countdown.bullet_turns"),
        text("switch.countdown.bullet_mcp"),
        text("switch.countdown.bullet_commands"),
    ]
}

/// The full-restart confirmation (title, reason, body), plan 7.3.
pub fn confirm_full(kind: &RestartKind) -> Vec<LocalizedText> {
    let mut out = vec![text("switch.confirm.title")];
    if let RestartKind::FullAppRestart { reason } = kind {
        out.push(text("switch.confirm.reason").arg_text("reason", reason.text()));
    }
    out.push(text("switch.confirm.body"));
    out
}

/// `ssh:devbox` -> `devbox`; other ids unchanged.
pub fn target_label(target_id: &str) -> String {
    let key = target_id.strip_prefix("ssh:").unwrap_or(target_id);
    // A WSL distribution is called the way the app calls it (user bug 45).
    match superba_core::wsl::distro_of_host_id(key) {
        Some(distro) => superba_remote::discover::wsl_label(&distro),
        None => key.to_owned(),
    }
}

/// Seconds with one decimal for toasts (`1.2`).
pub fn seconds_1dp(ms: u64) -> String {
    format!("{}.{}", ms / 1000, (ms % 1000) / 100)
}
