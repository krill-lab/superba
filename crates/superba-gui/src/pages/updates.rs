//! Updates (UX §3.9): the ChatGPT desktop app (installed vs latest per OS), the
//! Codex the desktop app uses (`CODEX_CLI_PATH` pin), the Codex CLI on this
//! computer and on each remote host the app is connected to.
//!
//! - App: Linux downloads and verifies the package, then opens the system
//!   installer or shows the `apt` / `dnf` command (Superba never uses sudo);
//!   macOS updates itself (Sparkle); Windows opens the Microsoft Store.
//! - Pin: bundled / a fixed version from Superba's verified store / a custom
//!   path, with a compatibility badge; applying is a full app restart planned as
//!   `SwitchIntent::AppCodexPin`. Not offered on Windows in 0.3.
//! - CLI rows by install kind: standalone installs are updated in place (the
//!   previous release is kept for rollback), npm / Homebrew only get the command.
//! - Remote rows install the verified release into the host's `~/.local/bin`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use egui::{Align, Color32, FontId, Layout, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use superba_core::{
    AppCodexPin, AppDownloadReport, AppUpdateAction, CliUpdateAction, CliVersionRow,
    CodexInstallKind, CodexRelease, CodexVersion, CompatTier, InstallReport, InstallRequest,
    InstallTarget, ProfileId, RemoteVersionRow, SwitchIntent, SwitchRequest, TargetId,
    VersionsView,
};

use super::router::db::Loader;
use super::{
    Action, Cx, Page, PageStatus, PaletteGroup, PaletteItem, callout, error_text, kv, page_header,
    section_gap,
};
use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::{radius, size, text};
use crate::design::widgets::{Badge, Button, Dropdown, FormRow, Status, Text, Toggle, link_button};
use crate::runtime::ReadTask;

/// Which Codex the desktop app should use (the pin choice being edited).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PinChoice {
    /// The bundled Codex.
    #[default]
    Bundled,
    /// A version from Superba's store.
    Version,
    /// A custom executable.
    Path,
}

/// "Include pre-releases" key of the app's Codex pin picker (the install
/// pickers use their [`install_key`]).
pub const PIN_PICKER: &str = "pin";

/// A mark after a version in a version picker (menu rows and the closed field),
/// drawn as a [`Badge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionTag {
    /// A pre-release (`0.164.0-alpha.1`): amber.
    Prerelease,
    /// The version installed or pinned at the picker's target now: green.
    Current,
    /// The version the desktop app bundles: neutral.
    Bundled,
    /// Already in Superba's store (pin picker): neutral.
    Stored,
}

impl VersionTag {
    /// Locale key of its text.
    pub fn key(self) -> &'static str {
        match self {
            VersionTag::Prerelease => "updates.tag.prerelease",
            VersionTag::Current => "updates.tag.current",
            VersionTag::Bundled => "updates.tag.bundled",
            VersionTag::Stored => "updates.tag.stored",
        }
    }

    /// Status dot of its badge (`None`: a plain neutral badge).
    pub fn status(self) -> Option<Status> {
        match self {
            VersionTag::Prerelease => Some(Status::Warn),
            VersionTag::Current => Some(Status::Ok),
            VersionTag::Bundled | VersionTag::Stored => None,
        }
    }
}

/// What a picker marks besides pre-releases.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    /// The Codex bundled with the desktop app.
    pub bundled: Option<CodexVersion>,
    /// The version installed or pinned at the picker's target.
    pub current: Option<CodexVersion>,
}

/// A pre-release: published as one, or a version with a pre-release part.
pub fn is_prerelease(release: &CodexRelease) -> bool {
    release.prerelease || release.version.pre.is_some()
}

/// The tags of `version` in display order (`prerelease`: published as a
/// pre-release; `stored`: already in Superba's store).
pub fn version_tags(
    version: &CodexVersion,
    prerelease: bool,
    stored: bool,
    marks: &Marks,
) -> Vec<VersionTag> {
    let mut tags = Vec::new();
    if prerelease || version.pre.is_some() {
        tags.push(VersionTag::Prerelease);
    }
    if marks.current.as_ref() == Some(version) {
        tags.push(VersionTag::Current);
    }
    if marks.bundled.as_ref() == Some(version) {
        tags.push(VersionTag::Bundled);
    }
    if stored {
        tags.push(VersionTag::Stored);
    }
    tags
}

/// One version a picker offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerItem {
    /// The version.
    pub version: CodexVersion,
    /// Its tags.
    pub tags: Vec<VersionTag>,
}

/// The versions a picker offers: the known releases newest first by semver
/// precedence (a pre-release sorts below its release and above the release
/// before it; numeric pre-release parts compare as numbers), pre-releases only
/// when `prerelease` is on. `stored` tags the versions already in the store.
pub fn picker_items(
    view: &VersionsView,
    prerelease: bool,
    stored: bool,
    marks: &Marks,
) -> Vec<PickerItem> {
    let mut list: Vec<(CodexVersion, bool, bool)> = view
        .app_codex
        .candidates
        .iter()
        .map(|c| {
            (
                c.release.version.clone(),
                is_prerelease(&c.release),
                stored && c.stored,
            )
        })
        .collect();
    if list.is_empty() {
        // No release list: the newest release the CLI rows know.
        list = view
            .cli
            .iter()
            .filter_map(|r| r.latest.clone())
            .map(|v| (v, false, false))
            .collect();
    }
    list.retain(|(v, pre, _)| prerelease || !(*pre || v.pre.is_some()));
    list.sort_by(|a, b| b.0.cmp(&a.0));
    list.dedup_by(|a, b| a.0 == b.0);
    list.into_iter()
        .map(|(version, pre, stored)| PickerItem {
            tags: version_tags(&version, pre, stored, marks),
            version,
        })
        .collect()
}

/// The tags of `version` in a picker whether or not its list shows it (the
/// closed field keeps a selection the filter hides).
pub fn tags_of(
    view: &VersionsView,
    version: &CodexVersion,
    stored: bool,
    marks: &Marks,
) -> Vec<VersionTag> {
    let c = view
        .app_codex
        .candidates
        .iter()
        .find(|c| &c.release.version == version);
    version_tags(
        version,
        c.is_some_and(|c| is_prerelease(&c.release)),
        stored && c.is_some_and(|c| c.stored),
        marks,
    )
}

/// The Codex bundled with the desktop app.
fn bundled_version(view: &VersionsView) -> Option<CodexVersion> {
    view.app
        .install
        .as_ref()
        .and_then(|i| i.bundled_codex_version.clone())
}

