//! Session migration progress as switch events: the bridge from the migrator's
//! [`SessionProgressSink`] to the [`SwitchEvents`] of an execution
//! ([`SwitchEvent::Sessions`] reports; the UI's stop request flows back through
//! [`SwitchEvents::stop_requested`]), and the shared wording of a progress line
//! (`spb`, the GUI's migration dialog).

use std::path::Path;
use std::sync::Arc;

use superba_core::{
    LocalizedText, SessionDirection, SessionProgress, SessionProgressSink, SessionStage,
    SwitchEvent, SwitchEvents,
};

use crate::messages::text;

/// Forwards a migrator's reports to the execution's events.
pub(crate) struct SessionEvents {
    events: Arc<dyn SwitchEvents>,
}

impl SessionEvents {
    /// A sink for one migration or rollback of an execution.
    pub(crate) fn sink(events: Arc<dyn SwitchEvents>) -> Arc<dyn SessionProgressSink> {
        Arc::new(Self { events })
    }
}

impl SessionProgressSink for SessionEvents {
    fn report(&self, progress: SessionProgress) {
        self.events.emit(SwitchEvent::Sessions { progress });
    }

    fn stop_requested(&self) -> bool {
        self.events.stop_requested()
    }
}

/// The stage with its counter ([`SessionProgress::counter`]): "Reading 200 of
/// 600 sessions", "Backed up 312 of 601 files", "Migrated 120 of 600
/// sessions", "Updating databases: 1 of 2", "Checked 90 of 600 files";
/// restores: "Checking ... files", "Restored 3 of 12 sessions".
pub fn session_count(p: &SessionProgress) -> LocalizedText {
    let key = match (p.stage, p.direction) {
        (SessionStage::Reading, SessionDirection::Migrate) => "switch.sessions.count.reading",
        (SessionStage::Reading, SessionDirection::Restore) => "switch.sessions.count.checking",
        (SessionStage::BackingUp, _) => "switch.sessions.count.backing_up",
        (SessionStage::Databases, _) => "switch.sessions.count.databases",
        (SessionStage::Verifying, _) => "switch.sessions.count.verifying",
        (_, SessionDirection::Migrate) => "switch.sessions.migrated",
        (_, SessionDirection::Restore) => "switch.sessions.restored",
    };
    let (n, of) = p.counter();
    text(key).arg("done", n).arg("total", of)
}

/// The overall percentage, from the same weighting as the counter's stage
/// (rounded down; the epsilon keeps 75 % from showing as 74 % after the f32
/// sum of the stage shares).
pub fn session_percent(p: &SessionProgress) -> u32 {
    (f64::from(p.fraction()) * 100.0 + 1e-3).floor().min(100.0) as u32
}

/// What the stage is doing ("Backing up the sessions").
pub fn session_stage(p: &SessionProgress) -> LocalizedText {
    text(match (p.stage, p.direction) {
        (SessionStage::Reading, _) => "switch.sessions.stage.reading",
        (SessionStage::BackingUp, _) => "switch.sessions.stage.backing_up",
        (SessionStage::Moving, SessionDirection::Migrate) => "switch.sessions.stage.moving",
        (SessionStage::Moving, SessionDirection::Restore) => "switch.sessions.stage.restoring",
        (SessionStage::Databases, _) => "switch.sessions.stage.databases",
        (SessionStage::Verifying, _) => "switch.sessions.stage.verifying",
        _ => "switch.sessions.stage.done",
    })
}

/// The session being worked on: its title, else the file name (`None` when
/// neither is known).
pub fn session_current(p: &SessionProgress) -> Option<String> {
    let current = p.current.as_ref()?;
    current
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            current
                .file
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
        })
}

/// One progress line: "Migrated 120 of 600 sessions · 75% · Fix the login
/// flow" (the counter names the stage).
pub fn session_progress_line(p: &SessionProgress) -> LocalizedText {
    let line = match session_current(p) {
        Some(current) => text("switch.sessions.line_current").arg("current", shorten(&current)),
        None => text("switch.sessions.line"),
    };
    line.arg_text("count", session_count(p))
        .arg("percent", session_percent(p))
}

/// Longest session title in a progress line (characters).
const LINE_TITLE_CHARS: usize = 48;

fn shorten(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= LINE_TITLE_CHARS {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(LINE_TITLE_CHARS - 1).collect();
    out.push('…');
    out
}
