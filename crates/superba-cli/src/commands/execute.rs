use std::sync::Arc;
use std::time::Duration;

use superba_core::{
    AuthShape, CheckResult, Error, ExecuteOptions, LocalizedText, OutcomeStatus, PlanChoice,
    PlanSettings, RestartKind, SettingChange, StepStatus, SwitchEvent, SwitchOutcome, SwitchPhase,
    SwitchPlan, SwitchRequest, SwitchStep, TargetId,
};
use superba_switch::messages;

use super::{Result, preview};
use crate::{
    CliEvents, ExecArgs, FlagDecider, Output, Runtime,
    decider::Progress,
    exit,
    text::{destination_label, text},
};

pub(super) async fn run(
    request: SwitchRequest,
    args: &ExecArgs,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    // Start-up recovery (ARCHITECTURE §2): finish or roll back an interrupted
    // switch before planning the next one, like the GUI does when it opens. A
    // held state lock means another Superba is executing; planning still works and
    // `execute` reports the lock.
    if !args.dry_run {
        match rt.service.recover().await {
            Ok(report) => {
                if !report.entries.is_empty() {
                    out.progress(text("cli.recovery.auto").arg("n", report.entries.len()))?;
                }
                for n in &report.notices {
                    out.notice(n)?;
                }
            }
            Err(Error::Locked { .. }) => {}
            Err(e) if e.is_not_implemented() => {}
            Err(e) => return Err(e.into()),
        }
    }
    let plan = rt.service.plan(request).await?;
    // The diff preview (reason and target per key); older services have none.
    let settings = rt.service.plan_settings(&plan.id).await.ok();
    render_plan(&plan, settings.as_ref(), args, rt, out)?;
    if plan.is_blocked() {
        if let RestartKind::Blocked { reasons } = &plan.primary {
            for reason in reasons {
                out.progress(reason.text())?;
            }
        }
        return planned("blocked", exit::BLOCKED, &plan, settings.as_ref(), out);
    }
    if !plan.choices.is_empty() {
        // GH#5: what another tool changed, and (at a terminal) the answer.
        for choice in &plan.choices {
            external_rows(choice, out)?;
        }
        if let Some(next) = ask_external(&plan, args, rt, out)? {
            return match next {
                Some(request) => Box::pin(run(request, args, rt, out)).await,
                None => {
                    out.line(messages::outcome(OutcomeStatus::Cancelled))?;
                    planned("cancelled", exit::CANCELLED, &plan, settings.as_ref(), out)
                }
            };
        }
        for choice in &plan.choices {
            out.line(messages::choice(choice))?;
            out.line(text(match choice {
                PlanChoice::LegacySessions { .. } => "cli.hint.legacy_sessions",
                PlanChoice::ReplacementModel { .. } => "cli.hint.model",
                PlanChoice::ForeignEnv { .. } => "cli.hint.env",
                PlanChoice::UnknownVersion { .. } => "cli.hint.version",
                PlanChoice::OwnedKeyConflict { .. } => "cli.hint.keys",
                PlanChoice::SharedHome { .. } => "cli.hint.shared_home",
                PlanChoice::RestoreProvider { .. } => "cli.hint.provider",
                _ => "cli.hint.answers",
            }))?;
        }
        return planned(
            "needs_answers",
            exit::NEEDS_CONFIRMATION,
            &plan,
            settings.as_ref(),
            out,
        );
    }
    if args.dry_run {
        return planned("planned", exit::OK, &plan, settings.as_ref(), out);
    }
    if !plan.active_threads.is_empty() && !(args.wait_turn || args.force) {
        out.progress(text("cli.hint.turns"))?;
        out.line(messages::outcome(OutcomeStatus::Cancelled))?;
        // Preserve the cancellation document's outcome shape without executing
        // a plan which the flags have already declined.
        let outcome = SwitchOutcome {
            plan_id: plan.id,
            status: OutcomeStatus::Cancelled,
            elapsed_ms: 0,
            generation: None,
            restart: plan.primary,
            verification: None,
            remotes: Vec::new(),
            stale_processes: Vec::new(),
            runtime: None,
            returned_to: None,
            notices: Vec::new(),
        };
        out.value(&serde_json::json!({ "status": outcome.status, "outcome": outcome }))?;
        return Ok(exit::CANCELLED);
    }
    if plan.needs_confirmation() && !(args.yes || args.allow_full_restart || args.write_only) {
        for line in messages::confirm_full(&plan.primary) {
            out.line(line)?;
        }
        out.line(text("cli.hint.confirm"))?;
        return planned(
            "needs_confirmation",
            exit::NEEDS_CONFIRMATION,
            &plan,
            settings.as_ref(),
            out,
        );
    }
    if plan.countdown_allowed() && args.countdown > 0 {
        countdown(&plan, args.countdown, out).await?;
    }
    let (events, mut receiver) = CliEvents::channel(out.json, rt.clock.clone());
    let destination = destination_label(&plan);
    let mut decider = FlagDecider::new(args.force, args.wait_turn, args.yes)
        .with_stop_codex(args.stop_codex)
        .with_events(events.clone());
    // The one question a running switch asks on a terminal: stop the Codex
    // processes it needs closed (GH#2).
    if rt.interactive && !out.json && !args.stop_codex {
        decider = decider.with_prompt(rt.read_stdin_line.clone());
    }
    let decider = Arc::new(decider);
    let execution = rt.service.execute(
        plan,
        ExecuteOptions {
            write_only: args.write_only,
            force_turns: args.force,
            allow_full_restart: args.yes || args.allow_full_restart,
            ..Default::default()
        },
        decider,
        events,
    );
    tokio::pin!(execution);
    let mut open = true;
    let mut output_error = None;
    let mut rendered = RenderedProgress::default();
    let result = loop {
        tokio::select! {
            result = &mut execution => break result,
            message = receiver.recv(), if open => match message {
                Some(message) => {
                    if let Err(error) = rendered.render(message, out) { output_error.get_or_insert(error); }
                }
                None => open = false,
            }
        }
    };
    while let Ok(message) = receiver.try_recv() {
        if let Err(error) = rendered.render(message, out) {
            output_error.get_or_insert(error);
        }
    }
    // Never drop a live execution just because its progress writer failed.
    let outcome = result?;
    if let Some(error) = output_error {
        return Err(error.into());
    }
    render_outcome(&outcome, &destination, &rendered, out)?;
    out.value(&serde_json::json!({ "status": outcome.status, "outcome": outcome }))?;
    // Written, but the check of the restarted Codex core failed: not a
    // success for scripts either (user bug 69b).
    let unverified = outcome.verification.as_ref().is_some_and(|v| !v.passed());
    Ok(match outcome.status {
        OutcomeStatus::WrittenOnly if unverified => exit::ERROR,
        OutcomeStatus::Switched | OutcomeStatus::WrittenOnly => exit::OK,
        OutcomeStatus::Cancelled => exit::CANCELLED,
        OutcomeStatus::RolledBack | OutcomeStatus::Failed | OutcomeStatus::Partial => exit::ERROR,
    })
}