/// Marks of the pin picker: the pinned version is current (the bundled one
/// while the app uses its bundled Codex).
pub fn pin_marks(view: &VersionsView) -> Marks {
    let bundled = bundled_version(view);
    let current = match &view.app_codex.pin {
        AppCodexPin::Version { version } => Some(version.clone()),
        AppCodexPin::Bundled => bundled.clone(),
        _ => view.app_codex.effective_version.clone(),
    };
    Marks { bundled, current }
}

/// Marks of the local CLI picker: the standalone install (what Superba updates
/// in place) is current.
pub fn local_marks(view: &VersionsView) -> Marks {
    Marks {
        bundled: bundled_version(view),
        current: view
            .cli
            .iter()
            .find(|r| r.kind == CodexInstallKind::Standalone)
            .and_then(|r| r.binary.version.clone()),
    }
}

/// Marks of a remote host's picker: the host's Codex is current.
pub fn remote_marks(view: &VersionsView, row: &RemoteVersionRow) -> Marks {
    Marks {
        bundled: bundled_version(view),
        current: row.version.clone(),
    }
}

/// Badge of a compatibility tier: status, label key and tooltip key.
pub fn compat_badge(tier: CompatTier) -> (Status, &'static str, Option<&'static str>) {
    match tier {
        CompatTier::Same | CompatTier::Compatible => (Status::Ok, "updates.compat.match", None),
        CompatTier::Older => (
            Status::Warn,
            "updates.compat.older",
            Some("updates.compat.older_tip"),
        ),
        CompatTier::Newer => (
            Status::Warn,
            "updates.compat.newer",
            Some("updates.compat.newer_tip"),
        ),
        _ => (
            Status::Error,
            "updates.compat.below_floor",
            Some("updates.compat.below_floor_tip"),
        ),
    }
}

/// Check a custom path: must be set, must not be npm's `codex.js` launcher.
pub fn check_path(path: &str) -> Result<PathBuf, &'static str> {
    let t = path.trim();
    if t.is_empty() {
        return Err("updates.pin.path_empty");
    }
    let lower = t.to_ascii_lowercase();
    if [".js", ".cjs", ".mjs"].iter().any(|s| lower.ends_with(s)) {
        return Err("updates.pin.not_native");
    }
    Ok(PathBuf::from(t))
}

/// The pin for a choice (`None` when the choice is incomplete or invalid).
pub fn pin_for(
    choice: PinChoice,
    version: Option<&CodexVersion>,
    path: &str,
) -> Option<AppCodexPin> {
    match choice {
        PinChoice::Bundled => Some(AppCodexPin::Bundled),
        PinChoice::Version => version.map(|v| AppCodexPin::Version { version: v.clone() }),
        PinChoice::Path => check_path(path).ok().map(|path| AppCodexPin::Path { path }),
    }
}

/// The switch request applying a pin (full restart of the app).
pub fn pin_request(pin: AppCodexPin) -> SwitchRequest {
    let mut r = SwitchRequest::profile(ProfileId::new(""));
    r.intent = SwitchIntent::AppCodexPin { pin };
    r
}

/// Key of an install in flight / its result.
pub fn install_key(target: &InstallTarget) -> String {
    match target {
        InstallTarget::Remote { target_id } => format!("remote:{target_id}"),
        InstallTarget::Store => "store".into(),
        _ => "local".into(),
    }
}

/// The installed app changed since the page first saw it (updated while Superba
/// was running): offer to reopen it through Superba to keep hot switching.
pub fn app_updated(first: Option<&str>, view: &VersionsView) -> bool {
    let now = view
        .app
        .install
        .as_ref()
        .and_then(|i| i.app_version.as_deref());
    matches!((first, now), (Some(a), Some(b)) if a != b)
}

/// Whether anything can be updated (sidebar dot).
pub fn update_available(view: &VersionsView) -> bool {
    view.app.update_available
        || view
            .cli
            .iter()
            .any(|r| !matches!(r.action, CliUpdateAction::None))
}

/// Page-local state, kept across navigation.
#[derive(Debug, Default)]
pub struct State {
    /// The versions.
    pub view: Loader<VersionsView>,
    check_now: bool,
    /// Pin being edited.
    pub choice: PinChoice,
    /// Version chosen for [`PinChoice::Version`].
    pub version: Option<CodexVersion>,
    /// Path typed for [`PinChoice::Path`].
    pub path: String,
    /// "Include pre-releases" of each version picker, by [`PIN_PICKER`] or
    /// [`install_key`] (unset: see [`State::shows_prerelease`]).
    pub prerelease: BTreeMap<String, bool>,
    /// Version picked in each install picker, by [`install_key`] (unset or
    /// filtered out: the newest offered).
    pub picks: BTreeMap<String, CodexVersion>,
    pin_loaded: bool,
    persist_task: Option<ReadTask<()>>,
    /// Persist value saved but not yet visible in the profile set.
    pub persist_pending: Option<bool>,
    installs: BTreeMap<String, ReadTask<InstallReport>>,
    /// Results of installs by [`install_key`] (`Err` = translated error).
    pub results: BTreeMap<String, Result<InstallReport, String>>,
    download: Option<ReadTask<AppDownloadReport>>,
    /// The downloaded, verified app package.
    pub downloaded: Option<AppDownloadReport>,
    /// Why the download failed (translated).
    pub download_error: Option<String>,
    /// Remote row to bring into view.
    pub focus: Option<TargetId>,
    /// App version seen when the page first loaded (a later different one means
    /// the app was updated while Superba ran).
    pub first_app_version: Option<String>,
    toasts: Vec<(Status, String)>,
    /// The page's view was fetched online or an install finished: the shell
    /// reloads its own copy (the sidebar badge and the Overview read it).
    notify_shell: bool,
    refreshing: bool,
}

impl State {
    /// Check online now.
    pub fn check(&mut self) {
        self.check_now = true;
        self.view.invalidate();
    }

    /// Whether picker `key` offers pre-releases. Until its box is ticked or
    /// cleared, only when the picker's `current` version is a pre-release.
    pub fn shows_prerelease(&self, key: &str, current: Option<&CodexVersion>) -> bool {
        self.prerelease
            .get(key)
            .copied()
            .unwrap_or_else(|| current.is_some_and(|v| v.pre.is_some()))
    }

    /// The version install picker `key` targets among `items` (newest first):
    /// the one picked, else the newest.
    pub fn pick(&self, key: &str, items: &[PickerItem]) -> Option<CodexVersion> {
        self.picks
            .get(key)
            .filter(|v| items.iter().any(|i| &i.version == *v))
            .or_else(|| items.first().map(|i| &i.version))
            .cloned()
    }

    /// Something is running.
    pub fn busy(&self) -> bool {
        self.view.loading()
            || self.persist_task.is_some()
            || !self.installs.is_empty()
            || self.download.is_some()
    }

