//! The eframe application: owns the backend handle, the flow state machine, the
//! loaded data and the page states; draws the shell and routes page actions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Frame, Layout, Panel, ScrollArea, UiBuilder, pos2};
use superba_core::{
    EnvironmentEvent, KeyInput, LineCaps, Locale, MigrationRequest, MigrationSelector,
    OutcomeStatus, ProfileId, RestartKind, Settings, Severity, SiteId, StepStatus, SwitchEvent,
    SwitchIntent, SwitchPhase, SwitchRequest, SwitchService, TargetId, TargetSelection, Theme,
    Timestamp, catalog, codex::PROVIDER_OPENAI,
};

use crate::cadence::{self, Cadence, Due};
use crate::design::backdrop::{self, Backdrop};
use crate::design::fonts::{self, InstalledFonts};
use crate::design::layout;
use crate::design::style;
use crate::design::tokens::{Palette, size};
use crate::design::widgets::{Status, Toast, Toasts};
use crate::flow::{Flow, FlowAttention, FlowNotice, Stage};
use crate::i18n::{Tr, system_locale};
use crate::pages::wizard::{Wizard, WizardOutcome};
use crate::pages::{self, Action, Cx, Data, LogLine, Page, PageLayout, PageStatus, error_text};
use crate::runtime::{Backend, Command, Reply};
use crate::shell::{self, BannerState};

mod tray_glue;

/// Startup options of the app.
#[derive(Clone, Debug, Default)]
pub struct AppOptions {
    /// Forced theme.
    pub theme: Option<egui::Theme>,
    /// Forced language.
    pub locale: Option<Locale>,
    /// Page to open first.
    pub page: Option<Page>,
    /// Started by `superba --launch-chatgpt` without a Superba window to hand
    /// the request to: open ChatGPT through Superba (or plan its reopen) at once.
    pub launch_chatgpt: bool,
    /// Register this window so a second start hands its requests over
    /// ([`crate::instance`]); off in tests.
    pub single_instance: bool,
    /// The tray icon (item 70); off in tests.
    pub tray: crate::tray::TrayOption,
    /// Start with the window hidden in the tray (`--start-in-tray` or the
    /// preference, read before the window opened).
    pub start_in_tray: bool,
    /// `--command CMD`: a page command run on the first page once the
    /// overview has loaded (screenshot and e2e tooling; the command palette
    /// that used to reach these is gone, user request 118).
    pub command: Option<String>,
}

/// Build a [`Cx`] from the app's fields without borrowing the whole app (page
/// states are borrowed mutably next to it).
macro_rules! cx {
    ($app:ident, $pal:expr, $busy:expr) => {
        Cx {
            tr: &$app.tr,
            pal: $pal,
            data: &$app.data,
            busy: $busy,
            now: Timestamp::now(),
            fonts: $app.fonts.report(),
            service: &$app.service,
            log: &$app.log,
        }
    };
}

/// Actions behind toast buttons.
#[derive(Clone, Debug)]
pub enum ToastAction {
    /// Launch the app through Superba.
    OpenApp,
    /// Plan a full restart through Superba.
    FullRestart,
    /// Show the activity log.
    ViewLog,
    /// Plan this request again.
    Retry(Box<SwitchRequest>),
    /// Switch to this provider ("Switch to this provider" after adding one).
    Switch(ProfileId),
    /// Open a file (the self-update log).
    OpenPath(PathBuf),
    /// Open "Codex in effect".
    ShowRuntime,
}

/// How long the "added" toast with its "Switch to this provider" button stays
/// (hovering keeps it open).
const OFFER_TOAST_SECS: f64 = 10.0;

/// A request for `intent` on this computer (remote hosts that follow this
/// computer are added by the planner).
pub fn local_request(intent: SwitchIntent) -> SwitchRequest {
    let mut request = SwitchRequest::profile(ProfileId::new(""));
    request.intent = intent;
    request.targets = TargetSelection {
        local: true,
        remotes: Vec::new(),
    };
    request
}

/// The switch an action plans, if it plans one. These are the only actions the
/// app turns into a plan (and so into a restart prompt): switching, restoring,
/// restarting, migrating, uninstalling and explicit plans. Saving a profile,
/// a site or settings never plans anything by itself.
pub fn planned_request(action: &Action) -> Option<SwitchRequest> {
    let intent = match action {
        Action::Switch {
            profile,
            line,
            auth,
        } => SwitchIntent::Profile {
            profile_id: profile.clone(),
            line_id: line.clone(),
            auth_mode: *auth,
        },
        Action::Restore(target) => SwitchIntent::Restore {
            target: target.clone(),
        },
        Action::Plan(request) => return Some((**request).clone()),
        Action::RestartApp => SwitchIntent::RestartApp { install_id: None },
        Action::MigrateProvider(provider) => SwitchIntent::MigrateSessions {
            request: MigrationRequest {
                to_provider: PROVIDER_OPENAI.into(),
                selectors: vec![MigrationSelector::Provider {
                    provider: provider.clone(),
                }],
                include_archived: true,
                exclude: Vec::new(),
            },
        },
        Action::RollbackMigration(id) => SwitchIntent::RollbackMigration {
            migration_id: id.clone(),
        },
        Action::Uninstall => SwitchIntent::Uninstall {
            delete_backups: false,
            provider: None,
        },
        _ => return None,
    };
    Some(local_request(intent))
}

/// States of the page modules, kept across navigation (see the page module
/// contract in [`crate::pages`]).
#[derive(Debug, Default)]
pub struct PageStates {
    /// Overview (its menu and the dismissed empty state).
    pub overview: pages::overview::State,
    /// Providers.
    pub providers: pages::providers::State,
    /// Models.
    pub models: pages::models::State,
    /// Router (settings) and Usage (request log, statistics): one state.
    pub router: pages::router::State,
    /// Codex settings.
    pub settings: pages::settings::State,
    /// Sessions.
    pub sessions: pages::sessions::State,
    /// Remote hosts.
    pub hosts: pages::hosts::State,
    /// Updates.
    pub updates: pages::superba::updates::State,
    /// Claude Code (both of its pages share one state).
    pub claude: pages::claude::State,
    /// Shared tool configuration.
    pub tools: pages::tools::State,
    /// Superba section navigation and summary.
    pub superba: pages::superba::State,
    /// Notifications.
    pub notify: pages::notify::State,
}

impl PageStates {
    /// What `page` reports to the shell (default for pages without a module state).
    pub fn status(&self, page: Page) -> PageStatus {
        match page {
            Page::Providers => pages::providers::status(&self.providers),
            Page::Models => pages::models::status(&self.models),
            Page::Router => pages::router::status(&self.router),
            Page::Usage => pages::router::usage_status(&self.router),
            Page::Settings => pages::settings::status(&self.settings),
            Page::Sessions => pages::sessions::status(&self.sessions),
            Page::Hosts => pages::hosts::status(&self.hosts),
            Page::ClaudeCode | Page::ClaudeSettings => pages::claude::status(&self.claude, page),
            Page::Overview => pages::overview::status(&self.overview),
            Page::Superba(_) => PageStatus {
                modal: self.notify.modal()
                    || pages::superba::updates::status(&self.updates).modal
                    || self.superba.backups.modal()
                    || self.superba.storage.busy(),
                ..PageStatus::default()
            },
            Page::Tools => pages::tools::status(&self.tools),
        }
    }

    /// A dialog of any page module is open.
    pub fn any_modal(&self) -> bool {
        Page::ALL.iter().any(|p| self.status(*p).modal)
    }

    /// Run a page command (the shell already navigated to `page`).
    pub fn command(&mut self, page: Page, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
        match page {
            Page::Providers => pages::providers::command(&mut self.providers, cx, command, out),
            Page::Models => pages::models::command(&mut self.models, cx, command, out),
            Page::Router => pages::router::command(&mut self.router, cx, command, out),
            Page::Usage => pages::router::usage_command(&mut self.router, cx, command, out),
            Page::Settings => pages::settings::command(&mut self.settings, cx, command, out),
            Page::Sessions => pages::sessions::command(&mut self.sessions, cx, command, out),
            Page::Hosts => pages::hosts::command(&mut self.hosts, cx, command, out),
            Page::ClaudeCode | Page::ClaudeSettings => {
                pages::claude::command(&mut self.claude, page, cx, command, out);
            }
            Page::Overview => pages::overview::command(&mut self.overview, cx, command, out),
            Page::Superba(section) => {
                if let Some(key) = command.strip_prefix("focus:") {
                    if pages::superba::search::ROWS.contains(&(section, key)) {
                        self.superba.focus = Some(key.to_owned());
                    } else if section == pages::SuperbaSection::Updates {
                        pages::superba::updates::command(&mut self.updates, cx, command, out);
                    }
                } else if section == pages::SuperbaSection::Updates {
                    pages::superba::updates::command(&mut self.updates, cx, command, out);
                } else if section == pages::SuperbaSection::Notify {
                    pages::notify::command(&mut self.notify, cx, command);
                }
            }
            Page::Tools => pages::tools::command(&mut self.tools, cx, command),
        }
    }

    /// Draw `page` (header included).
    pub fn show(
        &mut self,
        page: Page,
        ui: &mut egui::Ui,
        cx: &Cx<'_>,
        prefs: &mut Settings,
        out: &mut Vec<Action>,
    ) {
        match page {
            Page::Overview => pages::overview::show(ui, cx, &mut self.overview, out),
            Page::Providers => pages::providers::show(ui, cx, &mut self.providers, out),
            Page::Models => pages::models::show(ui, cx, &mut self.models, out),
            Page::Router => pages::router::show(ui, cx, &mut self.router, out),
            Page::Usage => pages::router::show_usage(ui, cx, &mut self.router, out),
            Page::Settings => pages::settings::show(ui, cx, &mut self.settings, out),
            Page::Sessions => pages::sessions::show(ui, cx, &mut self.sessions, out),
            Page::Hosts => pages::hosts::show(ui, cx, &mut self.hosts, out),
            Page::ClaudeCode | Page::ClaudeSettings => {
                pages::claude::show(ui, cx, &mut self.claude, page, out);
            }
            Page::Tools => pages::tools::show(ui, cx, &mut self.tools, out),
            Page::Superba(section) => pages::superba::show(
                ui,
                cx,
                section,
                &mut self.superba,
                prefs,
                &mut self.notify,
                &mut self.updates,
                out,
            ),
        }
    }
}

