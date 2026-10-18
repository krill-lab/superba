//! Notifications through the service (GH#12): channels and their secrets,
//! sending (hooks, Superba's own events, the test button), the Codex hooks in
//! `config.toml` and the watcher of failed Codex turns. The sending itself is
//! `superba-notify`; this module adds what only the service knows (the
//! preferences, the key store, the Codex home, session titles, prices) and
//! keeps hook processes cheap: a disabled feature returns before any work.

use std::path::PathBuf;
use std::time::Duration;

use superba_core::notify::harness;
use superba_core::{
    Catalog, CodexFileKind, CodexFileSave, Error, EventKind, HookRequest, HookState, HookView,
    LocalizedText, NotifyChannel, NotifyEvent, NotifyHooks, NotifyReport, NotifySecrets,
    NotifySettings, NotifySkip, ProfileSet, Result, SaveReason, SecretBytes, SendOptions, TargetId,
    Timestamp,
};
use superba_notify::{
    Dispatch, Notifier, PolicyInput, channels, codex_config, enrich, hooks, load_secrets,
    secret_entry, secret_hints, secrets_to_secret, watch,
};

use crate::Switcher;

pub use superba_notify::hooks::{claude_code_hooks, split_codex_args};

/// Check a channel's fields before saving (`secrets` = the typed ones plus
/// placeholders for the stored ones): the first missing or malformed field.
pub fn validate_channel(
    channel: &NotifyChannel,
    secrets: &NotifySecrets,
) -> std::result::Result<(), LocalizedText> {
    channels::validate(channel, secrets).map_err(|e| e.0)
}

/// Environment variable naming the program hooks run (tests, portable
/// installs); default: `spb` next to the running binary.
pub const ENV_NOTIFY_PROGRAM: &str = "SUPERBA_NOTIFY_PROGRAM";
/// The `notify` mechanism of Codex.
pub const MECH_CODEX_NOTIFY: &str = "notify";
/// Codex's `PermissionRequest` lifecycle hook.
pub const MECH_CODEX_PERMISSION: &str = "hooks.PermissionRequest";
/// Record Codex's trust for Superba's `PermissionRequest` hook (the user's
/// explicit choice; Codex computes the hash, see [`superba_notify::codex_rpc`]).
pub const MECH_CODEX_TRUST: &str = "hooks.PermissionRequest.trust";
/// Codex runs `async` lifecycle hooks from this version on (0.140 skips them).
pub const ASYNC_HOOKS_SINCE: superba_core::CodexVersion =
    superba_core::CodexVersion::new(0, 150, 0);
/// Claude Code's `settings.json` hooks.
pub const MECH_CLAUDE_HOOKS: &str = "settings.json hooks";

/// How long a hook waits for Codex to write the end of the turn it reports.
const ROLLOUT_WAIT: Duration = Duration::from_millis(1500);

impl Switcher {
    fn notifier(&self) -> Notifier {
        Notifier::new(self.deps.ctx.data_dir.notify_dir())
    }

    /// The program the hooks run (absolute path).
    pub(crate) fn notify_program(&self) -> String {
        if let Some(p) = self.deps.ctx.env.var_non_empty(ENV_NOTIFY_PROGRAM) {
            return p;
        }
        let exe = std::env::current_exe().unwrap_or_default();
        let name = if cfg!(windows) { "spb.exe" } else { "spb" };
        let sibling = exe.parent().map(|d| d.join(name));
        match sibling {
            Some(p) if p.is_file() => p.display().to_string(),
            // `superba <subcommand>` behaves like `spb`.
            _ => exe.display().to_string(),
        }
    }

    fn local_minute(&self, now: Timestamp) -> Option<u16> {
        let offset = crate::localtime::offset_secs(now)?;
        let secs = (now.as_millis().div_euclid(1000) + offset).rem_euclid(86_400);
        u16::try_from(secs / 60).ok()
    }