    /// An install for `target` is running.
    pub fn installing(&self, target: &InstallTarget) -> bool {
        self.installs.contains_key(&install_key(target))
    }

    /// Install `version` at `target` (background).
    pub fn install(&mut self, cx: &Cx<'_>, version: CodexVersion, target: InstallTarget) {
        let key = install_key(&target);
        if self.installs.contains_key(&key) {
            return;
        }
        self.results.remove(&key);
        let service = cx.service.clone();
        self.installs.insert(
            key,
            crate::runtime::task(async move {
                service
                    .install_codex(InstallRequest { version, target })
                    .await
            }),
        );
    }

    /// Switch `target` back to the release its last install replaced
    /// (background; the result replaces the install result).
    pub fn rollback(&mut self, cx: &Cx<'_>, target: InstallTarget) {
        let key = install_key(&target);
        if self.installs.contains_key(&key) {
            return;
        }
        self.results.remove(&key);
        let service = cx.service.clone();
        self.installs.insert(
            key,
            crate::runtime::task(async move { service.rollback_codex(target).await }),
        );
    }

    /// Download the app update (background).
    pub fn download(&mut self, cx: &Cx<'_>) {
        if self.download.is_some() {
            return;
        }
        self.download_error = None;
        let service = cx.service.clone();
        self.download = Some(crate::runtime::task(async move {
            service.download_app_update().await
        }));
    }

