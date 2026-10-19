//! The Updates page's first section: Superba itself. Current and latest
//! version, the update channel (Stable / Preview, the same choice as in
//! Preferences) with what each channel has ("You're on preview 0.1.1-alpha.3 ·
//! latest preview … · latest stable …"), the release notes as plain text with a
//! little markdown, and the user's choices (update now, later, skip this
//! version). Nothing is installed without the click on the update button, and
//! an older version is never offered.
//!
//! States: not checked, up to date, update available (also skipped), the
//! release page cannot be read (a private repository today: a calm line, never
//! an error toast), a check that failed, online checks off, and the install
//! (running, installed, handed off to the new version, manual steps).

use std::time::Duration;

use egui::{Sense, Ui, vec2};
use superba_core::paths::display_path;
use superba_core::{
    SelfInstallOutcome, SelfUpdateInstallRequest, SelfUpdateProblem, SelfUpdateReport,
    SelfUpdateStatus, SelfUpdateView, UpdateChannel,
};

use super::panels::kv;
use crate::design::icons::Icon;
use crate::design::tokens::radius;
use crate::design::widgets::{Badge, Button, Segmented, Status, Text};
use crate::i18n::Tr;
use crate::pages::{Action, Cx, callout, error_text};
use crate::runtime::ReadTask;

/// One line of rendered release notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteLine {
    /// A heading (any level).
    Heading(String),
    /// A list item.
    Bullet(String),
    /// A paragraph line.
    Text(String),
    /// A blank line.
    Gap,
}

/// Inline markdown to plain text: `**bold**`, `__bold__`, `` `code` ``,
/// `[text](url)` and `<tag>` markers dropped.
pub fn inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' | '_' | '`' if chars.get(i + 1) == Some(&chars[i]) || chars[i] == '`' => {
                i += 1;
                while chars.get(i) == Some(&chars[i - 1]) {
                    i += 1;
                }
            }
            '[' => {
                // [text](url) keeps the text.
                if let Some(close) = chars[i..].iter().position(|c| *c == ']')
                    && chars.get(i + close + 1) == Some(&'(')
                    && let Some(end) = chars[i + close..].iter().position(|c| *c == ')')
                {
                    out.extend(&chars[i + 1..i + close]);
                    i += close + end + 1;
                } else {
                    out.push('[');
                    i += 1;
                }
            }
            '<' => {
                // An HTML tag or autolink: drop the brackets, keep the content.
                if let Some(close) = chars[i..].iter().position(|c| *c == '>') {
                    let inner: String = chars[i + 1..i + close].iter().collect();
                    if inner.starts_with("http") {
                        out.push_str(&inner);
                    }
                    i += close + 1;
                } else {
                    out.push('<');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.trim_end().to_owned()
}

/// Release notes as lines (markdown-lite: headings, list items, paragraphs;
/// fences, quotes and inline markers are dropped).
pub fn note_lines(notes: &str) -> Vec<NoteLine> {
    let mut out: Vec<NoteLine> = Vec::new();
    for raw in notes.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let item = if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            continue;
        } else if trimmed.is_empty() {
            if !matches!(out.last(), Some(NoteLine::Gap) | None) {
                out.push(NoteLine::Gap);
            }
            continue;
        } else if trimmed.starts_with('#') {
            NoteLine::Heading(inline(trimmed.trim_start_matches('#').trim()))
        } else if let Some(rest) = ["- ", "* ", "+ "]
            .iter()
            .find_map(|p| trimmed.strip_prefix(p))
        {
            NoteLine::Bullet(inline(rest))
        } else if let Some((num, rest)) = trimmed.split_once(". ")
            && !num.is_empty()
            && num.bytes().all(|b| b.is_ascii_digit())
        {
            NoteLine::Bullet(inline(rest))
        } else {
            NoteLine::Text(inline(trimmed.trim_start_matches('>').trim_start()))
        };
        out.push(item);
    }
    while matches!(out.last(), Some(NoteLine::Gap)) {
        out.pop();
    }
    out
}

/// What the channels have: "You're on preview 0.1.1-alpha.3 · latest preview
/// 0.1.1-alpha.4 · latest stable 0.1.0" (the parts that are known).
pub fn channel_line(tr: &Tr, view: &SelfUpdateView) -> String {
    let on = if view.on_prerelease() {
        "updates.superba.on_preview"
    } else {
        "updates.superba.on_stable"
    };
    let mut parts = vec![tr.f(on, &[("version", &view.current)])];
    if let Some(p) = &view.latest_preview {
        parts.push(tr.f("updates.superba.latest_preview", &[("version", &p.version)]));
    }
    if let Some(s) = &view.latest_stable {
        parts.push(tr.f("updates.superba.latest_stable", &[("version", &s.version)]));
    }
    parts.join(" · ")
}

