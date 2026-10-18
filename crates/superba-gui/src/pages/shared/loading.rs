//! Loads that follow a choice (user bug 83): the host of "Edit Codex files",
//! the provider of a settings page, the host of Sessions. Reading another host
//! takes seconds (SSH, `wsl.exe`), so the page shows the new choice at once and
//! a loading state in place of the old choice's data until the new data
//! arrives; a failure or a timeout is shown in place with Retry.
//!
//! [`Latest`] keeps the load of the latest choice only: choosing again while a
//! load runs drops the older load, so its answer is never read (the last choice
//! wins, whatever order the answers come in), and a load running past its time
//! limit fails with a timeout. It does not keep the loaded value: the page
//! keeps that together with the choice it belongs to, and shows it only while
//! that is still the choice.
//!
//! The drawing helpers name the loading state for screen readers and UI
//! Automation: one busy progress indicator named by its text ("Loading the
//! settings of devbox…").

use std::time::{Duration, Instant};

use egui::{Response, Sense, TextWrapMode, Ui, WidgetInfo, WidgetText, vec2};
use superba_core::Error;

use crate::design::icons::{self, Icon};
use crate::design::widgets::{Button, Card, Text};
use crate::pages::{Cx, error_text};
use crate::runtime::ReadTask;

/// Why the load of the current choice failed.
#[derive(Debug)]
pub enum Failure {
    /// The load answered with an error.
    Error(Error),
    /// No answer within the time limit.
    TimedOut(Duration),
}

impl Failure {
    /// The reason, localized.
    pub fn text(&self, cx: &Cx<'_>) -> String {
        match self {
            Failure::Error(e) => error_text(cx.tr, e),
            Failure::TimedOut(limit) => cx
                .tr
                .f("load.timed_out", &[("secs", &limit.as_secs().max(1))]),
        }
    }
}

/// The running load of the current choice.
#[derive(Debug)]
struct Running<K, T> {
    key: K,
    task: ReadTask<T>,
    started: Instant,
    limit: Duration,
}

/// The background load of the latest choice (see the module docs).
#[derive(Debug)]
pub struct Latest<K, T> {
    chosen: Option<K>,
    running: Option<Running<K, T>>,
    failed: Option<(K, Failure)>,
}

impl<K, T> Default for Latest<K, T> {
    fn default() -> Self {
        Self {
            chosen: None,
            running: None,
            failed: None,
        }
    }
}

impl<K: Clone + PartialEq, T: Send + 'static> Latest<K, T> {
    /// Choose `key` and load it with `task`, which answers within `limit` or
    /// fails with a timeout. A load of an earlier choice (or an earlier load of
    /// this one) is dropped: its answer is never read.
    pub fn start(&mut self, key: K, task: ReadTask<T>, limit: Duration) {
        self.chosen = Some(key.clone());
        self.failed = None;
        self.running = Some(Running {
            key,
            task,
            started: Instant::now(),
            limit,
        });
    }

    /// Choose `key` without loading it (its data is at hand): a load of another
    /// choice is dropped and its failure forgotten.
    pub fn choose(&mut self, key: K) {
        if self.running.as_ref().is_some_and(|r| r.key != key) {
            self.running = None;
        }
        if self.failed.as_ref().is_some_and(|(k, _)| *k != key) {
            self.failed = None;
        }
        self.chosen = Some(key);
    }

    /// Stop waiting: a running load is dropped (the choice stays).
    pub fn cancel(&mut self) {
        self.running = None;
    }

    /// The latest choice.
    pub fn chosen(&self) -> Option<&K> {
        self.chosen.as_ref()
    }

    /// The latest choice is `key`.
    pub fn is_chosen(&self, key: &K) -> bool {
        self.chosen.as_ref() == Some(key)
    }

    /// A load of the latest choice runs.
    pub fn loading(&self) -> bool {
        self.running.is_some()
    }

    /// How long the running load has been waiting.
    pub fn waited(&self) -> Option<Duration> {
        self.running.as_ref().map(|r| r.started.elapsed())
    }

    /// Why the last load of the latest choice failed.
    pub fn failure(&self) -> Option<&Failure> {
        self.failed
            .as_ref()
            .filter(|(k, _)| self.chosen.as_ref() == Some(k))
            .map(|(_, f)| f)
    }

    /// Take the answer of the latest choice's load, once it arrived: the value
    /// with the choice it belongs to. An error is kept as the
    /// [`Latest::failure`]; past its time limit the load fails with
    /// [`Failure::TimedOut`] and a late answer is never read.
    pub fn poll(&mut self) -> Option<(K, T)> {
        let running = self.running.as_ref()?;
        let answer = running.task.poll();
        let timed_out = answer.is_none() && running.started.elapsed() >= running.limit;
        if answer.is_none() && !timed_out {
            return None;
        }
        let Running { key, limit, .. } = self.running.take()?;
        if self.chosen.as_ref() != Some(&key) {
            return None;
        }
        match answer {
            Some(Ok(value)) => {
                self.failed = None;
                Some((key, value))
            }
            Some(Err(e)) => {
                self.failed = Some((key, Failure::Error(e)));
                None
            }
            None => {
                self.failed = Some((key, Failure::TimedOut(limit)));
                None
            }
        }
    }
}