    fn poll(&mut self, cx: &Cx<'_>) {
        let finished = self.view.poll();
        if finished && std::mem::take(&mut self.refreshing) {
            self.notify_shell = true;
        }
        if finished
            && let Some(v) = &self.view.value
            && self.first_app_version.is_none()
        {
            self.first_app_version = v.app.install.as_ref().and_then(|i| i.app_version.clone());
        }
        if let Some(v) = &self.view.value
            && !self.pin_loaded
        {
            self.pin_loaded = true;
            match &v.app_codex.pin {
                AppCodexPin::Version { version } => {
                    self.choice = PinChoice::Version;
                    self.version = Some(version.clone());
                }
                AppCodexPin::Path { path } => {
                    self.choice = PinChoice::Path;
                    self.path = path.display().to_string();
                }
                _ => self.choice = PinChoice::Bundled,
            }
        }
        let done: Vec<String> = self
            .installs
            .iter()
            .filter_map(|(k, t)| t.poll().map(|r| (k.clone(), r)))
            .map(|(k, r)| {
                let ok = r.is_ok();
                let text = match &r {
                    Ok(rep) => cx.tr.f("updates.installed", &[("version", &rep.version)]),
                    Err(e) => error_text(cx.tr, e),
                };
                self.toasts
                    .push((if ok { Status::Ok } else { Status::Error }, text));
                self.results
                    .insert(k.clone(), r.map_err(|e| error_text(cx.tr, &e)));
                k
            })
            .collect();
        for k in &done {
            self.installs.remove(k);
        }
        if !done.is_empty() {
            self.view.invalidate();
            self.notify_shell = true;
        }
        if let Some(t) = &self.download
            && let Some(r) = t.poll()
        {
            self.download = None;
            match r {
                Ok(rep) => self.downloaded = Some(rep),
                Err(e) => self.download_error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(t) = &self.persist_task
            && let Some(r) = t.poll()
        {
            self.persist_task = None;
            if let Err(e) = r {
                self.persist_pending = None;
                self.toasts.push((Status::Error, error_text(cx.tr, &e)));
            }
        }
    }

    fn load(&mut self, cx: &Cx<'_>) {
        if self.view.due(None) {
            let refresh = std::mem::take(&mut self.check_now);
            self.refreshing |= refresh;
            let service = cx.service.clone();
            self.view
                .start(async move { service.versions(refresh).await });
        }
    }

    /// The persisted "also outside Superba" choice.
    pub fn persist(&self, cx: &Cx<'_>) -> bool {
        self.persist_pending.unwrap_or_else(|| {
            cx.data
                .profiles
                .value
                .as_ref()
                .is_some_and(|p| p.settings.app_codex.persist_env)
        })
    }

    fn set_persist(&mut self, cx: &Cx<'_>, on: bool) {
        let Some(set) = cx.data.profiles.value.as_ref() else {
            return;
        };
        let mut settings = set.settings.clone();
        settings.app_codex.persist_env = on;
        self.persist_pending = Some(on);
        let service = cx.service.clone();
        self.persist_task = Some(crate::runtime::task(async move {
            service.save_settings(settings).await
        }));
    }
}

/// Draw the page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.poll(cx);
    state.load(cx);
    for (status, text) in state.toasts.drain(..) {
        out.push(Action::Toast { status, text });
    }
    if std::mem::take(&mut state.notify_shell) {
        out.push(Action::LoadVersions);
    }
    if state.busy() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    if state.persist_pending.is_some()
        && state.persist_task.is_none()
        && cx
            .data
            .profiles
            .value
            .as_ref()
            .is_some_and(|p| Some(p.settings.app_codex.persist_env) == state.persist_pending)
    {
        state.persist_pending = None;
    }
    let checked = state.view.value.as_ref().and_then(|v| v.checked_at);
    let checking = state.view.loading();
    page_header(ui, cx, Page::Updates, |ui| {
        if ui
            .add(
                Button::new(cx.tr.t("updates.check"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(checking)
                    .enabled(!checking),
            )
            .clicked()
        {
            state.check();
        }
        if let Some(t) = checked {
            ui.label(Text::caption(
                cx.tr.f("updates.checked", &[("age", &cx.ago(t))]),
                cx.pal,
            ));
        }
    });
    let Some(view) = state.view.value.clone() else {
        match &state.view.error {
            Some(e) => crate::pages::error_state(ui, cx, &cx.tr.t("updates.load_failed"), e),
            None => {
                ui.label(Text::muted(cx.tr.t("updates.loading"), cx.pal));
            }
        }
        return;
    };
    if let Some(e) = &state.view.error {
        callout(ui, cx, Status::Warn, &error_text(cx.tr, e));
    }
    for n in &view.notes {
        callout(
            ui,
            cx,
            match n.severity {
                superba_core::Severity::Info => Status::Idle,
                _ => Status::Warn,
            },
            &cx.localize(&n.text),
        );
    }
    app_section(ui, cx, state, &view, out);
    section_gap(ui, cx);
    pin_section(ui, cx, state, &view, out);
    section_gap(ui, cx);
    cli_section(ui, cx, state, &view, out);
    if !view.remote.is_empty() {
        section_gap(ui, cx);
        remote_section(ui, cx, state, &view);
    }
}

fn app_section(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &VersionsView,
    out: &mut Vec<Action>,
) {
    icon_section(ui, cx, Icon::AppWindow, &cx.tr.t("updates.app.title"));
    match &view.app.install {
        Some(i) => kv(ui, cx, &cx.tr.t("updates.app.installed_label"), |ui| {
            ui.label(Text::mono(
                app_line(
                    cx,
                    i.app_version.as_deref(),
                    None,
                    i.bundled_codex_version.as_ref(),
                ),
                cx.pal,
            ));
        }),
        None => {
            ui.label(Text::small(cx.tr.t("updates.app.not_installed"), cx.pal));
        }
    }
    if app_updated(state.first_app_version.as_deref(), view) {
        callout(ui, cx, Status::Warn, &cx.tr.t("updates.app.reopen"));
        if ui
            .add(Button::new(cx.tr.t("updates.app.reopen_button")).small())
            .on_hover_text(cx.tr.t("updates.app.reopen_tip"))
            .clicked()
        {
            out.push(Action::RestartApp);
        }
    }
    let latest = view.app.latest.as_ref();
    kv(ui, cx, &cx.tr.t("updates.app.latest_label"), |ui| {
        match latest {
            Some(l) => {
                let date = l.date.map(crate::pages::router::requests::local_day);
                ui.label(Text::mono(
                    app_line(
                        cx,
                        Some(&l.version),
                        date.as_deref(),
                        l.bundled_codex.as_ref(),
                    ),
                    cx.pal,
                ));
            }
            None => {
                ui.label(Text::small(cx.tr.t("updates.app.latest_unknown"), cx.pal));
            }
        }
        if latest.is_some() && view.app.install.is_some() && !view.app.update_available {
            ui.add(Badge::new(cx.tr.t("updates.app.up_to_date")).status(Status::Ok));
        }
    });
    let Some(action) = view.app.action.as_ref() else {
        return;
    };
    match action {
        AppUpdateAction::LinuxPackage { package, command } => {
            if view.app.update_available {
                FormRow::unlabeled().show(ui, |ui| {
                    let kind = match package.kind {
                        superba_core::AppPackageKind::Rpm => "rpm",
                        _ => "deb",
                    };
                    let size = package
                        .size
                        .map(|s| format!("{} MB", s / 1_000_000))
                        .unwrap_or_else(|| "?".into());
                    let downloading = state.download.is_some();
                    if ui
                        .add(
                            Button::primary(
                                cx.tr
                                    .f("updates.app.download", &[("kind", &kind), ("size", &size)]),
                            )
                            .icon(Icon::Download)
                            .small()
                            .loading(downloading)
                            .enabled(!downloading),
                        )
                        .clicked()
                    {
                        state.download(cx);
                    }
                    if downloading {
                        ui.label(Text::caption(cx.tr.t("updates.downloading"), cx.pal));
                    }
                });
            }
            if let Some(e) = &state.download_error {
                callout(ui, cx, Status::Error, e);
            }
            if let Some(rep) = &state.downloaded {
                kv(ui, cx, &cx.tr.t("updates.app.file"), |ui| {
                    ui.label(Text::mono(rep.path.display().to_string(), cx.pal));
                    if ui
                        .add(Button::new(cx.tr.t("updates.app.open_installer")).small())
                        .clicked()
                    {
                        out.push(Action::OpenUrl(rep.path.display().to_string()));
                    }
                });
                let cmd = rep.command.clone().unwrap_or_else(|| command.clone());
                command_row(ui, cx, &cmd, out);
            } else if view.app.update_available {
                command_row(ui, cx, command, out);
            }
            ui.add(
                egui::Label::new(Text::caption(cx.tr.t("updates.app.linux_note"), cx.pal)).wrap(),
            );
        }
        AppUpdateAction::MacSparkle { package } => {
            ui.add(egui::Label::new(Text::small(cx.tr.t("updates.app.mac_note"), cx.pal)).wrap());
            if let Some(p) = package
                && link_button(ui, &cx.tr.t("updates.app.mac_download"), true).clicked()
            {
                out.push(Action::OpenUrl(p.url.clone()));
            }
        }
        AppUpdateAction::WindowsStore { store_url, command } => {
            if ui
                .add(Button::new(cx.tr.t("updates.app.store")).small())
                .clicked()
            {
                out.push(Action::OpenUrl(store_url.clone()));
            }
            command_row(ui, cx, command, out);
        }
        _ => {}
    }
}

/// A section heading with its icon tile (the Surge list look, user request 24).
fn icon_section(ui: &mut Ui, cx: &Cx<'_>, icon: Icon, title: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
        crate::design::widgets::list_row::icon_tile(ui, r.center(), 28.0, icon, None);
        ui.add_space(2.0);
        ui.label(Text::section(title, cx.pal));
    });
    ui.add_space(8.0);
}

/// `26.1013.31256 · released 2026-10-15 · bundled codex 0.163.0`, leaving out
/// what is unknown.
pub fn app_line(
    cx: &Cx<'_>,
    version: Option<&str>,
    date: Option<&str>,
    codex: Option<&CodexVersion>,
) -> String {
    let mut parts = vec![version.unwrap_or("—").to_owned()];
    if let Some(d) = date {
        parts.push(cx.tr.f("updates.app.released", &[("date", &d)]));
    }
    if let Some(c) = codex {
        parts.push(cx.tr.f("updates.app.bundled", &[("codex", c)]));
    }
    parts.join(" · ")
}

fn command_row(ui: &mut Ui, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    kv(ui, cx, &cx.tr.t("updates.command"), |ui| {
        ui.label(Text::mono(command, cx.pal));
        if ui
            .add(
                Button::ghost(cx.tr.t("updates.copy"))
                    .icon(Icon::Copy)
                    .small(),
            )
            .clicked()
        {
            out.push(Action::Copy(command.to_owned()));
        }
    });
}

/// Height of a picker row: the dropdown's (checkbox, radio and buttons centre on it).
const PICKER_ROW: f32 = size::CONTROL_SM;
/// Inset of the dropdown's value from its left edge, and the room its chevron
/// takes on the right (inset, gap, chevron, inset): `Dropdown`'s own geometry.
const FIELD_INSET: f32 = 10.0;
const FIELD_CHROME: f32 = 40.0;
/// Narrowest version field.
const FIELD_MIN: f32 = 180.0;
/// Space between a version and its first tag, and between tags.
const TAG_GAP: f32 = 6.0;
/// Height and side inset of a menu row.
const OPTION_H: f32 = 26.0;
const OPTION_PAD: f32 = 8.0;

/// Font of a version in the field and the menu (the small dropdown's).
fn value_font() -> FontId {
    font(crate::design::tokens::text::SMALL, Weight::Regular)
}

fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
        .size()
        .x
}

