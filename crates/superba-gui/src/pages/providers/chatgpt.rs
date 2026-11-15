//! The saved ChatGPT accounts on the official login's detail (PR #20): add one
//! through the browser sign-in, save the current login, the five-hour and
//! weekly limits of each (read by the shell, [`crate::quota`]), switch to one
//! through the ordinary switch flow, refresh or forget one. Credentials never
//! reach this page: the service keeps them in Superba's vault.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use egui::{Align, Layout, Sense, Stroke, Ui, vec2};
use superba_core::{
    ChatGptAccount, ChatGptLoginPrompt, ChatGptQuota, ChatGptWindow, RestoreTarget,
};

use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::text;
use crate::design::widgets::{Badge, Button, IconButton, Status, Text};
use crate::i18n::Tr;
use crate::pages::{Action, Cx, callout, divider, elided, error_text};
use crate::quota::QuotaRow;
use crate::runtime::{ReadTask, task};

/// Height of a quota bar (rounded track).
pub const QUOTA_BAR_HEIGHT: f32 = 6.0;
/// Width of one quota column in a wide row.
const QUOTA_COLUMN: f32 = 156.0;
/// A row is laid out on one line from this width on.
const WIDE: f32 = 760.0;

/// A browser sign-in waiting for its result.
struct Login {
    task: ReadTask<ChatGptAccount>,
    prompts: mpsc::Receiver<ChatGptLoginPrompt>,
    alive: Arc<AtomicBool>,
    url: Option<String>,
}

impl Drop for Login {
    fn drop(&mut self) {
        // Cancels the sign-in: the service's next poll of `events` stops it.
        self.alive.store(false, Ordering::Relaxed);
    }
}

/// The section's own work (no credentials).
#[derive(Default)]
pub(crate) struct Accounts {
    login: Option<Login>,
    save: Option<ReadTask<ChatGptAccount>>,
    remove: Option<ReadTask<()>>,
    /// The account whose removal waits for a confirmation.
    confirm_remove: Option<String>,
    error: Option<String>,
}

impl Accounts {
    /// A browser sign-in waits (limits reads pause meanwhile).
    pub(crate) fn signing_in(&self) -> bool {
        self.login.is_some()
    }

    /// The sign-in page of the waiting sign-in, once Codex gave it.
    pub(crate) fn sign_in_url(&self) -> Option<&str> {
        self.login.as_ref().and_then(|l| l.url.as_deref())
    }

    fn working(&self) -> bool {
        self.login.is_some() || self.save.is_some() || self.remove.is_some()
    }

    /// Collect the answers of the section's tasks (also while another page
    /// shows or the window is hidden); `true` while one runs.
    pub(crate) fn poll(&mut self, tr: &Tr, out: &mut Vec<Action>) -> bool {
        if let Some(login) = &mut self.login {
            while let Ok(prompt) = login.prompts.try_recv() {
                if login.url.is_none() {
                    out.push(Action::OpenUrl(prompt.url.clone()));
                }
                login.url = Some(prompt.url);
            }
        }
        let mut done = Vec::new();
        if let Some(r) = self.login.as_ref().and_then(|l| l.task.poll()) {
            self.login = None;
            done.push(r);
        }
        if let Some(r) = self.save.as_ref().and_then(ReadTask::poll) {
            self.save = None;
            done.push(r);
        }
        for result in done {
            match result {
                Ok(account) => {
                    self.error = None;
                    out.push(Action::Toast {
                        status: Status::Ok,
                        text: tr.f("chatgpt.saved", &[("email", &account.email)]),
                    });
                    out.push(Action::Refresh);
                }
                Err(superba_core::Error::Cancelled) => {}
                Err(e) => self.error = Some(error_text(tr, &e)),
            }
        }
        if let Some(r) = self.remove.as_ref().and_then(ReadTask::poll) {
            self.remove = None;
            match r {
                Ok(()) => {
                    self.error = None;
                    out.push(Action::Refresh);
                }
                Err(e) => self.error = Some(error_text(tr, &e)),
            }
        }
        self.working()
    }