/// The rows of an owned-key conflict: who, each key, the credential.
fn external_rows(choice: &PlanChoice, out: &mut Output) -> superba_core::Result<()> {
    let PlanChoice::OwnedKeyConflict {
        conflicts,
        auth,
        source,
    } = choice
    else {
        return Ok(());
    };
    if out.json {
        return Ok(());
    }
    out.line(text("cli.external.title").arg_text("source", source.text()))?;
    for k in conflicts {
        out.line(
            text("cli.format.change")
                .arg("path", &k.path)
                .arg("before", out.optional(k.before.as_ref()))
                .arg("after", out.optional(k.after.as_ref())),
        )?;
    }
    if let Some(a) = auth {
        out.line(
            text("cli.format.change")
                .arg("path", superba_core::AUTH_JSON_PATH)
                .arg(
                    "before",
                    out.render(&superba_core::auth_shape_label(&a.written)),
                )
                .arg(
                    "after",
                    out.render(&superba_core::auth_shape_label(&a.live)),
                ),
        )?;
    }
    Ok(())
}

/// GH#5: a person at a terminal answers a plan whose only question is a
/// change another tool made: take it over (`Some(Some(request))` with every
/// change answered `Overwrite`), let that tool manage it (a hand-off request),
/// keep its values (only without a changed credential) or cancel
/// (`Some(None)`). `None`: not asked (flags, JSON, other questions).
fn ask_external(
    plan: &SwitchPlan,
    args: &ExecArgs,
    rt: &Runtime,
    out: &mut Output,
) -> superba_core::Result<Option<Option<SwitchRequest>>> {
    let [PlanChoice::OwnedKeyConflict { auth, source, .. }] = plan.choices.as_slice() else {
        return Ok(None);
    };
    if out.json || args.dry_run || !out.interactive() {
        return Ok(None);
    }
    // A switch cannot keep another tool's credential: take over or cancel.
    let keep_possible = auth.is_none();
    out.line(text("cli.external.ask").arg_text(
        "tool",
        match source.tool() {
            Some(_) => source.text(),
            None => text("cli.external.other_tool"),
        },
    ))?;
    if keep_possible {
        out.line(text("cli.external.ask_keep"))?;
    }
    let answer = (rt.read_stdin_line)()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    Ok(Some(match answer.as_str() {
        "t" | "take" | "take over" => {
            let mut request = plan.request.clone();
            superba_switch::ownership::take_over(&mut request);
            Some(request)
        }
        "k" | "keep" if keep_possible => {
            let mut request = plan.request.clone();
            request.key_resolutions.insert(
                superba_core::ALL_CHANGED.to_owned(),
                superba_core::KeyResolution::KeepCurrent,
            );
            Some(request)
        }
        "h" | "hand off" | "hand-off" => Some(super::request(
            superba_core::SwitchIntent::HandOff { to: source.clone() },
            args,
        )),
        _ => None,
    }))
}