/// Width of `tags` drawn after a version (each with the gap before it). A
/// [`Badge`] is its caption text, 8 px padding on each side and a 12 px dot.
fn tags_width(ui: &Ui, cx: &Cx<'_>, tags: &[VersionTag]) -> f32 {
    let caption = font(crate::design::tokens::text::CAPTION, Weight::Medium);
    tags.iter()
        .map(|t| {
            let dot = if t.status().is_some() { 12.0 } else { 0.0 };
            TAG_GAP + text_width(ui, &cx.tr.t(t.key()), caption.clone()) + 16.0 + dot
        })
        .sum()
}

/// Draw `tags` as badges from the left of `rect`, centred on its height and
/// clipped to it.
fn paint_tags(ui: &mut Ui, cx: &Cx<'_>, rect: Rect, tags: &[VersionTag]) {
    if tags.is_empty() || rect.width() <= 0.0 {
        return;
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.spacing_mut().item_spacing.x = TAG_GAP;
    for t in tags {
        let badge = Badge::new(cx.tr.t(t.key()));
        child.add(match t.status() {
            Some(s) => badge.status(s),
            None => badge,
        });
    }
}

/// A row of controls centred on one line as tall as a dropdown ([`FormRow`]).
fn picker_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    FormRow::unlabeled().height(PICKER_ROW).show(ui, add).inner
}

/// One menu row: the version, then its tags.
fn option_row(ui: &mut Ui, cx: &Cx<'_>, item: &PickerItem, selected: bool) -> egui::Response {
    let text = item.version.to_string();
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), value_font(), cx.pal.text);
    let text_w = galley.size().x;
    let width = ui
        .available_width()
        .max(OPTION_PAD * 2.0 + text_w + tags_width(ui, cx, &item.tags));
    let (rect, response) = ui.allocate_exact_size(vec2(width, OPTION_H), Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::ListBoxOption, enabled, selected, &text)
    });
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            Some(cx.pal.active)
        } else if response.hovered() || response.has_focus() {
            Some(cx.pal.hover)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(rect, radius::CONTROL, fill);
        }
        let x = rect.min.x + OPTION_PAD;
        ui.painter().galley(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            cx.pal.text,
        );
        let tags = Rect::from_min_max(pos2(x + text_w + TAG_GAP, rect.min.y), rect.max);
        paint_tags(ui, cx, tags, &item.tags);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A version picker on the current row: the "Include pre-releases" checkbox
/// (`shown`), then the dropdown of `items` it filters. Menu rows and the closed
/// field show each version's tags after it (`selected`: the version and its
/// tags). Returns the version chosen in the menu.
fn version_picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    shown: &mut bool,
    id_salt: &str,
    items: &[PickerItem],
    selected: Option<(&CodexVersion, &[VersionTag])>,
) -> Option<CodexVersion> {
    if ui
        .checkbox(shown, cx.tr.t("updates.picker.prerelease"))
        .changed()
    {
        ui.ctx().request_repaint();
    }
    let (text, tags) = match selected {
        Some((v, tags)) => (v.to_string(), tags),
        None => (cx.tr.t("updates.pin.pick"), &[][..]),
    };
    let text_w = text_width(ui, &text, value_font());
    let widest = items
        .iter()
        .map(|i| text_width(ui, &i.version.to_string(), value_font()) + tags_width(ui, cx, &i.tags))
        .fold(text_w + tags_width(ui, cx, tags), f32::max);
    // Room for the menu's own margins and rows' insets too.
    let width = (FIELD_CHROME + widest + 12.0).max(FIELD_MIN);
    let current = selected.map(|(v, _)| v);
    let r = Dropdown::new(id_salt)
        .selected_text(text)
        .width(width)
        .show_ui(ui, |ui| {
            if items.is_empty() {
                ui.label(Text::caption(cx.tr.t("updates.picker.empty"), cx.pal));
            }
            let mut chosen = None;
            for item in items {
                if option_row(ui, cx, item, current == Some(&item.version)).clicked() {
                    chosen = Some(item.version.clone());
                }
            }
            chosen
        });
    // The selected version's tags, inside the field after its text.
    let field = r.response.rect;
    let room = Rect::from_min_max(
        pos2(field.min.x + FIELD_INSET + text_w + TAG_GAP, field.min.y),
        pos2(field.max.x - FIELD_CHROME + FIELD_INSET, field.max.y),
    );
    paint_tags(ui, cx, room, tags);
    r.inner.flatten()
}

