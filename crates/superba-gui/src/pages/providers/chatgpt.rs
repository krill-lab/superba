//! Official account management, using the existing provider page and switch flow.

use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::text;
use crate::design::widgets::{Badge, Button, IconButton, Status, Text};
use crate::pages::{Action, Cx, callout, divider, elided, error_text};
use crate::runtime::{ReadTask, task};
use egui::{Align, Layout, Sense, Stroke, Ui, vec2};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use superba_core::{ChatGptAccount, ChatGptLoginPrompt, ChatGptQuota};

#[derive(Debug)]
struct Login {
    task: ReadTask<ChatGptAccount>,
    prompts: mpsc::Receiver<ChatGptLoginPrompt>,
    alive: Arc<AtomicBool>,
    url: Option<String>,
}

impl Drop for Login {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
struct Quota {
    value: Option<ChatGptQuota>,
    task: Option<ReadTask<ChatGptQuota>>,
    error: Option<String>,
}

/// Page-owned login and quota work; credentials never enter GUI state.
#[derive(Debug, Default)]
pub(crate) struct State {
    login: Option<Login>,
    import: Option<ReadTask<ChatGptAccount>>,
    quotas: HashMap<String, Quota>,
    error: Option<String>,
}

impl State {
    pub(super) fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        if let Some(login) = &mut self.login {
            while let Ok(prompt) = login.prompts.try_recv() {
                login.url = Some(prompt.url.clone());
                out.push(Action::OpenUrl(prompt.url));
            }
        }
        let login_result = self.login.as_ref().and_then(|login| login.task.poll());
        let import_result = self.import.as_ref().and_then(ReadTask::poll);
        let finished = login_result.is_some() || import_result.is_some();
        if login_result.is_some() {
            self.login = None;
        }
        if import_result.is_some() {
            self.import = None;
        }
        for result in login_result.into_iter().chain(import_result) {
            match result {
                Ok(_) => {
                    self.error = None;
                    out.push(Action::Toast {
                        status: Status::Ok,
                        text: cx.tr.t("chatgpt.saved"),
                    });
                    out.push(Action::Refresh);
                }
                Err(error) => self.error = Some(error_text(cx.tr, &error)),
            }
        }
        for quota in self.quotas.values_mut() {
            if let Some(result) = quota.task.as_ref().and_then(ReadTask::poll) {
                quota.task = None;
                match result {
                    Ok(value) => {
                        quota.value = Some(value);
                        quota.error = None;
                    }
                    Err(error) => quota.error = Some(error_text(cx.tr, &error)),
                }
            }
        }
        finished
            || self.login.is_some()
            || self.import.is_some()
            || self.quotas.values().any(|q| q.task.is_some())
    }

