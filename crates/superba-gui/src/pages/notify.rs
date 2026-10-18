//! Preferences → Notifications (GitHub issue 12): channels (add, edit, test,
//! delete), which events are sent, what a message says, quiet hours, the rate
//! limit and the hooks that make Codex / Claude Code report events.
//!
//! Toggles edit the Preferences copy of the settings (saved like every other
//! preference); channels and their secrets go through the service
//! (`notify_save_channel`), which keeps tokens in the key store, and the copy
//! takes the saved channel back. Hooks are read and written through the
//! service as well (Codex's `config.toml`, with a backup).

use std::collections::BTreeMap;

use egui::{Align, Layout, TextEdit, Ui, vec2};
use superba_core::notify::{harness, hhmm, parse_hhmm};
use superba_core::{
    ChannelKind, ChannelOutcome, EventKind, HookRequest, HookState, NotifyChannel, NotifyEvent,
    NotifyHooks, NotifyReport, NotifySecrets, Secret, SendOptions, Settings, Timestamp,
};

use super::preferences::row;
use super::{Action, Cx, Page, PaletteGroup, PaletteItem, error_text, section};
use crate::design::icons::Icon;
use crate::design::widgets::{
    Badge, Button, Card, Dropdown, KeyInput, Segmented, Status, Text, Toggle, dialog,
};
use crate::runtime::{ReadTask, task};

/// Page-local state of the notifications section.
#[derive(Debug, Default)]
pub struct State {
    hooks: Option<NotifyHooks>,
    hooks_task: Option<ReadTask<NotifyHooks>>,
    hooks_error: Option<String>,
    hook_op: Option<ReadTask<NotifyHooks>>,
    editor: Option<Editor>,
    delete: Option<(String, String)>,
    delete_task: Option<(String, ReadTask<()>)>,
    toggle_task: Option<ReadTask<NotifyChannel>>,
    test: Option<(Option<String>, ReadTask<NotifyReport>)>,
    /// Last test result per channel id: Ok(ms) or the error.
    results: BTreeMap<String, Result<u64, String>>,
    quiet_from: Option<String>,
    quiet_to: Option<String>,
    /// Scroll the section into view on the next frame (palette, commands).
    scroll_to: bool,
}

/// The add / edit dialog.
#[derive(Debug)]
pub struct Editor {
    /// Id of the edited channel (empty = new).
    pub id: String,
    /// Type.
    pub kind: ChannelKind,
    /// Name.
    pub name: String,
    /// Non-secret fields.
    pub params: BTreeMap<String, String>,
    /// Typed secret fields (empty = keep the stored one).
    pub secrets: BTreeMap<String, String>,
    /// Masked hints of the stored secrets.
    pub hints: BTreeMap<String, String>,
    /// Revealed secret fields.
    pub reveal: BTreeMap<String, bool>,
    /// On.
    pub enabled: bool,
    /// Validation / save error.
    pub error: Option<String>,
    saving: Option<(bool, ReadTask<NotifyChannel>)>,
}

impl Editor {
    /// A dialog for a new channel of `kind`.
    pub fn new(kind: ChannelKind) -> Self {
        Self {
            id: String::new(),
            kind,
            name: String::new(),
            params: BTreeMap::new(),
            secrets: BTreeMap::new(),
            hints: BTreeMap::new(),
            reveal: BTreeMap::new(),
            enabled: true,
            error: None,
            saving: None,
        }
    }

    /// A dialog for an existing channel.
    pub fn edit(c: &NotifyChannel) -> Self {
        Self {
            id: c.id.clone(),
            kind: c.kind,
            name: c.name.clone(),
            params: c.params.clone(),
            secrets: BTreeMap::new(),
            hints: c.secret_hints.clone(),
            reveal: BTreeMap::new(),
            enabled: c.enabled,
            error: None,
            saving: None,
        }
    }

    fn channel(&self) -> (NotifyChannel, NotifySecrets) {
        let mut c = NotifyChannel::new(self.id.clone(), self.kind, self.name.trim());
        c.enabled = self.enabled;
        c.params = self
            .params
            .iter()
            .filter(|(k, v)| !v.trim().is_empty() && self.kind.field(k).is_some())
            .map(|(k, v)| (k.clone(), v.trim().to_owned()))
            .collect();
        let mut s = NotifySecrets::default();
        for (k, v) in &self.secrets {
            if !v.trim().is_empty() {
                s.set(k.clone(), Secret::new(v.clone()));
            }
        }
        (c, s)
    }
}

impl State {
    /// Open the add dialog (tests, the palette).
    pub fn open_editor(&mut self, editor: Editor) {
        self.editor = Some(editor);
    }

