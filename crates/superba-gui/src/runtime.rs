//! Background runtime (plan 5.4): egui renders on the main thread; a tokio runtime
//! on its own thread owns the `SwitchService`. The UI sends [`Command`]s; results,
//! switch events and mid-execution questions come back as [`Reply`]s, each followed
//! by a repaint request, so an idle window costs no CPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use superba_core::{
    AppSession, AuthMode, Decision, DecisionRequest, DoctorFinding, Error, ExecuteOptions,
    KeyCheck, KeyInput, LineCaps, LineId, MigrationReceipt, Overview, Profile, ProfileSet,
    RecoveryReport, Result, Secret, SessionReport, Settings, Site, SiteId, SiteKind, SwitchDecider,
    SwitchEvent, SwitchEvents, SwitchOutcome, SwitchPlan, SwitchRequest, SwitchService,
    async_trait,
};
use tokio::sync::{mpsc as tmpsc, oneshot};

/// A one-shot, read-only background task owned by a page. No credential is
/// serialized or passed through the activity log.
#[derive(Debug)]
pub struct ReadTask<T> {
    receiver: mpsc::Receiver<Result<T>>,
}

/// Run `future` on its own short-lived runtime thread and return a handle the page
/// polls each frame ([`ReadTask::poll`]). Pages use it with [`crate::pages::Cx::service`]
/// for reads that should not go through the app's command queue; call
/// `ctx.request_repaint_after(..)` while it is pending.
pub fn task<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T>> + Send + 'static,
) -> ReadTask<T> {
    ReadTask::start(future)
}

impl<T: Send + 'static> ReadTask<T> {
    fn start(future: impl std::future::Future<Output = Result<T>> + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("superba-key-read".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| Error::other(e.to_string()))
                    .and_then(|runtime| runtime.block_on(future));
                let _ = sender.send(result);
            });
        Self { receiver }
    }

    /// Take the completed result without blocking the UI.
    pub fn poll(&self) -> Option<Result<T>> {
        match self.receiver.try_recv() {
            Ok(value) => Some(value),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(Error::other("credential read stopped")))
            }
        }
    }
}

/// Whether the observed configuration points at one of `site`'s lines (its
/// `openai_base_url` host is a line host of the site, or `model_provider` is the
/// site id). A key found in auth.json is only offered for that site, never for
/// the official login or an unrelated provider.
pub fn config_matches_site(config: &superba_core::ConfigState, site: &Site) -> bool {
    if config.parse_error.is_some() || site.lines.is_empty() {
        return false;
    }
    if config.model_provider.as_deref() == Some(site.id.as_str()) {
        return true;
    }
    let Some(host) = config
        .openai_base_url
        .as_deref()
        .and_then(|base| url::Url::parse(base).ok())
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    site.lines
        .iter()
        .any(|l| l.origin.host().eq_ignore_ascii_case(&host))
        || (site.kind == SiteKind::Krill && superba_core::catalog::is_krill_host(&host))
}

/// Read the key of the observed configuration only while both source files
/// still match the snapshot the wizard displayed and the configuration points at
/// `site`. Tests pass an isolated context; this never modifies files.
pub async fn read_original_key(
    ctx: superba_core::LocalContext,
    expected: &superba_core::ConfigState,
    site: &Site,
) -> Result<Option<Secret>> {
    if !config_matches_site(expected, site) || !expected.auth.mode().is_some_and(AuthMode::uses_key)
    {
        return Ok(None);
    }
    let home = superba_core::CodexHome::new(expected.codex_home.clone());
    let config = superba_switch::SwitcherDeps::with_defaults(ctx).config;
    let before = config.read_state(&home).await?;
    if before.config_sha256 != expected.config_sha256
        || before.auth_sha256 != expected.auth_sha256
        || !config_matches_site(&before, site)
    {
        return Ok(None);
    }
    let credentials = config.active_credentials(&home).await?;
    let after = config.read_state(&home).await?;
    if after.config_sha256 != before.config_sha256 || after.auth_sha256 != before.auth_sha256 {
        return Ok(None);
    }
    Ok(credentials
        .filter(|credentials| credentials.auth_mode.uses_key() && !credentials.key.is_empty())
        .map(|credentials| credentials.key))
}