/// Why nothing is offered although a release exists: a preview build on the
/// Stable channel waits for the next final release (never a downgrade).
pub fn no_downgrade_note(tr: &Tr, view: &SelfUpdateView) -> Option<String> {
    (view.channel == UpdateChannel::Stable
        && view.on_prerelease()
        && view.status == SelfUpdateStatus::UpToDate)
        .then(|| {
            tr.f(
                "updates.superba.no_downgrade",
                &[("version", &view.current)],
            )
        })
}

/// How the last install attempt's problem is told: the start-up toast and the
/// Updates page use the same words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemText {
    /// Error: the update failed; warning: installed, but a later step failed.
    pub status: Status,
    /// One line.
    pub title: String,
    /// The reason (`reason`, already localized) and where the log is.
    pub body: String,
}

/// The words for `problem`, whose reason reads `reason` in the UI language.
pub fn problem_text(tr: &Tr, problem: &SelfUpdateProblem, reason: &str) -> ProblemText {
    let (status, key) = if problem.installed {
        (Status::Warn, "updates.superba.problem_installed")
    } else {
        (Status::Error, "updates.superba.problem_failed")
    };
    ProblemText {
        status,
        title: tr.f(key, &[("version", &problem.version)]),
        body: tr.f(
            "updates.superba.problem_body",
            &[("reason", &reason), ("path", &display_path(&problem.log))],
        ),
    }
}

/// The last attempt's problem on the page (it stays until the next attempt).
fn problem_callout(ui: &mut Ui, cx: &Cx<'_>, problem: &SelfUpdateProblem, out: &mut Vec<Action>) {
    let text = problem_text(cx.tr, problem, &cx.localize(&problem.reason));
    callout(
        ui,
        cx,
        text.status,
        &format!("{}\n{}", text.title, text.body),
    );
    ui.add_space(6.0);
    if ui
        .add(
            Button::new(cx.tr.t("updates.superba.open_log"))
                .icon(Icon::External)
                .small(),
        )
        .clicked()
    {
        out.push(Action::OpenUrl(problem.log.display().to_string()));
    }
}

/// State of the section (kept across navigation).
#[derive(Debug, Default)]
pub struct SelfState {
    install: Option<ReadTask<SelfUpdateReport>>,
    /// Cancellation state until the backend finishes.
    pub control: Option<std::sync::Arc<superba_core::UpdateControl>>,
    /// How the last install ended (`Err` = translated error).
    pub result: Option<Result<SelfUpdateReport, String>>,
}

impl SelfState {
    /// An install is running.
    pub fn busy(&self) -> bool {
        self.install.is_some()
    }

    /// Download, verify and install the offered release (background). The click
    /// on the button is the only caller.
    pub fn install(&mut self, cx: &Cx<'_>) {
        let version = cx
            .data
            .selfupdate
            .value
            .as_ref()
            .and_then(|v| v.latest.as_ref())
            .map(|r| r.version.clone());
        self.install_version(cx, version);
    }

    /// Keep the reviewed release fixed while earlier queued updates run.
    pub fn install_version(&mut self, cx: &Cx<'_>, version: Option<String>) {
        if self.install.is_some() {
            return;
        }
        self.result = None;
        let control = std::sync::Arc::new(superba_core::UpdateControl::default());
        self.control = Some(control.clone());
        let service = cx.service.clone();
        self.install = Some(crate::runtime::task(async move {
            service
                .self_update_install_controlled(
                    SelfUpdateInstallRequest {
                        relaunch: true,
                        version,
                    },
                    control,
                )
                .await
        }));
    }

    /// Request a stop before replacing the running app.
    pub fn cancel(&self) {
        if let Some(control) = &self.control {
            control.cancel();
        }
    }

    /// Collect a finished install; returns the toast to show.
    pub fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> Option<(Status, String)> {
        let done = self.install.as_ref()?.poll()?;
        self.install = None;
        self.control = None;
        if matches!(done, Err(superba_core::Error::Cancelled)) {
            self.result = None;
            return Some((Status::Idle, cx.tr.t("updates.win.cancelled")));
        }
        let toast = match &done {
            Ok(report) => {
                if report.relaunching {
                    out.push(Action::QuitApp);
                }
                Some((
                    Status::Ok,
                    cx.tr.f(
                        "updates.superba.installed_toast",
                        &[("version", &report.version)],
                    ),
                ))
            }
            Err(e) => Some((Status::Error, error_text(cx.tr, e))),
        };
        self.result = Some(done.map_err(|e| error_text(cx.tr, &e)));
        toast
    }
}