    /// The open dialog (tests).
    pub fn editor(&self) -> Option<&Editor> {
        self.editor.as_ref()
    }

    /// Use these hooks (tests, screenshots).
    pub fn set_hooks(&mut self, hooks: NotifyHooks) {
        self.hooks = Some(hooks);
    }

    /// A dialog is open.
    pub fn modal(&self) -> bool {
        self.editor.is_some() || self.delete.is_some()
    }
}

/// Command-palette entries: open the section, add a channel, send a test.
pub fn palette(cx: &Cx<'_>) -> Vec<PaletteItem> {
    let item = |key: &str, command: &str, english: &str| {
        let mut keywords: Vec<String> = [
            "notifications",
            "telegram",
            "wechat",
            "feishu",
            "lark",
            "dingtalk",
            "wecom",
            "bark",
            "ntfy",
            "webhook",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        // English words for the Chinese UI too (a query typed in English).
        keywords.push(english.to_owned());
        PaletteItem {
            title: cx.tr.t(key),
            detail: Some(cx.tr.t("prefs.notify.title")),
            keywords,
            group: PaletteGroup::Commands,
            action: Action::PageCommand {
                page: Page::Preferences,
                command: command.into(),
            },
        }
    };
    let mut items = vec![
        item("prefs.notify.title", "notify:show", "notification settings"),
        item(
            "prefs.notify.channels.add",
            "notify:add",
            "add notification channel",
        ),
        item(
            "prefs.notify.channels.test_all",
            "notify:test",
            "test notifications",
        ),
    ];
    // "Add a channel: WeChat (ServerChan)", one per kind.
    for kind in ChannelKind::ALL {
        let label = kind_label(cx, kind);
        let mut it = item(
            "prefs.notify.channels.add",
            &format!("notify:add:{}", kind.id()),
            &format!("add {} channel", kind.id()),
        );
        it.title = cx
            .tr
            .f("prefs.notify.palette.add_kind", &[("kind", &label)]);
        it.keywords.push(kind.label().fallback);
        items.push(it);
    }
    items
}

/// Run a Preferences command addressed to this section.
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str) {
    match command {
        "notify:show" => state.scroll_to = true,
        "notify:add" => {
            state.scroll_to = true;
            state.editor = Some(Editor::new(ChannelKind::Telegram));
        }
        c if c.starts_with("notify:add:") => {
            let kind =
                ChannelKind::parse(&c["notify:add:".len()..]).unwrap_or(ChannelKind::Telegram);
            state.scroll_to = true;
            state.editor = Some(Editor::new(kind));
        }
        "notify:test" => {
            state.scroll_to = true;
            start_test(cx, state, None);
        }
        _ => tracing::debug!(%command, "unknown notifications command"),
    }
}

/// Whether the shell should look for failed Codex turns (notifications on,
/// the event wanted, a channel to send to).
pub fn watch_wanted(settings: &Settings) -> bool {
    let n = &settings.notify;
    n.enabled && n.triggers.turn_failed && n.channels.iter().any(|c| c.enabled)
}

fn kind_label(cx: &Cx<'_>, kind: ChannelKind) -> String {
    cx.localize(&kind.label())
}