/// Start the wizard's read of the key in auth.json for `site`.
pub fn original_key_task(
    config: superba_core::ConfigState,
    site: Site,
) -> ReadTask<Option<Secret>> {
    ReadTask::start(async move {
        read_original_key(superba_switch::detect_context()?, &config, &site).await
    })
}

/// Read only a stored key's display suffix; the secret is dropped on the worker.
pub fn profile_key_task(reference: superba_core::KeyRef) -> ReadTask<Option<String>> {
    ReadTask::start(async move {
        let deps = superba_switch::SwitcherDeps::with_defaults(superba_switch::detect_context()?);
        Ok(Some(deps.secrets.get(&reference).await?.last4()))
    })
}

/// Something the UI asks the backend to do.
#[derive(Debug)]
pub enum Command {
    /// Reload overview, profiles and sites.
    Refresh,
    /// Reload only the overview (periodic poll).
    Overview,
    /// Probe all lines of a site.
    ProbeLines(SiteId),
    /// Validate a key on a line.
    CheckKey {
        /// Site.
        site: SiteId,
        /// Line.
        line: LineId,
        /// Mode.
        mode: AuthMode,
        /// Key.
        key: KeyInput,
    },
    /// Load the session report and history.
    Sessions,
    /// Run diagnostics.
    Doctor,
    /// Plan a switch; `seq` identifies the answer (only the latest one counts).
    Plan {
        /// Sequence number.
        seq: u64,
        /// Request.
        request: SwitchRequest,
    },
    /// Execute a plan.
    Execute {
        /// Plan returned by `Plan`.
        plan: Box<SwitchPlan>,
        /// Options.
        options: ExecuteOptions,
    },
    /// Stop the session migration of the running switch where that is safe
    /// (`SwitchEvents::stop_requested`; the migration then undoes itself).
    StopSessions,
    /// Save settings.
    SaveSettings(Settings),
    /// Save a profile (and its key).
    SaveProfile {
        /// Profile.
        profile: Box<Profile>,
        /// New key.
        key: Option<Secret>,
    },
    /// Save a custom site (new or edited) and then its profile and key (the
    /// setup wizard's Apply); a site created here is removed again when the
    /// profile cannot be saved. Replies like `SaveProfile`.
    SaveSiteAndProfile {
        /// Site.
        site: Box<Site>,
        /// Profile using it.
        profile: Box<Profile>,
        /// New key.
        key: Option<Secret>,
    },
    /// Save a compatible site and its first profile, rolling back the site on failure.
    AddCompatibleSite {
        /// Validated site.
        site: Box<Site>,
        /// Profile using this site.
        profile: Box<Profile>,
        /// API key to store with the profile.
        key: Secret,
    },
    /// Remove an unused compatible site and all profiles referencing it.
    RemoveCompatibleSite(SiteId),
    /// Launch the app through Superba.
    OpenApp,
    /// Start-up recovery.
    Recover,
    /// Identity and balance (and, when `request.usage` is set, the usage report).
    Account(superba_core::AccountRequest),
    /// One router watchdog tick (falls back to direct mode when the router stays
    /// down).
    RouterWatch,
    /// Remote hosts the app is connected to (sidebar badge, Overview).
    Hosts,
    /// Versions of the app and Codex (`refresh` = check online now).
    Versions {
        /// Check online now instead of using the cached lists.
        refresh: bool,
    },
    /// Look for a newer Superba (`manual` = the user asked: ignores the
    /// automatic-check preference and the 24 h interval).
    SelfUpdateCheck {
        /// The user asked.
        manual: bool,
    },
    /// Save the settings, then check for a newer Superba (a Preferences choice
    /// about updates changed).
    SaveSettingsThenSelfUpdate(Settings),
    /// Skip a version of Superba (`None` clears the skip).
    SelfUpdateSkip(Option<String>),
    /// State of the feeds (`refresh` = fetch now).
    Feeds {
        /// Fetch now.
        refresh: bool,
    },
    /// Models view of a profile (the Overview's speed control).
    ModelsView(superba_core::ModelsViewRequest),
    /// The language the UI shows (the router writes the default stats line in
    /// it; a change re-configures a router that uses the default template).
    UiLocale(superba_core::Locale),
    /// Save model defaults / overrides, then plan `then` when it is set.
    SaveModels {
        /// The edit.
        edit: Box<superba_core::ModelsEdit>,
        /// The switch that applies it (the applied profile), planned by the UI
        /// after the save succeeded.
        then: Option<Box<SwitchRequest>>,
    },
}

