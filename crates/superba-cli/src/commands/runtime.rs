//! "Codex in effect" (GH#4): for every running Codex consumer of this computer,
//! the configuration it runs next to the one written last, whether a turn runs
//! in it, and what makes it use the new configuration. Shared by `spb status`
//! and the report after a switch.

use superba_core::{
    BusyState, ConsumerRuntime, LocalizedText, RunningGeneration, RuntimeEffect, RuntimeView,
    UnknownReason,
};

use crate::Output;
use crate::text::text;

/// What `c` runs, in words.
pub(super) fn effect_text(c: &ConsumerRuntime) -> LocalizedText {
    match &c.effect {
        RuntimeEffect::Current => text("cli.runtime.current"),
        RuntimeEffect::Previous { running } => previous_text(running),
        RuntimeEffect::Unknown { reason } => text(match reason {
            UnknownReason::NothingWritten => "cli.runtime.unknown_nothing",
            UnknownReason::NoStartTime => "cli.runtime.unknown_start",
            UnknownReason::NoCore => "cli.runtime.unknown_core",
            _ => "cli.runtime.unknown_start",
        }),
        _ => text("cli.runtime.unknown_start"),
    }
}

fn previous_text(running: &RunningGeneration) -> LocalizedText {
    match running.generation {
        None => text("cli.runtime.previous_original"),
        Some(g) => match &running.profile_id {
            Some(p) => text("cli.runtime.previous_profile")
                .arg("generation", g)
                .arg("profile", p),
            None => text("cli.runtime.previous").arg("generation", g),
        },
    }
}

fn busy_text(busy: &BusyState) -> LocalizedText {
    text(match busy {
        BusyState::Running { .. } => "cli.runtime.busy_running",
        BusyState::Idle => "cli.runtime.busy_idle",
        BusyState::Unknown => "cli.runtime.busy_unknown",
        _ => "cli.runtime.busy_not_checked",
    })
}

/// The table, then one line of advice per consumer still on an older
/// configuration (and a warning when restarting it interrupts a turn).
pub(super) fn render(view: &RuntimeView, out: &mut Output) -> superba_core::Result<()> {
    match &view.written {
        Some(w) => out.line(
            text("cli.runtime.written")
                .arg("generation", w.generation)
                .arg("at", out.time(w.applied_at)),
        )?,
        None => out.line(text("cli.runtime.written_none"))?,
    }
    if let Some(e) = &view.edited {
        out.line(
            text("cli.runtime.edited")
                .arg("file", e.kind.file_name())
                .arg("at", out.time(e.at)),
        )?;
    }
    // Started after the configuration Superba wrote, before the hand edit.
    let before_edit = |c: &ConsumerRuntime| {
        view.edited.is_some()
            && matches!(&c.effect, RuntimeEffect::Previous { running }
                if running.generation == view.written.as_ref().map(|w| w.generation))
    };
    let rows = view
        .consumers
        .iter()
        .map(|c| {
            vec![
                out.render(&c.role.text()),
                c.pid.to_string(),
                out.optional_time(c.started_at),
                out.render(&if before_edit(c) {
                    text("cli.runtime.before_edit")
                } else {
                    effect_text(c)
                }),
                out.render(&busy_text(&c.busy)),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.runtime.title",
        &[
            "cli.label.consumer",
            "cli.label.pid",
            "cli.label.started",
            "cli.label.runs",
            "cli.label.turn",
        ],
        rows,
    )?;
    let previous = view.previous_count();
    if previous > 0 {
        out.line(text("cli.runtime.summary_old").arg("count", previous))?;
    }
    for c in view.previous() {
        out.line(
            text("cli.runtime.advice")
                .arg_text("role", c.role.text())
                .arg("pid", c.pid)
                .arg_text("advice", c.advice.text()),
        )?;
        if c.busy.is_running() {
            out.line(text("cli.runtime.interrupt"))?;
        }
    }
    Ok(())
}