/// Poll the background work; returns whether something still runs.
fn poll(cx: &Cx<'_>, settings: &mut Settings, state: &mut State, out: &mut Vec<Action>) -> bool {
    let mut running = false;
    let shown = settings.notify.enabled || !settings.notify.channels.is_empty();
    if shown && state.hooks.is_none() && state.hooks_task.is_none() && state.hooks_error.is_none() {
        let s = cx.service.clone();
        state.hooks_task = Some(task(async move { s.notify_hooks().await }));
    }
    for (slot, announce) in [(&mut state.hooks_task, false), (&mut state.hook_op, true)] {
        if let Some(t) = slot {
            match t.poll() {
                Some(Ok(h)) => {
                    state.hooks = Some(h);
                    *slot = None;
                    if announce {
                        out.push(Action::Toast {
                            status: Status::Ok,
                            text: cx.tr.t("prefs.notify.toast.hook_done"),
                        });
                    }
                }
                Some(Err(e)) => {
                    state.hooks_error = Some(error_text(cx.tr, &e));
                    out.push(Action::Toast {
                        status: Status::Error,
                        text: error_text(cx.tr, &e),
                    });
                    *slot = None;
                }
                None => running = true,
            }
        }
    }
    if let Some(ed) = &mut state.editor
        && let Some((and_test, t)) = &ed.saving
    {
        match t.poll() {
            Some(Ok(saved)) => {
                let and_test = *and_test;
                let channels = &mut settings.notify.channels;
                match channels.iter_mut().find(|c| c.id == saved.id) {
                    Some(slot) => *slot = saved.clone(),
                    None => channels.push(saved.clone()),
                }
                out.push(Action::Toast {
                    status: Status::Ok,
                    text: cx.tr.t("prefs.notify.toast.saved"),
                });
                state.editor = None;
                if and_test {
                    start_test(cx, state, Some(saved.id));
                }
            }
            Some(Err(e)) => {
                ed.error = Some(error_text(cx.tr, &e));
                ed.saving = None;
            }
            None => running = true,
        }
    }
    if let Some(t) = &state.toggle_task {
        match t.poll() {
            Some(Ok(saved)) => {
                if let Some(slot) = settings
                    .notify
                    .channels
                    .iter_mut()
                    .find(|c| c.id == saved.id)
                {
                    *slot = saved;
                }
                state.toggle_task = None;
            }
            Some(Err(e)) => {
                state.toggle_task = None;
                out.push(Action::Toast {
                    status: Status::Error,
                    text: error_text(cx.tr, &e),
                });
            }
            None => running = true,
        }
    }
    if let Some((id, t)) = &state.delete_task {
        match t.poll() {
            Some(Ok(())) => {
                let id = id.clone();
                settings.notify.channels.retain(|c| c.id != id);
                state.results.remove(&id);
                state.delete_task = None;
                out.push(Action::Toast {
                    status: Status::Ok,
                    text: cx.tr.t("prefs.notify.toast.deleted"),
                });
            }
            Some(Err(e)) => {
                state.delete_task = None;
                out.push(Action::Toast {
                    status: Status::Error,
                    text: error_text(cx.tr, &e),
                });
            }
            None => running = true,
        }
    }
    if let Some((_, t)) = &state.test {
        match t.poll() {
            Some(Ok(report)) => {
                for c in &report.channels {
                    match &c.outcome {
                        ChannelOutcome::Sent { ms } => {
                            state.results.insert(c.channel.clone(), Ok(*ms));
                        }
                        ChannelOutcome::Failed { error } => {
                            state.results.insert(c.channel.clone(), Err(error.clone()));
                        }
                        ChannelOutcome::Skipped { .. } => {}
                    }
                }
                let failed: Vec<&str> = report
                    .channels
                    .iter()
                    .filter_map(|c| match &c.outcome {
                        ChannelOutcome::Failed { error } => Some(error.as_str()),
                        _ => None,
                    })
                    .collect();
                out.push(match failed.first() {
                    None => Action::Toast {
                        status: Status::Ok,
                        text: cx
                            .tr
                            .f("prefs.notify.toast.test_ok", &[("n", &report.sent())]),
                    },
                    Some(e) => Action::Toast {
                        status: Status::Error,
                        text: cx.tr.f("prefs.notify.toast.test_failed", &[("error", e)]),
                    },
                });
                state.test = None;
            }
            Some(Err(e)) => {
                out.push(Action::Toast {
                    status: Status::Error,
                    text: error_text(cx.tr, &e),
                });
                state.test = None;
            }
            None => running = true,
        }
    }
    running
}

fn start_test(cx: &Cx<'_>, state: &mut State, channel: Option<String>) {
    if state.test.is_some() {
        return;
    }
    let mut event = NotifyEvent::new(EventKind::Test, harness::SUPERBA, Timestamp::EPOCH);
    event.detail = Some(cx.tr.t("notify.msg.test_body"));
    let options = SendOptions {
        channel: channel.clone(),
        force: true,
    };
    let s = cx.service.clone();
    state.test = Some((
        channel,
        task(async move { s.notify_send(event, options).await }),
    ));
}