    fn start_login(&mut self, cx: &Cx<'_>) {
        self.error = None;
        let service = cx.service.clone();
        let (sender, prompts) = mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let running = alive.clone();
        let events: superba_core::ChatGptLoginEvents = Arc::new(move |prompt| {
            if !running.load(Ordering::Relaxed) {
                return false;
            }
            match prompt {
                Some(prompt) => sender.send(prompt).is_ok(),
                None => true,
            }
        });
        self.login = Some(Login {
            task: task(async move { service.chatgpt_add_account(events).await }),
            prompts,
            alive,
            url: None,
        });
    }
}

/// Draw the section.
pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut Accounts, out: &mut Vec<Action>) {
    if state.poll(cx.tr, out) {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let accounts = cx.data.chatgpt_accounts();
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Label::new(Text::h2(cx.tr.t("chatgpt.title"), cx.pal)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let idle = !cx.busy && !state.working();
            if ui
                .add(
                    Button::new(cx.tr.t("chatgpt.import"))
                        .small()
                        .loading(state.save.is_some())
                        .enabled(idle && unsaved_login(cx)),
                )
                .on_hover_text(cx.tr.t("chatgpt.import_tip"))
                .clicked()
            {
                state.error = None;
                let service = cx.service.clone();
                state.save = Some(task(async move { service.chatgpt_save_current().await }));
            }
            if ui
                .add(
                    Button::primary(cx.tr.t("chatgpt.add"))
                        .icon(Icon::Plus)
                        .small()
                        .loading(state.login.is_some())
                        .enabled(idle),
                )
                .clicked()
            {
                state.start_login(cx);
            }
        });
    });
    ui.add_space(4.0);
    ui.add(egui::Label::new(Text::small(cx.tr.t("chatgpt.body"), cx.pal)).wrap());
    if let Some(login) = &state.login {
        ui.add_space(8.0);
        callout(ui, cx, Status::Idle, &cx.tr.t("chatgpt.waiting"));
        let mut cancel = false;
        ui.horizontal_wrapped(|ui| {
            ui.spinner();
            if let Some(url) = &login.url {
                if ui
                    .add(Button::new(cx.tr.t("chatgpt.open")).small())
                    .clicked()
                {
                    out.push(Action::OpenUrl(url.clone()));
                }
                if ui
                    .add(Button::new(cx.tr.t("chatgpt.copy")).small())
                    .clicked()
                {
                    out.push(Action::Copy(url.clone()));
                }
            }
            cancel = ui
                .add(Button::ghost(cx.tr.t("chatgpt.cancel")).small())
                .clicked();
        });
        if cancel {
            state.login = None;
        }
    }
    if let Some(error) = &state.error {
        ui.add_space(6.0);
        callout(ui, cx, Status::Error, error);
    }
    if unsaved_login(cx) && state.login.is_none() {
        ui.add_space(6.0);
        callout(ui, cx, Status::Warn, &cx.tr.t("chatgpt.unsaved"));
    }
    ui.add_space(10.0);
    if accounts.is_empty() {
        ui.add(egui::Label::new(Text::small(cx.tr.t("chatgpt.empty"), cx.pal)).wrap());
    }
    for (index, account) in accounts.iter().enumerate() {
        if index > 0 {
            divider(ui, cx);
        }
        ui.push_id(("chatgpt-account", &account.id), |ui| {
            account_row(ui, cx, state, account, index, out);
        });
    }
    if !accounts.is_empty() {
        ui.add_space(6.0);
        ui.add(egui::Label::new(Text::caption(cx.tr.t("chatgpt.switching"), cx.pal)).wrap());
    }
}

/// The live `auth.json` holds a ChatGPT login that is not saved yet.
fn unsaved_login(cx: &Cx<'_>) -> bool {
    let live_login = cx
        .data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.config.as_ref())
        .is_some_and(|c| matches!(c.auth, superba_core::AuthShape::ChatGpt { .. }));
    live_login && !cx.data.chatgpt_accounts().iter().any(|a| a.active)
}

