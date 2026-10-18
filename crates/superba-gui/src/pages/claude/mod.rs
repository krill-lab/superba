//! Claude Code (user bug 79): its connection and models
//! ([`Page::ClaudeCode`]) and its `settings.json` ([`Page::ClaudeSettings`]).
//! Both pages share one state.

use egui::Ui;

use super::{Action, Cx, Page, PageStatus, PaletteItem, empty_state, page_header};
use crate::design::icons::Icon;

/// State of both Claude Code pages, kept across navigation.
#[derive(Debug, Default)]
pub struct State {}

/// Draw `page` (header included).
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, page: Page, out: &mut Vec<Action>) {
    let _ = (state, out);
    page_header(ui, cx, page, |_| {});
    empty_state(
        ui,
        cx,
        Icon::Info,
        &cx.tr.t("page.placeholder.title"),
        &cx.tr.t("page.placeholder.body"),
    );
}

/// Unsaved edits and dialogs of `page`.
pub fn status(state: &State, page: Page) -> PageStatus {
    let _ = (state, page);
    PageStatus::default()
}

/// Run a command addressed to `page`.
pub fn command(state: &mut State, page: Page, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    let _ = (state, cx, out);
    tracing::debug!(?page, %command, "Claude Code page command");
}

/// Palette entries of the Claude Code pages.
pub fn palette(cx: &Cx<'_>, state: &State) -> Vec<PaletteItem> {
    let _ = (cx, state);
    Vec::new()
}
