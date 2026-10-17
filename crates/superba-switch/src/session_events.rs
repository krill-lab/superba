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

/// "Migrated 120 of 600 sessions" / "Restored 3 of 12 sessions".
pub fn session_count(p: &SessionProgress) -> LocalizedText {
    let key = match p.direction {
        SessionDirection::Migrate => "switch.sessions.migrated",
        SessionDirection::Restore => "switch.sessions.restored",
    };
    text(key).arg("done", p.done).arg("total", p.total)
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

/// One progress line: "Migrated 120 of 600 sessions · 35% · Backing up the
/// sessions · Fix the login flow".
pub fn session_progress_line(p: &SessionProgress) -> LocalizedText {
    let percent = (p.fraction() * 100.0).floor() as u32;
    let line = match session_current(p) {
        Some(current) => text("switch.sessions.line_current").arg("current", shorten(&current)),
        None => text("switch.sessions.line"),
    };
    line.arg_text("count", session_count(p))
        .arg("percent", percent)
        .arg_text("stage", session_stage(p))
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