/// The application.
pub struct SuperbaApp {
    backend: Backend,
    service: Arc<dyn SwitchService>,
    opts: AppOptions,
    tr: Tr,
    locale: Locale,
    flow: Flow,
    data: Data,
    page: Page,
    pages: PageStates,
    banner: BannerState,
    /// The session migration dialog (scene textures and clock).
    movers: crate::movers::dialog::MoversDialog,
    /// GH#5: the dialog about changes another tool made.
    external: crate::external::ExternalDialog,
    /// "Codex in effect" (GH#4).
    in_effect: crate::in_effect::InEffect,
    risk_memory: Option<PathBuf>,
    wizard: Option<Wizard>,
    wizard_dismissed: bool,
    settings: Settings,
    settings_loaded: bool,
    applied_theme: Option<Option<egui::Theme>>,
    toasts: Toasts<ToastAction>,
    log: Vec<LogLine>,
    fonts: InstalledFonts,
    started: Instant,
    /// The shell's own reads (overview, watchdog, hosts, update check) and the
    /// window events for the environment monitor.
    cadence: Cadence,
    /// Always empty: remote hosts follow switches through their own toggle
    /// (`Settings.hosts`), not a per-switch selection ([`Cx`] keeps the field
    /// for the Phase 1 page tests).
    probed_once: bool,
    entered: bool,
    router_watch_supported: bool,
    router_watch_busy: bool,
    account_loaded_for: Option<(Option<superba_core::ProfileId>, u64)>,
    models_loaded_for: Option<(superba_core::ProfileId, u64)>,
    /// Preferences choices about updates as last sent to the backend.
    selfupdate_prefs: (bool, Option<superba_core::UpdateChannel>),
    had_providers: bool,
    pending_key_save: bool,
    wizard_request: Option<SwitchRequest>,
    /// The router model list the wizard changed, saved after its profile.
    wizard_catalog_mode: Option<superba_core::CatalogMode>,
    /// The last update attempt's problem was announced in this run.
    selfupdate_problem_announced: bool,
    /// A scan for failed Codex turns to notify about (GH#12) is running.
    notify_watch: Option<crate::runtime::ReadTask<superba_core::NotifyReport>>,
    /// Start-up notices wait until the main window shows (not behind the
    /// setup wizard): the elevated-token notice (who runs, the data
    /// directory for its marker) and toasts held back, with whether showing
    /// them acknowledges the self-update problem.
    elevated_notice: Option<(superba_switch::privilege::RunAs, PathBuf)>,
    held_toasts: Vec<Toast<ToastAction>>,
    ack_selfupdate_problem: bool,
    /// This window's registration for requests of other starts (GH#7).
    instance: Option<crate::instance::Listener>,
    /// Requests other starts handed over, not handled yet.
    instance_requests: Arc<std::sync::Mutex<Vec<crate::instance::Request>>>,
    /// The start-up preference "Open ChatGPT through Superba" was looked at.
    startup_launch_checked: bool,
    /// The tray icon (item 70).
    tray: crate::tray::Tray,
    /// The window's life: shown, hidden in the tray, asking, quitting.
    life: crate::tray::WindowLife,
    /// "Start in the tray" was looked at.
    start_hidden_checked: bool,
}