/// A question from `execute`, answered by the UI. Dropping it answers nothing; the
/// backend treats a dropped question as cancelled (and `execute` may drop its
/// side first when the situation resolves itself, see [`PendingDecision::is_stale`]).
#[derive(Debug)]
pub struct PendingDecision {
    /// The question.
    pub request: DecisionRequest,
    responder: Option<oneshot::Sender<Decision>>,
}

impl PendingDecision {
    /// Wrap a question (used by tests and the backend).
    pub fn new(request: DecisionRequest, responder: oneshot::Sender<Decision>) -> Self {
        Self {
            request,
            responder: Some(responder),
        }
    }

    /// Answer the question.
    pub fn answer(mut self, decision: Decision) {
        if let Some(tx) = self.responder.take() {
            let _ = tx.send(decision);
        }
    }

    /// True once `execute` stopped waiting for this answer (turns finished on their
    /// own): the dialog should disappear.
    pub fn is_stale(&self) -> bool {
        self.responder.as_ref().is_none_or(|tx| tx.is_closed())
    }
}

/// Results coming back from the backend.
// Each reply is moved once through the channel; boxing the plan would only add
// indirection on the UI side.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Reply {
    /// Overview loaded.
    Overview(Result<Overview>),
    /// Profiles loaded.
    Profiles(Result<ProfileSet>),
    /// Sites loaded.
    Sites(Result<Vec<Site>>),
    /// Line probes finished.
    Lines {
        /// Site.
        site: SiteId,
        /// Result.
        result: Result<Vec<LineCaps>>,
    },
    /// Key check finished.
    KeyChecked {
        /// Site.
        site: SiteId,
        /// Line.
        line: LineId,
        /// Mode.
        mode: AuthMode,
        /// Result.
        result: Result<KeyCheck>,
    },
    /// Sessions loaded.
    Sessions(Result<(SessionReport, Vec<MigrationReceipt>)>),
    /// Diagnostics finished.
    Doctor(Result<Vec<DoctorFinding>>),
    /// A plan is ready.
    Planned {
        /// Sequence number of the request.
        seq: u64,
        /// Result.
        result: Result<SwitchPlan>,
    },
    /// Progress during execution.
    Event(SwitchEvent),
    /// A question during execution.
    Decide(PendingDecision),
    /// Execution finished.
    Executed(Result<SwitchOutcome>),
    /// Settings saved.
    SettingsSaved(Result<()>),
    /// Profile saved.
    ProfileSaved(Result<Profile>),
    /// Compatible site and profile saved.
    SiteAdded(Result<Profile>),
    /// Compatible site and its profiles removed.
    SiteRemoved(Result<()>),
    /// App launched.
    AppOpened(Result<AppSession>),
    /// Recovery finished.
    Recovered(Result<RecoveryReport>),
    /// Router watchdog tick finished.
    RouterWatch(Result<superba_core::RouterWatch>),
    /// Settings diff of a computed plan.
    PlanSettings {
        /// Plan id.
        plan_id: String,
        /// Result.
        result: Result<superba_core::PlanSettings>,
    },
    /// Account data loaded; `usage` tells whether a usage report was requested.
    Account {
        /// A usage report was requested.
        usage: bool,
        /// Result.
        result: Result<superba_core::AccountReport>,
    },
    /// Remote hosts loaded.
    Hosts(Result<Vec<superba_core::HostView>>),
    /// Versions loaded.
    Versions(Result<superba_core::VersionsView>),
    /// The self-update state (after a check or a skip).
    SelfUpdate(Result<superba_core::SelfUpdateView>),
    /// Feed state loaded.
    Feeds(Result<superba_core::FeedsStatus>),
    /// Models view loaded.
    ModelsView(Result<superba_core::ModelsView>),
    /// Model settings saved; `then` is the switch to plan now.
    ModelsSaved {
        /// Result.
        result: Result<superba_core::ModelsEditReport>,
        /// The switch to plan after a successful save.
        then: Option<Box<SwitchRequest>>,
    },
}

