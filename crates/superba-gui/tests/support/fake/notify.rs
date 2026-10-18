//! Fake service, area "notify": Preferences → Notifications (GH#12).
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here.

use superba_core::*;

use super::FakeService;

/// Fake state of the area.
#[derive(Debug, Default)]
pub(super) struct State {
    sends: Vec<(NotifyEvent, SendOptions)>,
    saves: Vec<(NotifyChannel, Vec<String>)>,
    hook_requests: Vec<HookRequest>,
    fail_channel: Option<String>,
}

impl FakeService {
    /// Events sent (test button, `spb notify send`).
    pub fn notify_sends(&self) -> Vec<(NotifyEvent, SendOptions)> {
        self.areas.notify.lock().unwrap().sends.clone()
    }

    /// Channels saved, with the names of the secret fields typed.
    pub fn notify_saves(&self) -> Vec<(NotifyChannel, Vec<String>)> {
        self.areas.notify.lock().unwrap().saves.clone()
    }

    /// Make sends to this channel fail.
    pub fn fail_notify_channel(&self, id: &str) {
        self.areas.notify.lock().unwrap().fail_channel = Some(id.to_owned());
    }
}

pub(super) async fn save_channel(
    f: &FakeService,
    mut channel: NotifyChannel,
    secrets: NotifySecrets,
) -> Result<NotifyChannel> {
    if channel.id.is_empty() {
        channel.id = NotifyChannel::id_for(channel.kind, "f00d");
    }
    for (k, v) in &secrets.0 {
        channel.secret_hints.insert(k.clone(), v.masked());
    }
    f.areas
        .notify
        .lock()
        .unwrap()
        .saves
        .push((channel.clone(), secrets.0.keys().cloned().collect()));
    Ok(channel)
}

pub(super) async fn send(
    f: &FakeService,
    event: NotifyEvent,
    options: SendOptions,
) -> Result<NotifyReport> {
    let set = f.profile_set().await?;
    let fail = f.areas.notify.lock().unwrap().fail_channel.clone();
    f.areas
        .notify
        .lock()
        .unwrap()
        .sends
        .push((event, options.clone()));
    // The page's copy of the settings may hold channels the fake's profile
    // set does not: a channel asked for by id answers anyway.
    let mut listed: Vec<NotifyChannel> = set.settings.notify.channels.clone();
    if let Some(id) = &options.channel
        && !listed.iter().any(|c| &c.id == id)
    {
        listed.push(NotifyChannel::new(
            id.clone(),
            ChannelKind::Webhook,
            id.clone(),
        ));
    }
    let channels = listed
        .iter()
        .filter(|c| options.channel.as_ref().is_none_or(|id| *id == c.id))
        .map(|c| ChannelResult {
            channel: c.id.clone(),
            name: c.display_name(),
            kind: c.kind,
            outcome: if fail.as_deref() == Some(c.id.as_str()) {
                ChannelOutcome::Failed {
                    error: "the server refused the message: 400 chat not found".into(),
                }
            } else {
                ChannelOutcome::Sent { ms: 12 }
            },
        })
        .collect();
    Ok(NotifyReport {
        skipped: None,
        channels,
    })
}

pub(super) async fn hooks(f: &FakeService) -> Result<NotifyHooks> {
    let installed = f
        .areas
        .notify
        .lock()
        .unwrap()
        .hook_requests
        .iter()
        .rev()
        .find(|r| r.mechanism == "notify")
        .map(|r| r.install)
        .unwrap_or(true);
    let view = |mechanism: &str, state: HookState, harness: &str| HookView {
        harness: harness.into(),
        events: vec![EventKind::TurnComplete],
        mechanism: mechanism.into(),
        file: Some("/home/user/.codex/config.toml".into()),
        state,
        command: vec![],
        chained: vec![],
        notes: vec![LocalizedText::new(
            "notify.hook.codex_notify",
            "Codex runs Superba when a task finishes (config.toml notify).",
        )],
    };
    Ok(NotifyHooks {
        program: "/usr/bin/spb".into(),
        hooks: vec![
            view(
                "notify",
                if installed {
                    HookState::Installed
                } else {
                    HookState::Missing
                },
                "codex",
            ),
            view("hooks.PermissionRequest", HookState::Untrusted, "codex"),
            view("settings.json hooks", HookState::External, "claude-code"),
        ],
    })
}

pub(super) async fn set_hook(f: &FakeService, request: HookRequest) -> Result<NotifyHooks> {
    f.areas.notify.lock().unwrap().hook_requests.push(request);
    hooks(f).await
}