    fn add(&mut self, cx: &Cx<'_>) {
        self.error = None;
        let service = cx.service.clone();
        let (sender, prompts) = mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let running = alive.clone();
        let events = Arc::new(move |prompt| {
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

    fn refresh(&mut self, cx: &Cx<'_>, id: &str) {
        let entry = self.quotas.entry(id.into()).or_default();
        if entry.task.is_some() {
            return;
        }
        entry.error = None;
        let service = cx.service.clone();
        let id = id.to_owned();
        entry.task = Some(task(async move { service.chatgpt_quota(&id).await }));
    }
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    ui.add(egui::Label::new(Text::h2(cx.tr.t("chatgpt.title"), cx.pal)));
    ui.add_space(8.0);
    let signing_in = state.login.is_some() || state.import.is_some();
    ui.horizontal_wrapped(|ui| {
        if ui
            .add(
                Button::primary(cx.tr.t("chatgpt.add"))
                    .small()
                    .enabled(!cx.busy && !signing_in),
            )
            .clicked()
        {
            state.add(cx);
        }
        if ui
            .add(
                Button::new(cx.tr.t("chatgpt.import"))
                    .small()
                    .loading(state.import.is_some())
                    .enabled(!cx.busy && !signing_in),
            )
            .clicked()
        {
            let service = cx.service.clone();
            state.import = Some(task(async move { service.chatgpt_import_current().await }));
        }
    });
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
                    ui.ctx().copy_text(url.clone());
                }
            }
            cancel = ui
                .add(Button::new(cx.tr.t("chatgpt.cancel")).small())
                .clicked();
        });
        if cancel {
            state.login = None;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    if let Some(error) = &state.error {
        callout(ui, cx, Status::Error, error);
    }
    if let Some(error) = &cx.data.chatgpt_accounts.error {
        callout(ui, cx, Status::Error, &error_text(cx.tr, error));
    }
    let accounts = cx
        .data
        .chatgpt_accounts
        .value
        .as_deref()
        .unwrap_or_default();
    if accounts.is_empty() {
        ui.add_space(12.0);
        ui.add(egui::Label::new(Text::small(cx.tr.t("chatgpt.empty"), cx.pal)).wrap());
    }
    ui.add_space(12.0);
    for (index, account) in accounts.iter().enumerate() {
        if !state.quotas.contains_key(&account.id) {
            state.refresh(cx, &account.id);
        }
        if index > 0 {
            divider(ui, cx);
        }
        let mut refresh = false;
        ui.push_id(&account.id, |ui| {
            refresh = account_row(
                ui,
                cx,
                account,
                index,
                state.quotas.get(&account.id),
                signing_in,
                out,
            );
        });
        if refresh {
            state.refresh(cx, &account.id);
        }
    }
    ui.add_space(8.0);
    ui.add(egui::Label::new(Text::caption(cx.tr.t("chatgpt.switching"), cx.pal)).wrap());
    ui.add_space(16.0);
}

/// The same flat, divided rows as provider lines, with stable quota columns.
fn account_row(
    ui: &mut Ui,
    cx: &Cx<'_>,
    account: &ChatGptAccount,
    index: usize,
    quota: Option<&Quota>,
    signing_in: bool,
    out: &mut Vec<Action>,
) -> bool {
    let active = account.active && cx.data.official_active();
    let working = quota.is_some_and(|q| q.task.is_some());
    let value = quota.and_then(|q| q.value.as_ref());
    let action_width = Button::new(cx.tr.t("chatgpt.switch"))
        .small()
        .desired_size(ui)
        .x
        + 44.0;
    // Reserve room for a readable identity, both quota windows and the actions.
    let wide = ui.available_width() >= 760.0;
    let mut refresh = false;
    let mut actions = |ui: &mut Ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if working {
                ui.add_sized([28.0, 28.0], egui::Spinner::new().size(16.0));
            } else if ui
                .add(
                    IconButton::new(Icon::Refresh, cx.tr.t("chatgpt.refresh"))
                        .size(28.0)
                        .enabled(!cx.busy),
                )
                .clicked()
            {
                refresh = true;
            }
            ui.add_space(4.0);
            if active {
                ui.add(Badge::new(cx.tr.t("chatgpt.in_use")).status(Status::Ok));
            } else if ui
                .add(
                    Button::new(cx.tr.t("chatgpt.switch"))
                        .small()
                        .enabled(!cx.busy && !working && !signing_in),
                )
                .clicked()
            {
                out.push(Action::SwitchChatGptAccount(account.id.clone()));
            }
        });
    };
    let identity = |ui: &mut Ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        elided(ui, Text::strong(&account.email, cx.pal));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let plan = value
                .and_then(|q| q.plan_type.as_deref())
                .or(account.plan_type.as_deref());
            ui.add(Badge::new(plan_label(cx, plan)).outline());
            if let Some(value) = value {
                elided(
                    ui,
                    Text::caption(
                        cx.tr
                            .f("chatgpt.updated", &[("when", &cx.ago(value.fetched_at))]),
                        cx.pal,
                    ),
                );
            }
        });
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.set_min_height(if wide { 54.0 } else { 38.0 });
        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        ui.painter().circle(
            r.center(),
            10.0,
            if active {
                cx.pal.primary
            } else {
                cx.pal.surface
            },
            Stroke::new(
                1.0,
                if active {
                    cx.pal.primary
                } else {
                    cx.pal.border_strong
                },
            ),
        );
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            (index + 1).to_string(),
            font(text::CAPTION, Weight::Semibold),
            if active {
                cx.pal.on_primary
            } else {
                cx.pal.text_muted
            },
        );
        let quota_width = if wide { 336.0 } else { 0.0 };
        let name_width = (ui.available_width() - action_width - quota_width - 24.0)
            .clamp(40.0, if wide { 300.0 } else { f32::INFINITY });
        ui.allocate_ui_with_layout(vec2(name_width, 38.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(name_width);
            identity(ui);
        });
        if wide {
            quota_windows(ui, cx, value, working, 156.0);
        }
        actions(ui);
    });
    if !wide {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.add_space(22.0);
            let width = ((ui.available_width() - 12.0) / 2.0).min(200.0);
            quota_windows(ui, cx, value, working, width);
        });
    }
    if let Some(error) = quota.and_then(|q| q.error.as_ref()) {
        callout(ui, cx, Status::Error, error);
    }
    refresh
}

