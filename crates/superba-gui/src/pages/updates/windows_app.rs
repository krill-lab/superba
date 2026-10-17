//! The ChatGPT app row on Windows: download the installer package (with
//! progress, cancel and resume), show what was verified (source, version,
//! signer, framework packages to install first), open it with App Installer
//! where the user clicks Install, then offer to reopen ChatGPT through Superba
//! so hot switching keeps working. The Microsoft Store page and `winget` stay
//! as alternatives. The page decides by the update action, not by the OS, so
//! the row is tested on every platform.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Sense, Ui, vec2};
use superba_core::{
    AppDownloadReport, AppDownloadRequest, AppPackage, AppPackageSource, DownloadProgress,
    DownloadProgressSink, DownloadStage, Error, SharedDownloadProgress, WindowsAppDownload,
    WindowsAppSource,
};

use super::super::{Action, Cx, Page, callout, error_text, kv};
use super::command_row;
use crate::design::icons::Icon;
use crate::design::widgets::{Button, FormRow, Status, Text, link_button};
use crate::runtime::ReadTask;

/// Download, open and their results.
#[derive(Debug, Default)]
pub struct WinState {
    progress: Option<Arc<SharedDownloadProgress>>,
    task: Option<ReadTask<AppDownloadReport>>,
    /// The verified download.
    pub report: Option<AppDownloadReport>,
    /// Why the download failed (translated).
    pub error: Option<String>,
    /// The last download was stopped by the user (its partial file is kept).
    pub cancelled: bool,
    open_task: Option<(PathBuf, ReadTask<()>)>,
    /// Packages handed to App Installer.
    pub opened: Vec<PathBuf>,
    /// Why opening failed (translated).
    pub open_error: Option<String>,
}

impl WinState {
    /// A download or an open is running.
    pub fn busy(&self) -> bool {
        self.task.is_some() || self.open_task.is_some()
    }

    /// The download is running.
    pub fn downloading(&self) -> bool {
        self.task.is_some()
    }

    /// The latest progress report of the running download.
    pub fn progress(&self) -> Option<DownloadProgress> {
        self.progress.as_ref().and_then(|p| p.latest())
    }

    /// Start the download (background).
    pub fn start(&mut self, cx: &Cx<'_>) {
        if self.task.is_some() {
            return;
        }
        self.error = None;
        self.cancelled = false;
        self.open_error = None;
        let shared = Arc::new(SharedDownloadProgress::default());
        let sink: Arc<dyn DownloadProgressSink> = shared.clone();
        self.progress = Some(shared);
        let service = cx.service.clone();
        self.task = Some(crate::runtime::task(async move {
            service
                .download_app_package(AppDownloadRequest::default(), sink)
                .await
        }));
    }

    /// Ask the running download to stop.
    pub fn cancel(&mut self) {
        if let Some(p) = &self.progress {
            p.cancel();
        }
    }

    /// Open `path` with App Installer (background).
    pub fn open(&mut self, cx: &Cx<'_>, path: PathBuf) {
        if self.open_task.is_some() {
            return;
        }
        self.open_error = None;
        let service = cx.service.clone();
        let p = path.clone();
        self.open_task = Some((
            path,
            crate::runtime::task(async move { service.open_app_package(p).await }),
        ));
    }