/// Draw the section (title, cards and dialogs).
pub fn section_ui(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    state: &mut State,
    out: &mut Vec<Action>,
) {
    if poll(cx, settings, state, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    if std::mem::take(&mut state.scroll_to) {
        ui.scroll_to_cursor(Some(Align::TOP));
    }
    section(ui, cx, &cx.tr.t("prefs.notify.title"), None);
    body(ui, cx, settings, state, out);
}

/// The section without its title, scrolling or page layout (for a later
/// layout of Preferences that draws its own title). Toggles edit `settings`
/// like every preference; channel changes, the test button and the Codex
/// hook buttons act through the service at once.
pub fn body_ui(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    state: &mut State,
    out: &mut Vec<Action>,
) {
    if poll(cx, settings, state, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    body(ui, cx, settings, state, out);
}

fn body(
    ui: &mut Ui,
    cx: &Cx<'_>,
    settings: &mut Settings,
    state: &mut State,
    out: &mut Vec<Action>,
) {
    Card::new().flat().show(ui, |ui| {
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.enabled.title"),
            &cx.tr.t("prefs.notify.enabled.desc"),
            false,
            |ui| {
                ui.add(Toggle::new(
                    &mut settings.notify.enabled,
                    cx.tr.t("prefs.notify.enabled.title"),
                ));
            },
        );
        channels(ui, cx, settings, state);
    });
    // Unused (off, no channel): the switch and "Add channel" only, so the
    // page stays short for people who do not want notifications.
    let on = settings.notify.enabled;
    if on || !settings.notify.channels.is_empty() {
        ui.add_space(12.0);
        ui.add_enabled_ui(on, |ui| {
            events_card(ui, cx, settings);
            ui.add_space(12.0);
            content_card(ui, cx, settings, state);
            ui.add_space(12.0);
            hooks_card(ui, cx, state, out);
        });
    }
    editor_dialog(ui, cx, state);
    delete_dialog(ui, cx, state);
}

fn channels(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, state: &mut State) {
    ui.add_space(8.0);
    ui.label(Text::strong(cx.tr.t("prefs.notify.channels.title"), cx.pal));
    ui.add_space(4.0);
    if settings.notify.channels.is_empty() {
        ui.add(
            egui::Label::new(Text::small(cx.tr.t("prefs.notify.channels.empty"), cx.pal)).wrap(),
        );
    }
    let testing = state.test.as_ref().map(|(c, _)| c.clone());
    let mut edit = None;
    let mut test = None;
    let mut delete = None;
    let mut toggled = None;
    for c in &settings.notify.channels {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(Badge::new(kind_label(cx, c.kind)).status(if c.enabled {
                Status::Ok
            } else {
                Status::Idle
            }));
            ui.label(Text::strong(c.display_name(), cx.pal));
            if c.kind.third_party_relay() {
                ui.add(
                    Badge::new(cx.tr.t("prefs.notify.channels.third_party"))
                        .status(Status::Warn)
                        .outline(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(Button::ghost(cx.tr.t("prefs.notify.channels.delete")).small())
                    .clicked()
                {
                    delete = Some((c.id.clone(), c.display_name()));
                }
                if ui
                    .add(Button::ghost(cx.tr.t("prefs.notify.channels.edit")).small())
                    .clicked()
                {
                    edit = Some(Editor::edit(c));
                }
                let busy = testing.as_ref() == Some(&Some(c.id.clone())) || testing == Some(None);
                if ui
                    .add(
                        Button::new(cx.tr.t("prefs.notify.channels.test"))
                            .icon(Icon::Bolt)
                            .small()
                            .loading(busy)
                            .enabled(testing.is_none()),
                    )
                    .clicked()
                {
                    test = Some(Some(c.id.clone()));
                }
                let mut enabled = c.enabled;
                if ui
                    .add(Toggle::new(
                        &mut enabled,
                        cx.tr.t("prefs.notify.channels.enabled"),
                    ))
                    .changed()
                {
                    toggled = Some((c.id.clone(), enabled));
                }
            });
        });
        let note = match state.results.get(&c.id) {
            Some(Ok(ms)) => Some(
                Text::small(cx.tr.f("prefs.notify.channels.sent", &[("ms", ms)]), cx.pal)
                    .color(cx.pal.green),
            ),
            Some(Err(e)) => Some(Text::small(e, cx.pal).color(cx.pal.red)),
            None => None,
        };
        let detail: Vec<String> = c
            .params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .chain(c.secret_hints.iter().map(|(k, v)| format!("{k}={v}")))
            .collect();
        ui.label(Text::mono_muted(detail.join("  "), cx.pal));
        if let Some(note) = note {
            ui.add(egui::Label::new(note).wrap());
        }
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add(
                Button::new(cx.tr.t("prefs.notify.channels.add"))
                    .icon(Icon::Plus)
                    .small(),
            )
            .clicked()
        {
            edit = Some(Editor::new(ChannelKind::Telegram));
        }
        if !settings.notify.channels.is_empty()
            && ui
                .add(
                    Button::ghost(cx.tr.t("prefs.notify.channels.test_all"))
                        .small()
                        .loading(testing == Some(None))
                        .enabled(testing.is_none()),
                )
                .clicked()
        {
            test = Some(None);
        }
    });
    if let Some(e) = edit {
        state.editor = Some(e);
    }
    if let Some(t) = test {
        start_test(cx, state, t);
    }
    if let Some(d) = delete {
        state.delete = Some(d);
    }
    if let Some((id, on)) = toggled
        && state.toggle_task.is_none()
        && let Some(slot) = settings.notify.channels.iter_mut().find(|c| c.id == id)
    {
        // Through the service like an edit (the stored secrets stay).
        slot.enabled = on;
        let (channel, secrets) = Editor::edit(slot).channel();
        let s = cx.service.clone();
        state.toggle_task = Some(task(async move {
            s.notify_save_channel(channel, secrets).await
        }));
    }
}