fn quota_windows(
    ui: &mut Ui,
    cx: &Cx<'_>,
    value: Option<&ChatGptQuota>,
    working: bool,
    width: f32,
) {
    let Some(value) = value else {
        ui.label(Text::small(
            cx.tr.t(if working {
                "chatgpt.loading_limits"
            } else {
                "chatgpt.unknown"
            }),
            cx.pal,
        ));
        return;
    };
    if value.five_hour.is_none() && value.weekly.is_none() {
        ui.label(Text::small(cx.tr.t("chatgpt.no_windows"), cx.pal));
        return;
    }
    for (label, window) in [
        ("chatgpt.five_hour", value.five_hour.as_ref()),
        ("chatgpt.weekly", value.weekly.as_ref()),
    ] {
        // Absence is not an unlimited plan: show only windows actually reported.
        let Some(window) = window else {
            continue;
        };
        ui.allocate_ui_with_layout(vec2(width, 48.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(Text::small(cx.tr.t(label), cx.pal));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let remaining = cx.tr.f(
                        "chatgpt.remaining",
                        &[("percent", &format!("{:.0}", window.remaining))],
                    );
                    ui.label(Text::small(remaining, cx.pal));
                });
            });
            let (track, _) = ui.allocate_exact_size(vec2(width, 3.0), Sense::hover());
            ui.painter().rect_filled(track, 2.0, cx.pal.border);
            let filled = egui::Rect::from_min_size(
                track.min,
                vec2(
                    width * (window.remaining as f32 / 100.0).clamp(0.0, 1.0),
                    3.0,
                ),
            );
            if filled.width() > 0.0 {
                let color = if window.remaining <= 10.0 {
                    cx.pal.red
                } else if window.remaining <= 20.0 {
                    cx.pal.amber
                } else {
                    cx.pal.green
                };
                ui.painter().rect_filled(filled, 2.0, color);
            }
            if let Some(reset) = window.resets_at {
                let minutes = reset
                    .duration_since(cx.now)
                    .map_or(0, |d| d.as_secs().div_ceil(60));
                let time = if minutes >= 1440 {
                    format!("{}d {}h", minutes / 1440, (minutes % 1440) / 60)
                } else {
                    format!("{}h {}m", minutes / 60, minutes % 60)
                };
                elided(
                    ui,
                    Text::caption(cx.tr.f("chatgpt.reset", &[("time", &time)]), cx.pal),
                );
            }
        });
    }
}

/// Keep service plan names recognizable, without treating them as quota rules.
fn plan_label(cx: &Cx<'_>, plan: Option<&str>) -> String {
    match plan {
        Some("free") => "Free",
        Some("go") => "Go",
        Some("plus") => "Plus",
        Some("pro") => "Pro",
        Some("pro_lite") => "Pro Lite",
        Some("team") => "Team",
        Some("business") => "Business",
        Some("enterprise") => "Enterprise",
        Some("edu") => "Edu",
        _ => return cx.tr.t("chatgpt.plan_unknown"),
    }
    .into()
}