    /// Collect finished work.
    pub fn poll(&mut self, cx: &Cx<'_>) {
        if let Some(t) = &self.task
            && let Some(r) = t.poll()
        {
            self.task = None;
            self.progress = None;
            match r {
                Ok(rep) => self.report = Some(rep),
                Err(Error::Cancelled) => self.cancelled = true,
                Err(e) => self.error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some((path, t)) = &self.open_task
            && let Some(r) = t.poll()
        {
            let path = path.clone();
            self.open_task = None;
            match r {
                Ok(()) => {
                    if !self.opened.contains(&path) {
                        self.opened.push(path);
                    }
                }
                Err(e) => self.open_error = Some(error_text(cx.tr, &e)),
            }
        }
    }
}

/// Locale key of a source setting.
pub fn source_key(source: WindowsAppSource) -> &'static str {
    match source {
        WindowsAppSource::OfficialOnly => "updates.win.source_official_only",
        WindowsAppSource::StoreMirrorOnly => "updates.win.source_store_only",
        _ => "updates.win.source_official_first",
    }
}

/// `905.0` style megabytes (decimal, like the Linux download button).
fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// The progress line of `p` (translated).
pub fn progress_line(cx: &Cx<'_>, p: &DownloadProgress) -> String {
    match p.stage {
        DownloadStage::Resolving => {
            let source = p.source.map(|s| cx.localize(&s.text())).unwrap_or_default();
            cx.tr.f("updates.win.resolving", &[("source", &source)])
        }
        DownloadStage::Verifying => cx.tr.f("updates.win.verifying", &[("file", &p.file)]),
        _ => {
            let percent = p
                .fraction()
                .map(|f| format!("{:.0}%", f * 100.0))
                .unwrap_or_else(|| "?".into());
            let total = p.total.map(mb).unwrap_or_else(|| "?".into());
            let mut line = cx.tr.f(
                "updates.win.downloading",
                &[
                    ("file", &p.file),
                    ("done", &mb(p.done)),
                    ("total", &total),
                    ("percent", &percent),
                ],
            );
            if p.resumed_from > 0 {
                line.push_str(" · ");
                line.push_str(
                    &cx.tr
                        .f("updates.win.resumed", &[("done", &mb(p.resumed_from))]),
                );
            }
            if p.count > 1 {
                line.push_str(&format!(" · {}/{}", p.index, p.count));
            }
            line
        }
    }
}

/// A thin neutral progress bar.
fn bar(ui: &mut Ui, cx: &Cx<'_>, fraction: Option<f32>) {
    let h = 6.0;
    let w = ui.available_width().min(420.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let r = (h / 2.0) as u8;
    ui.painter().rect_filled(rect, r, cx.pal.active);
    if let Some(f) = fraction {
        let mut fill = rect;
        fill.set_width((rect.width() * f.clamp(0.0, 1.0)).max(h));
        ui.painter().rect_filled(fill, r, cx.pal.primary);
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What was downloaded and verified, with the open buttons.
fn report_rows(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut WinState,
    w: &WindowsAppDownload,
    out: &mut Vec<Action>,
) {
    kv(ui, cx, &cx.tr.t("updates.win.file"), |ui| {
        ui.label(Text::mono(w.package.path.display().to_string(), cx.pal));
    });
    let id = &w.package.identity;
    kv(ui, cx, &cx.tr.t("updates.win.version"), |ui| {
        let text = match &w.release_version {
            Some(r) => cx.tr.f(
                "updates.win.version_value",
                &[("version", &id.version), ("release", r)],
            ),
            None => id.version.clone(),
        };
        ui.label(Text::mono(format!("{text} · {}", id.arch), cx.pal));
    });
    kv(ui, cx, &cx.tr.t("updates.win.source"), |ui| {
        ui.label(Text::small(cx.localize(&w.source.text()), cx.pal));
    });
    if w.source == AppPackageSource::StoreMirror {
        ui.add(egui::Label::new(Text::caption(cx.tr.t("updates.win.mirror_note"), cx.pal)).wrap());
    }
    for reason in &w.fallback_reasons {
        ui.add(
            egui::Label::new(Text::caption(
                cx.tr.f("updates.win.fallback", &[("reason", reason)]),
                cx.pal,
            ))
            .wrap(),
        );
    }
    if let Some(s) = &w.package.signer {
        kv(ui, cx, &cx.tr.t("updates.win.signer"), |ui| {
            ui.label(Text::small(
                cx.tr
                    .f("updates.win.signer_value", &[("subject", &s.subject)]),
                cx.pal,
            ))
            .on_hover_text(s.issuer.clone().unwrap_or_default());
        });
    }
    let opening = state.open_task.as_ref().map(|(p, _)| p.clone());
    for d in &w.dependencies {
        FormRow::unlabeled().show(ui, |ui| {
            ui.label(Text::small(
                cx.tr.f(
                    "updates.win.dependency",
                    &[("name", &d.identity.name), ("version", &d.identity.version)],
                ),
                cx.pal,
            ));
            let busy = opening.as_ref() == Some(&d.path);
            if ui
                .add(
                    Button::new(cx.tr.t("updates.win.open_installer"))
                        .small()
                        .loading(busy)
                        .enabled(opening.is_none()),
                )
                .on_hover_text(file_name(&d.path))
                .clicked()
            {
                state.open(cx, d.path.clone());
            }
        });
    }
    for d in &w.unresolved {
        callout(
            ui,
            cx,
            Status::Warn,
            &cx.tr.f(
                "updates.win.unresolved",
                &[("name", &d.name), ("version", &d.min_version)],
            ),
        );
    }
    let app = w.package.path.clone();
    let deps_left = w
        .dependencies
        .iter()
        .any(|d| !state.opened.contains(&d.path));
    FormRow::unlabeled().show(ui, |ui| {
        let busy = opening.as_ref() == Some(&app);
        let button = if deps_left {
            Button::new(cx.tr.t("updates.win.open_installer"))
        } else {
            Button::primary(cx.tr.t("updates.win.open_installer"))
        };
        if ui
            .add(
                button
                    .icon(Icon::AppWindow)
                    .small()
                    .loading(busy)
                    .enabled(opening.is_none()),
            )
            .on_hover_text(cx.tr.t("updates.win.open_tip"))
            .clicked()
        {
            state.open(cx, app.clone());
        }
    });
    if let Some(e) = &state.open_error {
        callout(ui, cx, Status::Error, e);
    }
    if state.opened.contains(&app) {
        callout(ui, cx, Status::Idle, &cx.tr.t("updates.win.opened"));
        if ui
            .add(Button::new(cx.tr.t("updates.win.reopen")).small())
            .on_hover_text(cx.tr.t("updates.app.reopen_tip"))
            .clicked()
        {
            out.push(Action::RestartApp);
        }
    }
}

/// Draw the Windows rows of the app section.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut WinState,
    package: Option<&AppPackage>,
    source: WindowsAppSource,
    store_url: Option<&str>,
    command: Option<&str>,
    out: &mut Vec<Action>,
) {
    state.poll(cx);
    let downloading = state.downloading();
    let windows = state.report.as_ref().and_then(|r| r.windows.clone());
    if windows.is_none() {
        FormRow::unlabeled().show(ui, |ui| {
            let label = match package.and_then(|p| p.size) {
                Some(s) => cx.tr.f(
                    "updates.win.download",
                    &[("size", &format!("{} MB", (s + 500_000) / 1_000_000))],
                ),
                None => cx.tr.t("updates.win.download_unknown_size"),
            };
            if ui
                .add(
                    Button::primary(label)
                        .icon(Icon::Download)
                        .small()
                        .loading(downloading)
                        .enabled(!downloading),
                )
                .clicked()
            {
                state.start(cx);
            }
            if downloading
                && ui
                    .add(Button::ghost(cx.tr.t("updates.win.cancel")).small())
                    .clicked()
            {
                state.cancel();
            }
        });
    }
    if downloading {
        let p = state.progress();
        bar(ui, cx, p.as_ref().and_then(DownloadProgress::fraction));
        let line = p
            .as_ref()
            .map(|p| progress_line(cx, p))
            .unwrap_or_else(|| cx.tr.t("updates.downloading"));
        ui.label(Text::caption(line, cx.pal));
    }
    kv(ui, cx, &cx.tr.t("updates.win.source_label"), |ui| {
        ui.label(Text::small(cx.tr.t(source_key(source)), cx.pal));
        if link_button(ui, &cx.tr.t("updates.win.change_source"), true).clicked() {
            out.push(Action::Go(Page::Preferences));
        }
    });
    if state.cancelled {
        callout(ui, cx, Status::Idle, &cx.tr.t("updates.win.cancelled"));
    }
    if let Some(e) = &state.error {
        callout(ui, cx, Status::Error, e);
        ui.add(egui::Label::new(Text::caption(cx.tr.t("updates.win.failed_hint"), cx.pal)).wrap());
    }
    if let Some(w) = &windows {
        report_rows(ui, cx, state, w, out);
    }
    if let Some(url) = store_url
        && link_button(ui, &cx.tr.t("updates.app.store"), true).clicked()
    {
        out.push(Action::OpenUrl(url.to_owned()));
    }
    if let Some(c) = command {
        command_row(ui, cx, c, out);
    }
    ui.add(egui::Label::new(Text::caption(cx.tr.t("updates.win.note"), cx.pal)).wrap());
}