/// The full paths behind a pin mismatch, one per line: the running Codex, then
/// the expected one.
pub fn mismatch_paths(cx: &Cx<'_>, view: &VersionsView) -> Option<String> {
    let lines: Vec<String> = [
        (
            "updates.pin.running_path",
            view.app_codex.effective_exe.clone(),
        ),
        ("updates.pin.expected_path", view.expected_codex()),
    ]
    .into_iter()
    .filter_map(|(key, path)| path.map(|p| cx.tr.f(key, &[("path", &p.display())])))
    .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The pin mismatch warning: short names in the sentence (the service words it),
/// the full paths on hover and behind a copy button.
fn mismatch_callout(
    ui: &mut Ui,
    cx: &Cx<'_>,
    view: &VersionsView,
    text: &str,
    out: &mut Vec<Action>,
) {
    let r = ui.scope(|ui| callout(ui, cx, Status::Warn, text)).response;
    if let Some(paths) = mismatch_paths(cx, view) {
        r.on_hover_text(&paths);
        if ui
            .add(
                Button::ghost(cx.tr.t("updates.pin.copy_paths"))
                    .icon(Icon::Copy)
                    .small(),
            )
            .clicked()
        {
            out.push(Action::Copy(paths));
        }
    }
}

fn pin_section(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &VersionsView,
    out: &mut Vec<Action>,
) {
    icon_section(ui, cx, Icon::Process, &cx.tr.t("updates.pin.title"));
    let pin = &view.app_codex;
    if !pin.supported {
        ui.label(Text::small(cx.tr.t("updates.pin.unsupported"), cx.pal));
        return;
    }
    if view.app.install.is_none() {
        ui.label(Text::small(cx.tr.t("updates.pin.no_app"), cx.pal));
        return;
    }
    if let Some(v) = &pin.effective_version {
        kv(ui, cx, &cx.tr.t("updates.pin.effective"), |ui| {
            let r = ui.label(Text::mono(v.to_string(), cx.pal));
            if let Some(exe) = &pin.effective_exe {
                r.on_hover_text(exe.display().to_string());
            }
        });
    }
    if let Some(m) = &pin.mismatch {
        mismatch_callout(ui, cx, view, &cx.localize(m), out);
    }
    let bundled = view
        .app
        .install
        .as_ref()
        .and_then(|i| i.bundled_codex_version.clone());
    // Bundled, a fixed version or a custom path: one radio button per row in the
    // label column, the row's controls next to it, label and controls on one
    // center line (user bug 19).
    let label_w = pin_label_width(ui, cx);
    let mut pick = None;
    // Bundled.
    let label = match &bundled {
        Some(v) => cx.tr.f("updates.pin.bundled", &[("version", v)]),
        None => cx.tr.t("updates.pin.bundled_unknown"),
    };
    FormRow::default().label_width(label_w).show_with(
        ui,
        |ui| ui.radio_value(&mut state.choice, PinChoice::Bundled, label),
        |ui| ui.label(Text::caption(cx.tr.t("updates.pin.bundled_hint"), cx.pal)),
    );
    // Fixed version.
    let marks = pin_marks(view);
    let was = state.shows_prerelease(PIN_PICKER, state.version.as_ref());
    let mut shown = was;
    let items = picker_items(view, shown, true, &marks);
    let selected = state
        .version
        .clone()
        .map(|v| (tags_of(view, &v, true, &marks), v));
    FormRow::default()
        .label_width(label_w)
        .height(PICKER_ROW)
        .hint(Text::caption(cx.tr.t("updates.pin.version_hint"), cx.pal))
        .show_with(
            ui,
            |ui| {
                ui.radio_value(
                    &mut state.choice,
                    PinChoice::Version,
                    cx.tr.t("updates.pin.version"),
                )
            },
            |ui| {
                let picked = version_picker(
                    ui,
                    cx,
                    &mut shown,
                    "updates-pin-version",
                    &items,
                    selected.as_ref().map(|(t, v)| (v, t.as_slice())),
                );
                if let Some(v) = picked {
                    state.version = Some(v);
                    pick = Some(PinChoice::Version);
                    // The box keeps its state when the selection changes.
                    state.prerelease.insert(PIN_PICKER.into(), shown);
                }
                if let Some(v) = &state.version
                    && let Some(c) = pin.candidates.iter().find(|c| &c.release.version == v)
                {
                    let (status, key, tip) = compat_badge(c.tier);
                    let r = ui.add(Badge::new(cx.tr.t(key)).status(status));
                    if let Some(t) = tip {
                        r.on_hover_text(cx.tr.t(t));
                    }
                }
            },
        );
    if shown != was {
        state.prerelease.insert(PIN_PICKER.into(), shown);
    }
    // Custom path (an invalid one is named under the field).
    let mut path_row = FormRow::default().label_width(label_w);
    if state.choice == PinChoice::Path
        && !state.path.trim().is_empty()
        && let Err(key) = check_path(&state.path)
    {
        path_row = path_row.hint(Text::caption(cx.tr.t(key), cx.pal).color(cx.pal.red));
    }
    path_row.show_with(
        ui,
        |ui| {
            ui.radio_value(
                &mut state.choice,
                PinChoice::Path,
                cx.tr.t("updates.pin.path"),
            )
        },
        |ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut state.path)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("/path/to/codex")
                    .desired_width(320.0),
            );
            if r.changed() {
                pick = Some(PinChoice::Path);
            }
            #[cfg(feature = "file-dialogs")]
            if ui
                .add(Button::ghost(cx.tr.t("updates.pin.browse")).small())
                .clicked()
                && let Some(p) = rfd::FileDialog::new().pick_file()
            {
                state.path = p.display().to_string();
                pick = Some(PinChoice::Path);
            }
        },
    );
    if let Some(choice) = pick {
        state.choice = choice;
        // The path was checked before this edit: draw again with the new one.
        ui.ctx().request_repaint();
    }
    // Persist.
    let mut persist = state.persist(cx);
    ui.horizontal(|ui| {
        if ui
            .add(
                Toggle::new(&mut persist, cx.tr.t("updates.pin.persist"))
                    .enabled(state.persist_task.is_none()),
            )
            .changed()
        {
            state.set_persist(cx, persist);
        }
        ui.label(Text::small(cx.tr.t("updates.pin.persist"), cx.pal));
    });
    ui.add(egui::Label::new(Text::caption(cx.tr.t("updates.pin.note"), cx.pal)).wrap());
    ui.add(egui::Label::new(Text::caption(cx.tr.t("updates.pin.no_daemon"), cx.pal)).wrap());
    let chosen = pin_for(state.choice, state.version.as_ref(), &state.path);
    let blocked = match &chosen {
        Some(AppCodexPin::Version { version }) => pin
            .candidates
            .iter()
            .find(|c| &c.release.version == version)
            .is_some_and(|c| c.tier.is_blocked()),
        _ => false,
    };
    let changed = chosen.as_ref().is_some_and(|c| c != &pin.pin);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = ui
                .add(
                    Button::primary(cx.tr.t("updates.pin.apply"))
                        .small()
                        .enabled(changed && !blocked && !cx.busy),
                )
                .on_hover_text(cx.tr.t("updates.pin.apply_tip"));
            if r.clicked()
                && let Some(p) = chosen.clone()
            {
                out.push(Action::Plan(Box::new(pin_request(p))));
            }
            if blocked {
                ui.label(
                    Text::caption(cx.tr.t("updates.compat.below_floor_tip"), cx.pal)
                        .color(cx.pal.red),
                );
            }
        });
    });
}

/// What a CLI row offers for the version picked for this computer (`target`):
/// a standalone install switches to `target` when that differs from the row's
/// version; npm / Homebrew rows get their command; anything else nothing.
pub fn row_action(row: &CliVersionRow, target: Option<&CodexVersion>) -> CliUpdateAction {
    if let CliUpdateAction::Command { command } = &row.action {
        return CliUpdateAction::Command {
            command: command.clone(),
        };
    }
    let installable = row.kind == CodexInstallKind::Standalone
        || matches!(row.action, CliUpdateAction::Install { .. });
    match (target, &row.action) {
        (Some(t), _) if installable && row.binary.version.as_ref() != Some(t) => {
            CliUpdateAction::Install { version: t.clone() }
        }
        (None, CliUpdateAction::Install { version }) => CliUpdateAction::Install {
            version: version.clone(),
        },
        _ => CliUpdateAction::None,
    }
}

/// Label of an install button: update to a newer version, switch to an older
/// one, or `fresh_key` where no version is known.
fn install_label(
    cx: &Cx<'_>,
    have: Option<&CodexVersion>,
    target: &CodexVersion,
    fresh_key: &str,
) -> String {
    let key = match have {
        Some(h) if target < h => "updates.cli.switch_to",
        Some(_) => "updates.cli.update_to",
        None => fresh_key,
    };
    cx.tr.f(key, &[("version", target)])
}

/// Indent of the picker row under a CLI or remote row.
const PICKER_INDENT: f32 = 24.0;