fn planned(
    status: &str,
    code: u8,
    plan: &SwitchPlan,
    settings: Option<&PlanSettings>,
    out: &mut Output,
) -> Result {
    out.value(&serde_json::json!({ "status": status, "plan": plan, "settings": settings }))?;
    Ok(code)
}

/// The diff preview: `path: before → after (reason)`, grouped by Codex home.
pub(super) fn render_changes(
    changes: &[SettingChange],
    out: &mut Output,
) -> superba_core::Result<()> {
    if changes.is_empty() {
        return out.line(text("cli.settings.no_changes"));
    }
    let mut target: Option<&TargetId> = None;
    for c in changes {
        if target != Some(&c.target) {
            target = Some(&c.target);
            let label = if c.target.is_local() {
                out.label("cli.settings.local_target")
            } else {
                c.target_label.clone()
            };
            out.line(text("cli.format.section").arg("title", label))?;
        }
        out.line(
            text("cli.format.change_reason")
                .arg("path", &c.path)
                .arg("before", out.optional(c.from.as_ref()))
                .arg("after", out.optional(c.to.as_ref()))
                .arg_text("reason", c.reason.text()),
        )?;
    }
    Ok(())
}

async fn countdown(plan: &SwitchPlan, seconds: u32, out: &mut Output) -> superba_core::Result<()> {
    let cancel = tokio::signal::ctrl_c();
    tokio::pin!(cancel);
    // Poll once to install the signal handler before showing the first banner.
    // Otherwise an immediate Ctrl-C could terminate the process instead of
    // returning the CLI's stable cancellation code and JSON error.
    tokio::select! {
        biased;
        result = &mut cancel => {
            result.map_err(|e| Error::io("signal", &e))?;
            return Err(Error::Cancelled);
        }
        () = std::future::ready(()) => {}
    }
    let destination = destination_label(plan);
    // Keep the interruption warning visible once, then update only the timer.
    for line in messages::countdown_banner(&destination, u64::from(seconds))
        .into_iter()
        .skip(1)
    {
        out.progress(line)?;
    }
    for remaining in (1..=seconds).rev() {
        out.countdown_tick(
            text("cli.countdown.tick")
                .arg_text("destination", destination.clone())
                .arg("seconds", remaining),
        )?;
        tokio::select! {
            biased;
            result = &mut cancel => {
                out.finish_progress()?;
                result.map_err(|e| Error::io("signal", &e))?;
                return Err(Error::Cancelled);
            }
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
    out.finish_progress()?;
    Ok(())
}

fn render_plan(
    plan: &SwitchPlan,
    settings: Option<&PlanSettings>,
    args: &ExecArgs,
    rt: &Runtime,
    out: &mut Output,
) -> superba_core::Result<()> {
    if out.json {
        return Ok(());
    }
    out.field(
        "cli.label.destination",
        out.render(&destination_label(plan)),
    )?;
    out.field(
        "cli.label.restart",
        out.render(&messages::restart(&plan.primary)),
    )?;
    if let RestartKind::FullAppRestart { reason } = &plan.primary {
        out.line(reason.text())?;
    }
    for restart in &plan.restarts {
        out.line(
            text("cli.format.consumer")
                .arg_text("consumer", messages::consumer(&restart.consumer))
                .arg_text("restart", messages::restart(&restart.restart))
                .arg("count", restart.pids.len()),
        )?;
    }
    for warning in &plan.warnings {
        out.notice(warning)?;
    }
    if let Some(settings) = settings.filter(|s| !s.changes.is_empty()) {
        render_changes(&settings.changes, out)?;
    }
    if let Some(config) = &plan.config {
        if settings.is_none_or(|s| s.changes.is_empty()) {
            for change in &config.key_changes {
                out.line(
                    text("cli.format.change")
                        .arg("path", &change.path)
                        .arg("before", out.optional(change.before.as_ref()))
                        .arg("after", out.optional(change.after.as_ref())),
                )?;
            }
        }
        if let Some(auth) = &config.auth_change {
            out.line(
                text("cli.format.change")
                    .arg("path", "auth.json")
                    .arg("before", auth_shape(&auth.before, out))
                    .arg("after", auth_shape(&auth.after, out)),
            )?;
            out.field("cli.label.stash_login", out.boolean(auth.stash_login))?;
        }
        for notice in &config.notes {
            out.notice(notice)?;
        }
    }
    if let Some(env) = &plan.env {
        preview::environment(env, args, rt, out)?;
    }
    if let Some(migration) = &plan.migration {
        // One item per session; `thread_count` counts rows in every thread table
        // (threads and catalogs), so it is shown as database rows.
        let sessions = if migration.items.is_empty() {
            migration.thread_count.max(migration.rollout_count)
        } else {
            u32::try_from(migration.items.len()).unwrap_or(u32::MAX)
        };
        out.line(
            text("cli.format.migration")
                .arg("sessions", sessions)
                .arg("rows", migration.thread_count)
                .arg("rollouts", migration.rollout_count)
                .arg("provider", &migration.request.to_provider),
        )?;
        for (provider, count) in &migration.by_source_provider {
            out.line(
                text("cli.format.source_count")
                    .arg("provider", provider)
                    .arg("count", count),
            )?;
        }
    }
    for alternative in &plan.alternatives {
        // What the alternative does, then the command that runs it.
        let label = out.render(&alternative.label);
        out.field(
            "cli.label.alternative",
            match preview::alternative(&alternative.request, args, out) {
                Some(command) => format!("{label} — {command}"),
                None => label,
            },
        )?;
    }
    for thread in &plan.active_threads {
        out.line(messages::busy_thread(thread))?;
    }
    for item in &plan.cleanup {
        out.line(item.clone())?;
    }
    Ok(())
}

fn auth_shape(shape: &AuthShape, out: &Output) -> String {
    match shape {
        AuthShape::Missing => out.label("cli.value.absent"),
        AuthShape::Unknown { detail } => out.render(
            &text("cli.format.detail")
                .arg("label", out.label("cli.value.unknown"))
                .arg("detail", detail),
        ),
        AuthShape::ApiKey { key_last4 } | AuthShape::Pat { key_last4 } => out.render(
            &text("cli.format.auth_shape")
                .arg(
                    "mode",
                    shape
                        .mode()
                        .map(|mode| out.render(&mode.text()))
                        .unwrap_or_else(|| out.none()),
                )
                .arg("last4", key_last4),
        ),
        _ => shape
            .mode()
            .map(|mode| out.render(&mode.text()))
            .unwrap_or_else(|| out.label("cli.value.unknown")),
    }
}

#[derive(Default)]
struct RenderedProgress {
    steps: Vec<(SwitchStep, StepStatus)>,
    logs: Vec<LocalizedText>,
}

impl RenderedProgress {
    fn render(&mut self, message: Progress, out: &mut Output) -> superba_core::Result<()> {
        match &message {
            Progress::Event(SwitchEvent::Step { step, status, .. }) => {
                self.steps.retain(|(previous, _)| previous != step);
                self.steps.push((step.clone(), status.clone()));
            }
            Progress::Event(SwitchEvent::Log { text, .. }) => self.logs.push(text.clone()),
            // A run that ends with a failed step (ChatGPT not reopened, a failed
            // check) is not "Done" (user bug 40).
            Progress::Event(SwitchEvent::Phase {
                phase: SwitchPhase::Done,
                ..
            }) if self
                .steps
                .iter()
                .any(|(_, status)| matches!(status, StepStatus::Failed { .. })) =>
            {
                return out.progress(
                    text("cli.format.phase")
                        .arg_text("phase", messages::text("switch.phase.done_failed")),
                );
            }
            _ => {}
        }
        message.render(out)
    }

    fn check_shown(&self, step: &SwitchStep, check: &CheckResult) -> bool {
        self.steps.iter().any(|(shown, status)| {
            shown == step
                && match (status, check) {
                    (StepStatus::Done, CheckResult::Passed) => true,
                    (StepStatus::Failed { message }, CheckResult::Failed { detail }) => {
                        message == detail
                    }
                    _ => false,
                }
        })
    }
}

fn render_outcome(
    outcome: &SwitchOutcome,
    destination: &LocalizedText,
    rendered: &RenderedProgress,
    out: &mut Output,
) -> superba_core::Result<()> {
    if out.json {
        return Ok(());
    }
    let summary = outcome.notices.iter().find(|notice| {
        matches!(
            notice.text.key.as_str(),
            "switch.done.switched"
                | "switch.done.restored"
                | "switch.done.written_only"
                | "switch.done.reopened_unverified"
                | "switch.done.launch_failed"
        )
    });
    if let Some(summary) = summary {
        if !rendered.logs.contains(&summary.text) {
            out.line(
                summary
                    .text
                    .clone()
                    .arg_text("destination", destination.clone()),
            )?;
        }
    } else {
        out.line(messages::outcome(outcome.status))?;
        out.field(
            "cli.label.elapsed",
            out.render(
                &text("cli.format.seconds")
                    .arg("seconds", messages::seconds_1dp(outcome.elapsed_ms)),
            ),
        )?;
    }
    if let Some(verification) = &outcome.verification {
        for (step, check) in [
            (SwitchStep::VerifyProcess, &verification.process),
            (SwitchStep::VerifyFiles, &verification.files),
            (SwitchStep::VerifyAuth, &verification.auth),
            (SwitchStep::VerifyLine, &verification.line),
            (SwitchStep::VerifyRenderer, &verification.renderer),
        ] {
            if rendered.check_shown(&step, check) {
                continue;
            }
            let status = match check {
                CheckResult::Passed => text("cli.check.passed"),
                CheckResult::Pending => messages::step_status(&superba_core::StepStatus::Pending),
                CheckResult::Failed { detail } => {
                    messages::step_status(&superba_core::StepStatus::Failed {
                        message: detail.clone(),
                    })
                }
                CheckResult::Skipped { reason } => text("cli.format.detail")
                    .arg_text(
                        "label",
                        messages::step_status(&superba_core::StepStatus::Skipped),
                    )
                    .arg("detail", reason),
            };
            out.line(
                text("cli.format.step")
                    .arg_text("step", messages::step(&step))
                    .arg_text("status", status),
            )?;
        }
    }
    for remote in &outcome.remotes {
        out.line(
            text("cli.format.field")
                .arg("label", &remote.target_id)
                .arg_text("value", messages::outcome(remote.status)),
        )?;
        if let Some(error) = &remote.error {
            out.line(error.to_text())?;
        }
    }
    if !outcome.stale_processes.is_empty() {
        out.field("cli.label.stale", outcome.stale_processes.len())?;
    }
    for process in &outcome.stale_processes {
        out.line(
            text("cli.format.process")
                .arg("pid", process.pid)
                .arg("started", out.optional_time(process.started_at)),
        )?;
    }
    // What every running Codex consumer uses now (GH#4).
    if let Some(view) = outcome.runtime.as_ref().filter(|v| !v.consumers.is_empty()) {
        super::runtime::render(view, out)?;
    }
    for notice in &outcome.notices {
        if summary.is_some_and(|summary| std::ptr::eq(summary, notice))
            || rendered.logs.contains(&notice.text)
        {
            continue;
        }
        out.notice(notice)?;
    }
    Ok(())
}
