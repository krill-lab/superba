//! Notices the GUI gives once at start (user bug 44):
//!
//! - **Elevated Windows token**: running as administrator is fine (UAC off makes
//!   every process elevated), so it is never blocked; a one-time toast says
//!   so. Its marker file in the data directory keeps it from coming back.
//!
//! Both wait until the main window shows (the setup wizard of a first start
//! would cover them).
//! - **A failed self-update**: the last install attempt failed or ended with a
//!   problem ([`superba_core::SelfUpdateView::problem`]): a sticky toast with
//!   the reason and the log, until it is acknowledged (it stays on the
//!   Updates page until the next attempt).
//!
//! Another account than the signed-in user is not a notice: the GUI does not
//! start at all then ([`crate::startup::refusal`]).

use std::path::Path;

use superba_core::SelfUpdateView;
use superba_switch::privilege::RunAs;

use crate::app::ToastAction;
use crate::design::widgets::{Toast, Toasts};
use crate::i18n::Tr;
use crate::pages::superba::updates::self_update::problem_text;

/// The marker that the elevated notice was shown (in the data directory).
pub const ELEVATED_SEEN_FILE: &str = "gui-elevated-notice-seen-v1";

/// Whether the elevated notice is due: an elevated token of the signed-in
/// user, and not shown before.
pub fn elevated_notice_due(run_as: &RunAs, seen: bool) -> bool {
    *run_as == RunAs::ElevatedUser && !seen
}

/// The elevated notice (dismissible, until closed).
pub fn elevated_notice(tr: &Tr) -> Toast<ToastAction> {
    Toast::new(
        crate::design::widgets::Status::Idle,
        tr.t("notice.elevated_title"),
    )
    .body(tr.t("notice.elevated_body"))
    .sticky()
}

/// Show the elevated notice once: when it is due for `run_as` and the marker
/// in `data_dir` is absent, push it and write the marker. Returns whether it
/// was shown.
pub fn show_elevated_once(
    tr: &Tr,
    run_as: &RunAs,
    data_dir: Option<&Path>,
    toasts: &mut Toasts<ToastAction>,
) -> bool {
    let marker = data_dir.map(|d| d.join(ELEVATED_SEEN_FILE));
    let seen = marker.as_ref().is_some_and(|m| m.is_file());
    if !elevated_notice_due(run_as, seen) {
        return false;
    }
    toasts.push(elevated_notice(tr));
    if let Some(m) = marker {
        let _ = m.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(m, b"shown\n");
    }
    true
}

/// The toast for the last update attempt's problem, when it was not shown yet;
/// `reason` renders a domain text in the UI language.
pub fn selfupdate_problem_toast(
    tr: &Tr,
    view: &SelfUpdateView,
    reason: impl Fn(&superba_core::LocalizedText) -> String,
) -> Option<Toast<ToastAction>> {
    let problem = view.problem.as_ref().filter(|p| !p.seen)?;
    let text = problem_text(tr, problem, &reason(&problem.reason));
    Some(
        Toast::new(text.status, text.title)
            .body(text.body)
            .action(
                tr.t("updates.superba.open_log"),
                ToastAction::OpenPath(problem.log.clone()),
            )
            .sticky(),
    )
}
