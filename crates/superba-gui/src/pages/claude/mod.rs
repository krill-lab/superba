//! Claude Code (user bug 79): its connection and models
//! ([`Page::ClaudeCode`], [`connect`]) and its `settings.json`
//! ([`Page::ClaudeSettings`], [`settings`]). Both pages share one state: the
//! service's [`ClaudeView`] (where the file is, the provider and endpoint
//! Claude Code uses, what Superba applied), loaded when either page shows and
//! again after a profile or the file changed.
//!
//! Commands: `apply`, `restore`, `pull` (connection page); `search`, `save`,
//! `files`, `files-backups`, `focus:<path>` (settings page).

pub mod connect;
pub mod editors;
pub mod model;
pub mod settings;

use egui::Ui;
use superba_core::ClaudeView;

use super::{Action, Cx, Page, PageStatus, PaletteGroup, PaletteItem};
use crate::runtime::ReadTask;

/// State of both Claude Code pages, kept across navigation.
#[derive(Debug, Default)]
pub struct State {
    /// The service's view (`None` before the first load).
    pub view: Option<ClaudeView>,
    /// The last load failed.
    pub view_error: Option<superba_core::Error>,
    task: Option<ReadTask<ClaudeView>>,
    /// Profiles the view was loaded with (reload when they change).
    loaded_profiles: Option<superba_core::ProfileSet>,
    /// The connection and models page.
    pub connect: connect::ConnectState,
    /// The settings page.
    pub settings: settings::SettingsState,
}

impl State {
    /// Load the view again (unless a load runs).
    pub fn reload(&mut self, cx: &Cx<'_>) {
        if self.task.is_some() {
            return;
        }
        self.loaded_profiles = cx.data.profiles.value.clone();
        let service = cx.service.clone();
        self.task = Some(crate::runtime::task(
            async move { service.claude_view().await },
        ));
    }

    /// Replace the view (tests and the loader).
    pub fn set_view(&mut self, view: ClaudeView) {
        self.connect.on_view(&view);
        self.view = Some(view);
        self.view_error = None;
    }

    /// Poll the load; true while it runs.
    fn poll(&mut self, cx: &Cx<'_>) -> bool {
        let profiles_changed = self.view.is_some()
            && cx.data.profiles.value.is_some()
            && self.loaded_profiles.as_ref() != cx.data.profiles.value.as_ref();
        if profiles_changed {
            // A provider's endpoint or model list setting may have changed:
            // read the lists again (the service answers from its cache).
            self.connect.models.clear();
            self.connect.load = Default::default();
        }
        if (self.view.is_none() && self.view_error.is_none()) || profiles_changed {
            self.reload(cx);
        }
        let Some(task) = &self.task else {
            return false;
        };
        match task.poll() {
            Some(Ok(view)) => {
                self.task = None;
                self.set_view(view);
                false
            }
            Some(Err(e)) => {
                self.task = None;
                self.view_error = Some(e);
                false
            }
            None => true,
        }
    }

    /// The view is loading.
    pub fn loading(&self) -> bool {
        self.task.is_some()
    }
}

/// Draw `page` (header included).
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, page: Page, out: &mut Vec<Action>) {
    if state.poll(cx) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    match page {
        Page::ClaudeSettings => settings::show(ui, cx, state, out),
        _ => connect::show(ui, cx, state, out),
    }
}

/// Unsaved edits and dialogs of `page`.
pub fn status(state: &State, page: Page) -> PageStatus {
    match page {
        Page::ClaudeSettings => settings::status(&state.settings),
        _ => connect::status(&state.connect, state.view.as_ref()),
    }
}

/// Run a command addressed to `page`.
pub fn command(state: &mut State, page: Page, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    match page {
        Page::ClaudeSettings => settings::command(state, cx, command, out),
        _ => connect::command(state, cx, command, out),
    }
}

/// Palette entries of the Claude Code pages.
pub fn palette(cx: &Cx<'_>, state: &State) -> Vec<PaletteItem> {
    let mut items = vec![
        PaletteItem {
            title: cx.tr.t("claude.palette.apply"),
            detail: None,
            keywords: vec!["claude".into(), "apply".into(), "settings.json".into()],
            group: PaletteGroup::Commands,
            action: Action::PageCommand {
                page: Page::ClaudeCode,
                command: "apply".into(),
            },
        },
        PaletteItem {
            title: cx.tr.t("claude.palette.pull"),
            detail: None,
            keywords: vec!["claude".into(), "models".into(), "/v1/models".into()],
            group: PaletteGroup::Commands,
            action: Action::PageCommand {
                page: Page::ClaudeCode,
                command: "pull".into(),
            },
        },
        PaletteItem {
            title: cx.tr.t("claude.palette.files"),
            detail: None,
            keywords: vec!["claude".into(), "settings.json".into(), "edit".into()],
            group: PaletteGroup::Commands,
            action: Action::PageCommand {
                page: Page::ClaudeSettings,
                command: "files".into(),
            },
        },
    ];
    items.extend(settings::palette(cx, &state.settings));
    items
}