/// Wakes the UI thread.
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct Outbox {
    tx: mpsc::Sender<Reply>,
    repaint: Repaint,
}

impl Outbox {
    fn send(&self, reply: Reply) {
        if self.tx.send(reply).is_ok() {
            (self.repaint)();
        }
    }
}

/// Forwards switch events to the UI; `stop` is set by [`Command::StopSessions`].
struct GuiEvents {
    out: Outbox,
    stop: Arc<AtomicBool>,
}

impl SwitchEvents for GuiEvents {
    fn emit(&self, event: SwitchEvent) {
        self.out.send(Reply::Event(event));
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// Asks the UI and awaits the click. Cancel safe: dropping the future drops the
/// receiver, which the UI notices through [`PendingDecision::is_stale`].
struct GuiDecider(Outbox);

#[async_trait]
impl SwitchDecider for GuiDecider {
    async fn decide(&self, request: DecisionRequest) -> Decision {
        let (tx, rx) = oneshot::channel();
        self.0
            .send(Reply::Decide(PendingDecision::new(request.clone(), tx)));
        match rx.await {
            Ok(decision) if request.allowed().contains(&decision) => decision,
            // Window closed or an invalid answer: cancel (the trait's rule).
            _ => Decision::Cancel,
        }
    }
}

/// Handle to the background runtime.
pub struct Backend {
    commands: tmpsc::UnboundedSender<Command>,
    replies: mpsc::Receiver<Reply>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

impl Backend {
    /// Start the runtime thread serving `service`.
    pub fn start(service: Arc<dyn SwitchService>, repaint: Repaint) -> std::io::Result<Self> {
        let (cmd_tx, mut cmd_rx) = tmpsc::unbounded_channel::<Command>();
        let (reply_tx, reply_rx) = mpsc::channel::<Reply>();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("superba-backend")
            .enable_all()
            .build()?;
        let outbox = Outbox {
            tx: reply_tx,
            repaint,
        };
        let thread = std::thread::Builder::new()
            .name("superba-runtime".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let switches = Arc::new(AtomicU64::new(0));
                    // The stop request of the running switch's session migration.
                    let stop = Arc::new(AtomicBool::new(false));
                    while let Some(command) = cmd_rx.recv().await {
                        match &command {
                            // In order with the commands, before any task runs.
                            Command::Execute { .. } => stop.store(false, Ordering::SeqCst),
                            Command::StopSessions => {
                                stop.store(true, Ordering::SeqCst);
                                continue;
                            }
                            _ => {}
                        }
                        let service = service.clone();
                        let out = outbox.clone();
                        let switches = switches.clone();
                        let stop = stop.clone();
                        tokio::spawn(async move {
                            handle(service, out, command, &switches, stop).await
                        });
                    }
                });
                // Dropping the runtime cancels in-flight tasks when the window closes.
                runtime.shutdown_timeout(std::time::Duration::from_millis(200));
            })?;
        Ok(Self {
            commands: cmd_tx,
            replies: reply_rx,
            thread: Some(thread),
        })
    }

    /// Queue a command.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Take every reply that arrived since the last call.
    pub fn drain(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }

    /// Wait up to `timeout` for the next reply (tests).
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<Reply> {
        self.replies.recv_timeout(timeout).ok()
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Closing the command channel ends the runtime loop.
        let (tx, _) = tmpsc::unbounded_channel();
        self.commands = tx;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Run `compute` until no switch finished while it ran (`switches` is bumped when
/// one does). Commands run concurrently: an overview started before a switch may
/// read the old configuration and, finishing after the switch's own refresh,
/// overwrite it (seen in an e2e run: the GUI kept showing the previous line after
/// a successful switch). Results that straddle a switch are recomputed instead.
pub async fn after_switches<T, F, Fut>(switches: &AtomicU64, mut compute: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    loop {
        let seen = switches.load(Ordering::SeqCst);
        let value = compute().await;
        if switches.load(Ordering::SeqCst) == seen {
            return value;
        }
    }
}

async fn handle(
    service: Arc<dyn SwitchService>,
    out: Outbox,
    command: Command,
    switches: &AtomicU64,
    stop: Arc<AtomicBool>,
) {
    match command {
        Command::Refresh => {
            let (o, p, s) = after_switches(switches, || async {
                tokio::join!(service.overview(), service.profile_set(), service.sites())
            })
            .await;
            out.send(Reply::Profiles(p));
            out.send(Reply::Sites(s));
            out.send(Reply::Overview(o));
        }
        Command::Overview => {
            let o = after_switches(switches, || service.overview()).await;
            out.send(Reply::Overview(o));
        }
        Command::ProbeLines(site) => {
            let result = service.probe_lines(&site).await;
            out.send(Reply::Lines { site, result });
        }
        Command::CheckKey {
            site,
            line,
            mode,
            key,
        } => {
            let result = service.check_key(&site, &line, mode, key).await;
            out.send(Reply::KeyChecked {
                site,
                line,
                mode,
                result,
            });
        }
        Command::Sessions => out.send(Reply::Sessions(service.sessions().await)),
        Command::Doctor => out.send(Reply::Doctor(service.doctor().await)),
        Command::Plan { seq, request } => {
            let result = service.plan(request).await;
            let plan_id = result.as_ref().ok().map(|p| p.id.clone());
            out.send(Reply::Planned { seq, result });
            // The settings diff preview of the plan (local and remote rows).
            if let Some(plan_id) = plan_id {
                let result = service.plan_settings(&plan_id).await;
                out.send(Reply::PlanSettings { plan_id, result });
            }
        }
        Command::Execute { plan, options } => {
            let decider: Arc<dyn SwitchDecider> = Arc::new(GuiDecider(out.clone()));
            let events: Arc<dyn SwitchEvents> = Arc::new(GuiEvents {
                out: out.clone(),
                stop,
            });
            let result = service.execute(*plan, options, decider, events).await;
            switches.fetch_add(1, Ordering::SeqCst);
            out.send(Reply::Executed(result));
            // The switch changed the world: refresh what the pages show.
            let (o, p) = tokio::join!(service.overview(), service.profile_set());
            out.send(Reply::Profiles(p));
            out.send(Reply::Overview(o));
        }
        Command::SaveSettings(settings) => {
            out.send(Reply::SettingsSaved(service.save_settings(settings).await));
        }
        Command::SaveProfile { profile, key } => {
            let saved = service.save_profile(*profile, key).await;
            out.send(Reply::Profiles(service.profile_set().await));
            out.send(Reply::ProfileSaved(saved));
        }
        Command::SaveSiteAndProfile { site, profile, key } => {
            let saved = save_site_and_profile(service.as_ref(), *site, *profile, key).await;
            out.send(Reply::Sites(service.sites().await));
            out.send(Reply::Profiles(service.profile_set().await));
            out.send(Reply::ProfileSaved(saved));
        }
        Command::AddCompatibleSite { site, profile, key } => {
            out.send(Reply::SiteAdded(
                add_compatible_site(service.as_ref(), *site, *profile, key).await,
            ));
            out.send(Reply::Sites(service.sites().await));
            out.send(Reply::Profiles(service.profile_set().await));
        }
        Command::RemoveCompatibleSite(id) => {
            out.send(Reply::SiteRemoved(
                remove_compatible_site(service.as_ref(), &id).await,
            ));
            out.send(Reply::Sites(service.sites().await));
            out.send(Reply::Profiles(service.profile_set().await));
        }
        Command::OpenApp => {
            out.send(Reply::AppOpened(service.open_app().await));
            let o = after_switches(switches, || service.overview()).await;
            out.send(Reply::Overview(o));
        }
        Command::Recover => out.send(Reply::Recovered(service.recover().await)),
        Command::RouterWatch => {
            let result = service.router_watch().await;
            let fell_back = matches!(
                &result,
                Ok(w) if matches!(w.state, superba_core::RouterWatchState::FellBack { .. })
            );
            out.send(Reply::RouterWatch(result));
            if fell_back {
                // The fallback switch changed the configuration.
                switches.fetch_add(1, Ordering::SeqCst);
                let (o, p) = tokio::join!(service.overview(), service.profile_set());
                out.send(Reply::Profiles(p));
                out.send(Reply::Overview(o));
            }
        }
        Command::Account(request) => {
            let usage = request.usage.is_some();
            out.send(Reply::Account {
                usage,
                result: service.account(request).await,
            });
        }
        Command::Hosts => out.send(Reply::Hosts(service.hosts().await)),
        Command::Versions { refresh } => {
            out.send(Reply::Versions(service.versions(refresh).await));
        }
        Command::SelfUpdateCheck { manual } => {
            out.send(Reply::SelfUpdate(
                service
                    .self_update_check(superba_core::SelfUpdateCheckRequest { manual })
                    .await,
            ));
        }
        Command::SaveSettingsThenSelfUpdate(settings) => {
            let saved = service.save_settings(settings).await;
            let ok = saved.is_ok();
            out.send(Reply::SettingsSaved(saved));
            if ok {
                out.send(Reply::SelfUpdate(
                    service
                        .self_update_check(superba_core::SelfUpdateCheckRequest { manual: true })
                        .await,
                ));
            }
        }
        Command::SelfUpdateSkip(version) => {
            out.send(Reply::SelfUpdate(service.self_update_skip(version).await));
        }
        Command::Feeds { refresh } => {
            let result = if refresh {
                service.refresh_feeds().await
            } else {
                service.feeds_status().await
            };
            out.send(Reply::Feeds(result));
        }
        Command::ModelsView(request) => {
            out.send(Reply::ModelsView(service.models_view(request).await));
        }
        Command::SaveModels { edit, then } => {
            let result = service.save_models(*edit).await;
            out.send(Reply::Profiles(service.profile_set().await));
            out.send(Reply::ModelsSaved { result, then });
        }
        Command::UiLocale(locale) => {
            if let Err(e) = service.set_ui_locale(locale).await {
                tracing::warn!(error = %e, "the router did not get the stats line language");
            }
        }
        // Handled by the command loop (it sets the stop flag in order).
        Command::StopSessions => {}
    }
}

async fn save_site_and_profile(
    service: &dyn SwitchService,
    site: Site,
    profile: Profile,
    key: Option<Secret>,
) -> Result<Profile> {
    let id = site.id.clone();
    let existed = service.sites().await?.iter().any(|s| s.id == id);
    service.save_site(site).await?;
    let result = service.save_profile(profile, key).await;
    if result.is_err() && !existed {
        let _ = service.delete_site(&id).await;
    }
    result
}

async fn add_compatible_site(
    service: &dyn SwitchService,
    site: Site,
    profile: Profile,
    key: Secret,
) -> Result<Profile> {
    // A stale form must not overwrite an existing site or profile: rollback
    // below is only safe for a site created by this operation.
    let (sites, profiles) = tokio::try_join!(service.sites(), service.profile_set())?;
    if sites
        .iter()
        .any(|s| s.id == site.id || s.id.as_str() == profile.id.as_str())
        || profiles
            .profiles
            .iter()
            .any(|p| p.id == profile.id || p.id.as_str() == site.id.as_str())
    {
        return Err(Error::Conflict {
            message: "site.id_taken".into(),
        });
    }
    let id = site.id.clone();
    service.save_site(site).await?;
    let result = service.save_profile(profile, Some(key)).await;
    if result.is_err() {
        let _ = service.delete_site(&id).await;
    }
    result
}

async fn remove_compatible_site(service: &dyn SwitchService, id: &SiteId) -> Result<()> {
    let (sites, profiles, overview) =
        tokio::try_join!(service.sites(), service.profile_set(), service.overview())?;
    if !sites
        .iter()
        .any(|s| &s.id == id && s.kind == SiteKind::Compatible)
    {
        return Err(Error::NotFound {
            what: id.to_string(),
        });
    }
    let profiles: Vec<_> = profiles
        .profiles
        .iter()
        .filter(|p| &p.site_id == id)
        .collect();
    // Check the entire set before deleting anything, including profiles listed
    // before the active one. last_selected is not the applied profile.
    if overview.active.as_ref().is_some_and(|active| {
        active.site_id.as_ref() == Some(id)
            || profiles
                .iter()
                .any(|p| active.profile_id.as_ref() == Some(&p.id))
    }) {
        return Err(Error::Conflict {
            message: "site.remove_active".into(),
        });
    }
    for profile in profiles {
        service.delete_profile(&profile.id).await?;
    }
    service.delete_site(id).await
}