    async fn dispatch_with(
        &self,
        set: &ProfileSet,
        event: &NotifyEvent,
        options: SendOptions,
    ) -> NotifyReport {
        let catalog = Catalog::new(self.stats_locale(&set.settings));
        let now = self.deps.clock.now();
        let policy = PolicyInput {
            now_ms: now.as_millis(),
            local_minute: self.local_minute(now),
            only_channel: options.channel,
            force: options.force,
        };
        self.notifier()
            .dispatch(
                event,
                Dispatch {
                    settings: &set.settings.notify,
                    catalog: &catalog,
                    policy,
                    secrets: self.deps.secrets.as_ref(),
                },
            )
            .await
    }

    pub(crate) async fn notify_save_channel_impl(
        &self,
        mut channel: NotifyChannel,
        secrets: NotifySecrets,
    ) -> Result<NotifyChannel> {
        self.refuse_elevated()?;
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        if channel.id.trim().is_empty() {
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            channel.id = NotifyChannel::id_for(channel.kind, &suffix);
        }
        let existing = set.settings.notify.channel(&channel.id).cloned();
        if existing.as_ref().is_some_and(|e| e.kind != channel.kind) {
            return Err(Error::invalid("a channel cannot change its type"));
        }
        // Secret fields: the stored ones, overlaid with the new non-empty ones,
        // limited to this kind's secret fields.
        let mut merged = match &existing {
            Some(e) => load_secrets(e, d.secrets.as_ref())
                .await
                .unwrap_or_default(),
            None => NotifySecrets::default(),
        };
        for (k, v) in secrets.0 {
            if !v.is_empty() {
                merged.set(k, v);
            }
        }
        let kind = channel.kind;
        merged
            .0
            .retain(|k, v| !v.is_empty() && kind.field(k).is_some_and(|f| f.secret));
        channel.params = channel
            .params
            .into_iter()
            .map(|(k, v)| (k, v.trim().to_owned()))
            .filter(|(k, v)| !v.is_empty() && kind.field(k).is_some_and(|f| !f.secret))
            .collect();
        if let Err(e) = channels::validate(&channel, &merged) {
            return Err(Error::invalid(e.0.render_fallback()));
        }
        channel.key_ref = if merged.is_empty() {
            None
        } else {
            Some(
                d.secrets
                    .put(&secret_entry(&channel.id), &secrets_to_secret(&merged))
                    .await?,
            )
        };
        channel.secret_hints = secret_hints(&merged);
        let channels = &mut set.settings.notify.channels;
        match channels.iter_mut().find(|c| c.id == channel.id) {
            Some(slot) => *slot = channel.clone(),
            None => channels.push(channel.clone()),
        }
        d.profiles.save(&set).await?;
        // A key reference the channel no longer uses (another backend).
        if let Some(old) = existing.and_then(|e| e.key_ref)
            && Some(&old) != channel.key_ref.as_ref()
            && superba_notify::is_notify_key(&old)
            && let Err(e) = d.secrets.delete(&old).await
        {
            tracing::warn!(error = %e, "cannot delete the channel's old secret");
        }
        Ok(channel)
    }

    pub(crate) async fn notify_delete_channel_impl(&self, id: &str) -> Result<()> {
        self.refuse_elevated()?;
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        let Some(pos) = set.settings.notify.channels.iter().position(|c| c.id == id) else {
            return Err(superba_notify::no_channel(id));
        };
        let channel = set.settings.notify.channels.remove(pos);
        d.profiles.save(&set).await?;
        if let Some(r) = channel.key_ref
            && superba_notify::is_notify_key(&r)
            && let Err(e) = d.secrets.delete(&r).await
        {
            tracing::warn!(error = %e, "cannot delete the channel's secret");
        }
        Ok(())
    }