/// One install picker (the local CLI, a remote host) for one frame.
struct InstallPicker {
    key: String,
    was: bool,
    shown: bool,
    /// A version was chosen: the box keeps its state from now on.
    chosen: bool,
    items: Vec<PickerItem>,
    target: Option<CodexVersion>,
    target_tags: Vec<VersionTag>,
}

impl InstallPicker {
    fn new(state: &State, view: &VersionsView, key: String, marks: &Marks) -> Self {
        let was = state.shows_prerelease(&key, marks.current.as_ref());
        let items = picker_items(view, was, false, marks);
        let target = state.pick(&key, &items);
        let target_tags = target
            .as_ref()
            .map(|v| tags_of(view, v, false, marks))
            .unwrap_or_default();
        Self {
            key,
            was,
            shown: was,
            chosen: false,
            items,
            target,
            target_tags,
        }
    }

    /// Draw the picker; a version chosen in its menu becomes the target.
    fn show(&mut self, ui: &mut Ui, cx: &Cx<'_>, picks: &mut BTreeMap<String, CodexVersion>) {
        let id = format!("updates-picker-{}", self.key);
        let selected = self
            .target
            .as_ref()
            .map(|v| (v, self.target_tags.as_slice()));
        if let Some(v) = version_picker(ui, cx, &mut self.shown, &id, &self.items, selected) {
            picks.insert(self.key.clone(), v);
            self.chosen = true;
            ui.ctx().request_repaint();
        }
    }

    /// Remember a ticked or cleared checkbox (and its state once a version
    /// was chosen).
    fn finish(self, state: &mut State) {
        if self.shown != self.was || self.chosen {
            state.prerelease.insert(self.key, self.shown);
        }
    }
}

/// A left-aligned, truncated cell of a fixed width.
/// Width of a version cell: room for `0.163.0-alpha.10.2` in mono.
const VERSION_COLUMN: f32 = 150.0;
/// The CLI row's columns after the path (version, kind, PATH rank, the copy
/// command button) with their gaps.
const CLI_FIXED_COLUMNS: f32 = VERSION_COLUMN + 100.0 + 70.0 + 150.0 + 4.0 * 12.0;

fn cell(ui: &mut Ui, width: f32, label: egui::Label) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 22.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(label.truncate())
        },
    )
    .inner
}

fn kind_label(cx: &Cx<'_>, kind: CodexInstallKind) -> String {
    cx.tr.text(&kind.text())
}

/// Width of the pin rows' label column: the wider of the two radio buttons that
/// have controls next to them (the bundled one's hint follows its own label).
fn pin_label_width(ui: &Ui, cx: &Cx<'_>) -> f32 {
    let spacing = ui.spacing();
    let text = ["updates.pin.version", "updates.pin.path"]
        .into_iter()
        .map(|key| {
            egui::WidgetText::from(cx.tr.t(key))
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Button,
                )
                .size()
                .x
        })
        .fold(0.0, f32::max);
    text + spacing.icon_width + spacing.icon_spacing
}

fn cli_section(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    view: &VersionsView,
    out: &mut Vec<Action>,
) {
    icon_section(ui, cx, Icon::Terminal, &cx.tr.t("updates.cli.title"));
    let local = InstallTarget::Local;
    let key = install_key(&local);
    let mut picker = InstallPicker::new(state, view, key.clone(), &local_marks(view));
    let target = picker.target.clone();
    if view.cli.is_empty() {
        ui.label(Text::small(cx.tr.t("updates.cli.none"), cx.pal));
        picker_row(ui, |ui| {
            picker.show(ui, cx, &mut state.picks);
            let busy = state.installing(&local);
            if let Some(t) = &target
                && ui
                    .add(
                        Button::primary(cx.tr.t("updates.cli.install"))
                            .small()
                            .loading(busy)
                            .enabled(!busy),
                    )
                    .clicked()
            {
                state.install(cx, t.clone(), local.clone());
            }
        });
    }
    // The first standalone install gets the picker (one local install target).
    let mut picker_shown = false;
    for row in &view.cli {
        let action = row_action(row, target.as_ref());
        let standalone = row.kind == CodexInstallKind::Standalone
            || matches!(row.action, CliUpdateAction::Install { .. });
        let with_picker = standalone && !std::mem::replace(&mut picker_shown, true);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            // The path takes what the other columns leave, shortened in the
            // middle so the file name stays visible (review ux-visual #12).
            let path = row.binary.path.display().to_string();
            let path_w = (ui.available_width() - CLI_FIXED_COLUMNS).clamp(220.0, 440.0);
            let shown = crate::design::widgets::value_chip::fit(
                ui,
                &path,
                &crate::design::fonts::mono_font(
                    text::SMALL,
                    crate::design::fonts::Weight::Regular,
                ),
                path_w,
                crate::design::elide::Ellipsis::Path,
            );
            cell(ui, path_w, egui::Label::new(Text::mono(shown, cx.pal))).on_hover_text(path);
            cell(
                ui,
                VERSION_COLUMN,
                egui::Label::new(Text::mono(
                    row.binary
                        .version
                        .as_ref()
                        .map_or_else(|| "—".into(), |v| v.to_string()),
                    cx.pal,
                )),
            );
            cell(
                ui,
                100.0,
                egui::Label::new(Text::small(kind_label(cx, row.kind), cx.pal)),
            );
            cell(
                ui,
                70.0,
                egui::Label::new(Text::caption(
                    row.binary.path_rank.map_or_else(
                        || "—".into(),
                        |r| cx.tr.f("updates.cli.path_pos", &[("n", &(r + 1))]),
                    ),
                    cx.pal,
                )),
            );
            match &action {
                CliUpdateAction::Command { command } => {
                    if ui
                        .add(
                            Button::ghost(cx.tr.t("updates.cli.copy_cmd"))
                                .icon(Icon::Copy)
                                .small(),
                        )
                        .on_hover_text(command.clone())
                        .clicked()
                    {
                        out.push(Action::Copy(command.clone()));
                    }
                }
                _ if with_picker => {}
                _ => {
                    ui.label(Text::caption("—", cx.pal));
                }
            }
        });
        if with_picker {
            picker_row(ui, |ui| {
                ui.add_space(PICKER_INDENT);
                picker.show(ui, cx, &mut state.picks);
                if let CliUpdateAction::Install { version } = action {
                    let busy = state.installing(&local);
                    if ui
                        .add(
                            Button::new(install_label(
                                cx,
                                row.binary.version.as_ref(),
                                &version,
                                "updates.cli.update_to",
                            ))
                            .small()
                            .loading(busy)
                            .enabled(!busy),
                        )
                        .clicked()
                    {
                        state.install(cx, version, local.clone());
                    }
                }
            });
        }
        if row.kind == CodexInstallKind::Standalone
            && let Some(result) = state.results.get(&key).cloned()
        {
            result_row(ui, cx, state, result, local.clone());
        }
    }
    picker.finish(state);
}