fn status_badge(cx: &Cx<'_>, view: &SelfUpdateView) -> Option<Badge> {
    Some(match view.status {
        SelfUpdateStatus::UpToDate => {
            Badge::new(cx.tr.t("updates.superba.up_to_date")).status(Status::Ok)
        }
        SelfUpdateStatus::Available if view.is_skipped() => {
            Badge::new(cx.tr.t("updates.superba.skipped")).status(Status::Idle)
        }
        SelfUpdateStatus::Available => {
            Badge::new(cx.tr.t("updates.superba.available")).status(Status::Warn)
        }
        _ => return None,
    })
}

fn bullet(ui: &mut Ui, cx: &Cx<'_>, text: &str) {
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::hover());
        ui.painter()
            .circle_filled(r.center() + vec2(-1.0, 1.0), 2.0, cx.pal.text_muted);
        ui.add(egui::Label::new(Text::small(text, cx.pal).color(cx.pal.text)).wrap());
    });
}

fn notes_box(ui: &mut Ui, cx: &Cx<'_>, notes: &str) {
    let lines = note_lines(notes);
    if lines.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(Text::small(cx.tr.t("updates.superba.notes"), cx.pal));
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(cx.pal.bg_subtle)
        .stroke(egui::Stroke::new(1.0, cx.pal.border))
        .corner_radius(radius::CONTROL + 2)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("superba-release-notes")
                .max_height(180.0)
                .min_scrolled_height(180.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for line in &lines {
                        match line {
                            NoteLine::Heading(t) => {
                                ui.add(egui::Label::new(Text::strong(t, cx.pal)).wrap());
                            }
                            NoteLine::Bullet(t) => bullet(ui, cx, t),
                            NoteLine::Text(t) => {
                                ui.add(
                                    egui::Label::new(Text::small(t, cx.pal).color(cx.pal.text))
                                        .wrap(),
                                );
                            }
                            NoteLine::Gap => ui.add_space(4.0),
                        }
                    }
                });
        });
}

/// The state line when there is no newer release to talk about.
fn quiet_line(cx: &Cx<'_>, view: &SelfUpdateView) -> Option<String> {
    Some(match view.status {
        SelfUpdateStatus::PageUnavailable => cx.tr.t("updates.superba.page_unavailable"),
        SelfUpdateStatus::CheckFailed => cx.tr.f(
            "updates.superba.check_failed",
            &[("reason", &view.detail.clone().unwrap_or_default())],
        ),
        SelfUpdateStatus::OnlineChecksOff => cx.tr.t("superba.network.online_off"),
        SelfUpdateStatus::NotChecked => cx.tr.t("updates.superba.not_checked"),
        _ => return None,
    })
}

fn install_result(
    ui: &mut Ui,
    cx: &Cx<'_>,
    result: &Result<SelfUpdateReport, String>,
    out: &mut Vec<Action>,
) {
    match result {
        Err(e) => callout(ui, cx, Status::Error, e),
        Ok(r) => match r.outcome {
            SelfInstallOutcome::Manual => {
                callout(ui, cx, Status::Warn, &cx.tr.t("updates.superba.manual"));
                for step in &r.steps {
                    ui.add(
                        egui::Label::new(Text::small(cx.localize(step), cx.pal).color(cx.pal.text))
                            .wrap(),
                    );
                }
                if let Some(dir) = r.package.as_ref().and_then(|p| p.parent()) {
                    ui.add_space(6.0);
                    if ui
                        .add(
                            Button::new(cx.tr.t("updates.superba.reveal"))
                                .icon(Icon::External)
                                .small(),
                        )
                        .clicked()
                    {
                        out.push(Action::OpenUrl(dir.display().to_string()));
                    }
                }
            }
            SelfInstallOutcome::Handoff => {
                callout(
                    ui,
                    cx,
                    Status::Ok,
                    &cx.tr
                        .f("updates.superba.handoff", &[("version", &r.version)]),
                );
            }
            _ => {
                let key = if r.relaunching {
                    "updates.superba.restarting"
                } else {
                    "updates.superba.installed"
                };
                callout(
                    ui,
                    cx,
                    Status::Ok,
                    &cx.tr.f(key, &[("version", &r.version)]),
                );
                if r.router_restarted {
                    ui.add(
                        egui::Label::new(Text::caption(
                            cx.tr.t("updates.superba.router_restarted"),
                            cx.pal,
                        ))
                        .wrap(),
                    );
                }
            }
        },
    }
}