/// One account: number, e-mail, plan and age of the limits, the five-hour and
/// weekly columns (a window that is not reported leaves its column empty so
/// the columns stay aligned), then the actions.
fn account_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut Accounts,
    account: &ChatGptAccount,
    index: usize,
    out: &mut Vec<Action>,
) {
    let in_use = account.active && cx.data.official_active();
    let row = cx.data.chatgpt_quota.get(&account.id);
    let reading = row.is_some_and(|r| r.in_flight);
    let value = row.and_then(|r| r.value.as_ref());
    let wide = ui.available_width() >= WIDE;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.set_min_height(if wide { 52.0 } else { 36.0 });
        number(ui, cx, index, in_use);
        let actions_width = 220.0;
        let quota_width = if wide { 2.0 * QUOTA_COLUMN + 12.0 } else { 0.0 };
        let name_width = (ui.available_width() - actions_width - quota_width - 12.0)
            .clamp(80.0, if wide { 300.0 } else { f32::INFINITY });
        ui.allocate_ui_with_layout(vec2(name_width, 36.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(name_width);
            identity(ui, cx, account, row, reading);
        });
        if wide {
            windows(ui, cx, value, reading, QUOTA_COLUMN);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            actions(ui, cx, state, account, in_use, reading, out);
        });
    });
    if !wide {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.add_space(34.0);
            let width = ((ui.available_width() - 12.0) / 2.0).clamp(80.0, 220.0);
            windows(ui, cx, value, reading, width);
        });
    }
    if let Some(error) = row.and_then(|r| r.error.as_ref()) {
        let shown = error_text(cx.tr, error);
        let text = match value {
            Some(v) => cx.tr.f(
                "chatgpt.read_failed_kept",
                &[("error", &shown), ("when", &cx.ago(v.fetched_at))],
            ),
            None => shown,
        };
        ui.horizontal(|ui| {
            ui.add_space(34.0);
            ui.add(egui::Label::new(Text::caption(text, cx.pal).color(cx.pal.red)).wrap());
        });
    }
    if state.confirm_remove.as_deref() == Some(account.id.as_str()) {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(34.0);
            ui.add(
                egui::Label::new(Text::small(
                    cx.tr
                        .f("chatgpt.remove_confirm", &[("email", &account.email)]),
                    cx.pal,
                ))
                .wrap(),
            );
            if ui
                .add(
                    Button::danger(cx.tr.t("chatgpt.remove"))
                        .small()
                        .enabled(!state.working()),
                )
                .clicked()
            {
                let service = cx.service.clone();
                let id = account.id.clone();
                state.remove = Some(task(
                    async move { service.chatgpt_remove_account(&id).await },
                ));
                state.confirm_remove = None;
            }
            if ui
                .add(Button::ghost(cx.tr.t("action.cancel")).small())
                .clicked()
            {
                state.confirm_remove = None;
            }
        });
    }
}

fn number(ui: &mut Ui, cx: &Cx<'_>, index: usize, in_use: bool) {
    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let (fill, stroke, ink) = if in_use {
        (cx.pal.primary, cx.pal.primary, cx.pal.on_primary)
    } else {
        (cx.pal.surface, cx.pal.border_strong, cx.pal.text_muted)
    };
    ui.painter()
        .circle(r.center(), 10.0, fill, Stroke::new(1.0, stroke));
    ui.painter().text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        (index + 1).to_string(),
        font(text::CAPTION, Weight::Semibold),
        ink,
    );
}

fn identity(
    ui: &mut Ui,
    cx: &Cx<'_>,
    account: &ChatGptAccount,
    row: Option<&QuotaRow>,
    reading: bool,
) {
    ui.spacing_mut().item_spacing.y = 2.0;
    elided(ui, Text::strong(&account.email, cx.pal));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let plan = row
            .and_then(|r| r.value.as_ref())
            .and_then(|q| q.plan_type.as_deref())
            .or(account.plan_type.as_deref())
            .and_then(superba_core::chatgpt::plan_label);
        if let Some(plan) = plan {
            ui.add(Badge::new(plan).outline());
        }
        let age = match row.and_then(|r| r.value.as_ref()) {
            Some(v) => cx
                .tr
                .f("chatgpt.updated", &[("when", &cx.ago(v.fetched_at))]),
            None if reading => cx.tr.t("chatgpt.loading"),
            None => String::new(),
        };
        if !age.is_empty() {
            elided(ui, Text::caption(age, cx.pal));
        }
    });
}