impl std::fmt::Debug for SuperbaApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuperbaApp")
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl SuperbaApp {
    /// Create the app: install fonts and styles, start the backend and request the
    /// initial data.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        service: Arc<dyn SwitchService>,
        opts: AppOptions,
    ) -> std::io::Result<Self> {
        let ctx = cc.egui_ctx.clone();
        let fonts = fonts::install(&ctx);
        style::install(&ctx, opts.theme);
        // The window frame without its title bar (Windows: the caption goes).
        crate::chrome::install(cc);
        let repaint_ctx = ctx.clone();
        let backend = Backend::start(
            service.clone(),
            Arc::new(move || repaint_ctx.request_repaint()),
        )?;
        let locale = opts.locale.unwrap_or_else(system_locale);
        // eframe persistence is optional in this build. Keep this one UI preference
        // in Superba's data directory too.
        let risk_memory = superba_switch::detect_context()
            .ok()
            .map(|context| context.data_dir.root().join("gui-hot-risk-seen-v1"));
        if risk_memory.as_ref().is_some_and(|p| p.is_file()) {
            BannerState::restore_risk_memory(&ctx, true);
        }
        let mut app = Self {
            backend,
            service,
            tr: Tr::new(locale),
            locale,
            flow: Flow::new(Settings::default().countdown_secs),
            data: Data::default(),
            page: opts.page.unwrap_or(Page::Overview),
            pages: PageStates::default(),
            banner: BannerState::default(),
            movers: Default::default(),
            external: Default::default(),
            in_effect: Default::default(),
            risk_memory,
            wizard: None,
            wizard_dismissed: false,
            settings: Settings::default(),
            settings_loaded: false,
            applied_theme: None,
            toasts: Toasts::default(),
            log: Vec::new(),
            fonts,
            started: Instant::now(),
            cadence: Cadence::new(Instant::now()),
            probed_once: false,
            entered: false,
            router_watch_supported: true,
            router_watch_busy: false,
            account_loaded_for: None,
            models_loaded_for: None,
            selfupdate_prefs: (true, None),
            had_providers: false,
            pending_key_save: false,
            wizard_request: None,
            wizard_catalog_mode: None,
            selfupdate_problem_announced: false,
            notify_watch: None,
            elevated_notice: None,
            held_toasts: Vec::new(),
            ack_selfupdate_problem: false,
            instance: None,
            instance_requests: Arc::default(),
            startup_launch_checked: false,
            tray: crate::tray::Tray::disabled(),
            life: crate::tray::WindowLife::new(),
            start_hidden_checked: false,
            opts,
        };
        app.data.sites = catalog::builtin_sites();
        app.data.paths = superba_switch::detect_context()
            .ok()
            .map(|context| pages::DataPaths {
                router_db: superba_router::RouterPaths::new(&context.data_dir).usage_db(),
                data_dir: context.data_dir.root().to_path_buf(),
            });
        // An elevated token of the signed-in user is fine (user bug 44): say so
        // once, when the main window shows.
        if let Ok(context) = superba_switch::detect_context() {
            let run_as = superba_switch::privilege::current(context.env.as_ref());
            app.elevated_notice = Some((run_as, context.data_dir.root().to_path_buf()));
        }
        app.backend.send(Command::Recover);
        app.data.overview.start();
        app.data.profiles.start();
        app.backend.send(Command::Refresh);
        if app.opts.single_instance {
            app.listen_for_other_starts(&ctx);
        }
        app.start_tray(&ctx);
        if app.opts.launch_chatgpt {
            // The start-up preference has nothing more to do.
            app.startup_launch_checked = true;
            app.launch_app(crate::launch::LaunchOrigin::Shortcut);
        }
        Ok(app)
    }

    /// Take requests of later starts of `superba` (GH#7: `--launch-chatgpt`).
    fn listen_for_other_starts(&mut self, ctx: &egui::Context) {
        let Some(dir) = superba_switch::detect_context()
            .ok()
            .map(|c| c.data_dir.anchor().to_path_buf())
        else {
            return;
        };
        let queue = Arc::clone(&self.instance_requests);
        let repaint = ctx.clone();
        #[cfg(target_os = "macos")]
        crate::macos::install_reopen(Arc::clone(&queue), repaint.clone());
        match crate::instance::listen(&dir, move |request| {
            queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(request);
            repaint.request_repaint();
        }) {
            Ok(listener) => self.instance = Some(listener),
            Err(e) => {
                tracing::warn!(error = %e, "other starts of Superba cannot reach this window")
            }
        }
    }

    /// Handle what later starts handed over ([`crate::instance::Request`]).
    fn handle_instance_requests(&mut self) {
        let requests = std::mem::take(
            &mut *self
                .instance_requests
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        for request in requests {
            match request {
                crate::instance::Request::LaunchChatGpt => {
                    self.launch_app(crate::launch::LaunchOrigin::Shortcut);
                }
                crate::instance::Request::Show => {
                    #[cfg(target_os = "macos")]
                    {
                        use superba_config::autostart::LoginStartup;
                        if let Ok(ctx) = superba_switch::detect_context()
                            && let Err(e) = superba_config::autostart::Manager::new(&ctx).refresh()
                        {
                            tracing::warn!(error = %e, "could not refresh login startup on reopen");
                        }
                    }
                    self.life.show();
                }
                crate::instance::Request::Autostart => {}
            }
        }
    }

    /// Open ChatGPT through Superba, bring it forward, or plan its reopen
    /// ([`crate::launch`]); the result comes back as [`Reply::AppLaunched`].
    /// The entry point of every "Open ChatGPT with Superba" (the hero, the
    /// palette, toasts, other starts, the start-up preference).
    pub(crate) fn launch_app(&mut self, origin: crate::launch::LaunchOrigin) {
        self.data.launching_app = true;
        self.backend.send(Command::LaunchApp(origin));
    }

    /// Once the settings and the first overview are known: the preference
    /// "Open ChatGPT through Superba when Superba starts".
    fn maybe_launch_on_start(&mut self) {
        if self.startup_launch_checked
            || !self.settings_loaded
            || self.data.overview.value.is_none()
        {
            return;
        }
        self.startup_launch_checked = true;
        if self.settings.open_app_on_start {
            self.launch_app(crate::launch::LaunchOrigin::Startup);
        }
    }

    /// What [`Command::LaunchApp`] did.
    fn on_app_launched(
        &mut self,
        ctx: &egui::Context,
        origin: crate::launch::LaunchOrigin,
        result: superba_core::Result<crate::launch::LaunchResult>,
    ) {
        use crate::launch::{LaunchOrigin, LaunchResult};
        self.data.launching_app = false;
        match result {
            Ok(LaunchResult::Opened) => self
                .toasts
                .push(Toast::new(Status::Ok, self.tr.t("toast.app_opened"))),
            Ok(LaunchResult::Activated) if origin == LaunchOrigin::Window => self.toasts.push(
                Toast::new(Status::Idle, self.tr.t("toast.app_already_open")),
            ),
            Ok(LaunchResult::Activated | LaunchResult::Nothing) => {}
            Ok(LaunchResult::NeedsReopen) => {
                // A full restart, planned and confirmed (running turns waited for).
                self.request(SwitchIntent::RestartApp { install_id: None });
                if origin.from_outside() {
                    self.show_window(ctx);
                }
            }
            Err(e) => {
                self.error_toast(self.tr.t("toast.app_open_failed"), &e);
                if origin.from_outside() {
                    self.show_window(ctx);
                }
            }
        }
    }

    /// The applied state the account card was loaded for: reload after a switch
    /// (another key or line) or a refresh.
    fn account_stamp(&self) -> Option<(Option<superba_core::ProfileId>, u64)> {
        let o = self.data.overview.value.as_ref()?;
        let a = o.active.as_ref();
        Some((
            a.and_then(|a| a.profile_id.clone()),
            a.map_or(0, |a| a.generation),
        ))
    }

    fn maybe_load_account(&mut self) {
        let stamp = self.account_stamp();
        if stamp.is_some() && stamp != self.account_loaded_for && !self.data.account.loading {
            self.account_loaded_for = stamp;
            self.load_account(false);
        }
    }

    /// Load the models view of the applied provider (the Overview's speed
    /// control) whenever another profile or generation is applied.
    fn maybe_load_models(&mut self) {
        let stamp = self
            .data
            .active_profile()
            .map(|p| p.id.clone())
            .zip(self.data.active_state().map(|a| a.generation));
        if stamp.is_some() && stamp != self.models_loaded_for && !self.data.active_models.loading {
            self.models_loaded_for = stamp.clone();
            self.load_models(stamp.map(|(id, _)| id));
        }
    }

    fn load_models(&mut self, profile: Option<superba_core::ProfileId>) {
        self.data.active_models.start();
        self.backend
            .send(Command::ModelsView(superba_core::ModelsViewRequest {
                profile_id: profile,
                refresh: false,
            }));
    }

    /// The first overview arrived: load what the sidebar badges and the
    /// Overview's columns need.
    fn load_shell_data(&mut self) {
        self.data.hosts.start();
        self.backend.send(Command::Hosts);
        self.data.versions.start();
        self.backend.send(Command::Versions { refresh: false });
        self.data.feeds.start();
        self.backend.send(Command::Feeds { refresh: false });
        self.check_self_update(false);
    }

    /// Ask the backend for Superba's own update state. An automatic check does
    /// nothing while the preference is off or the answer is fresh.
    fn check_self_update(&mut self, manual: bool) {
        self.cadence.selfupdate_checked(Instant::now());
        self.data.selfupdate.start();
        self.backend.send(Command::SelfUpdateCheck { manual });
    }

    fn load_account(&mut self, refresh: bool) {
        let _ = refresh;
        self.data.account.start();
        self.backend
            .send(Command::Account(superba_core::AccountRequest::default()));
    }

    /// Surface a router watchdog tick: a toast on each change of state, the
    /// fallback to direct mode as a sticky warning.
    fn on_router_watch(&mut self, result: superba_core::Result<superba_core::RouterWatch>) {
        use superba_core::RouterWatchState as W;
        self.router_watch_busy = false;
        let watch = match result {
            Ok(w) => w,
            Err(superba_core::Error::NotImplemented { .. }) => {
                self.router_watch_supported = false;
                return;
            }
            Err(e) => {
                let text = error_text(&self.tr, &e);
                self.log(Status::Warn, text);
                return;
            }
        };
        let previous = self.data.router_watch.clone();
        // A fallback that needed a full restart only wrote the configuration: the
        // running app still waits on the dead router until it is restarted.
        let written_only = watch
            .outcome
            .as_ref()
            .is_some_and(|o| o.status == OutcomeStatus::WrittenOnly);
        if let Some((status, key)) = router_watch_notice(previous.as_ref(), &watch.state) {
            let detail = match &watch.state {
                W::Suspect { detail } | W::FellBack { detail } => detail.clone(),
                _ => String::new(),
            };
            let key = if written_only && matches!(watch.state, W::FellBack { .. }) {
                "router.watch.fell_back_written"
            } else {
                key
            };
            let text = match &watch.state {
                W::Moved { from, to } => self.tr.f(key, &[("from", from), ("to", to)]),
                _ => self.tr.f(key, &[("detail", &detail)]),
            };
            self.log(status, text.clone());
            let toast = if matches!(watch.state, W::FellBack { .. }) {
                let mut t = Toast::new(status, self.tr.t("router.watch.fell_back_title"))
                    .body(text)
                    .sticky();
                if written_only {
                    t = t.action(
                        self.tr.t("action.full_restart_app"),
                        ToastAction::FullRestart,
                    );
                }
                t
            } else {
                Toast::new(status, text)
            };
            self.toasts.push(toast);
        }
        if let Some(outcome) = &watch.outcome {
            for n in outcome
                .notices
                .iter()
                .filter(|n| n.severity != Severity::Info)
            {
                let text = pages::localize_text(&self.tr, &self.data, &n.text);
                self.log(Status::Warn, text);
            }
        }
        if let Some(e) = &watch.error {
            let text = error_text(&self.tr, e);
            self.log(Status::Error, text);
        }
        // Claude Code moved back to its provider (or followed the router):
        // said once, as a sticky toast for a fallback.
        for n in &watch.notes {
            let text = pages::localize_text(&self.tr, &self.data, &n.text);
            let status = if n.severity == Severity::Info {
                Status::Ok
            } else {
                Status::Warn
            };
            self.log(status, text.clone());
            let toast = Toast::new(status, text);
            self.toasts.push(if n.severity == Severity::Info {
                toast
            } else {
                toast.sticky()
            });
        }
        self.data.router_watch = Some(watch.state);
    }

    /// The router watchdog has work: router mode on and no switch planned or
    /// running (one tick every 5 s, also while the window is hidden).
    fn router_watch_wanted(&self) -> bool {
        self.router_watch_supported
            && !self.router_watch_busy
            && (self.data.router_enabled() || self.data.claude_routed())
            && !self.flow.is_running()
            && !self.flow.is_planning()
    }

    fn cadence_inputs(&self, ctx: &egui::Context, visible: bool) -> cadence::Inputs {
        cadence::Inputs {
            visible,
            focused: ctx.input(|i| i.focused),
            entered: self.entered,
            switching: self.flow.is_running(),
            router_watch: self.router_watch_wanted(),
            overview_loading: self.data.overview.loading,
            hosts_loading: self.data.hosts.loading,
            selfupdate_loading: self.data.selfupdate.loading,
            notify_watch: self.notify_watch.is_none()
                && pages::notify::watch_wanted(&self.settings),
            environment_generation: self.service.environment_generation(),
        }
    }

    /// Run what the shell's cadence says is due, tell the environment monitor
    /// about the window, and sleep until the next timer (user request 34).
    fn run_cadence(&mut self, ctx: &egui::Context, visible: bool) {
        if self.pages.superba.storage.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        if !self.data.router_enabled() && !self.data.claude_routed() {
            self.data.router_watch = None;
        }
        let now = Instant::now();
        if self
            .notify_watch
            .as_ref()
            .is_some_and(|t| t.poll().is_some())
        {
            self.notify_watch = None;
        }
        let inputs = self.cadence_inputs(ctx, visible);
        for due in self.cadence.tick(now, inputs) {
            match due {
                Due::Environment(event) => self.service.environment_event(event),
                Due::Overview => self.backend.send(Command::Overview),
                Due::RouterWatch => {
                    self.router_watch_busy = true;
                    self.backend.send(Command::RouterWatch);
                }
                Due::Hosts => self.handle_action(ctx, Action::LoadHosts),
                Due::SelfUpdate => self.check_self_update(false),
                Due::NotifyWatch => {
                    let service = self.service.clone();
                    self.notify_watch =
                        Some(crate::runtime::task(
                            async move { service.notify_watch().await },
                        ));
                }
                Due::EnvironmentChanged => {
                    // Installs or Codex versions changed: the views built from them.
                    if self.data.versions.value.is_some() && !self.data.versions.loading {
                        self.data.versions.start();
                        self.backend.send(Command::Versions { refresh: false });
                    }
                    self.models_loaded_for = None;
                }
            }
        }
        let inputs = self.cadence_inputs(ctx, visible);
        if let Some(wait) = self.cadence.next_wake(now, inputs) {
            ctx.request_repaint_after(wait.max(Duration::from_millis(100)));
        }
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn log(&mut self, status: Status, text: String) {
        self.log.push(LogLine {
            at: Timestamp::now(),
            status,
            text,
        });
        if self.log.len() > 500 {
            self.log.remove(0);
        }
    }

    fn set_locale(&mut self, locale: Option<Locale>) {
        let l = self.opts.locale.or(locale).unwrap_or_else(system_locale);
        if l != self.locale {
            self.locale = l;
            self.tr = Tr::new(l);
        }
        // The router writes the default stats line in the language shown here
        // (the service ignores a repeat).
        self.backend.send(Command::UiLocale(l));
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        self.flow.set_countdown_secs(self.settings.countdown_secs);
        self.set_locale(self.settings.locale);
        let theme = self.opts.theme.or(match self.settings.theme {
            Theme::Dark => Some(egui::Theme::Dark),
            Theme::Light => Some(egui::Theme::Light),
            _ => None,
        });
        if self.applied_theme != Some(theme) {
            style::install(ctx, theme);
            self.applied_theme = Some(theme);
        }
        backdrop::set(ctx, self.settings.background);
    }

    // -------------------------------------------------------------------
    // Replies
    // -------------------------------------------------------------------

    fn handle_replies(&mut self, ctx: &egui::Context) {
        let mut actions = Vec::new();
        self.pages
            .superba
            .storage
            .poll(&mut self.settings, &mut actions);
        for action in actions {
            self.handle_action(ctx, action);
        }
        for reply in self.backend.drain() {
            let now = self.now();
            match reply {
                Reply::Overview(r) => {
                    self.pages
                        .superba
                        .storage
                        .check_location(self.service.clone());
                    if let Err(e) = &r
                        && self.data.overview.value.is_none()
                    {
                        let text = error_text(&self.tr, e);
                        self.log(Status::Error, text);
                    }
                    self.data.overview.finish(r);
                    self.maybe_load_account();
                    self.maybe_load_models();
                    self.maybe_launch_on_start();
                    // Page-entry loads need the applied site, known only now.
                    if !self.entered && self.data.overview.value.is_some() {
                        self.entered = true;
                        self.load_shell_data();
                        let page = self.page;
                        self.go(page);
                    }
                    // Refresh delivers profiles and sites before the overview.
                    if self.settings_loaded
                        && self.settings.auto_probe
                        && !self.probed_once
                        && let Some(site) = self.data.lines_site_id()
                    {
                        self.probe(site);
                    }
                    self.maybe_open_wizard();
                }
                Reply::Profiles(r) => {
                    if let Ok(set) = &r
                        && !self.settings_loaded
                    {
                        self.selfupdate_prefs =
                            (set.settings.auto_update_check, set.settings.update_channel);
                        self.settings = set.settings.clone();
                        self.settings_loaded = true;
                        self.apply_settings(ctx);
                    }
                    self.data.profiles.finish(r);
                    self.maybe_launch_on_start();
                    self.maybe_open_wizard();
                }
                Reply::Sites(r) => {
                    if let Ok(sites) = r
                        && !sites.is_empty()
                    {
                        self.data.sites = sites;
                    }
                }
                Reply::Lines { site, result } => {
                    match &result {
                        Ok(caps) => {
                            let (status, text) = probe_log(&self.tr, &self.data, &site, caps);
                            self.log(status, text);
                        }
                        Err(e) => {
                            let text = error_text(&self.tr, e);
                            self.log(Status::Error, text);
                        }
                    }
                    self.data.caps.entry(site).or_default().finish(result);
                }
                Reply::KeyChecked {
                    line, mode, result, ..
                } => {
                    self.data.key_checking.remove(&(line.clone(), mode));
                    self.data.key_checks.insert((line, mode), result);
                }
                Reply::Sessions(r) => self.data.sessions.finish(r),
                Reply::Doctor(r) => self.data.doctor.finish(r),
                Reply::ClaudeDoctor(r) => self.data.claude_diag.finish(r),
                Reply::Planned { seq, result } => self.flow.on_planned(seq, result, now),
                Reply::PlanEvent { seq, event } => self.flow.on_plan_event(seq, &event, now),
                Reply::Event(ev) => self.on_event(ev),
                Reply::Decide(d) => self.flow.on_decision(d, now),
                Reply::Executed(r) => self.flow.on_executed(r, now),
                Reply::AutostartSet(result) => {
                    match result {
                        Ok(status) => self.settings.autostart = status.enabled,
                        Err(e) => self.error_toast(self.tr.t("toast.settings_failed"), &e),
                    }
                    self.backend.send(Command::Refresh);
                }
                Reply::SettingsSaved(r) => {
                    if let Err(e) = r {
                        self.error_toast(self.tr.t("toast.settings_failed"), &e);
                    }
                }
                Reply::ProfileSaved(r) => match r {
                    Ok(_) if self.wizard_request.is_some() => {
                        if let Some(mode) = self.wizard_catalog_mode.take() {
                            self.backend.send(Command::SaveRouterCatalogMode(mode));
                        }
                        if let Some(request) = self.wizard_request.take() {
                            self.flow.request(request, now);
                        }
                    }
                    Ok(_) if self.pending_key_save => {
                        self.pending_key_save = false;
                        self.toasts
                            .push(Toast::new(Status::Ok, self.tr.t("toast.key_saved")));
                        self.backend.send(Command::Overview);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.pending_key_save = false;
                        self.wizard_request = None;
                        self.wizard_catalog_mode = None;
                        self.error_toast(self.tr.t("toast.profile_failed"), &e);
                    }
                },
                Reply::SiteAdded(result) => match result {
                    Ok(profile) => {
                        let text = self.tr.f("site.added", &[("name", &profile.name)]);
                        self.log(Status::Ok, text.clone());
                        self.toasts.push(Toast::new(Status::Ok, text));
                    }
                    Err(e) => self.log(Status::Error, error_text(&self.tr, &e)),
                },
                Reply::SiteRemoved(result) => match result {
                    Ok(()) => {
                        let text = self.tr.t("site.removed");
                        self.log(Status::Ok, text.clone());
                        self.toasts.push(Toast::new(Status::Ok, text));
                    }
                    Err(e) => self.log(Status::Error, error_text(&self.tr, &e)),
                },
                Reply::AppLaunched { origin, result } => self.on_app_launched(ctx, origin, result),
                Reply::RouterWatch(result) => self.on_router_watch(result),
                Reply::PlanSettings { plan_id, result } => match result {
                    Ok(settings) => self.data.plan_settings = Some((plan_id, settings)),
                    Err(superba_core::Error::NotImplemented { .. }) => {}
                    Err(e) => tracing::debug!(error = %e, "no settings preview for the plan"),
                },
                // Usage reports are loaded by the Providers page itself.
                Reply::Account {
                    usage: false,
                    result,
                } => self.data.account.finish(result),
                Reply::Account { .. } => {}
                Reply::Hosts(r) => self.data.hosts.finish(r),
                Reply::Versions(r) => {
                    if let Err(e) = &r
                        && !e.is_not_implemented()
                    {
                        let text = error_text(&self.tr, e);
                        self.log(Status::Warn, text);
                    }
                    if r.is_ok() {
                        // Refresh the inventory after a shell check or an external
                        // install; its local cache also drives both update badges.
                        self.pages.updates.view.invalidate();
                    }
                    self.data.versions.finish(r);
                }
                Reply::SelfUpdate(r) => {
                    // Never a toast: an unreachable release page is a quiet state.
                    if let Err(e) = &r
                        && !e.is_not_implemented()
                    {
                        tracing::debug!(error = %e, "self-update check failed");
                    }
                    if let Ok(view) = &r {
                        self.announce_selfupdate_problem(view);
                    }
                    self.data.selfupdate.finish(r);
                }
                Reply::Feeds(r) => self.data.feeds.finish(r),
                Reply::ModelsView(r) => self.data.active_models.finish(r),
                Reply::ModelsSaved { result, then } => match result {
                    Ok(report) => {
                        for n in report.notes.iter().filter(|n| n.severity != Severity::Info) {
                            let text = pages::localize_text(&self.tr, &self.data, &n.text);
                            self.log(Status::Warn, text.clone());
                            self.toasts.push(Toast::new(Status::Warn, text));
                        }
                        // The view changed (speed, defaults): reload it.
                        self.models_loaded_for = None;
                        if let Some(request) = then {
                            let now = self.now();
                            self.flow.request(*request, now);
                        }
                        self.backend.send(Command::Overview);
                    }
                    Err(e) => self.error_toast(self.tr.t("toast.models_failed"), &e),
                },
                Reply::Recovered(r) => match r {
                    Ok(report) => {
                        if report.entries.is_empty() {
                            let text = self.tr.t("log.recovered_nothing");
                            self.log(Status::Ok, text);
                        } else {
                            let text = self
                                .tr
                                .f("toast.recovered", &[("n", &report.entries.len())]);
                            self.log(Status::Warn, text.clone());
                            self.toasts.push(Toast::new(Status::Warn, text));
                        }
                        // Start-up repairs (a model catalog file a changed Codex
                        // refused was rebuilt or dropped).
                        for n in &report.notices {
                            let status = match n.severity {
                                Severity::Info => Status::Ok,
                                Severity::Warning => Status::Warn,
                                _ => Status::Error,
                            };
                            let text = pages::localize_text(&self.tr, &self.data, &n.text);
                            self.log(status, text.clone());
                            self.toasts.push(Toast::new(status, text));
                        }
                    }
                    Err(e) => {
                        let text = error_text(&self.tr, &e);
                        self.log(Status::Idle, text);
                    }
                },
            }
            // Observe each reply, so a phase and its decision arriving together
            // request attention only once, without losing intervening transitions.
            self.request_attention(ctx);
        }
    }

    fn request_attention(&mut self, ctx: &egui::Context) {
        if let Some(attention) = self.flow.take_attention() {
            // A question while hidden in the tray: the window comes back.
            if self.life.is_hidden() {
                self.show_window(ctx);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Critical,
            ));
            if attention == FlowAttention::Recovery {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
    }

    fn on_event(&mut self, ev: SwitchEvent) {
        let now = self.now();
        self.flow.on_event(&ev, now);
        match &ev {
            SwitchEvent::Phase { phase, .. } => {
                // The end of a run with a step that failed (a relaunch that did
                // not happen, a failed check) is not "Done" (user bug 40).
                let failed = *phase == SwitchPhase::Done
                    && self.flow.run().is_some_and(|run| {
                        run.steps
                            .iter()
                            .any(|(_, status)| matches!(status, StepStatus::Failed { .. }))
                    });
                let key = if failed {
                    "phase.done_failed"
                } else {
                    phase_key(*phase)
                };
                let text = self.tr.f("log.phase", &[("phase", &self.tr.t(key))]);
                self.log(if failed { Status::Warn } else { Status::Idle }, text);
            }
            SwitchEvent::Step {
                step,
                status: StepStatus::Failed { message },
                ..
            } => {
                let text = format!("{step:?}: {message}");
                self.log(Status::Error, text);
            }
            SwitchEvent::Log { level, text } => {
                let status = match level {
                    Severity::Info => Status::Ok,
                    Severity::Warning => Status::Warn,
                    Severity::Danger => Status::Error,
                };
                let t = self.tr.text(text);
                self.log(status, t);
            }
            _ => {}
        }
    }

    fn error_toast(&mut self, title: String, e: &superba_core::Error) {
        let body = error_text(&self.tr, e);
        self.log(Status::Error, format!("{title}: {body}"));
        self.toasts
            .push(Toast::new(Status::Error, title).body(body));
    }

    /// Open the setup wizard on its own (UX §3.2 entry point a): on the first
    /// run (no baseline and no key-based provider), and again when every
    /// key-based provider was deleted while Superba runs. Every other entry point
    /// sends [`Action::StartWizard`].
    fn maybe_open_wizard(&mut self) {
        let (Some(o), Some(p)) = (&self.data.overview.value, &self.data.profiles.value) else {
            return;
        };
        let has_providers = p.profiles.iter().any(|p| p.builtin.is_none());
        let all_deleted = self.had_providers && !has_providers;
        self.had_providers = has_providers;
        if self.wizard.is_some()
            || ((self.wizard_dismissed || p.settings.wizard_dismissed) && !all_deleted)
        {
            return;
        }
        let first_run = !o.baseline_captured && !has_providers;
        if first_run || all_deleted {
            self.wizard = Some(Wizard::start(&self.data));
        }
    }

    fn process_flow_notices(&mut self, ctx: &egui::Context) {
        for notice in self.flow.take_notices() {
            match notice {
                FlowNotice::PlanFailed(e) => {
                    // A switch started from the tray failed: say so on screen.
                    self.show_window(ctx);
                    self.error_toast(self.tr.t("toast.plan_failed"), &e);
                }
                FlowNotice::ExecuteFailed { plan, error } => {
                    self.show_window(ctx);
                    let key = if shell::is_reapply(&plan) {
                        "banner.reapply.failed"
                    } else {
                        "toast.switch_failed"
                    };
                    let title = self.tr.f(
                        key,
                        &[("dest", &pages::dest_text(&self.tr, &self.data, &plan.to))],
                    );
                    let body = error_text(&self.tr, &error);
                    self.log(Status::Error, format!("{title}: {body}"));
                    self.toasts.push(
                        Toast::new(Status::Error, title)
                            .body(body)
                            .action(self.tr.t("action.view_log"), ToastAction::ViewLog)
                            .sticky(),
                    );
                }
                FlowNotice::Finished { plan, outcome } => {
                    // A switch changes what runs and what is pending on the
                    // WSL distributions (user bug 68).
                    self.pages.hosts.after_switch();
                    if matches!(
                        outcome.status,
                        OutcomeStatus::Switched | OutcomeStatus::Partial
                    ) && self.banner.complete_switch(ctx, &plan.id)
                        && let Some(path) = &self.risk_memory
                        && let Err(error) = remember_risks(path)
                    {
                        tracing::warn!(%error, "could not save hot-restart disclosure preference");
                    }
                    // What was applied: a switch names its line or destination,
                    // a re-apply of the provider in use, router mode or a host
                    // sync say so (user bug 25).
                    let applied =
                        shell::applied(&self.tr, &self.data, &plan, &|id| self.remote_label(id));
                    match outcome.status {
                        OutcomeStatus::Switched => {
                            // Verification events arrive after the crash watch and
                            // all checks. They cannot measure core readiness alone.
                            self.log(Status::Ok, applied.log_text(&self.tr, outcome.elapsed_ms));
                            self.toasts
                                .push(Toast::new(Status::Ok, applied.title(&self.tr)));
                            if let Some(t) = &outcome.returned_to {
                                let text = self.tr.f(
                                    "toast.returned",
                                    &[(
                                        "title",
                                        &t.title.clone().unwrap_or_else(|| t.thread_id.clone()),
                                    )],
                                );
                                self.toasts.push(Toast::new(Status::Idle, text));
                            }
                        }
                        OutcomeStatus::Partial => {
                            let failed: Vec<&superba_core::RemoteOutcome> = outcome
                                .remotes
                                .iter()
                                .filter(|r| r.status != OutcomeStatus::Switched)
                                .collect();
                            let hosts: Vec<String> = failed
                                .iter()
                                .map(|r| self.remote_label(&r.target_id))
                                .collect();
                            let reasons: Vec<String> = failed
                                .iter()
                                .filter_map(|r| r.error.as_ref().map(|e| error_text(&self.tr, e)))
                                .collect();
                            let title = self.tr.f("toast.partial", &[("hosts", &hosts.join(", "))]);
                            self.log(Status::Warn, format!("{title} {}", reasons.join("; ")));
                            self.toasts.push(
                                Toast::new(Status::Warn, title)
                                    .body(reasons.join("; "))
                                    .action(
                                        self.tr.t("action.retry"),
                                        ToastAction::Retry(Box::new(plan.request.clone())),
                                    )
                                    .action(self.tr.t("action.details"), ToastAction::ViewLog)
                                    .sticky(),
                            );
                        }
                        OutcomeStatus::WrittenOnly
                            if outcome
                                .notices
                                .iter()
                                .any(|n| n.severity == Severity::Danger) =>
                        {
                            // Written, but something the user expects did not
                            // happen (ChatGPT did not open again): an error toast
                            // with the reason, not the green "written" one.
                            let reason = outcome
                                .notices
                                .iter()
                                .find(|n| n.severity == Severity::Danger)
                                .map(|n| pages::localize_text(&self.tr, &self.data, &n.text))
                                .unwrap_or_default();
                            let next = outcome
                                .notices
                                .iter()
                                .find(|n| n.text.key == "switch.done.launch_failed")
                                .map(|n| pages::localize_text(&self.tr, &self.data, &n.text));
                            let body = match &next {
                                Some(next) => format!("{reason}\n{next}"),
                                None => reason.clone(),
                            };
                            self.log(Status::Error, body.replace('\n', " "));
                            self.toasts.push(
                                Toast::new(Status::Error, self.tr.t("toast.written_launch_failed"))
                                    .body(body)
                                    .action(self.tr.t("action.open_app"), ToastAction::OpenApp)
                                    .action(self.tr.t("action.view_log"), ToastAction::ViewLog)
                                    .sticky(),
                            );
                        }
                        OutcomeStatus::WrittenOnly
                            if outcome
                                .notices
                                .iter()
                                .any(|n| n.text.key == "switch.done.reopened_unverified") =>
                        {
                            // Superba reopened ChatGPT, but the check of its new
                            // Codex core failed (user bug 69b): not "restart it
                            // yourself".
                            let notice = |key: &str| {
                                outcome
                                    .notices
                                    .iter()
                                    .find(|n| n.text.key == key)
                                    .map(|n| pages::localize_text(&self.tr, &self.data, &n.text))
                                    .unwrap_or_default()
                            };
                            let detail = notice("switch.question.verification_failed");
                            let next = notice("switch.done.reopened_unverified");
                            let body = if detail.is_empty() {
                                next
                            } else {
                                format!("{detail}\n{next}")
                            };
                            self.log(Status::Warn, body.replace('\n', " "));
                            self.toasts.push(
                                Toast::new(Status::Warn, self.tr.t("toast.reopened_unverified"))
                                    .body(body)
                                    .action(
                                        self.tr.t("action.full_restart_app"),
                                        ToastAction::FullRestart,
                                    )
                                    .action(self.tr.t("action.view_log"), ToastAction::ViewLog)
                                    .sticky(),
                            );
                        }
                        OutcomeStatus::WrittenOnly => {
                            if plan.primary == RestartKind::None {
                                self.toasts.push(
                                    Toast::new(Status::Ok, self.tr.t("toast.written_not_running"))
                                        .action(self.tr.t("action.open_app"), ToastAction::OpenApp)
                                        .sticky(),
                                );
                            } else {
                                self.toasts
                                    .push(Toast::new(Status::Ok, self.tr.t("toast.written_only")));
                            }
                        }
                        OutcomeStatus::Cancelled => {
                            self.toasts
                                .push(Toast::new(Status::Idle, self.tr.t("toast.cancelled")));
                        }
                        OutcomeStatus::RolledBack | OutcomeStatus::Failed => {
                            let reason = outcome
                                .notices
                                .iter()
                                .find(|n| n.severity == Severity::Danger)
                                .or(outcome.notices.first())
                                .map(|n| pages::localize_text(&self.tr, &self.data, &n.text));
                            let title = if outcome.status == OutcomeStatus::RolledBack {
                                self.tr.t("toast.rolled_back")
                            } else {
                                self.tr.t("toast.failed")
                            };
                            self.log(
                                Status::Error,
                                reason.clone().unwrap_or_else(|| title.clone()),
                            );
                            let mut toast = Toast::new(Status::Error, title)
                                .action(
                                    self.tr.t("action.full_restart_app"),
                                    ToastAction::FullRestart,
                                )
                                .action(self.tr.t("action.view_log"), ToastAction::ViewLog)
                                .sticky();
                            if let Some(r) = reason {
                                toast = toast.body(r);
                            }
                            self.toasts.push(toast);
                        }
                    }
                    // GH#4: which running Codex still uses the previous
                    // configuration, with the details one click away.
                    let old_consumers: Vec<String> = outcome
                        .runtime
                        .as_ref()
                        .map(|v| {
                            v.previous()
                                .map(|c| {
                                    format!(
                                        "{} (pid {})",
                                        self.tr.text(&c.role.text()),
                                        c.core_pid.unwrap_or(c.pid)
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(view) = &outcome.runtime {
                        self.in_effect.view = Some((**view).clone());
                    }
                    if !old_consumers.is_empty()
                        && matches!(
                            outcome.status,
                            OutcomeStatus::Switched
                                | OutcomeStatus::Partial
                                | OutcomeStatus::WrittenOnly
                        )
                    {
                        let busy = outcome
                            .runtime
                            .as_ref()
                            .is_some_and(|v| v.previous().any(|c| c.busy.is_running()));
                        let mut body = old_consumers.join("\n");
                        if busy {
                            body.push('\n');
                            body.push_str(&self.tr.t("toast.in_effect_busy"));
                        }
                        self.toasts.push(
                            Toast::new(
                                Status::Warn,
                                self.tr.f("toast.in_effect", &[("n", &old_consumers.len())]),
                            )
                            .body(body)
                            .action(self.tr.t("action.details"), ToastAction::ShowRuntime)
                            .sticky(),
                        );
                    } else if outcome.runtime.is_none()
                        && !outcome.stale_processes.is_empty()
                        && matches!(
                            outcome.status,
                            OutcomeStatus::Switched | OutcomeStatus::Partial
                        )
                    {
                        let times: Vec<String> = outcome
                            .stale_processes
                            .iter()
                            .filter_map(|p| p.started_at)
                            .map(local_hm)
                            .collect();
                        let text = self.tr.f(
                            "toast.stale",
                            &[
                                ("n", &outcome.stale_processes.len()),
                                ("times", &times.join(", ")),
                            ],
                        );
                        self.toasts.push(Toast::new(Status::Idle, text));
                    }
                    self.data.key_checks.clear();
                }
            }
        }
    }

    fn remote_label(&self, id: &TargetId) -> String {
        self.data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.remotes.iter().find(|r| &r.host.target_id == id))
            .map(|r| crate::pages::shared::hosts::title(&self.tr, &r.host))
            .unwrap_or_else(|| self.tr.text(&superba_core::target_title(id)))
    }

    // -------------------------------------------------------------------
    // Actions
    // -------------------------------------------------------------------

    /// Save the profile's speed tier and re-apply the profile when it is the
    /// applied one (a hot restart writes the new `service_tier`, §11).
    fn set_speed(&mut self, profile: ProfileId, tier: superba_core::SpeedTier) {
        let Some(p) = self.data.profile(&profile) else {
            return;
        };
        let mut defaults = p.model.clone();
        defaults.speed = Some(tier);
        let then = self
            .data
            .active_state()
            .filter(|a| a.profile_id.as_ref() == Some(&profile))
            .map(|a| {
                let mut request = SwitchRequest::profile(profile.clone());
                request.intent = SwitchIntent::Profile {
                    profile_id: profile.clone(),
                    line_id: a.line_id.clone(),
                    auth_mode: None,
                };
                request.targets = TargetSelection {
                    local: true,
                    remotes: Vec::new(),
                };
                Box::new(request)
            });
        self.backend.send(Command::SaveModels {
            edit: Box::new(superba_core::ModelsEdit {
                profile_id: Some(profile),
                defaults: Some(defaults),
                ..Default::default()
            }),
            then,
        });
    }

    fn probe(&mut self, site: SiteId) {
        self.probed_once = true;
        self.data.caps.entry(site.clone()).or_default().start();
        self.backend.send(Command::ProbeLines(site));
    }

    fn request(&mut self, intent: SwitchIntent) {
        let now = self.now();
        self.flow.request(local_request(intent), now);
    }

    fn go(&mut self, page: Page) {
        if page == Page::Hosts && self.page != Page::Hosts {
            // What runs and what a switch left pending may have changed while
            // the page was not shown (user bug 68).
            self.pages.hosts.entered();
        }
        if let Page::Superba(section) = page {
            self.pages.superba.enter(section);
            if self.page != page {
                self.pages.superba.focus = None;
            }
        }
        self.page = page;
        if !self.entered {
            // Loads wait for the first overview (it names the applied site).
            return;
        }
        match page {
            Page::Providers => {
                // The line probes of the applied provider (the page selects it
                // first; it probes other providers itself).
                if let Some(site) = self.data.lines_site_id() {
                    let probed = self.data.site(&site).and_then(|s| {
                        s.default_line()
                            .and_then(|l| self.data.site_caps(&s.id, &l.id).cloned())
                    });
                    if probed.is_none() && !self.data.probing_site(&site) {
                        self.probe(site);
                    }
                }
            }
            Page::Sessions if self.data.sessions.value.is_none() && !self.data.sessions.loading => {
                self.data.sessions.start();
                self.backend.send(Command::Sessions);
            }
            Page::Superba(pages::SuperbaSection::Diagnostics | pages::SuperbaSection::Home)
                if self.data.doctor.value.is_none() && !self.data.doctor.loading =>
            {
                self.data.doctor.start();
                self.data.claude_diag.start();
                self.backend.send(Command::Doctor);
            }
            _ => {}
        }
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: Action) {
        if self.pages.superba.storage.busy()
            && !matches!(action, Action::OpenDataDir | Action::DataLocationChanged(_))
        {
            return;
        }
        if let Some(request) = planned_request(&action) {
            let now = self.now();
            self.flow.request(request, now);
            return;
        }
        match action {
            // Planned above.
            Action::Switch { .. }
            | Action::Restore(_)
            | Action::Plan(_)
            | Action::RestartApp
            | Action::MigrateProvider(_)
            | Action::RollbackMigration(_)
            | Action::Uninstall => {}
            Action::Go(p) => self.go(p),
            Action::Refresh => {
                // Also look at the machine again (installs, Codex versions).
                self.service.environment_event(EnvironmentEvent::Refresh);
                self.cadence.overview_read(Instant::now());
                self.data.overview.start();
                self.backend.send(Command::Refresh);
                self.load_account(true);
            }
            Action::Probe(site) => self.probe(site),
            Action::SaveProfile { profile, key } => {
                self.pending_key_save = key.is_some();
                if key.is_some() {
                    self.data.key_checks.clear();
                }
                self.backend.send(Command::SaveProfile { profile, key });
            }
            Action::CheckKey {
                site,
                profile,
                line,
                mode,
                key,
            } => {
                let key = match (key, profile) {
                    (Some(secret), _) => KeyInput::Secret(secret),
                    (None, Some(id)) => KeyInput::Profile(id),
                    (None, None) => return,
                };
                self.data.key_checking.insert((line.clone(), mode));
                self.backend.send(Command::CheckKey {
                    site,
                    line,
                    mode,
                    key,
                });
            }
            Action::LoadAccount { refresh } => self.load_account(refresh),
            Action::OpenApp => self.launch_app(crate::launch::LaunchOrigin::Window),
            Action::SetAutostart(enabled) => self.backend.send(Command::SetAutostart(enabled)),
            Action::SaveSettings => {
                self.apply_settings(ctx);
                let prefs = (
                    self.settings.auto_update_check,
                    self.settings.update_channel,
                );
                if prefs != self.selfupdate_prefs {
                    // A changed update choice is acted on at once: check again.
                    self.selfupdate_prefs = prefs;
                    self.cadence.selfupdate_checked(Instant::now());
                    self.data.selfupdate.start();
                    self.backend
                        .send(Command::SaveSettingsThenSelfUpdate(self.settings.clone()));
                } else {
                    self.backend
                        .send(Command::SaveSettings(self.settings.clone()));
                }
            }
            Action::CheckSelfUpdate => self.check_self_update(true),
            Action::SetUpdateChannel(channel) => {
                // From the Updates page: the same as picking it in Preferences.
                self.settings.update_channel = Some(channel);
                if let Some(view) = self.data.selfupdate.value.as_mut() {
                    view.channel = channel;
                    view.channel_chosen = true;
                }
                self.handle_action(ctx, Action::SaveSettings);
            }
            Action::SkipSelfUpdate(version) => {
                self.data.selfupdate.start();
                self.backend.send(Command::SelfUpdateSkip(version));
            }
            Action::LaterSelfUpdate(version) => self.data.selfupdate_later = Some(version),
            Action::QuitApp => self.life.quit(),
            Action::SetLocale(l) => self.set_locale(l),
            Action::OpenUrl(url) => {
                let _ = open::that_detached(url);
            }
            Action::Copy(text) => {
                ctx.copy_text(text);
                self.toasts
                    .push(Toast::new(Status::Ok, self.tr.t("toast.copied")));
            }
            Action::LoadSessions => {
                self.data.sessions.start();
                self.backend.send(Command::Sessions);
            }
            Action::RunDoctor => {
                self.pages.superba.diagnostics.invalidate();
                self.data.doctor.start();
                self.data.claude_diag.start();
                self.backend.send(Command::Doctor);
            }
            Action::ShowExternal => {
                if let Some(x) = self
                    .data
                    .overview
                    .value
                    .as_ref()
                    .and_then(|o| o.external.as_ref())
                {
                    self.external.open(x);
                }
            }
            Action::ShowRuntime => self.in_effect.open(&self.service),
            Action::StartWizard => {
                if self.wizard.is_none() {
                    self.wizard = Some(Wizard::start(&self.data));
                }
            }
            Action::SetSpeed { profile, tier } => self.set_speed(profile, tier),
            Action::CheckUpdates => {
                self.service.environment_event(EnvironmentEvent::Refresh);
                self.data.versions.start();
                self.backend.send(Command::Versions { refresh: true });
                self.data.feeds.start();
                self.backend.send(Command::Feeds { refresh: true });
            }
            Action::LoadVersions => {
                self.data.versions.start();
                self.backend.send(Command::Versions { refresh: false });
            }
            Action::LoadHosts => {
                self.cadence.hosts_loaded(Instant::now());
                self.data.hosts.start();
                self.backend.send(Command::Hosts);
            }
            Action::OpenDataDir => {
                if let Some(paths) = &self.data.paths {
                    let _ = open::that_detached(&paths.data_dir);
                }
            }
            Action::DataLocationChanged(location) => {
                if self
                    .data
                    .paths
                    .as_ref()
                    .is_some_and(|p| p.data_dir == location.directory)
                {
                    return;
                }
                self.data.paths = Some(pages::DataPaths {
                    router_db: superba_router::RouterPaths::new(&superba_core::DataDir::new(
                        &location.directory,
                    ))
                    .usage_db(),
                    data_dir: location.directory.clone(),
                });
                self.risk_memory = Some(location.directory.join("gui-hot-risk-seen-v1"));
                // Forms may contain absolute file-key references from before the
                // move. Rebuild them before the user can save another edit.
                let storage = std::mem::take(&mut self.pages.superba.storage);
                self.pages = PageStates::default();
                self.pages.superba.storage = storage;
                self.pages.superba.storage.relocated();
                self.backend.send(Command::Refresh);
            }
            Action::PageCommand { page, command } => {
                self.go(page);
                let pal = Palette::of(ctx);
                let busy = self.flow.is_running() || self.flow.is_planning();
                let cx = cx!(self, pal, busy);
                let mut more = Vec::new();
                self.pages.command(page, &cx, &command, &mut more);
                for action in more {
                    self.handle_action(ctx, action);
                }
            }
            Action::Toast { status, text } => {
                self.log(status, text.clone());
                self.toasts.push(Toast::new(status, text));
            }
            Action::OfferSwitch { text, profile } => {
                self.log(Status::Ok, text.clone());
                let mut toast = Toast::new(Status::Ok, text)
                    .body(self.tr.t("providers.add.saved_only"))
                    .action(self.tr.t("providers.use"), ToastAction::Switch(profile));
                toast.ttl = Some(OFFER_TOAST_SECS);
                self.toasts.push(toast);
            }
        }
    }

    /// The last update attempt failed (or ended with a problem) and that was
    /// not shown yet: a sticky toast with the reason and the log, once.
    fn announce_selfupdate_problem(&mut self, view: &superba_core::SelfUpdateView) {
        if self.selfupdate_problem_announced {
            return;
        }
        let (tr, data) = (&self.tr, &self.data);
        let Some(toast) = crate::notices::selfupdate_problem_toast(tr, view, |t| {
            pages::localize_text(tr, data, t)
        }) else {
            return;
        };
        self.selfupdate_problem_announced = true;
        let line = match &toast.body {
            Some(body) => format!("{}: {body}", toast.title),
            None => toast.title.clone(),
        };
        self.log(toast.status, line);
        self.held_toasts.push(toast);
        self.ack_selfupdate_problem = true;
    }

    /// Show the start-up notices once the main window is what the user sees:
    /// the first data is in and the setup wizard (which would cover them) is
    /// closed or was never opened.
    fn release_startup_notices(&mut self) {
        let decided = self.data.overview.value.is_some() && self.data.profiles.value.is_some();
        if !decided || self.wizard.is_some() {
            return;
        }
        if let Some((run_as, dir)) = self.elevated_notice.take() {
            crate::notices::show_elevated_once(&self.tr, &run_as, Some(&dir), &mut self.toasts);
        }
        for toast in self.held_toasts.drain(..) {
            self.toasts.push(toast);
        }
        if std::mem::take(&mut self.ack_selfupdate_problem) {
            self.backend.send(Command::SelfUpdateAcknowledge);
        }
    }

    fn handle_toast_action(&mut self, a: ToastAction) {
        match a {
            ToastAction::OpenApp => self.launch_app(crate::launch::LaunchOrigin::Window),
            ToastAction::FullRestart => self.request(SwitchIntent::RestartApp { install_id: None }),
            ToastAction::ViewLog => {
                self.go(Page::Superba(crate::pages::SuperbaSection::Diagnostics))
            }
            ToastAction::OpenPath(path) => {
                let _ = open::that_detached(path);
            }
            ToastAction::Retry(r) => {
                let now = self.now();
                self.flow.request(*r, now);
            }
            ToastAction::Switch(profile) => self.request(SwitchIntent::Profile {
                profile_id: profile,
                line_id: None,
                auth_mode: None,
            }),
            ToastAction::ShowRuntime => self.in_effect.open(&self.service),
        }
    }

    // -------------------------------------------------------------------
    // Frame
    // -------------------------------------------------------------------

    fn keyboard(&mut self, ctx: &egui::Context) {
        if self.pages.superba.storage.busy() {
            return;
        }
        // Dialogs own Enter / Escape; the flow must not react to them.
        let modal = self.wizard.is_some() || self.pages.any_modal() || self.in_effect.is_open();
        if modal {
            return;
        }
        let preferences = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Comma);
        if ctx.input_mut(|i| i.consume_shortcut(&preferences)) {
            self.go(Page::Superba(pages::SuperbaSection::General));
        }
        // Page shortcuts: Ctrl/Cmd+F focuses the page search, Ctrl/Cmd+S saves the
        // pending edits of the page.
        let search = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F);
        let save = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        let page = self.page;
        if self.page.layout() == PageLayout::Fit && ctx.input_mut(|i| i.consume_shortcut(&search)) {
            self.page_command(ctx, page, "search");
        }
        if self.pages.status(page).pending > 0 && ctx.input_mut(|i| i.consume_shortcut(&save)) {
            self.page_command(ctx, page, "save");
        }
        let typing = ctx.memory(|m| m.focused().is_some()) && ctx.egui_wants_keyboard_input();
        let (enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let now = self.now();
        if enter
            && !self.flow.rollback_now()
            && !typing
            && matches!(self.flow.stage(), Stage::Armed { .. })
        {
            self.flow.switch_now(now);
        }
        if esc {
            self.flow.cancel(now);
        }
    }

    /// Run a page command right away (keyboard shortcuts).
    fn page_command(&mut self, ctx: &egui::Context, page: Page, command: &str) {
        let pal = Palette::of(ctx);
        let busy = self.flow.is_running() || self.flow.is_planning();
        let cx = cx!(self, pal, busy);
        let mut more = Vec::new();
        self.pages.command(page, &cx, command, &mut more);
        for action in more {
            self.handle_action(ctx, action);
        }
    }

    fn content(&mut self, ui: &mut egui::Ui, out: &mut Vec<Action>) {
        let now = self.now();
        let busy = self.flow.is_running() || self.flow.is_planning();
        let pal = Palette::of(ui.ctx());
        if matches!(
            self.page,
            Page::Overview | Page::Superba(pages::SuperbaSection::Home)
        ) {
            let cx = cx!(self, pal, busy);
            let reading_hooks =
                pages::notify::poll(&cx, &mut self.settings, &mut self.pages.notify, out);
            if reading_hooks {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        {
            let cx = cx!(self, pal, busy);
            self.pages.updates.background(&cx, out);
            let tools = self.pages.updates.tools.view(&cx);
            let entries =
                pages::superba::updates::items::inventory(&cx, &self.pages.updates, &tools);
            let count = entries.iter().filter(|e| e.status.actionable()).count() as u32;
            if self.pages.updates.busy() || self.pages.updates.operation_busy(&cx) || tools.checking
            {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            if self.data.update_inventory_count.replace(count) != Some(count) {
                // The sidebar is drawn before this page body.
                ui.ctx().request_repaint();
            }
        }
        self.data.notify_hooks_attention =
            self.settings.notify.enabled && self.pages.notify.needs_attention();
        self.data.tool_update_errors = self
            .pages
            .updates
            .results
            .iter()
            .filter_map(|(target, result)| {
                result
                    .as_ref()
                    .err()
                    .map(|error| (target.clone(), error.clone()))
            })
            .collect();
        let cx = cx!(self, pal, busy);
        let full = ui.available_rect_before_wrap();
        let fit = self.page.layout() == PageLayout::Fit;
        // The Overview's cards float on the window background across the wide
        // content width; every other page sits in one large rounded panel.
        let floating = self.page == Page::Overview;
        let page_rect = layout::page_rect(full, fit || floating);
        let panel = backdrop::panel_rect(full);
        if !floating {
            backdrop::content_card(ui.painter(), panel, &Backdrop::of(ui.ctx()));
        }
        let width = page_rect.width();
        let left = page_rect.min.x;

        let armed = match self.flow.stage() {
            Stage::Armed { plan, countdown } => Some(pages::ArmedSwitch {
                destination: plan.to.clone(),
                seconds: Some(countdown.remaining(now).as_millis().div_ceil(1000) as u64),
            }),
            Stage::WaitingBeforeSwitch { plan, .. } => Some(pages::ArmedSwitch {
                destination: plan.to.clone(),
                seconds: None,
            }),
            _ => None,
        };
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::unique("superba-armed-destination"),
                self.flow.armed_destination().cloned(),
            );
        });
        pages::ArmedSwitch::publish(ui.ctx(), armed);

        // The switch overlay floats over the bottom of the page; the page only gets
        // enough extra scroll room to bring its last row above it.
        let overlay_id = egui::Id::unique("superba-switch-overlay");
        let overlay_h: f32 = ui.ctx().data(|d| d.get_temp(overlay_id)).unwrap_or(0.0);
        let page = self.page;
        if fit {
            // Fit pages: exactly the page rectangle, no page-level ScrollArea (the
            // switch overlay floats over their footer).
            let mut area = ui.new_child(
                UiBuilder::new()
                    .max_rect(page_rect)
                    .id_salt(("page", page.sidebar_page()))
                    .layout(Layout::top_down(Align::Min)),
            );
            layout::fit_area(&mut area, page.min_height(), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.add_enabled_ui(!self.pages.superba.storage.busy(), |ui| {
                    self.pages.show(page, ui, &cx, &mut self.settings, out);
                });
            });
        } else {
            // Scroll pages scroll inside the panel (its top edge clips them).
            let area_rect = if floating { full } else { panel };
            let mut area = ui.new_child(
                UiBuilder::new()
                    .max_rect(area_rect)
                    .layout(Layout::top_down(Align::Min)),
            );
            ScrollArea::vertical()
                .id_salt(("page", self.page))
                .auto_shrink([false, false])
                .show(&mut area, |ui| {
                    ui.add_space(size::PAGE_TOP - (area_rect.min.y - full.min.y));
                    ui.horizontal(|ui| {
                        ui.add_space(left - full.min.x);
                        ui.vertical(|ui| {
                            ui.set_width(width);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            ui.add_enabled_ui(!self.pages.superba.storage.busy(), |ui| {
                                self.pages.show(page, ui, &cx, &mut self.settings, out);
                            });
                            ui.add_space(40.0 + overlay_h);
                        });
                    });
                });
        }

        let ow = width.min(size::OVERLAY_MAX);
        let shown = shell::overlay_visible(&self.flow);
        let t = ui.ctx().animate_bool_with_time(
            overlay_id.with("anim"),
            shown,
            crate::design::tokens::ANIM_SECS,
        );
        let mut drawn_h = 0.0;
        if shown {
            let bottom = full.max.y - 20.0 + 12.0 * (1.0 - t);
            let resp = egui::Area::new(overlay_id.with("area"))
                .order(egui::Order::Middle)
                .pivot(egui::Align2::CENTER_BOTTOM)
                .fixed_pos(pos2(left + width / 2.0, bottom))
                .constrain(false)
                .show(ui.ctx(), |ui| {
                    ui.set_opacity(t);
                    ui.set_width(ow);
                    let max_h = (full.height() * 0.62).max(160.0);
                    // An area offers only last frame's size; let a growing
                    // overlay (planning -> countdown) take its full height now.
                    ui.set_max_height(max_h + 40.0);
                    shell::banner(ui, &cx, &mut self.flow, &mut self.banner, now, max_h)
                });
            if resp.inner {
                drawn_h = resp.response.rect.height() + 20.0;
            }
        }
        if (drawn_h - overlay_h).abs() > 0.5 {
            ui.ctx().data_mut(|d| d.insert_temp(overlay_id, drawn_h));
            ui.ctx().request_repaint();
        }
        self.toasts.set_bottom_inset(drawn_h);
        if t > 0.0 && t < 1.0 {
            ui.ctx().request_repaint();
        }
    }
}

impl eframe::App for SuperbaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fonts.poll(&ctx);
        self.handle_instance_requests();
        self.handle_replies(&ctx);
        self.tray_tick(&ctx);
        let now = self.now();
        // Let Escape/Enter win over a countdown expiring in this same frame.
        self.keyboard(&ctx);
        self.flow.tick(now);
        self.request_attention(&ctx);

        let pal = Palette::of(&ctx);
        let mut actions: Vec<Action> = Vec::new();

        // The window background (user request 24): a soft tinted gradient behind
        // the sidebar and the content; the panels draw no fill of their own.
        backdrop::paint(ui.painter(), ui.max_rect(), &Backdrop::of(&ctx));
        // The empty top of the window moves it (user bug 85); registered
        // first, so everything drawn there later takes the pointer before it.
        crate::chrome::drag_areas(ui);

        Panel::left("nav")
            .exact_size(size::SIDEBAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(shell::sidebar_frame())
            .show(ui, |ui| {
                let cx = cx!(self, pal, false);
                let mut page = self.page;
                let badges = shell_badges(&self.tr, &self.data);
                let pages = &self.pages;
                let status = move |p: Page| {
                    let mut s = pages.status(p);
                    // The shell's own data (UX §2.1) unless the page reports one.
                    if s.badge == pages::NavBadge::None
                        && let Some((badge, tip)) = badges
                            .iter()
                            .find(|(page, ..)| *page == p)
                            .map(|(_, b, t)| (*b, t.clone()))
                    {
                        s.badge = badge;
                        s.badge_tip = Some(tip);
                    }
                    s
                };
                shell::sidebar(ui, &cx, &mut page, &status, &mut actions);
                if page != self.page {
                    if page.is_superba() && !self.page.is_superba() {
                        page = Page::Superba(self.pages.superba.last);
                    }
                    actions.push(Action::Go(page));
                }
            });

        egui::CentralPanel::default()
            .frame(Frame::new())
            .show(ui, |ui| self.content(ui, &mut actions));

        // Overlays: confirmation, decisions, wizard, toasts.
        {
            let cx = cx!(self, pal, self.flow.is_running() || self.flow.is_planning());
            shell::confirm_full(&ctx, &cx, &mut self.flow, &mut self.banner, now);
            shell::decision_dialog(&ctx, &cx, &mut self.flow, now);
            crate::in_effect::show(&ctx, &cx, &mut self.in_effect, &mut actions);
            let movers = crate::movers::dialog::show(
                &ctx,
                &cx,
                &mut self.flow,
                &mut self.movers,
                now,
                self.settings.reduce_motion,
            );
            // GH#5: a change another tool made is offered once per state of
            // the files, when nothing else asks for the user's attention.
            let quiet = !movers
                && self.wizard.is_none()
                && matches!(self.flow.stage(), crate::flow::Stage::Idle);
            self.external.offer(
                self.data
                    .overview
                    .value
                    .as_ref()
                    .and_then(|o| o.external.as_ref()),
                quiet,
            );
            crate::external::show(&ctx, &cx, &mut self.external, &mut actions);
            if let Some(w) = self.wizard.as_mut() {
                match pages::wizard::show(&ctx, &cx, w, &mut actions) {
                    WizardOutcome::Continue => {}
                    WizardOutcome::Close => {
                        self.wizard = None;
                        self.wizard_dismissed = true;
                        self.settings.wizard_dismissed = true;
                        if let Some(set) = self.data.profiles.value.as_mut() {
                            set.settings.wizard_dismissed = true;
                        }
                        self.backend
                            .send(Command::SaveSettings(self.settings.clone()));
                    }
                    WizardOutcome::Finish {
                        site,
                        profile,
                        key,
                        request,
                        catalog_mode,
                    } => {
                        self.wizard = None;
                        self.wizard_dismissed = true;
                        // Plan once the site, the profile (and its key) are saved;
                        // the router model list is saved after the profile too.
                        self.wizard_request = Some(*request);
                        self.wizard_catalog_mode = catalog_mode;
                        self.backend.send(match site {
                            Some(site) => Command::SaveSiteAndProfile { site, profile, key },
                            None => Command::SaveProfile { profile, key },
                        });
                    }
                }
            }
        }
        // The first-close question (item 70), above everything else.
        self.tray_prompt(&ctx);
        self.release_startup_notices();
        if let Some(a) = self.toasts.show(&ctx, &self.tr.t("action.close")) {
            self.handle_toast_action(a);
        }
        // The window buttons (Windows, Linux), above everything else.
        crate::chrome::overlay(&ctx, &self.tr);

        if self.data.overview.value.is_some()
            && let Some(command) = self.opts.command.take()
        {
            actions.push(Action::PageCommand {
                page: self.page,
                command,
            });
        }
        for action in actions {
            self.handle_action(&ctx, action);
        }
        for command in self.flow.take_commands() {
            self.backend.send(command);
        }
        self.process_flow_notices(&ctx);
        self.request_attention(&ctx);

        // The shell's own reads and the window's next wake-up.
        self.run_cadence(&ctx, true);
        self.tray_finish(&ctx);
    }

    /// Runs before every `ui` and alone while the window is hidden or
    /// minimised (eframe calls no `ui` then): the backend's answers keep being
    /// handled and the router watchdog keeps ticking; nothing else is read and
    /// the window is not woken up for anything else.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.fonts.poll(ctx);
        let visible = ctx.input(|i| i.viewport().visible()).unwrap_or(true);
        if visible {
            // `ui` follows in this pass and does all of it.
            return;
        }
        self.handle_instance_requests();
        self.handle_replies(ctx);
        self.tray_tick(ctx);
        // A switch chosen in the tray while the window hides: nobody sees a
        // countdown to cancel, so it starts at once (questions bring the
        // window back, `tray_tick`).
        let now = self.now();
        self.flow.tick(now);
        if self.flow.armed_destination().is_some()
            && matches!(self.flow.stage(), Stage::Armed { .. })
        {
            self.flow.switch_now(now);
        }
        for command in self.flow.take_commands() {
            self.backend.send(command);
        }
        self.process_flow_notices(ctx);
        self.request_attention(ctx);
        self.run_cadence(ctx, false);
        self.tray_finish(ctx);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}

/// Show the window and bring it to the front (a request of another start
/// needs the user: a reopen to confirm, an error to read). On Windows the
/// request's sender passes its foreground right on first
/// ([`crate::instance::forward`]).
pub(crate) fn bring_to_front(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

/// How a provider is named in lists: the profile name, else the site name.
pub fn provider_name(
    tr: &Tr,
    profile: &superba_core::Profile,
    site: &superba_core::Site,
) -> String {
    if profile.name.trim().is_empty() {
        tr.text(&site.name)
    } else {
        profile.name.clone()
    }
}

/// Sidebar badges computed from the shell's own data (UX §2.1): the router dot
/// while Codex is routed, the number of hosts with unsynced settings, a dot when
/// an app or Codex update is available. Each with its translated tooltip.
pub fn shell_badges(tr: &Tr, data: &Data) -> Vec<(Page, pages::NavBadge, String)> {
    let mut out = Vec::new();
    if data.router_enabled() {
        out.push((
            Page::Router,
            pages::NavBadge::Dot(Status::Ok),
            tr.t("shell.badge.routed"),
        ));
    }
    let unsynced = data.unsynced_hosts();
    if unsynced > 0 {
        out.push((
            Page::Hosts,
            pages::NavBadge::Count(unsynced),
            tr.f("shell.badge.unsynced", &[("n", &unsynced)]),
        ));
    }
    let count = data.update_count();
    if count > 0 {
        out.push((
            Page::Superba(pages::SuperbaSection::Home),
            pages::NavBadge::Count(count),
            tr.f("superba.updates_count", &[("n", &count)]),
        ));
    } else if data.doctor.value.as_ref().is_some_and(|rows| {
        rows.iter()
            .any(|r| r.severity != superba_core::Severity::Info)
    }) {
        out.push((
            Page::Superba(pages::SuperbaSection::Home),
            pages::NavBadge::Dot(Status::Warn),
            tr.t("superba.diagnostics_attention"),
        ));
    }
    out
}

/// The toast for a router watchdog state change (`None` when nothing changed
/// that the user needs to hear about): status and i18n key (`{detail}` argument).
pub fn router_watch_notice(
    previous: Option<&superba_core::RouterWatchState>,
    next: &superba_core::RouterWatchState,
) -> Option<(Status, &'static str)> {
    use superba_core::RouterWatchState as W;
    let changed = previous.map(std::mem::discriminant) != Some(std::mem::discriminant(next));
    match next {
        W::Suspect { .. } if changed => Some((Status::Warn, "router.watch.suspect")),
        W::FellBack { .. } if changed => Some((Status::Error, "router.watch.fell_back")),
        // Each move is news (the port changed again).
        W::Moved { .. } => Some((Status::Ok, "router.watch.moved")),
        W::Healthy if matches!(previous, Some(W::Suspect { .. })) => {
            Some((Status::Ok, "router.watch.recovered"))
        }
        _ => None,
    }
}

/// The armed profile, site, line and auth mode for pages drawing pending selections.
pub fn armed_destination(ctx: &egui::Context) -> Option<superba_core::PlanDestination> {
    ctx.data(|data| {
        data.get_temp::<Option<superba_core::PlanDestination>>(egui::Id::unique(
            "superba-armed-destination",
        ))
    })
    .flatten()
}

fn remember_risks(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A marker contains no user data. create_new makes interrupted/repeated saves
    // harmless and does not overwrite anything belonging to the switch service.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn probe_log(tr: &Tr, data: &Data, site: &SiteId, caps: &[LineCaps]) -> (Status, String) {
    let lines: Vec<String> = caps
        .iter()
        .map(|caps| {
            let name = data
                .sites
                .iter()
                .find(|s| &s.id == site)
                .and_then(|s| s.line(&caps.line_id))
                .map(|line| tr.text(&line.name))
                .unwrap_or_else(|| caps.line_id.to_string());
            let latency = if caps.reachable {
                caps.latency_ms.map_or("—".into(), |ms| format!("{ms} ms"))
            } else {
                tr.t("caps.unavailable")
            };
            format!("{name} {latency}")
        })
        .collect();
    let status = if caps.iter().any(|c| !c.reachable) {
        Status::Warn
    } else {
        Status::Ok
    };
    (status, tr.f("log.probed", &[("lines", &lines.join(", "))]))
}

fn phase_key(phase: SwitchPhase) -> &'static str {
    match phase {
        SwitchPhase::TurnCheck => "phase.turn_check",
        SwitchPhase::WaitingTurn => "phase.waiting_turn",
        SwitchPhase::Quitting => "phase.quitting",
        SwitchPhase::ForceQuitConfirm => "phase.force_quit",
        SwitchPhase::Applying => "phase.applying",
        SwitchPhase::RollingBack => "phase.rolling_back",
        SwitchPhase::Restarting => "phase.restarting",
        SwitchPhase::FullRestartOffer => "phase.full_restart_offer",
        SwitchPhase::Launching => "phase.launching",
        SwitchPhase::Verifying => "phase.verifying",
        SwitchPhase::NavRestore => "phase.nav_restore",
        SwitchPhase::Recovery => "phase.recovery",
        SwitchPhase::RollbackRestart => "phase.rollback_restart",
        SwitchPhase::Done => "phase.done",
        _ => "phase.other",
    }
}

/// `HH:MM` in local time.
fn local_hm(t: Timestamp) -> String {
    crate::pages::clock_time(t, false)
}

/// The window, taskbar and dock icon: Superba's pearl-tile mark, the 256 px
/// render of `assets/icon/superba.svg`.
///
/// macOS (user bug 52): eframe makes this image the Dock icon at runtime, so
/// there it is the render of `superba-macos.svg` (Apple's grid, as big as other
/// apps), and inside `Superba.app` no icon is set at all: `IconData::default()`
/// tells eframe to keep the bundle's own `Superba.icns`, which the Dock,
/// Finder and Launchpad then all show.
pub fn window_icon() -> egui::IconData {
    #[cfg(target_os = "macos")]
    if std::env::current_exe().is_ok_and(|exe| is_app_bundle_executable(&exe)) {
        return egui::IconData::default();
    }
    #[cfg(target_os = "macos")]
    let png = include_bytes!("../../../assets/icon/png/superba-macos-256.png");
    #[cfg(not(target_os = "macos"))]
    let png = include_bytes!("../../../assets/icon/png/superba-256.png");
    eframe::icon_data::from_png_bytes(png).expect("the embedded 256 px icon is a valid PNG")
}

/// Whether `exe` is the executable of an app bundle: `<X>.app/Contents/MacOS/<exe>`.
pub fn is_app_bundle_executable(exe: &std::path::Path) -> bool {
    fn name(p: Option<&std::path::Path>) -> Option<&str> {
        p?.file_name()?.to_str()
    }
    let macos = exe.parent();
    let contents = macos.and_then(|p| p.parent());
    let bundle = contents.and_then(|p| p.parent());
    name(macos) == Some("MacOS")
        && name(contents) == Some("Contents")
        && name(bundle).is_some_and(|n| n.len() > ".app".len() && n.ends_with(".app"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use superba_core::{CodexPath, LocalContext};

    #[test]
    fn probe_log_uses_localized_names_and_warns_for_any_unreachable_line() {
        let data = Data {
            sites: catalog::builtin_sites(),
            ..Data::default()
        };
        let site = data.site(&data.lines_site_id().unwrap()).unwrap();
        let mut caps: Vec<LineCaps> = site
            .lines
            .iter()
            .map(|line| LineCaps {
                site_id: site.id.clone(),
                line_id: line.id.clone(),
                checked_at: Timestamp::EPOCH,
                reachable: true,
                latency_ms: Some(38),
                pat_ok: true,
                apikey_ok: true,
                codex_path: Some(CodexPath::BackendApiCodex),
                blocked_by_edge: false,
                key_access: None,
                checks: vec![],
                detail: None,
            })
            .collect();
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let (status, text) = probe_log(&tr, &data, &site.id, &caps);
            assert_eq!(status, Status::Ok);
            for line in &site.lines {
                assert!(text.contains(&tr.text(&line.name)));
            }
            assert!(!text.contains("cn-fast"));
        }
        caps[0].reachable = false;
        let tr = Tr::new(Locale::En);
        let (status, text) = probe_log(&tr, &data, &site.id, &caps);
        assert_eq!(status, Status::Warn);
        assert!(text.contains(&format!("{} Unavailable", tr.text(&site.lines[0].name))));
        for cap in &mut caps {
            cap.reachable = false;
        }
        assert_eq!(probe_log(&tr, &data, &site.id, &caps).0, Status::Warn);
    }

    #[test]
    fn disclosure_marker_is_idempotent_and_restores_ui_memory() {
        let root = std::env::temp_dir().join(format!(
            "superba-gui-risk-{}-{}",
            std::process::id(),
            Timestamp::now().as_millis()
        ));
        let context = LocalContext::for_tests(&root);
        let marker = context.data_dir.root().join("gui-hot-risk-seen-v1");
        assert!(!marker.exists());
        remember_risks(&marker).unwrap();
        remember_risks(&marker).unwrap();
        assert!(marker.is_file());
        assert_eq!(std::fs::metadata(&marker).unwrap().len(), 0);
        let ctx = egui::Context::default();
        BannerState::restore_risk_memory(&ctx, marker.is_file());
        assert!(BannerState::risks_seen(&ctx));
        std::fs::remove_dir_all(root).unwrap();
    }
}