fn result_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    result: Result<InstallReport, String>,
    target: InstallTarget,
) {
    match result {
        Ok(rep) => {
            ui.horizontal(|ui| {
                ui.label(Text::caption(
                    cx.tr.f("updates.installed", &[("version", &rep.version)]),
                    cx.pal,
                ));
                for n in &rep.notes {
                    ui.label(Text::caption(cx.localize(&n.text), cx.pal));
                }
                if let Some(prev) = rep.previous.clone()
                    && link_button(
                        ui,
                        &cx.tr.f("updates.cli.rollback", &[("version", &prev)]),
                        false,
                    )
                    .clicked()
                {
                    state.rollback(cx, target);
                }
            });
        }
        Err(e) => callout(ui, cx, Status::Error, &e),
    }
}

fn remote_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &VersionsView) {
    icon_section(ui, cx, Icon::Server, &cx.tr.t("updates.remote.title"));
    let focus = state.focus.take();
    for row in &view.remote {
        let target = InstallTarget::Remote {
            target_id: row.target_id.clone(),
        };
        let unreachable = row.error.is_some() && row.version.is_none();
        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            cell(
                ui,
                160.0,
                egui::Label::new(Text::strong(&row.label, cx.pal)).truncate(),
            );
            cell(
                ui,
                VERSION_COLUMN,
                egui::Label::new(Text::mono(
                    row.version
                        .as_ref()
                        .map_or_else(|| "—".into(), |v| v.to_string()),
                    cx.pal,
                )),
            );
            cell(
                ui,
                100.0,
                egui::Label::new(Text::small(
                    row.install_kind
                        .map_or_else(|| "—".into(), |k| kind_label(cx, k)),
                    cx.pal,
                )),
            );
            if let Some(t) = row.tier {
                let (status, key, tip) = compat_badge(t);
                let r = ui.add(Badge::new(cx.tr.t(key)).status(status));
                if let Some(tip) = tip {
                    r.on_hover_text(cx.tr.t(tip));
                }
            }
            if unreachable {
                ui.label(Text::caption(cx.tr.t("updates.remote.unreachable"), cx.pal));
            } else if row.install_kind == Some(CodexInstallKind::Other) && row.version.is_some() {
                ui.label(Text::caption(cx.tr.t("updates.remote.app_command"), cx.pal));
            }
        });
        let mut rect = r.response.rect;
        if !unreachable {
            let mut picker =
                InstallPicker::new(state, view, install_key(&target), &remote_marks(view, row));
            rect = rect.union(picker_row(ui, |ui| {
                ui.add_space(PICKER_INDENT);
                picker.show(ui, cx, &mut state.picks);
                remote_action(ui, cx, state, row, &picker, &target);
                ui.min_rect()
            }));
            picker.finish(state);
        }
        if focus.as_ref() == Some(&row.target_id) {
            ui.scroll_to_rect(rect, Some(Align::Center));
            ui.painter().rect_stroke(
                rect.expand(3.0),
                crate::design::tokens::radius::CONTROL,
                egui::Stroke::new(1.0, cx.pal.focus),
                egui::StrokeKind::Outside,
            );
        }
        if let Some(e) = &row.error {
            ui.label(Text::caption(error_text(cx.tr, e), cx.pal).color(cx.pal.amber));
        }
        if let Some(result) = state.results.get(&install_key(&target)).cloned() {
            result_row(ui, cx, state, result, target);
        }
    }
    ui.add(egui::Label::new(Text::caption(cx.tr.t("updates.remote.note"), cx.pal)).wrap());
}

/// The button of a remote row: install the picked version when the host has
/// another one ("up to date" when it already has the newest offered).
fn remote_action(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    row: &RemoteVersionRow,
    picker: &InstallPicker,
    target: &InstallTarget,
) {
    let Some(t) = &picker.target else {
        return;
    };
    if row.version.as_ref() == Some(t) {
        if picker.items.first().map(|i| &i.version) == Some(t) {
            ui.label(Text::caption(cx.tr.t("updates.remote.current"), cx.pal));
        }
        return;
    }
    let busy = state.installing(target);
    if ui
        .add(
            Button::new(install_label(
                cx,
                row.version.as_ref(),
                t,
                "updates.remote.install",
            ))
            .small()
            .loading(busy)
            .enabled(!busy && !cx.busy),
        )
        .on_hover_text(cx.tr.t("updates.remote.install_tip"))
        .clicked()
    {
        state.install(cx, t.clone(), target.clone());
    }
}

/// Command palette entries.
pub fn palette(cx: &Cx<'_>, state: &State) -> Vec<PaletteItem> {
    let mut items = vec![PaletteItem {
        title: cx.tr.t("updates.palette.check"),
        detail: None,
        keywords: vec!["update".into(), "version".into(), "更新".into()],
        group: PaletteGroup::Commands,
        action: Action::PageCommand {
            page: Page::Updates,
            command: "check".into(),
        },
    }];
    if state.view.value.as_ref().is_some_and(|v| {
        v.cli
            .iter()
            .any(|r| matches!(r.action, CliUpdateAction::Install { .. }))
    }) {
        items.push(PaletteItem {
            title: cx.tr.t("updates.palette.update_cli"),
            detail: None,
            keywords: vec!["codex".into(), "cli".into(), "update".into()],
            group: PaletteGroup::Commands,
            action: Action::PageCommand {
                page: Page::Updates,
                command: "update_cli".into(),
            },
        });
    }
    items
}

/// Run a command addressed to this page: `check`, `update_cli` (update the
/// first updatable CLI), `focus:<target id>` (bring a remote row into view).
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    let _ = out;
    match command {
        "check" => state.check(),
        "update_cli" => {
            // The version the page's local picker targets, when it is newer.
            let version = state.view.value.as_ref().and_then(|v| {
                let key = install_key(&InstallTarget::Local);
                let target = InstallPicker::new(state, v, key, &local_marks(v)).target;
                v.cli
                    .iter()
                    .find_map(|r| match row_action(r, target.as_ref()) {
                        CliUpdateAction::Install { version }
                            if r.binary.version.as_ref().is_none_or(|h| &version > h) =>
                        {
                            Some(version)
                        }
                        _ => None,
                    })
            });
            if let Some(version) = version {
                state.install(cx, version, InstallTarget::Local);
            }
        }
        _ => {
            if let Some(id) = command.strip_prefix("focus:") {
                state.focus = Some(TargetId::new(id));
            }
        }
    }
}

/// Dialogs and the sidebar badge (an update is available).
pub fn status(state: &State) -> PageStatus {
    // The sidebar dot comes from the shell's own version data
    // (`app::shell_badges`), one source for every page.
    let _ = state;
    PageStatus::default()
}