fn actions(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut Accounts,
    account: &ChatGptAccount,
    in_use: bool,
    reading: bool,
    out: &mut Vec<Action>,
) {
    if !account.active
        && ui
            .add(
                IconButton::new(Icon::Close, cx.tr.t("chatgpt.remove"))
                    .size(28.0)
                    .enabled(!state.working()),
            )
            .clicked()
    {
        state.confirm_remove = Some(account.id.clone());
    }
    if reading {
        ui.add_sized([28.0, 28.0], egui::Spinner::new().size(16.0));
    } else if ui
        .add(IconButton::new(Icon::Refresh, cx.tr.t("chatgpt.refresh")).size(28.0))
        .clicked()
    {
        out.push(Action::RefreshChatGptQuota(account.id.clone()));
    }
    ui.add_space(4.0);
    if in_use {
        ui.add(Badge::new(cx.tr.t("chatgpt.in_use")).status(Status::Ok));
    } else if ui
        .add(
            Button::new(cx.tr.t("chatgpt.switch"))
                .small()
                .enabled(!cx.busy && !state.signing_in()),
        )
        .clicked()
    {
        out.push(Action::Restore(RestoreTarget::Account {
            identity: account.id.clone(),
        }));
    }
}

/// The five-hour and weekly columns, each `width` wide.
fn windows(ui: &mut Ui, cx: &Cx<'_>, value: Option<&ChatGptQuota>, reading: bool, width: f32) {
    let Some(value) = value else {
        let note = if reading {
            "chatgpt.loading_limits"
        } else {
            "chatgpt.not_read"
        };
        ui.allocate_ui_with_layout(
            vec2(2.0 * width + 12.0, 40.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(Text::small(cx.tr.t(note), cx.pal));
            },
        );
        return;
    };
    if value.is_empty() {
        ui.allocate_ui_with_layout(
            vec2(2.0 * width + 12.0, 40.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(Text::small(cx.tr.t("chatgpt.no_windows"), cx.pal));
            },
        );
        return;
    }
    for (label, window) in [
        ("chatgpt.five_hour", value.five_hour.as_ref()),
        ("chatgpt.weekly", value.weekly.as_ref()),
    ] {
        match window {
            Some(w) => column(ui, cx, label, w, width),
            // Not reported: an empty column keeps the next one aligned.
            None => {
                ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
            }
        }
    }
}

fn column(ui: &mut Ui, cx: &Cx<'_>, label: &str, window: &ChatGptWindow, width: f32) {
    ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(Text::caption(cx.tr.t(label), cx.pal));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(Text::caption(
                    cx.tr.f(
                        "chatgpt.remaining",
                        &[("percent", &format!("{:.0}", window.remaining))],
                    ),
                    cx.pal,
                ));
            });
        });
        ui.add(crate::design::widgets::QuotaBar::new(window.remaining).width(width));
        if let Some(reset) = window.resets_at {
            let left = reset.duration_since(cx.now).unwrap_or_default();
            elided(
                ui,
                Text::caption(
                    cx.tr
                        .f("chatgpt.reset", &[("time", &reset_in(cx.tr, left))]),
                    cx.pal,
                ),
            );
        }
    });
}

/// "2 d 3 h" / "4 h 12 min" / "8 min" until a reset.
pub fn reset_in(tr: &Tr, left: Duration) -> String {
    let minutes = left.as_secs().div_ceil(60);
    let (d, h, m) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    if d > 0 {
        tr.f("chatgpt.in_days_hours", &[("d", &d), ("h", &h)])
    } else if h > 0 {
        tr.f("chatgpt.in_hours_minutes", &[("h", &h), ("m", &m)])
    } else {
        tr.f("chatgpt.in_minutes", &[("m", &m)])
    }
}