/// Keep the frames coming while something loads (the spinner turns, the
/// answer is picked up).
pub fn repaint_soon(ui: &Ui) {
    ui.ctx().request_repaint_after(Duration::from_millis(60));
}

/// Mark `response`'s node busy for screen readers and UI Automation (its
/// content is loading).
pub fn mark_busy(ui: &Ui, response: &Response) {
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_busy());
}

/// The loading state in place of the content: a spinner and `text` ("Loading
/// the settings of devbox…"), one busy progress indicator named by `text`.
pub fn spinner_row(ui: &mut Ui, cx: &Cx<'_>, text: &str) -> Response {
    spinner_text(ui, Text::muted(text, cx.pal), 16.0, cx.pal.text_muted, text)
}

/// [`spinner_row`] in a line of small text (a part of a pane that loads).
pub fn spinner_small(ui: &mut Ui, cx: &Cx<'_>, text: &str) -> Response {
    spinner_text(ui, Text::small(text, cx.pal), 12.0, cx.pal.text_muted, text)
}

fn spinner_text(
    ui: &mut Ui,
    rich: egui::RichText,
    spin: f32,
    color: egui::Color32,
    name: &str,
) -> Response {
    let gap = spin / 2.0;
    let galley = WidgetText::from(rich).into_galley(
        ui,
        Some(TextWrapMode::Wrap),
        (ui.available_width() - spin - gap).max(40.0),
        egui::TextStyle::Body,
    );
    let size = vec2(
        spin + gap + galley.size().x,
        galley.size().y.max(spin) + 4.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let at = egui::Rect::from_center_size(
        egui::pos2(rect.min.x + spin / 2.0, rect.center().y),
        vec2(spin, spin),
    );
    icons::spinner(ui.painter(), at, color, ui.input(|i| i.time));
    ui.painter().galley(
        egui::pos2(at.max.x + gap, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    progress_node(ui, &response, name);
    repaint_soon(ui);
    response
}

/// Name `response` as a busy progress indicator.
fn progress_node(ui: &Ui, response: &Response, text: &str) {
    let name = text.to_owned();
    response.widget_info(|| WidgetInfo::labeled(egui::Role::ProgressIndicator, true, &name));
    mark_busy(ui, response);
}

/// A card like [`crate::pages::empty_state`] with a spinner in the icon tile:
/// `title` ("Scanning devbox…") over `body`, one busy progress indicator named
/// by `title`.
pub fn loading_card(ui: &mut Ui, cx: &Cx<'_>, title: &str, body: &str) {
    Card::new().padding(32).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            let (rect, response) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
            ui.painter().rect(
                rect,
                10,
                cx.pal.hover,
                egui::Stroke::new(1.0, cx.pal.border),
                egui::StrokeKind::Inside,
            );
            icons::spinner(
                ui.painter(),
                rect.shrink(9.0),
                cx.pal.text_muted,
                ui.input(|i| i.time),
            );
            progress_node(ui, &response, title);
            ui.add_space(8.0);
            ui.label(Text::strong(title, cx.pal));
            ui.label(Text::small(body, cx.pal));
        });
    });
    repaint_soon(ui);
}

/// A card like [`crate::pages::error_state`] with Retry: a warning, `title`,
/// `body` (the reason). True when Retry was clicked.
pub fn failed_card(ui: &mut Ui, cx: &Cx<'_>, title: &str, body: &str) -> bool {
    Card::new()
        .padding(32)
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                ui.painter().rect(
                    rect,
                    10,
                    cx.pal.hover,
                    egui::Stroke::new(1.0, cx.pal.border),
                    egui::StrokeKind::Inside,
                );
                icons::paint(ui.painter(), rect.shrink(9.0), Icon::Warning, cx.pal.amber);
                ui.add_space(8.0);
                ui.label(Text::strong(title, cx.pal));
                ui.add(egui::Label::new(Text::small(body, cx.pal)).wrap());
                ui.add_space(10.0);
                ui.add(
                    Button::new(cx.tr.t("action.retry"))
                        .icon(Icon::Refresh)
                        .small(),
                )
                .clicked()
            })
            .inner
        })
        .inner
}