fn events_card(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings) {
    let t = &mut settings.notify.triggers;
    Card::new().flat().show(ui, |ui| {
        ui.label(Text::strong(cx.tr.t("prefs.notify.events.title"), cx.pal));
        ui.add_space(4.0);
        let items: [(&mut bool, &str); 8] = [
            (&mut t.turn_complete, "prefs.notify.events.turn_complete"),
            (&mut t.needs_approval, "prefs.notify.events.needs_approval"),
            (&mut t.needs_input, "prefs.notify.events.needs_input"),
            (&mut t.turn_failed, "prefs.notify.events.turn_failed"),
            (&mut t.switch_done, "prefs.notify.events.switch_done"),
            (&mut t.switch_failed, "prefs.notify.events.switch_failed"),
            (
                &mut t.router_member_down,
                "prefs.notify.events.router_member_down",
            ),
            (
                &mut t.update_available,
                "prefs.notify.events.update_available",
            ),
        ];
        egui::Grid::new("notify-events")
            .num_columns(4)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                for (i, (value, key)) in items.into_iter().enumerate() {
                    let label = cx.tr.t(key);
                    ui.add(Toggle::new(value, label.clone()));
                    ui.label(Text::small(label, cx.pal).color(cx.pal.text));
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        ui.add_space(6.0);
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.events.min_title"),
            &cx.tr.t("prefs.notify.events.min_desc"),
            false,
            |ui| {
                Segmented::new("notify-min", &mut t.min_turn_secs)
                    .option(0, cx.tr.t("prefs.notify.events.min_any"))
                    .option(30, cx.tr.t("prefs.notify.events.min_30s"))
                    .option(120, cx.tr.t("prefs.notify.events.min_2m"))
                    .option(600, cx.tr.t("prefs.notify.events.min_10m"))
                    .small()
                    .show(ui);
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.events.harness_title"),
            &cx.tr.t("prefs.notify.events.harness_desc"),
            true,
            |ui| {
                for h in [harness::CLAUDE_CODE, harness::CODEX] {
                    let mut on = !t.muted_harnesses.contains(h);
                    let name = harness::display_name(h);
                    ui.label(Text::small(name.clone(), cx.pal).color(cx.pal.text));
                    if ui.add(Toggle::new(&mut on, name)).changed() {
                        if on {
                            t.muted_harnesses.remove(h);
                        } else {
                            t.muted_harnesses.insert(h.to_owned());
                        }
                    }
                    ui.add_space(8.0);
                }
            },
        );
    });
}

fn content_card(ui: &mut Ui, cx: &Cx<'_>, settings: &mut Settings, state: &mut State) {
    let n = &mut settings.notify;
    Card::new().flat().show(ui, |ui| {
        ui.label(Text::strong(cx.tr.t("prefs.notify.content.title"), cx.pal));
        ui.add_space(4.0);
        egui::Grid::new("notify-content")
            .num_columns(4)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                let items: [(&mut bool, &str); 3] = [
                    (&mut n.content.project, "prefs.notify.content.project"),
                    (&mut n.content.usage, "prefs.notify.content.usage"),
                    (&mut n.content.host, "prefs.notify.content.host"),
                ];
                for (i, (value, key)) in items.into_iter().enumerate() {
                    let label = cx.tr.t(key);
                    ui.add(Toggle::new(value, label.clone()));
                    ui.label(Text::small(label, cx.pal).color(cx.pal.text));
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        ui.add_space(6.0);
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.content.summary"),
            &cx.tr.t("prefs.notify.content.summary_desc"),
            false,
            |ui| {
                ui.add(Toggle::new(
                    &mut n.content.summary,
                    cx.tr.t("prefs.notify.content.summary"),
                ));
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.quiet.title"),
            &cx.tr.t("prefs.notify.quiet.desc"),
            false,
            |ui| {
                let q = &mut n.quiet_hours;
                let to = state.quiet_to.get_or_insert_with(|| hhmm(q.end_min));
                let r = ui.add_enabled(
                    q.enabled,
                    TextEdit::singleline(to)
                        .desired_width(64.0)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("08:00"),
                );
                if r.changed()
                    && let Some(m) = parse_hhmm(to)
                {
                    q.end_min = m;
                }
                ui.label(Text::small(cx.tr.t("prefs.notify.quiet.to"), cx.pal));
                let from = state.quiet_from.get_or_insert_with(|| hhmm(q.start_min));
                let r = ui.add_enabled(
                    q.enabled,
                    TextEdit::singleline(from)
                        .desired_width(64.0)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("23:00"),
                );
                if r.changed()
                    && let Some(m) = parse_hhmm(from)
                {
                    q.start_min = m;
                }
                ui.label(Text::small(cx.tr.t("prefs.notify.quiet.from"), cx.pal));
                ui.add_space(8.0);
                ui.add(Toggle::new(
                    &mut q.enabled,
                    cx.tr.t("prefs.notify.quiet.title"),
                ));
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.quiet.urgent"),
            &cx.tr.t("prefs.notify.quiet.urgent_desc"),
            false,
            |ui| {
                ui.add(
                    Toggle::new(
                        &mut n.quiet_hours.allow_urgent,
                        cx.tr.t("prefs.notify.quiet.urgent"),
                    )
                    .enabled(n.quiet_hours.enabled),
                );
            },
        );
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.rate.title"),
            &cx.tr.t("prefs.notify.rate.desc"),
            true,
            |ui| {
                let per = |n: u32| cx.tr.f("prefs.notify.rate.per_minute", &[("n", &n)]);
                Segmented::new("notify-rate", &mut n.rate_limit.per_minute)
                    .option(3, per(3))
                    .option(6, per(6))
                    .option(12, per(12))
                    .option(0, cx.tr.t("prefs.notify.rate.none"))
                    .small()
                    .show(ui);
            },
        );
    });
}

fn state_badge(cx: &Cx<'_>, state: HookState) -> Badge {
    let (status, key) = match state {
        HookState::Installed => (Status::Ok, "prefs.notify.hooks.installed"),
        HookState::Untrusted => (Status::Warn, "prefs.notify.hooks.untrusted"),
        HookState::Foreign => (Status::Warn, "prefs.notify.hooks.foreign"),
        HookState::Unreadable => (Status::Error, "prefs.notify.hooks.unreadable"),
        HookState::External => (Status::Idle, "prefs.notify.hooks.external"),
        _ => (Status::Idle, "prefs.notify.hooks.missing"),
    };
    Badge::new(cx.tr.t(key)).status(status)
}

fn hooks_card(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let busy = state.hook_op.is_some();
    let mut request = None;
    Card::new().flat().show(ui, |ui| {
        ui.label(Text::strong(cx.tr.t("prefs.notify.hooks.title"), cx.pal));
        ui.add(egui::Label::new(Text::small(cx.tr.t("prefs.notify.hooks.desc"), cx.pal)).wrap());
        ui.add_space(4.0);
        let Some(hooks) = &state.hooks else {
            if let Some(e) = &state.hooks_error {
                ui.label(Text::small(e, cx.pal).color(cx.pal.red));
            }
            return;
        };
        let find = |m: &str| hooks.hooks.iter().find(|h| h.mechanism == m);
        let notify = find(superba_switch::notify::MECH_CODEX_NOTIFY);
        let approvals = find(superba_switch::notify::MECH_CODEX_PERMISSION);
        let claude = find(superba_switch::notify::MECH_CLAUDE_HOOKS);
        if let Some(h) = notify {
            let desc = h
                .notes
                .iter()
                .take(2)
                .map(|n| cx.localize(n))
                .collect::<Vec<_>>()
                .join(" ");
            row(
                ui,
                cx,
                &cx.tr.t("prefs.notify.hooks.codex_notify"),
                &desc,
                false,
                |ui| {
                    let installed = h.state == HookState::Installed;
                    let label = if installed {
                        cx.tr.t("prefs.notify.hooks.remove")
                    } else {
                        cx.tr.t("prefs.notify.hooks.install")
                    };
                    if ui
                        .add(Button::new(label).small().loading(busy).enabled(!busy))
                        .clicked()
                    {
                        request = Some((superba_switch::notify::MECH_CODEX_NOTIFY, !installed));
                    }
                    ui.add(state_badge(cx, h.state));
                },
            );
        }
        if let Some(h) = approvals {
            let desc = if h.state == HookState::Untrusted {
                cx.tr.t("prefs.notify.hooks.trust_desc")
            } else {
                h.notes.first().map(|n| cx.localize(n)).unwrap_or_default()
            };
            row(
                ui,
                cx,
                &cx.tr.t("prefs.notify.hooks.codex_approvals"),
                &desc,
                false,
                |ui| {
                    let present = matches!(h.state, HookState::Installed | HookState::Untrusted);
                    let label = if present {
                        cx.tr.t("prefs.notify.hooks.remove")
                    } else {
                        cx.tr.t("prefs.notify.hooks.install")
                    };
                    let kind = if present {
                        Button::ghost(label)
                    } else {
                        Button::new(label)
                    };
                    if ui.add(kind.small().enabled(!busy)).clicked() {
                        request = Some((superba_switch::notify::MECH_CODEX_PERMISSION, !present));
                    }
                    if h.state == HookState::Untrusted {
                        // The description says why; the button is the next step.
                        if ui
                            .add(
                                Button::primary(cx.tr.t("prefs.notify.hooks.trust"))
                                    .small()
                                    .loading(busy)
                                    .enabled(!busy),
                            )
                            .clicked()
                        {
                            request = Some((superba_switch::notify::MECH_CODEX_TRUST, true));
                        }
                    } else {
                        ui.add(state_badge(cx, h.state));
                    }
                },
            );
        }
        row(
            ui,
            cx,
            &cx.tr.t("prefs.notify.hooks.errors_title"),
            &cx.tr.t("prefs.notify.hooks.failed_desc"),
            false,
            |ui| {
                ui.add(Badge::new(cx.tr.t("prefs.notify.hooks.errors_on")).status(Status::Ok));
            },
        );
        if let Some(h) = claude {
            let desc = h.notes.first().map(|n| cx.localize(n)).unwrap_or_default();
            row(
                ui,
                cx,
                &cx.tr.t("prefs.notify.hooks.claude"),
                &desc,
                true,
                |ui| {
                    if ui
                        .add(
                            Button::ghost(cx.tr.t("prefs.notify.hooks.copy"))
                                .icon(Icon::Copy)
                                .small(),
                        )
                        .clicked()
                    {
                        let fragment = superba_switch::notify::claude_code_hooks(&hooks.program);
                        let text =
                            serde_json::to_string_pretty(&serde_json::json!({ "hooks": fragment }))
                                .unwrap_or_default();
                        out.push(Action::Copy(text));
                        out.push(Action::Toast {
                            status: Status::Ok,
                            text: cx.tr.t("prefs.notify.hooks.copied"),
                        });
                    }
                },
            );
        }
        ui.add_space(4.0);
        ui.label(Text::caption(
            cx.tr
                .f("prefs.notify.hooks.program", &[("program", &hooks.program)]),
            cx.pal,
        ));
    });
    if let Some((mechanism, install)) = request {
        let s = cx.service.clone();
        let req = HookRequest {
            harness: harness::CODEX.into(),
            mechanism: mechanism.into(),
            install,
        };
        state.hook_op = Some(task(async move { s.notify_set_hook(req).await }));
    }
}

fn editor_dialog(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some(ed) = state.editor.as_mut() else {
        return;
    };
    let mut close = false;
    let mut save = None;
    let saving = ed.saving.is_some();
    let r = dialog(
        ui.ctx(),
        egui::Id::unique("notify-channel-editor"),
        520.0,
        |ui| {
            let title = if ed.id.is_empty() {
                cx.tr.t("prefs.notify.editor.add_title")
            } else {
                cx.tr
                    .f("prefs.notify.editor.edit_title", &[("name", &ed.name)])
            };
            ui.label(Text::title(title, cx.pal));
            ui.add_space(4.0);
            if ed.id.is_empty() {
                ui.label(Text::strong(cx.tr.t("prefs.notify.editor.kind"), cx.pal));
                let options: Vec<(ChannelKind, String)> = ChannelKind::ALL
                    .iter()
                    .map(|k| (*k, kind_label(cx, *k)))
                    .collect();
                let before = ed.kind;
                Dropdown::new("notify-kind")
                    .label(cx.tr.t("prefs.notify.editor.kind"))
                    .width(ui.available_width())
                    .show_options(ui, &mut ed.kind, &options);
                if ed.kind != before {
                    ed.params.clear();
                    ed.secrets.clear();
                    ed.error = None;
                }
            }
            let route = cx.localize(&ed.kind.route_note());
            let color = if ed.kind.third_party_relay() {
                cx.pal.amber
            } else {
                cx.pal.text_subtle
            };
            ui.add(egui::Label::new(Text::small(route, cx.pal).color(color)).wrap());
            ui.add(
                egui::Label::new(Text::small(
                    cx.tr
                        .t(&format!("prefs.notify.editor.help.{}", ed.kind.id())),
                    cx.pal,
                ))
                .wrap(),
            );
            ui.add_space(4.0);
            ui.label(Text::strong(cx.tr.t("prefs.notify.editor.name"), cx.pal));
            ui.add(
                TextEdit::singleline(&mut ed.name)
                    .hint_text(cx.tr.t("prefs.notify.editor.name_hint"))
                    .desired_width(f32::INFINITY)
                    .margin(vec2(12.0, 8.0)),
            );
            for f in ed.kind.fields() {
                let mut label = cx.localize(&f.label());
                if !f.required {
                    label = format!("{label} · {}", cx.tr.t("prefs.notify.editor.optional"));
                }
                ui.label(Text::strong(label, cx.pal));
                if f.secret {
                    let hint = match ed.hints.get(f.key) {
                        Some(h) => cx.tr.f("prefs.notify.editor.secret_saved", &[("hint", h)]),
                        None => String::new(),
                    };
                    let value = ed.secrets.entry(f.key.to_owned()).or_default();
                    let reveal = ed.reveal.entry(f.key.to_owned()).or_default();
                    KeyInput::new(
                        ("notify-secret", f.key),
                        value,
                        reveal,
                        hint,
                        cx.tr.t("prefs.notify.editor.reveal"),
                    )
                    .show(ui);
                } else {
                    let hint = f
                        .default
                        .map(|d| cx.tr.f("prefs.notify.editor.default", &[("value", &d)]))
                        .unwrap_or_default();
                    let value = ed.params.entry(f.key.to_owned()).or_default();
                    ui.add(
                        TextEdit::singleline(value)
                            .font(egui::TextStyle::Monospace)
                            .hint_text(hint)
                            .desired_width(f32::INFINITY)
                            .margin(vec2(12.0, 8.0)),
                    );
                }
            }
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(Text::caption(
                    cx.tr.t("prefs.notify.editor.secret_note"),
                    cx.pal,
                ))
                .wrap(),
            );
            if let Some(e) = &ed.error {
                ui.add(egui::Label::new(Text::small(e, cx.pal).color(cx.pal.red)).wrap());
            }
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        Button::primary(cx.tr.t("prefs.notify.editor.save_test"))
                            .small()
                            .loading(saving)
                            .enabled(!saving),
                    )
                    .clicked()
                {
                    save = Some(true);
                }
                if ui
                    .add(
                        Button::new(cx.tr.t("prefs.notify.editor.save"))
                            .small()
                            .enabled(!saving),
                    )
                    .clicked()
                {
                    save = Some(false);
                }
                if ui
                    .add(
                        Button::ghost(cx.tr.t("action.cancel"))
                            .kbd("Esc")
                            .small()
                            .enabled(!saving),
                    )
                    .clicked()
                {
                    close = true;
                }
            });
        },
    );
    if r.should_close() && !saving {
        close = true;
    }
    if let Some(and_test) = save {
        let (channel, secrets) = ed.channel();
        // The same check the service runs, in the user's language; stored
        // secrets count as filled.
        let mut check = secrets.clone();
        for k in ed.hints.keys() {
            if check.get(k).is_none() {
                check.set(k.clone(), Secret::new("stored"));
            }
        }
        match superba_switch::notify::validate_channel(&channel, &check) {
            Err(text) => ed.error = Some(cx.localize(&text)),
            Ok(()) => {
                ed.error = None;
                let s = cx.service.clone();
                ed.saving = Some((
                    and_test,
                    task(async move { s.notify_save_channel(channel, secrets).await }),
                ));
            }
        }
    }
    if close {
        state.editor = None;
    }
}

fn delete_dialog(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let Some((id, name)) = state.delete.clone() else {
        return;
    };
    let mut close = false;
    let r = dialog(ui.ctx(), egui::Id::unique("notify-delete"), 420.0, |ui| {
        ui.label(Text::title(
            cx.tr
                .f("prefs.notify.channels.delete_title", &[("name", &name)]),
            cx.pal,
        ));
        ui.add(
            egui::Label::new(Text::small(
                cx.tr.t("prefs.notify.channels.delete_body"),
                cx.pal,
            ))
            .wrap(),
        );
        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(Button::danger(cx.tr.t("prefs.notify.channels.delete")).small())
                .clicked()
            {
                let s = cx.service.clone();
                let id2 = id.clone();
                state.delete_task = Some((
                    id.clone(),
                    task(async move { s.notify_delete_channel(&id2).await }),
                ));
                close = true;
            }
            if ui
                .add(Button::ghost(cx.tr.t("action.cancel")).kbd("Esc").small())
                .clicked()
            {
                close = true;
            }
        });
    });
    if r.should_close() || close {
        state.delete = None;
    }
}