    pub(crate) async fn notify_send_impl(
        &self,
        mut event: NotifyEvent,
        options: SendOptions,
    ) -> Result<NotifyReport> {
        let set = self.deps.profiles.load().await?;
        if let Some(id) = &options.channel
            && set.settings.notify.channel(id).is_none()
        {
            return Err(superba_notify::no_channel(id));
        }
        if event.host.is_none() {
            event.host = enrich::host_name();
        }
        if event.at == Timestamp::EPOCH {
            event.at = self.deps.clock.now();
        }
        Ok(self.dispatch_with(&set, &event, options).await)
    }

    pub(crate) async fn notify_hook_impl(
        &self,
        harness_id: &str,
        payload: &str,
    ) -> Result<NotifyReport> {
        let set = self.deps.profiles.load().await?;
        if !set.settings.notify.enabled || set.settings.notify.channels.is_empty() {
            return Ok(NotifyReport {
                skipped: Some(NotifySkip::Disabled),
                channels: Vec::new(),
            });
        }
        let now = self.deps.clock.now();
        let Some(mut event) = hooks::parse(harness_id, payload, now).map_err(|_| {
            Error::invalid(
                LocalizedText::new("notify.error.payload", "the hook's data is not valid JSON")
                    .render_fallback(),
            )
        })?
        else {
            return Ok(NotifyReport {
                skipped: Some(NotifySkip::TriggerOff),
                channels: Vec::new(),
            });
        };
        let wanted = &set.settings.notify.triggers;
        if !wanted.wants(event.kind) || wanted.muted_harnesses.contains(&event.harness) {
            return Ok(NotifyReport {
                skipped: Some(if wanted.wants(event.kind) {
                    NotifySkip::HarnessMuted
                } else {
                    NotifySkip::TriggerOff
                }),
                channels: Vec::new(),
            });
        }
        let transcript = hooks::transcript_path(payload).map(PathBuf::from);
        self.enrich(&set, &mut event, transcript).await;
        Ok(self
            .dispatch_with(&set, &event, SendOptions::default())
            .await)
    }

