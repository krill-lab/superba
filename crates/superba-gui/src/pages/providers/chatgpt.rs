//! Official account management, using the existing provider page and switch flow.

use crate::design::widgets::{Button, Status, Text};
use crate::pages::{Action, Cx, callout, error_text};
use crate::runtime::{ReadTask, task};
use egui::Ui;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use superba_core::{ChatGptAccount, ChatGptLoginPrompt, ChatGptQuota, ChatGptWindow};

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
    for account in accounts {
        let active = account.active && cx.data.official_active();
        if !state.quotas.contains_key(&account.id) {
            state.refresh(cx, &account.id);
        }
        ui.add_space(12.0);
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, cx.pal.border))
            .corner_radius(8)
            .inner_margin(12)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(Text::body(&account.email, cx.pal));
                    if active {
                        ui.label(Text::small(cx.tr.t("chatgpt.in_use"), cx.pal));
                    }
                    let working = state
                        .quotas
                        .get(&account.id)
                        .is_some_and(|q| q.task.is_some());
                    if ui
                        .add(
                            Button::new(cx.tr.t("chatgpt.switch"))
                                .small()
                                .enabled(!active && !cx.busy && !working && !signing_in),
                        )
                        .clicked()
                    {
                        out.push(Action::SwitchChatGptAccount(account.id.clone()));
                    }
                    if ui
                        .add(
                            Button::new(cx.tr.t("chatgpt.refresh"))
                                .small()
                                .loading(working)
                                .enabled(!cx.busy && !working),
                        )
                        .clicked()
                    {
                        state.refresh(cx, &account.id);
                    }
                });
                let quota = state.quotas.get(&account.id);
                let value = quota.and_then(|q| q.value.as_ref());
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    window(
                        ui,
                        cx,
                        "chatgpt.five_hour",
                        value.and_then(|q| q.five_hour.as_ref()),
                        quota.is_some_and(|q| q.task.is_some()),
                    );
                    ui.add_space(20.0);
                    window(
                        ui,
                        cx,
                        "chatgpt.weekly",
                        value.and_then(|q| q.weekly.as_ref()),
                        quota.is_some_and(|q| q.task.is_some()),
                    );
                });
                if let Some(value) = value {
                    ui.label(Text::caption(
                        cx.tr
                            .f("chatgpt.updated", &[("when", &cx.ago(value.fetched_at))]),
                        cx.pal,
                    ));
                }
                if let Some(error) = quota.and_then(|q| q.error.as_ref()) {
                    callout(ui, cx, Status::Error, error);
                }
            });
    }
    ui.add_space(8.0);
    ui.add(egui::Label::new(Text::caption(cx.tr.t("chatgpt.switching"), cx.pal)).wrap());
    ui.add_space(16.0);
}

fn window(ui: &mut Ui, cx: &Cx<'_>, label: &str, window: Option<&ChatGptWindow>, loading: bool) {
    ui.vertical(|ui| {
        ui.label(Text::small(cx.tr.t(label), cx.pal));
        if let Some(window) = window {
            let label = cx.tr.f(
                "chatgpt.remaining",
                &[("percent", &format!("{:.0}", window.remaining))],
            );
            ui.add(
                egui::ProgressBar::new(window.remaining as f32 / 100.0)
                    .desired_width(150.0)
                    .text(label),
            );
            if let Some(reset) = window.resets_at {
                let minutes = reset
                    .duration_since(cx.now)
                    .map_or(0, |d| d.as_secs().div_ceil(60));
                let time = if minutes >= 1440 {
                    format!("{}d {}h", minutes / 1440, (minutes % 1440) / 60)
                } else {
                    format!("{}h {}m", minutes / 60, minutes % 60)
                };
                ui.label(Text::caption(
                    cx.tr.f("chatgpt.reset", &[("time", &time)]),
                    cx.pal,
                ));
            }
        } else {
            ui.label(Text::caption(
                cx.tr.t(if loading {
                    "chatgpt.loading_limits"
                } else {
                    "chatgpt.unknown"
                }),
                cx.pal,
            ));
        }
    });
}