/// Draw the section (the caller draws the heading and the gap after it).
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut SelfState, out: &mut Vec<Action>) {
    if state.busy() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let Some(view) = cx.data.selfupdate.value.clone() else {
        let text = if cx.data.selfupdate.loading {
            cx.tr.t("updates.superba.checking")
        } else {
            cx.tr.t("updates.superba.not_checked")
        };
        ui.label(Text::small(text, cx.pal));
        return;
    };
    kv(ui, cx, &cx.tr.t("updates.superba.current"), |ui| {
        ui.label(Text::mono(&view.current, cx.pal));
    });
    kv(ui, cx, &cx.tr.t("updates.superba.channel"), |ui| {
        let mut channel = view.channel;
        Segmented::new("superba-update-channel", &mut channel)
            .label(cx.tr.t("updates.superba.channel"))
            .option(
                UpdateChannel::Stable,
                cx.tr.t("updates.superba.channel_stable"),
            )
            .option(
                UpdateChannel::Preview,
                cx.tr.t("updates.superba.channel_preview"),
            )
            .small()
            .show(ui);
        if channel != view.channel {
            out.push(Action::SetUpdateChannel(channel));
        }
    });
    let latest = view.latest.as_ref().filter(|_| {
        matches!(
            view.status,
            SelfUpdateStatus::Available | SelfUpdateStatus::UpToDate
        )
    });
    kv(
        ui,
        cx,
        &cx.tr.t("updates.superba.latest"),
        |ui| match latest {
            Some(l) => {
                let date = l
                    .published_at
                    .as_deref()
                    .and_then(|d| d.get(..10))
                    .map(|d| cx.tr.f("updates.app.released", &[("date", &d)]));
                let mut text = l.version.clone();
                if let Some(d) = date {
                    text = format!("{text} · {d}");
                }
                ui.label(Text::mono(text, cx.pal));
                if l.prerelease {
                    ui.add(Badge::new(cx.tr.t("updates.superba.prerelease")).outline());
                }
                if let Some(b) = status_badge(cx, &view) {
                    ui.add(b);
                }
            }
            None => {
                ui.label(Text::small(
                    quiet_line(cx, &view).unwrap_or_else(|| "—".into()),
                    cx.pal,
                ));
            }
        },
    );
    if view.latest_stable.is_some() || view.latest_preview.is_some() {
        ui.add(egui::Label::new(Text::caption(channel_line(cx.tr, &view), cx.pal)).wrap());
    }
    if let Some(note) = no_downgrade_note(cx.tr, &view) {
        ui.add(egui::Label::new(Text::caption(note, cx.pal)).wrap());
    }
    if let Some(t) = view.limited_until {
        ui.add(
            egui::Label::new(Text::caption(
                cx.tr.f(
                    "updates.superba.rate_limited",
                    &[("time", &crate::pages::clock_time(t, false))],
                ),
                cx.pal,
            ))
            .wrap(),
        );
    }
    if view.status == SelfUpdateStatus::Available
        && let Some(release) = view.latest.clone()
    {
        notes_box(ui, cx, &release.notes);
        ui.add_space(8.0);
        if view.no_package {
            callout(ui, cx, Status::Warn, &cx.tr.t("updates.superba.no_package"));
        } else {
            let busy = state.busy();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        Button::primary(cx.tr.t("updates.superba.update_now"))
                            .icon(Icon::Download)
                            .small()
                            .loading(busy)
                            .enabled(!busy),
                    )
                    .clicked()
                {
                    state.install(cx);
                }
                if busy {
                    ui.label(Text::caption(cx.tr.t("updates.superba.installing"), cx.pal));
                    return;
                }
                if !view.is_skipped()
                    && ui
                        .add(Button::new(cx.tr.t("updates.superba.later")).small())
                        .on_hover_text(cx.tr.t("updates.superba.later_tip"))
                        .clicked()
                {
                    out.push(Action::LaterSelfUpdate(release.version.clone()));
                }
                let (label, skip) = if view.is_skipped() {
                    (cx.tr.t("updates.superba.unskip"), None)
                } else {
                    (
                        cx.tr.t("updates.superba.skip"),
                        Some(release.version.clone()),
                    )
                };
                if ui.add(Button::new(label).small()).clicked() {
                    out.push(Action::SkipSelfUpdate(skip));
                }
            });
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(Text::caption(cx.tr.t("updates.superba.consent"), cx.pal)).wrap(),
            );
        }
    }
    if let Some(result) = &state.result {
        ui.add_space(8.0);
        install_result(ui, cx, result, out);
    } else if let Some(problem) = &view.problem {
        ui.add_space(8.0);
        problem_callout(ui, cx, problem, out);
    }
    if let Some(t) = view.checked_at {
        ui.add_space(6.0);
        ui.label(Text::caption(
            cx.tr.f("updates.checked", &[("age", &cx.ago(t))]),
            cx.pal,
        ));
    }
}