    /// Add what the local files know: host, turn facts, title, cost.
    async fn enrich(&self, set: &ProfileSet, event: &mut NotifyEvent, transcript: Option<PathBuf>) {
        event.host = event.host.take().or_else(enrich::host_name);
        let home = self.deps.ctx.codex_home();
        let facts = if event.harness == harness::CODEX {
            let path = transcript.or_else(|| {
                event
                    .thread_id
                    .as_deref()
                    .and_then(|t| enrich::find_rollout(&home.sessions_dir(), t, 7))
            });
            match path {
                Some(path) => {
                    let turn = event.turn_id.clone();
                    let deadline = tokio::time::Instant::now() + ROLLOUT_WAIT;
                    loop {
                        let p = path.clone();
                        let t = turn.clone();
                        let facts = tokio::task::spawn_blocking(move || {
                            enrich::read_tail(&p)
                                .map(|text| enrich::codex_turn(&text, t.as_deref()))
                        })
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                        // Only a finished turn is worth waiting for (an
                        // approval is asked in the middle of its turn).
                        let done = facts.as_ref().is_some_and(|f| f.ended)
                            || event.kind != EventKind::TurnComplete;
                        if done || tokio::time::Instant::now() >= deadline {
                            break facts;
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
                None => None,
            }
        } else if let Some(path) = transcript {
            let now = event.at;
            tokio::task::spawn_blocking(move || {
                enrich::read_tail(&path).map(|text| enrich::claude_turn(&text, now))
            })
            .await
            .ok()
            .and_then(|r| r.ok())
        } else {
            None
        };
        if let Some(f) = facts {
            // A question in the middle of a turn: its time and tokens so far
            // would read like a result.
            if !event.kind.urgent() {
                event.duration_ms = event.duration_ms.or(f.duration_ms);
                event.tokens = event.tokens.or(f.tokens);
            }
            event.model = event.model.take().or(f.model);
            event.cwd = event.cwd.take().or(f.cwd);
            if event.turn_id.is_none() {
                event.turn_id = f.turn_id;
            }
            if event.kind == EventKind::TurnComplete && f.error.is_some() {
                event.kind = EventKind::TurnFailed;
                event.detail = f.error;
            }
        }
        if event.harness == harness::CODEX
            && event.title.is_none()
            && let Some(thread) = event.thread_id.clone()
        {
            let home_dir = self.deps.ctx.home_dir.clone();
            let key = thread.clone();
            let titles = tokio::task::spawn_blocking(move || {
                superba_sessions::titles::lookup_in(&home, Some(&home_dir), &[key])
            })
            .await
            .ok()
            .and_then(|r| r.ok());
            event.title = titles
                .and_then(|mut t| t.remove(&thread))
                .map(|t| t.title)
                .filter(|t| !t.trim().is_empty());
        }
        if event.cost_micro_usd.is_none()
            && let (Some(tokens), Some(model)) = (event.tokens, event.model.clone())
            && let Ok(table) = self.price_table_impl().await
        {
            let prices = &set.settings.router.stats.prices;
            let profile = self
                .applied_profile()
                .await
                .unwrap_or_else(|| superba_core::ProfileId::new("codex:openai"));
            if let Ok(Some(entry)) = superba_models::price::lookup(&table, prices, &profile, &model)
            {
                let usage = superba_models::price::UsageTokens {
                    input: tokens.input,
                    cached: tokens.cached,
                    cache_write: 0,
                    output: tokens.output,
                };
                let ratio = superba_models::price::ratio_of(prices, &profile);
                if let Ok(cost) = superba_models::price::cost(&entry, &usage, None, ratio) {
                    event.cost_micro_usd = u64::try_from(cost.micros()).ok();
                }
            }
        }
    }

    pub(crate) async fn notify_watch_impl(&self) -> Result<NotifyReport> {
        let set = self.deps.profiles.load().await?;
        let n = &set.settings.notify;
        if !n.enabled || n.channels.is_empty() || !n.triggers.turn_failed {
            return Ok(NotifyReport::default());
        }
        let sessions = self.deps.ctx.codex_home().sessions_dir();
        let now = self.deps.clock.now();
        let state = self.notifier().state().clone();
        let events =
            tokio::task::spawn_blocking(move || state.update(|st| watch::scan(&sessions, st, now)))
                .await
                .map_err(|e| Error::other(format!("notification watcher: {e}")))?
                .map_err(|e| Error::other(format!("notification state: {e}")))?;
        let mut report = NotifyReport::default();
        for mut event in events {
            self.enrich(&set, &mut event, None).await;
            let r = self
                .dispatch_with(&set, &event, SendOptions::default())
                .await;
            report.channels.extend(r.channels);
        }
        Ok(report)
    }

    /// A Superba event (switch, update, router): sent in the background of the
    /// caller, bounded so a slow channel never holds a switch up for long.
    pub(crate) async fn notify_superba(
        &self,
        kind: EventKind,
        detail: Vec<LocalizedText>,
        key: Option<String>,
    ) {
        let Ok(set) = self.deps.profiles.load().await else {
            return;
        };
        let n = &set.settings.notify;
        if !n.enabled || n.channels.is_empty() || !n.triggers.wants(kind) {
            return;
        }
        let catalog = Catalog::new(self.stats_locale(&set.settings));
        let mut event = NotifyEvent::new(kind, harness::SUPERBA, self.deps.clock.now());
        let parts: Vec<String> = detail
            .iter()
            .map(|t| catalog.text(t))
            .filter(|s| !s.trim().is_empty())
            .collect();
        event.detail = (!parts.is_empty()).then(|| parts.join(" — "));
        event.turn_id = key;
        event.host = enrich::host_name();
        let send = self.dispatch_with(&set, &event, SendOptions::default());
        if tokio::time::timeout(Duration::from_secs(12), send)
            .await
            .is_err()
        {
            tracing::warn!(kind = kind.id(), "notification took too long; given up");
        }
    }

    pub(crate) async fn notify_hooks_impl(&self) -> Result<NotifyHooks> {
        let program = self.notify_program();
        let home = self.deps.ctx.codex_home();
        let path = home.config_toml();
        let path_text = path.display().to_string();
        let text = match tokio::fs::read(&path).await {
            Ok(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(String::new()),
            Err(_) => None,
        };
        let facts = text.as_deref().map(|t| codex_config::read(t, &path_text));
        let mut hooks_out = Vec::new();
        let (state, command, chained) = match &facts {
            None | Some(Err(_)) => (HookState::Unreadable, Vec::new(), Vec::new()),
            Some(Ok(f)) => match &f.notify {
                hooks::NotifyShape::Missing => (HookState::Missing, Vec::new(), Vec::new()),
                hooks::NotifyShape::Foreign(v) => (HookState::Foreign, v.clone(), Vec::new()),
                hooks::NotifyShape::Ours { chained, .. } => (
                    HookState::Installed,
                    hooks::codex_notify_argv(&program, chained),
                    chained.clone(),
                ),
            },
        };
        let mut notes = vec![
            LocalizedText::new(
                "notify.hook.codex_notify",
                "Codex runs Superba when a task finishes (config.toml notify).",
            ),
            LocalizedText::new(
                "notify.hook.codex_restart",
                "Codex reads it when it starts: restart ChatGPT (or start a new Codex session) for it to take effect.",
            ),
            LocalizedText::new(
                "notify.hook.codex_errors",
                "Failed tasks are found in Codex's session files while Superba runs (also in the tray).",
            ),
        ];
        if state == HookState::Foreign || !chained.is_empty() {
            notes.push(LocalizedText::new(
                "notify.hook.codex_foreign",
                "Your own notify program stays: Superba's hook runs it with the same data, and puts it back when removed.",
            ));
        }
        hooks_out.push(HookView {
            harness: harness::CODEX.into(),
            events: vec![EventKind::TurnComplete],
            mechanism: MECH_CODEX_NOTIFY.into(),
            file: Some(path_text.clone()),
            state,
            command,
            chained,
            notes,
        });
        let perm = facts.as_ref().and_then(|f| f.as_ref().ok());
        hooks_out.push(HookView {
            harness: harness::CODEX.into(),
            events: vec![EventKind::NeedsApproval],
            mechanism: MECH_CODEX_PERMISSION.into(),
            file: Some(path_text),
            state: match perm {
                None => HookState::Unreadable,
                Some(f) if f.permission_hook && f.permission_trusted => HookState::Installed,
                Some(f) if f.permission_hook => HookState::Untrusted,
                Some(_) => HookState::Missing,
            },
            command: vec![hooks::hook_command(&program, harness::CODEX)],
            chained: Vec::new(),
            notes: if perm.is_some_and(|f| f.permission_hook && f.permission_trusted) {
                Vec::new()
            } else {
                vec![LocalizedText::new(
                    "notify.hook.codex_approvals",
                    "Approvals: Codex lifecycle hooks (PermissionRequest) must be reviewed and trusted in Codex (/hooks) before they run.",
                )]
            },
        });
        hooks_out.push(HookView {
            harness: harness::CLAUDE_CODE.into(),
            events: vec![EventKind::TurnComplete, EventKind::TurnFailed, EventKind::NeedsApproval, EventKind::NeedsInput],
            mechanism: MECH_CLAUDE_HOOKS.into(),
            file: None,
            state: HookState::External,
            command: vec![hooks::hook_command(&program, harness::CLAUDE_CODE)],
            chained: Vec::new(),
            notes: vec![LocalizedText::new(
                "notify.hook.claude_code",
                "Claude Code: Stop, StopFailure and Notification hooks in settings.json, written with the Claude Code settings.",
            )],
        });
        Ok(NotifyHooks {
            program,
            hooks: hooks_out,
        })
    }

    pub(crate) async fn notify_set_hook_impl(&self, request: HookRequest) -> Result<NotifyHooks> {
        self.refuse_elevated()?;
        if request.harness != harness::CODEX {
            return Err(Error::Unsupported {
                what: format!(
                    "{} hooks are written by its own settings page",
                    harness::display_name(&request.harness)
                ),
            });
        }
        let program = self.notify_program();
        let doc = self
            .codex_file_impl(TargetId::local(), CodexFileKind::ConfigToml)
            .await?;
        if let Some(why) = &doc.read_only {
            return Err(Error::Conflict {
                message: why.render_fallback(),
            });
        }
        let text = doc
            .contents
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.expose()).into_owned())
            .unwrap_or_default();
        let edited = match request.mechanism.as_str() {
            MECH_CODEX_NOTIFY => codex_config::set_notify(&text, &program, request.install),
            MECH_CODEX_PERMISSION => {
                let codexes = self.local_codexes().await;
                // Async (the approval prompt never waits for a send) only
                // when every local Codex of a known version runs async hooks.
                let asynchronous = !codexes.is_empty()
                    && codexes
                        .iter()
                        .all(|(_, v)| v.as_ref().is_none_or(|v| *v >= ASYNC_HOOKS_SINCE));
                codex_config::set_permission_hook(&text, &program, request.install, asynchronous)
            }
            MECH_CODEX_TRUST if request.install => {
                let entries = self.untrusted_codex_hooks().await?;
                codex_config::set_trusted(&text, &entries)
            }
            other => {
                return Err(Error::invalid(format!("unknown Codex hook {other}")));
            }
        }
        .map_err(|e| Error::invalid(e.0))?;
        if let Some(new_text) = edited {
            self.save_codex_file_impl(CodexFileSave {
                target: TargetId::local(),
                kind: CodexFileKind::ConfigToml,
                contents: SecretBytes::new(new_text.into_bytes()),
                base_sha256: doc.sha256.clone(),
                reason: SaveReason::Edit,
                // Only our key changed; checks of the rest are the user's.
                force: true,
            })
            .await?;
        }
        self.notify_hooks_impl().await
    }
}

impl Switcher {
    /// Codex programs of this computer with their versions: the desktop
    /// app's bundled Codex first, then the CLIs.
    async fn local_codexes(&self) -> Vec<(PathBuf, Option<superba_core::CodexVersion>)> {
        let mut out = Vec::new();
        if let Ok(installs) = self.deps.app.detect_installs().await {
            for i in installs {
                if let Some(p) = i.bundled_codex {
                    out.push((p, i.bundled_codex_version));
                }
            }
        }
        if let Ok(bins) = self.deps.app.codex_binaries().await {
            for b in bins {
                if !out.iter().any(|(p, _)| *p == b.path) {
                    out.push((b.path, b.version));
                }
            }
        }
        out
    }

    /// Superba's Codex hooks that Codex reports as not trusted, with the hash
    /// Codex computed for each (asked from the newest local Codex).
    async fn untrusted_codex_hooks(&self) -> Result<Vec<(String, String)>> {
        let mut codexes = self.local_codexes().await;
        codexes.sort_by(|a, b| b.1.cmp(&a.1));
        let Some((codex, _)) = codexes.into_iter().next() else {
            return Err(Error::NotFound {
                what: "a Codex program to ask about its hooks".into(),
            });
        };
        let home = self.deps.ctx.codex_home();
        let hooks =
            superba_notify::codex_rpc::hooks_list(&codex, home.root(), &self.deps.ctx.home_dir)
                .await
                .map_err(Error::other)?;
        Ok(hooks
            .into_iter()
            .filter(|h| h.command.as_deref().is_some_and(hooks::is_our_hook_command))
            .filter(|h| h.trust_status != "trusted" && !h.current_hash.is_empty())
            .map(|h| (h.key, h.current_hash))
            .collect())
    }
}

/// Whether the preferences would send anything at all (cheap check for callers
/// that would otherwise build an event).
pub fn notify_active(settings: &NotifySettings) -> bool {
    settings.enabled && settings.channels.iter().any(|c| c.enabled)
}
