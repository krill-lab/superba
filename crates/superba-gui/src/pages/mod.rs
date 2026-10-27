//! Pages and the helpers they share. Changes are returned as [`Action`]s for the
//! app to send to the backend. Credential discovery uses read-only runtime tasks;
//! pages never write configuration or execute a switch themselves.
//!
//! # Page module contract
//!
//! The shell (`app` / `shell`) mounts self-contained page modules. A module that
//! owns its own state ([`providers`], [`models`], [`router`], [`settings`],
//! [`sessions`], [`hosts`], [`updates`]) exposes:
//!
//! ```ignore
//! #[derive(Debug, Default)]
//! pub struct State { /* page-local state, kept across navigation */ }
//!
//! /// Draw the page (header included). A `PageLayout::Fit` page gets exactly the
//! /// rectangle below the window's top padding and draws without a page-level
//! /// ScrollArea; a `PageLayout::Scroll` page is drawn inside the shell's one.
//! pub fn show(ui: &mut egui::Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>);
//!
//! /// Run a command addressed to this page ([`Action::PageCommand`]); the shell
//! /// navigates to the page first. Shared commands: `search` (Ctrl/Cmd+F),
//! /// `save` (Ctrl/Cmd+S while [`PageStatus::pending`] > 0).
//! pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>);
//!
//! /// What the shell shows or respects for the page without drawing it: an open
//! /// dialog (Enter / Escape belong to it), unsaved edits (amber sidebar dot),
//! /// a sidebar badge.
//! pub fn status(state: &State) -> PageStatus;
//! ```
//!
//! Pages draw their header with [`page_header`]. Background reads and service
//! calls go through [`crate::runtime::task`] with [`Cx::service`]; switches are
//! planned with [`Action::Switch`] / [`Action::Plan`]; other shell-level effects
//! (navigate, toast, refresh) are returned as [`Action`]s. Helpers shared by
//! several pages live in [`shared`] (never import another page's module).

pub mod claude;
pub mod data;
pub mod hosts;
pub mod import;
pub mod models;
pub mod notify;
pub mod overview;
pub mod superba;
pub mod tools;
pub mod troubleshooting;
pub use superba::SuperbaSection;
pub mod providers;
pub mod router;
pub mod session_browser;
pub mod sessions;
pub mod settings;
pub mod shared;
pub mod simple_usage;
pub mod wizard;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use superba_core::harness::HarnessId;

use egui::{Align, Layout, RichText, Sense, Stroke, Ui, vec2};
use superba_core::{
    AuthMode, AuthShape, BuiltinProfile, CliUpdateAction, DoctorFinding, Error, FeedsStatus,
    HostView, HotSwitchStatus, KeyAccess, KeyCheck, LineCaps, LineId, Locale, MigrationReceipt,
    ModelsView, Overview, Profile, ProfileId, ProfileSet, RestoreTarget, Secret, SessionReport,
    Site, SiteId, SpeedTier, SwitchRequest, Timestamp, VersionsView, catalog,
};

use crate::design::Palette;
use crate::design::fonts::FontReport;
use crate::design::icons::{self, Icon};
use crate::design::tokens::radius;
use crate::design::widgets::{Card, Status, Text};
use crate::i18n::Tr;

/// Navigation targets (UX §2.1), in sidebar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Page {
    /// Overview: the active provider, its lines, this computer, remote hosts.
    Overview,
    /// Shared configuration for the other coding tools.
    Tools,
    /// Providers: connection (lines, auth, key), sites, account and usage.
    Providers,
    /// Models of the active provider, per-model context window and compaction,
    /// speed tiers.
    Models,
    /// Local router settings: on/off, members and failover order, stickiness,
    /// prices, the reply stats line, recording and retention.
    Router,
    /// Usage: recent requests, requests in flight and statistics (the router's
    /// log merged with the model requests in Codex's rollout files, so it works
    /// without the router).
    Usage,
    /// Codex settings of this computer (feature catalog).
    Settings,
    /// Sessions per host.
    Sessions,
    /// Remote hosts the desktop app is connected to (settings sync, Codex).
    Hosts,
    /// Claude Code: its provider and Anthropic endpoint, the models it lists
    /// and their slots and capabilities, apply / restore (user bug 79).
    ClaudeCode,
    /// Claude Code's `settings.json` (catalog of its keys and environment
    /// variables, the file editor).
    ClaudeSettings,
    /// Superba maintenance and preferences, with a section deep link.
    Superba(SuperbaSection),
    /// Guided Codex diagnostics, recovery and follow-up advice.
    Troubleshooting,
    /// One installed coding tool (dsh, opencode, pi, ZCode, WorkBuddy,
    /// Gemini CLI): the Tools page's detail of it as a page of its own; its
    /// sidebar entry shows only when the tool is installed (user request 114).
    Tool(superba_core::harness::HarnessId),
}

/// Sidebar groups (UX §2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NavGroup {
    /// Overview (no caption).
    Main,
    /// Where Codex connects: providers, models, router.
    Connect,
    /// Codex on this computer and remote hosts.
    Codex,
    /// Claude Code on this computer.
    Claude,
    /// The other coding tools that are installed.
    Others,
}

impl NavGroup {
    /// i18n key of the caption (`None` for the first group).
    pub fn caption_key(self) -> Option<&'static str> {
        match self {
            NavGroup::Main => None,
            NavGroup::Connect => Some("nav.group.connect"),
            NavGroup::Codex => Some("nav.group.codex"),
            NavGroup::Claude => Some("nav.group.claude"),
            NavGroup::Others => Some("nav.group.others"),
        }
    }
}

/// How a page uses the content area (UX §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageLayout {
    /// Fixed panes in exactly the window's content rectangle (up to
    /// `size::CONTENT_WIDE`), no page-level ScrollArea; lists page instead of
    /// scrolling. Below `layout::FIT_MIN_HEIGHT` the page scrolls as a whole.
    Fit,
    /// A column of at most `size::CONTENT_MAX` inside the shell's ScrollArea.
    Scroll,
}

impl Page {
    /// Sidebar destinations; Superba sections share one pinned entry.
    pub const ALL: [Page; 19] = [
        Page::Overview,
        Page::Tools,
        Page::Providers,
        Page::Models,
        Page::Router,
        Page::Usage,
        Page::Settings,
        Page::Sessions,
        Page::Hosts,
        Page::ClaudeCode,
        Page::ClaudeSettings,
        Page::Tool(HarnessId::Dsh),
        Page::Tool(HarnessId::OpenCode),
        Page::Tool(HarnessId::Pi),
        Page::Tool(HarnessId::ZCode),
        Page::Tool(HarnessId::WorkBuddy),
        Page::Tool(HarnessId::Gemini),
        Page::Troubleshooting,
        Page::Superba(SuperbaSection::Home),
    ];

    /// Every independently addressable page, including all Superba sections.
    pub const DESTINATIONS: [Page; 26] = [
        Page::Overview,
        Page::Tools,
        Page::Providers,
        Page::Models,
        Page::Router,
        Page::Usage,
        Page::Settings,
        Page::Sessions,
        Page::Hosts,
        Page::ClaudeCode,
        Page::ClaudeSettings,
        Page::Tool(HarnessId::Dsh),
        Page::Tool(HarnessId::OpenCode),
        Page::Tool(HarnessId::Pi),
        Page::Tool(HarnessId::ZCode),
        Page::Tool(HarnessId::WorkBuddy),
        Page::Tool(HarnessId::Gemini),
        Page::Troubleshooting,
        Page::Superba(SuperbaSection::Home),
        Page::Superba(SuperbaSection::Updates),
        Page::Superba(SuperbaSection::Diagnostics),
        Page::Superba(SuperbaSection::Data),
        Page::Superba(SuperbaSection::General),
        Page::Superba(SuperbaSection::Appearance),
        Page::Superba(SuperbaSection::Network),
        Page::Superba(SuperbaSection::Notify),
    ];

    /// The one pinned sidebar entry.
    pub fn is_superba(self) -> bool {
        matches!(self, Self::Superba(_))
    }

    /// Navigation entries kept above the version at the bottom of the sidebar.
    pub fn is_pinned(self) -> bool {
        self.is_superba() || self == Self::Troubleshooting
    }

    /// Canonical target used for the sidebar badge and accessible name.
    pub fn sidebar_page(self) -> Self {
        if self.is_superba() {
            Self::Superba(SuperbaSection::Home)
        } else {
            self
        }
    }

    /// Sidebar group.
    pub fn group(self) -> NavGroup {
        match self {
            Page::Overview | Page::Tools | Page::Superba(_) | Page::Troubleshooting => {
                NavGroup::Main
            }
            Page::Providers | Page::Models | Page::Router | Page::Usage => NavGroup::Connect,
            Page::Settings | Page::Sessions | Page::Hosts => NavGroup::Codex,
            Page::ClaudeCode | Page::ClaudeSettings => NavGroup::Claude,
            Page::Tool(_) => NavGroup::Others,
        }
    }

    /// Fit or Scroll.
    pub fn layout(self) -> PageLayout {
        PageLayout::Fit
    }

    /// Minimum content height before the shell uses its short-window fallback.
    pub fn min_height(self) -> f32 {
        if self == Self::Overview {
            return overview::MIN_HEIGHT;
        }
        if self.is_superba()
            || self == Self::Troubleshooting
            || self == Self::Router
            || self == Self::ClaudeCode
            || self == Self::Tools
            || matches!(self, Self::Tool(_))
            || self == Self::Models
            || self == Self::Providers
            || self == Self::Sessions
        {
            // These settings pages already scroll their own body (Tools: its
            // list and its detail; Models: its table pages instead; Providers:
            // the detail pane, when its tab is taller than the pane). Keep the
            // header and footer fixed and avoid a second scroll area at 960×640,
            // where the pager would fall off the card. Sessions: its list pages.
            480.0
        } else {
            crate::design::layout::FIT_MIN_HEIGHT
        }
    }

    /// i18n key of the title (page header, palette, the sidebar row's
    /// accessible name).
    pub fn title_key(self) -> &'static str {
        match self {
            Page::Overview => "nav.overview",
            Page::Tools => "superba.tools.title",
            Page::Troubleshooting => "simple.troubleshooting",
            Page::Providers => "nav.providers",
            Page::Models => "nav.models",
            Page::Router => "nav.router",
            Page::Usage => "nav.usage",
            Page::Settings => "nav.settings",
            Page::Sessions => "nav.sessions",
            Page::Hosts => "nav.hosts",
            Page::ClaudeCode => "nav.claude_code",
            Page::ClaudeSettings => "nav.claude_settings",
            Page::Tool(h) => match h {
                HarnessId::Dsh => "nav.tool.dsh",
                HarnessId::OpenCode => "nav.tool.opencode",
                HarnessId::Pi => "nav.tool.pi",
                HarnessId::ZCode => "nav.tool.zcode",
                HarnessId::WorkBuddy => "nav.tool.workbuddy",
                HarnessId::Gemini => "nav.tool.gemini",
                HarnessId::ClaudeCode => "nav.claude_code",
            },
            Page::Superba(section) => section.title_key(),
        }
    }

    /// i18n key of the sidebar label: the title, or a shorter one where the
    /// group caption says the rest ("设置" under "Claude Code").
    pub fn nav_key(self) -> &'static str {
        match self {
            Page::Superba(_) => "superba.title",
            Page::ClaudeCode => "nav.short.claude_code",
            Page::ClaudeSettings => "nav.short.claude_settings",
            other => other.title_key(),
        }
    }

    /// i18n key of the subtitle.
    pub fn subtitle_key(self) -> &'static str {
        match self {
            Page::Overview => "page.overview.subtitle",
            Page::Tools => "superba.tools.subtitle",
            Page::Troubleshooting => "simple.troubleshooting",
            Page::Providers => "page.providers.subtitle",
            Page::Models => "page.models.subtitle",
            Page::Router => "page.router.subtitle",
            Page::Usage => "page.usage.subtitle",
            Page::Settings => "page.settings.subtitle",
            Page::Sessions => "page.sessions.subtitle",
            Page::Hosts => "page.hosts.subtitle",
            Page::ClaudeCode => "page.claude_code.subtitle",
            Page::ClaudeSettings => "page.claude_settings.subtitle",
            Page::Tool(_) => "page.tool.subtitle",
            Page::Superba(section) => section.subtitle_key(),
        }
    }

    /// Navigation icon.
    pub fn icon(self) -> Icon {
        match self {
            Page::Overview => Icon::Overview,
            Page::Tools => Icon::Tools,
            Page::Troubleshooting => Icon::Help,
            Page::Providers => Icon::Plug,
            Page::Models => Icon::Model,
            Page::Router => Icon::Route,
            Page::Usage => Icon::Chart,
            Page::Settings => Icon::Settings,
            Page::Sessions => Icon::Chat,
            Page::Hosts => Icon::Server,
            Page::ClaudeCode => Icon::Cloud,
            Page::ClaudeSettings => Icon::Terminal,
            Page::Tool(_) => Icon::Tools,
            Page::Superba(_) => Icon::Superba,
        }
    }
}

/// A marker on a sidebar item (UX §2.1): a plain 6 px dot or a tabular number in
/// `text_subtle`, never a colored pill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavBadge {
    /// Nothing.
    #[default]
    None,
    /// A dot (router active: green; update available: neutral).
    Dot(Status),
    /// A count (hosts with unsynced settings).
    Count(u32),
}

/// What a page module reports to the shell without drawing (see the module
/// contract).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageStatus {
    /// A dialog of the page is open: the shell leaves Enter / Escape to it.
    pub modal: bool,
    /// Unsaved edits (amber sidebar dot; Ctrl/Cmd+S sends `save`).
    pub pending: usize,
    /// Sidebar badge.
    pub badge: NavBadge,
    /// Tooltip of the badge (translated).
    pub badge_tip: Option<String>,
    /// Not in the sidebar now (a tool that is not installed).
    pub hidden: bool,
}

impl Action {
    /// Restore through the built-in profile so the service can ask about
    /// session visibility before choosing a different provider policy.
    pub fn restore_original() -> Self {
        Self::Switch {
            profile: ProfileId::new(superba_core::catalog::PROFILE_ORIGINAL),
            line: None,
            auth: None,
        }
    }
}

/// Something a page asks the app to do.
#[derive(Debug)]
pub enum Action {
    /// A troubleshooting run asks the global owner to prepare and plan a repair.
    TroubleshootingRepair {
        /// Originating run.
        run: u64,
        /// Recovery operation.
        repair: troubleshooting::Repair,
    },
    /// Change the desktop navigation mode and persist it.
    SetSimpleMode(bool),
    /// One explicit simple-mode click authorizes the complete session batch.
    SessionBatch {
        /// Restore every migration, otherwise move every non-openai session.
        rollback: bool,
    },
    /// Navigate.
    Go(Page),
    /// Reload overview, profiles, sites.
    Refresh,
    /// Probe the lines of a site.
    Probe(SiteId),
    /// Save a profile (connection edits) and optionally its new key.
    SaveProfile {
        /// The edited profile.
        profile: Box<Profile>,
        /// A new key to store with it.
        key: Option<Secret>,
    },
    /// Check a key on one line of any site: the stored key of `profile`, or `key`.
    CheckKey {
        /// Site.
        site: SiteId,
        /// Profile whose stored key is checked (when `key` is `None`).
        profile: Option<ProfileId>,
        /// Line.
        line: LineId,
        /// Mode.
        mode: AuthMode,
        /// A typed key.
        key: Option<Secret>,
    },
    /// Reload identity and balance of the applied key-based profile.
    LoadAccount {
        /// Bypass the short cache.
        refresh: bool,
    },
    /// Switch to a profile with optional overrides.
    Switch {
        /// Profile.
        profile: ProfileId,
        /// Line override.
        line: Option<LineId>,
        /// Auth override.
        auth: Option<AuthMode>,
    },
    /// Restore.
    Restore(RestoreTarget),
    /// Plan an arbitrary request.
    Plan(Box<SwitchRequest>),
    /// Launch the app through Superba.
    OpenApp,
    /// Full restart through Superba (plans `RestartApp`).
    RestartApp,
    /// Persist the edited settings copy.
    SaveSettings,
    /// Change the current user’s GUI login registration.
    SetAutostart(bool),
    /// Change the UI language (`None` = system).
    SetLocale(Option<Locale>),
    /// Open a URL in the browser.
    OpenUrl(String),
    /// Copy text.
    Copy(String),
    /// Copy the shared diagnostic report with the current troubleshooting details.
    CopyDiagnostics(String),
    /// Load sessions.
    LoadSessions,
    /// Migrate every session of `provider` to `openai` (full restart).
    MigrateProvider(String),
    /// Roll back a migration (full restart).
    RollbackMigration(String),
    /// Run diagnostics.
    RunDoctor,
    /// Open "Codex in effect": what every running Codex uses (GH#4).
    ShowRuntime,
    /// Open the first-run wizard.
    StartWizard,
    /// GH#5: show what another tool changed in what Superba manages.
    ShowExternal,
    /// Set the speed tier of a key-based profile (`ModelDefaults::speed`) and,
    /// when it is the applied profile, plan the switch that applies it (hot
    /// restart, v0.3 §11).
    SetSpeed {
        /// Profile.
        profile: ProfileId,
        /// New tier.
        tier: SpeedTier,
    },
    /// Check for updates now (versions with `refresh`); also refreshes the
    /// model-list feed.
    CheckUpdates,
    /// Reload the remote hosts the app is connected to (sidebar badge,
    /// Overview).
    LoadHosts,
    /// Check for a newer Superba now (the Updates page, the command palette).
    CheckSelfUpdate,
    /// Use this Superba update channel (the Updates page; saved like the
    /// Preferences choice, then checked at once).
    SetUpdateChannel(superba_core::UpdateChannel),
    /// Skip a version of Superba (`None` clears the skip).
    SkipSelfUpdate(Option<String>),
    /// "Later": stop reminding about this Superba version until the next start
    /// (the sidebar dot and the Overview to-do).
    LaterSelfUpdate(String),
    /// Quit Superba (an update was installed and the new version starts by
    /// itself once this process has exited).
    QuitApp,
    /// Reload [`Data::versions`] from the cached lists (no online check), e.g.
    /// after an install changed what is installed.
    LoadVersions,
    /// Refresh both the installed Codex CLI and desktop client against online releases.
    CheckCodexUpdates,
    /// Open Superba's data directory in the file manager.
    OpenDataDir,
    /// The data pointer changed; refresh shell paths and cached page snapshots.
    DataLocationChanged(superba_core::data::DataLocation),
    /// Uninstall Superba's changes.
    Uninstall,
    /// Run a command of a page module (see the module contract); the shell
    /// navigates to `page` first.
    PageCommand {
        /// Page that handles the command.
        page: Page,
        /// Command string defined by that page (e.g. `focus:<config path>`).
        command: String,
    },
    /// Show a toast.
    Toast {
        /// Severity.
        status: Status,
        /// Text (already translated).
        text: String,
    },
    /// A provider was saved without switching (adding one only saves it): a toast
    /// that says so and offers "Switch to this provider", an explicit second step.
    OfferSwitch {
        /// Title (already translated).
        text: String,
        /// The saved provider.
        profile: ProfileId,
    },
}

/// Loaded data with loading / error state.
#[derive(Debug)]
pub struct Load<T> {
    /// Last value.
    pub value: Option<T>,
    /// A request is in flight.
    pub loading: bool,
    /// Last error.
    pub error: Option<Error>,
}

impl<T> Default for Load<T> {
    fn default() -> Self {
        Self {
            value: None,
            loading: false,
            error: None,
        }
    }
}

impl<T> Load<T> {
    /// Mark loading (keeps the old value visible).
    pub fn start(&mut self) {
        self.loading = true;
    }

    /// Store a result.
    pub fn finish(&mut self, result: superba_core::Result<T>) {
        self.loading = false;
        match result {
            Ok(v) => {
                self.value = Some(v);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

/// `HH:MM` (or `HH:MM:SS`) of `t` in the local time zone; in UTC with a
/// "UTC" suffix where the local offset is unknown.
pub fn clock_time(t: Timestamp, seconds: bool) -> String {
    superba_switch::localtime::local(t).time_of_day(seconds)
}

/// The local UTC offset at `t` in seconds (time-zone rules of the system, on
/// Windows too: user bug 62(b) showed every time in UTC there).
pub fn local_offset_secs(t: Timestamp) -> Option<i64> {
    superba_switch::localtime::offset_secs(t)
}

/// Everything the backend told us.
#[derive(Debug, Default)]
pub struct Data {
    /// Overview.
    pub overview: Load<Overview>,
    /// Profiles and settings.
    pub profiles: Load<ProfileSet>,
    /// Sites.
    pub sites: Vec<Site>,
    /// Line probes per site.
    pub caps: BTreeMap<SiteId, Load<Vec<LineCaps>>>,
    /// Latest key checks per (line, mode).
    pub key_checks: HashMap<(LineId, AuthMode), superba_core::Result<KeyCheck>>,
    /// Key checks in flight.
    pub key_checking: HashSet<(LineId, AuthMode)>,
    /// Sessions.
    pub sessions: Load<(SessionReport, Vec<MigrationReceipt>)>,
    /// Diagnostics.
    pub doctor: Load<Vec<DoctorFinding>>,
    /// Diagnostics of Claude Code (loaded with [`Data::doctor`]).
    pub claude_diag: Load<superba_core::ClaudeDiagnostics>,
    /// Identity and balance of the applied key-based profile.
    pub account: Load<superba_core::AccountReport>,
    /// Last router watchdog state (while router mode is on).
    pub router_watch: Option<superba_core::RouterWatchState>,
    /// Saved routing behavior, loaded by the shell at startup and on polls.
    /// `None` until read (older configurations use the automatic defaults).
    pub router_failover: Option<superba_core::RouterFailover>,
    /// Whether the tray icon is shown (Preferences → Window; item 70).
    pub tray: crate::tray::TrayStatus,
    /// Settings diff of the latest plan: (plan id, preview).
    pub plan_settings: Option<(String, superba_core::PlanSettings)>,
    /// Remote hosts the desktop app is connected to (app records only), with
    /// their follow toggle and pending sync rows: the sidebar badge and the
    /// Overview's hosts column. Loaded by the shell; pages may read it.
    pub hosts: Load<Vec<HostView>>,
    /// Versions of the app and Codex (the sidebar's update dot, the Overview's
    /// "this computer" column). Loaded by the shell; pages may read it.
    pub versions: Load<VersionsView>,
    /// Superba's own update (the Updates page's first section, the Overview
    /// to-do, the sidebar dot). Loaded by the shell.
    pub selfupdate: Load<superba_core::SelfUpdateView>,
    /// The Superba version the user answered "later" to in this session.
    pub selfupdate_later: Option<String>,
    /// State of the model-list / price / version feeds (Diagnostics).
    pub feeds: Load<FeedsStatus>,
    /// Models view of the applied key-based profile (the Overview's speed
    /// control: offered tiers and the per-consumer effect).
    pub active_models: Load<ModelsView>,
    /// Superba's own paths (Diagnostics); `None` in tests.
    pub paths: Option<DataPaths>,
    /// ChatGPT is being opened through Superba (GH#7; the hero's button spins).
    pub launching_app: bool,
    /// Last failed Codex installs by install target; shared by both home pages.
    pub tool_update_errors: BTreeMap<String, String>,
    /// Unified inventory count shared by the rail and sidebar after polling.
    pub update_inventory_count: Option<u32>,
    /// Enabled notifications have missing, untrusted or unreadable hooks.
    pub notify_hooks_attention: bool,
}

/// Superba's own paths, shown on Diagnostics and opened from Preferences.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataPaths {
    /// Superba's data directory.
    pub data_dir: PathBuf,
    /// The router's request log (`<data>/router/usage.sqlite3`).
    pub router_db: PathBuf,
}

impl Data {
    /// Claude Code's `settings.json` points at the local router.
    pub fn claude_routed(&self) -> bool {
        self.overview
            .value
            .as_ref()
            .is_some_and(|o| o.claude_routed)
    }

    /// Router mode is on in the saved settings.
    pub fn router_enabled(&self) -> bool {
        self.profiles
            .value
            .as_ref()
            .is_some_and(|p| p.settings.router.enabled)
    }

    /// Automatic line selection is distinct from the local forwarding service.
    pub fn router_failover_enabled(&self) -> bool {
        self.router_enabled() && self.router_failover.as_ref().is_none_or(|f| f.enabled)
    }

    /// One configured exit, independent of the last observed request.
    pub fn router_fixed_exit(&self) -> Option<(&Profile, LineId)> {
        if !self.router_enabled() || self.router_failover_enabled() {
            return None;
        }
        let set = self.profiles.value.as_ref()?;
        let route = self.router_failover.as_ref()?.fixed_route.as_ref();
        let profile = if let Some(route) = route {
            set.profiles
                .iter()
                .find(|p| superba_router::members::router_id(p.id.as_str()) == route.member_id)?
        } else {
            set.settings
                .router
                .members
                .iter()
                .find_map(|id| set.profile(id))?
        };
        let site = self.site(&profile.site_id)?;
        let line = if let Some(route) = route {
            site.lines
                .iter()
                .find(|l| superba_router::members::router_id(l.id.as_str()) == route.line_id)?
                .id
                .clone()
        } else {
            profile.enabled_line_ids(&site).into_iter().next()?
        };
        Some((profile, line))
    }

    /// The settings diff of `plan_id`, when it has arrived.
    pub fn plan_settings_for(&self, plan_id: &str) -> Option<&superba_core::PlanSettings> {
        self.plan_settings
            .as_ref()
            .filter(|(id, _)| id == plan_id)
            .map(|(_, s)| s)
    }

    /// Hosts with settings that a sync would change now (sidebar count, UX §2.1).
    pub fn unsynced_hosts(&self) -> u32 {
        self.hosts.value.as_ref().map_or(0, |hosts| {
            hosts
                .iter()
                .filter(|h| h.host.is_managed() && h.pending_sync.is_some_and(|n| n > 0))
                .count() as u32
        })
    }

    /// A newer Superba the user has neither skipped nor put off ("later").
    pub fn self_update_offered(&self) -> Option<&superba_core::SelfRelease> {
        let view = self.selfupdate.value.as_ref()?;
        let latest = view.latest.as_ref()?;
        (view.offered() && self.selfupdate_later.as_deref() != Some(latest.version.as_str()))
            .then_some(latest)
    }

    /// Unified inventory count, with shell data used until the first snapshot.
    pub fn update_count(&self) -> u32 {
        if let Some(count) = self.update_inventory_count {
            return count;
        }
        u32::from(self.self_update_offered().is_some())
            + self.versions.value.as_ref().map_or(0, |v| {
                u32::from(v.app.update_available)
                    + u32::from(
                        v.cli
                            .iter()
                            .any(|r| !matches!(r.action, CliUpdateAction::None)),
                    )
            })
    }

    /// Simple mode counts only Superba, the Codex client and local Codex CLI.
    pub fn simple_update_count(&self) -> u32 {
        u32::from(self.self_update_offered().is_some())
            + self.versions.value.as_ref().map_or(0, |v| {
                u32::from(v.app.update_available)
                    + u32::from(
                        v.cli
                            .iter()
                            .any(|r| !matches!(r.action, CliUpdateAction::None)),
                    )
            })
    }

    /// A newer Superba, ChatGPT app or a newer Codex CLI for an installed binary
    /// exists (sidebar dot, UX §2.1).
    pub fn update_available(&self) -> bool {
        self.self_update_offered().is_some()
            || self.versions.value.as_ref().is_some_and(|v| {
                v.app.update_available
                    || v.cli
                        .iter()
                        .any(|row| !matches!(row.action, CliUpdateAction::None))
            })
    }
}

/// Generic lookups of the applied provider (UX §2.2): pages read these, never a
/// fixed site id. A provider is a profile: its site, ordered lines and key.
impl Data {
    /// A site by id: the backend's list, else the built-in sites.
    pub fn site(&self, id: &SiteId) -> Option<Site> {
        self.sites
            .iter()
            .find(|s| &s.id == id)
            .cloned()
            .or_else(|| catalog::builtin_sites().into_iter().find(|s| &s.id == id))
    }

    /// The applied state, when Superba applied something.
    pub fn active_state(&self) -> Option<&superba_core::ActiveState> {
        self.overview.value.as_ref()?.active.as_ref()
    }

    /// Id of the applied site.
    pub fn active_site_id(&self) -> Option<&SiteId> {
        if let Some((profile, _)) = self.router_fixed_exit() {
            return Some(&profile.site_id);
        }
        self.active_state()?.site_id.as_ref()
    }

    /// The applied site (official login included).
    pub fn active_site(&self) -> Option<Site> {
        self.site(self.active_site_id()?)
    }

    /// The official ChatGPT login is applied.
    pub fn official_active(&self) -> bool {
        if self.router_fixed_exit().is_some() {
            return false;
        }
        self.active_site()
            .is_some_and(|s| s.kind == superba_core::SiteKind::Official)
            || self.active_state().is_some_and(|a| {
                a.profile_id.as_ref().is_some_and(|id| {
                    self.profile(id)
                        .is_some_and(|p| p.builtin == Some(BuiltinProfile::Official))
                })
            })
    }

    /// A profile by id.
    pub fn profile(&self, id: &ProfileId) -> Option<&Profile> {
        self.profiles
            .value
            .as_ref()?
            .profiles
            .iter()
            .find(|p| &p.id == id)
    }

    /// Key-based providers (profiles that are not built in), in profile-set
    /// order.
    pub fn providers(&self) -> Vec<&Profile> {
        self.profiles.value.as_ref().map_or_else(Vec::new, |set| {
            set.profiles
                .iter()
                .filter(|p| p.builtin.is_none())
                .collect()
        })
    }

    /// The applied key-based provider: the applied profile, else (an older
    /// state without a profile id) the first provider on the applied site.
    pub fn active_profile(&self) -> Option<&Profile> {
        if let Some((profile, _)) = self.router_fixed_exit() {
            return Some(profile);
        }
        let active = self.active_state()?;
        if let Some(id) = &active.profile_id {
            return self.profile(id).filter(|p| p.builtin.is_none());
        }
        let site = active.site_id.as_ref()?;
        self.providers().into_iter().find(|p| &p.site_id == site)
    }

    /// The probe of one line of `site`.
    pub fn site_caps(&self, site: &SiteId, line: &LineId) -> Option<&LineCaps> {
        self.caps
            .get(site)?
            .value
            .as_ref()?
            .iter()
            .find(|c| &c.line_id == line)
    }

    /// True while the lines of `site` are being probed.
    pub fn probing_site(&self, site: &SiteId) -> bool {
        self.caps.get(site).is_some_and(|l| l.loading)
    }

    /// The site whose lines the shell probes on start and for the line tiles:
    /// the applied site when it has lines, else the first provider's site, else
    /// the default preset's site.
    pub fn lines_site_id(&self) -> Option<SiteId> {
        let with_lines = |id: &SiteId| self.site(id).is_some_and(|s| !s.lines.is_empty());
        if let Some(id) = self.active_site_id().filter(|id| with_lines(id)) {
            return Some(id.clone());
        }
        if let Some(p) = self
            .providers()
            .into_iter()
            .find(|p| with_lines(&p.site_id))
        {
            return Some(p.site_id.clone());
        }
        catalog::presets()
            .into_iter()
            .find(|p| p.default && !p.site.lines.is_empty())
            .map(|p| p.site.id)
    }

    /// The display name of `line` of `site` (default: the applied site, else the
    /// site the shell probes); the line id when that site has no such line.
    pub fn line_label(&self, tr: &Tr, site: Option<&SiteId>, line: &LineId) -> String {
        let site = match site {
            Some(id) => self.site(id),
            None => self
                .active_site()
                .or_else(|| self.lines_site_id().and_then(|id| self.site(&id))),
        };
        site.and_then(|s| s.line(line).map(|l| tr.text(&l.name)))
            .unwrap_or_else(|| line.to_string())
    }

    /// The display name of a site: a built-in site always uses its translated
    /// built-in name (the backend's copy may carry an untranslated one).
    pub fn site_name(&self, tr: &Tr, site: &Site) -> String {
        catalog::builtin_sites()
            .into_iter()
            .find(|b| b.id == site.id)
            .map_or_else(|| tr.text(&site.name), |b| tr.text(&b.name))
    }

    /// Masked key suffix of the observed auth.json when it belongs to the
    /// applied site (never another site's key).
    pub fn active_key_last4(&self) -> Option<&str> {
        let overview = self.overview.value.as_ref()?;
        overview.active.as_ref()?.site_id.as_ref()?;
        auth_last4(&overview.config.as_ref()?.auth)
    }
}

/// Suffix safe for display from the observed authentication shape.
pub fn auth_last4(auth: &AuthShape) -> Option<&str> {
    match auth {
        AuthShape::ApiKey { key_last4 } | AuthShape::Pat { key_last4 } => Some(key_last4),
        _ => None,
    }
}

/// The mode Automatic authentication uses on a line: PAT unless the probe says
/// the line cannot do PAT but takes an API key.
pub fn auth_mode_for_line(preferred: AuthMode, caps: Option<&LineCaps>) -> AuthMode {
    if preferred == AuthMode::Pat && caps.is_some_and(|c| !c.allows(AuthMode::Pat)) {
        AuthMode::ApiKey
    } else {
        preferred
    }
}

/// Shared severity for key checks on both the wizard and Lines page.
pub fn key_access_status(access: &KeyAccess) -> Status {
    match access {
        KeyAccess::Ok => Status::Ok,
        KeyAccess::InvalidKey | KeyAccess::QuotaExhausted | KeyAccess::ChannelDenied => {
            Status::Error
        }
        _ => Status::Warn,
    }
}

/// Shared PAT severity for the table and its explanatory notes.
pub fn pat_status(caps: &LineCaps) -> Status {
    if caps.allows(AuthMode::Pat) {
        Status::Ok
    } else if caps.allows(AuthMode::ApiKey) || caps.blocked_by_edge {
        Status::Warn
    } else {
        Status::Error
    }
}

/// Text that fits in its cell, with the complete value available on hover.
pub fn elided(ui: &mut Ui, text: RichText) -> egui::Response {
    let full = text.text().to_owned();
    ui.add(egui::Label::new(text).truncate())
        .on_hover_text(full)
}

#[derive(Debug, Default)]
struct KeySuffix {
    task: Option<crate::runtime::ReadTask<Option<String>>>,
    value: Option<String>,
    stamp: Option<Timestamp>,
}

/// Cached metadata for a stored profile, shared by its account and key cards.
/// The cache holds only the masked suffix, never the credential itself.
pub fn profile_key_suffix(ui: &mut Ui, cx: &Cx<'_>, profile: Option<&Profile>) -> Option<String> {
    let profile = profile?;
    let reference = profile.key_ref.as_ref()?;
    let stamp = cx.data.overview.value.as_ref().map(|o| o.generated_at);
    let id = egui::Id::unique(("profile-key-suffix", &profile.id, reference));
    let state = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_insert_with(id, || {
            std::sync::Arc::new(std::sync::Mutex::new(KeySuffix {
                task: Some(crate::runtime::profile_key_task(reference.clone())),
                value: None,
                stamp,
            }))
        })
        .clone()
    });
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    // Refresh an overwritten reference without accumulating a cache entry per
    // overview poll or making the previous suffix flicker while the read runs.
    if state.stamp != stamp {
        state.stamp = stamp;
        state.task = Some(crate::runtime::profile_key_task(reference.clone()));
    }
    if let Some(task) = &state.task {
        if let Some(result) = task.poll() {
            state.value = result.ok().flatten();
            state.task = None;
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    state.value.clone()
}

/// A one-line tinted callout (notices, account problems).
pub fn callout(ui: &mut Ui, cx: &Cx<'_>, status: Status, text: &str) {
    let fill = if status == Status::Idle {
        cx.pal.bg_subtle
    } else {
        cx.pal.tint(cx.pal.status(status))
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, cx.pal.border))
        .corner_radius(radius::CONTROL + 2)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 18.0), Sense::hover());
                ui.painter()
                    .circle_filled(r.center(), 3.5, cx.pal.status(status));
                ui.add(egui::Label::new(Text::small(text, cx.pal).color(cx.pal.text)).wrap());
            });
        });
}

/// A bounded note whose severity agrees with the corresponding table cell.
pub fn note(ui: &mut Ui, cx: &Cx<'_>, status: Status, text: impl Into<String>) {
    ui.horizontal_top(|ui| {
        ui.add(crate::design::widgets::StatusDot::new(status));
        ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
    });
}

/// Armed destination published by the shell each frame. Keeping this in egui's
/// transient data avoids giving pages ownership of the switching state machine.
#[derive(Clone, Debug)]
pub struct ArmedSwitch {
    /// Destination of the armed plan.
    pub destination: superba_core::PlanDestination,
    /// Countdown rounded up to whole seconds, absent while paused.
    pub seconds: Option<u64>,
}

impl ArmedSwitch {
    /// Publish the current armed plan, or clear it on cancellation / execution.
    pub fn publish(ctx: &egui::Context, armed: Option<Self>) {
        ctx.data_mut(|data| {
            let id = egui::Id::unique("superba-pages-armed-switch");
            if let Some(armed) = armed {
                data.insert_temp(id, armed);
            } else {
                data.remove::<Self>(id);
            }
        });
    }

    /// Read the current countdown without mutating the flow.
    pub fn current(ctx: &egui::Context) -> Option<Self> {
        ctx.data(|data| data.get_temp(egui::Id::unique("superba-pages-armed-switch")))
    }
}

/// Render a domain text, replacing `line` / `site` arguments that hold a line id or
/// an English line name with the localized name (texts built by other crates pass
/// fallback strings as arguments).
pub fn localize_text(tr: &Tr, data: &Data, text: &superba_core::LocalizedText) -> String {
    // The applied site first, then every known site (built-in ones included).
    let mut sites: Vec<Site> = data.active_site().into_iter().collect();
    for s in data.sites.iter().cloned().chain(catalog::builtin_sites()) {
        if !sites.iter().any(|known| known.id == s.id) {
            sites.push(s);
        }
    }
    let mut t = text.clone();
    for (name, value) in &mut t.args {
        if name == "line" {
            if let Some(line) = sites
                .iter()
                .flat_map(|s| s.lines.iter())
                .find(|l| l.id.as_str() == value.as_str() || l.name.fallback == *value)
            {
                *value = tr.text(&line.name);
            }
        } else if name == "site"
            && let Some(site) = sites
                .iter()
                .find(|s| value.as_str() == s.id.as_str() || *value == s.name.fallback)
        {
            *value = tr.text(&site.name);
        }
    }
    tr.text(&t)
}

/// Localized destination of a plan: "site · line · auth" built from its ids for
/// a provider with several lines (the line id when it is unknown), otherwise
/// the backend's label (a single-line site is named by its label alone).
pub fn dest_text(tr: &Tr, data: &Data, to: &superba_core::PlanDestination) -> String {
    let site = to.site_id.as_ref().and_then(|id| data.site(id));
    match (site, &to.line_id, to.auth_mode) {
        (Some(site), Some(line), Some(mode)) if site.lines.len() > 1 => {
            let name = site
                .line(line)
                .map(|l| tr.text(&l.name))
                .unwrap_or_else(|| line.to_string());
            let auth = if mode == AuthMode::Pat {
                tr.t("auth.short.pat")
            } else if mode.uses_key() {
                tr.t("auth.short.apikey")
            } else {
                tr.t("auth.short.chatgpt")
            };
            format!("{} · {} · {}", data.site_name(tr, &site), name, auth)
        }
        _ => localize_text(tr, data, &to.label),
    }
}

/// One activity-log line.
#[derive(Clone, Debug)]
pub struct LogLine {
    /// Wall time.
    pub at: Timestamp,
    /// Severity.
    pub status: Status,
    /// Text (already rendered).
    pub text: String,
}

/// Read-only context for drawing a page.
pub struct Cx<'a> {
    /// Translator.
    pub tr: &'a Tr,
    /// Palette.
    pub pal: &'static Palette,
    /// Data.
    pub data: &'a Data,
    /// A switch is planned or running: switch buttons are disabled.
    pub busy: bool,
    /// Wall-clock now.
    pub now: Timestamp,
    /// Fonts found.
    pub fonts: &'a FontReport,
    /// The switching service, for page-owned background reads
    /// ([`crate::runtime::task`]).
    pub service: &'a std::sync::Arc<dyn superba_core::SwitchService>,
    /// Activity log.
    pub log: &'a [LogLine],
}

impl Cx<'_> {
    /// Localized destination of a plan.
    pub fn dest(&self, to: &superba_core::PlanDestination) -> String {
        dest_text(self.tr, self.data, to)
    }

    /// Render a domain text with localized line names.
    pub fn localize(&self, text: &superba_core::LocalizedText) -> String {
        localize_text(self.tr, self.data, text)
    }

    /// Display name of a line (the applied site first, then every known site).
    pub fn line_name(&self, line: &LineId) -> String {
        self.data.line_label(self.tr, None, line)
    }

    /// "x min ago" style relative time.
    pub fn ago(&self, t: Timestamp) -> String {
        let secs = (self.now.as_millis() - t.as_millis()) / 1000;
        if secs < 0 {
            let days = (-secs) / 86_400;
            if days >= 1 {
                return self.tr.f("time.in_days", &[("n", &days)]);
            }
            let hours = ((-secs) / 3600).max(1);
            return self.tr.f("time.in_hours", &[("n", &hours)]);
        }
        match secs {
            0..=44 => self.tr.t("time.just_now"),
            45..=3599 => self.tr.f("time.minutes_ago", &[("n", &(secs / 60).max(1))]),
            3600..=86_399 => self.tr.f("time.hours_ago", &[("n", &(secs / 3600))]),
            _ => self.tr.f("time.days_ago", &[("n", &(secs / 86_400))]),
        }
    }

    /// Badge status + text of the hot-switch state.
    pub fn hot_status(&self, s: &HotSwitchStatus) -> (Status, String) {
        match s {
            HotSwitchStatus::Ready => (Status::Ok, self.tr.t("hot.ready")),
            HotSwitchStatus::Unavailable { reason } => (
                Status::Warn,
                self.tr.f(
                    "hot.unavailable",
                    &[("reason", &self.tr.text(&reason.text()))],
                ),
            ),
            HotSwitchStatus::NotRunning => (Status::Idle, self.tr.t("hot.not_running")),
            HotSwitchStatus::NotInstalled => (Status::Idle, self.tr.t("hot.not_installed")),
            HotSwitchStatus::SafeMode => (Status::Idle, self.tr.t("hot.safe_mode")),
        }
    }

    /// Short hot-switch label (sidebar).
    pub fn hot_short(&self, s: &HotSwitchStatus) -> (Status, String) {
        match s {
            HotSwitchStatus::Ready => (Status::Ok, self.tr.t("hot.ready")),
            HotSwitchStatus::Unavailable { .. } => (Status::Warn, self.tr.t("hot.full_only")),
            HotSwitchStatus::NotRunning => (Status::Idle, self.tr.t("hot.not_running")),
            HotSwitchStatus::NotInstalled => (Status::Idle, self.tr.t("hot.not_installed")),
            HotSwitchStatus::SafeMode => (Status::Idle, self.tr.t("hot.safe_mode")),
        }
    }

    /// "Provider · Line" (site and line, without the auth mode) for the active
    /// state.
    pub fn destination_label(&self) -> Option<String> {
        let label = self.active_label()?;
        let o = self.data.overview.value.as_ref()?;
        let a = o.active.as_ref()?;
        Some(match a.auth_mode {
            Some(m) => label
                .strip_suffix(&format!(" · {}", self.auth_short(m)))
                .map(str::to_owned)
                .unwrap_or(label),
            None => label,
        })
    }

    /// "Provider · Line · PAT" for the active state.
    pub fn active_label(&self) -> Option<String> {
        let a = self.data.active_state()?;
        let site = a.site_id.as_ref().and_then(|id| self.data.site(id));
        let name = match (a.site_id.as_ref(), &site) {
            (Some(_), Some(s)) => self.data.site_name(self.tr, s),
            (Some(id), None) => id.to_string(),
            (None, _) => self.tr.t("overview.unknown_site"),
        };
        let mut parts = vec![name];
        if let Some(l) = &a.line_id {
            parts.push(
                site.as_ref()
                    .and_then(|s| s.line(l))
                    .map(|l| self.tr.text(&l.name))
                    .unwrap_or_else(|| l.to_string()),
            );
        }
        if let Some(m) = a.auth_mode {
            parts.push(self.auth_short(m));
        }
        Some(parts.join(" · "))
    }

    /// Short auth label ("PAT", "API key", "Official login").
    pub fn auth_short(&self, mode: AuthMode) -> String {
        if mode == AuthMode::Pat {
            self.tr.t("auth.short.pat")
        } else if mode.uses_key() {
            self.tr.t("auth.short.apikey")
        } else {
            self.tr.t("auth.short.chatgpt")
        }
    }
}

/// Compact page header: title and subtitle on the left, actions on the right.
/// Superba sections wrap their subtitles inside the scrolling detail pane.
pub fn page_header(ui: &mut Ui, cx: &Cx<'_>, page: Page, actions: impl FnOnce(&mut Ui)) {
    // Subtitles may name Codex's current `model_provider` (Sessions).
    let provider = cx
        .data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.config.as_ref())
        .and_then(|c| c.model_provider.clone())
        .unwrap_or_else(|| superba_core::codex::PROVIDER_OPENAI.to_owned());
    let subtitle = cx.tr.f(page.subtitle_key(), &[("provider", &provider)]);
    page_header_with(ui, cx, page, &subtitle, actions);
}

/// [`page_header`] with an already formatted subtitle (a page whose subtitle
/// depends on its own state, e.g. Sessions on a remote host).
pub fn page_header_with(
    ui: &mut Ui,
    cx: &Cx<'_>,
    page: Page,
    subtitle: &str,
    actions: impl FnOnce(&mut Ui),
) {
    // A large bold title and a gray subtitle (user request 24, after Surge), no
    // divider: the page sits in its rounded panel. Fit pages budget exactly
    // `size::PAGE_HEADER`; Superba's scrolling detail pane can grow as needed.
    let top = ui.cursor().min.y;
    // The actions keep the width they used last frame so the subtitle cannot
    // squeeze them at the 960 px window size.
    let actions_id = egui::Id::unique(("superba-page-header-actions", page));
    let actions_w: f32 = ui.data(|d| d.get_temp(actions_id)).unwrap_or(0.0);
    ui.horizontal(|ui| {
        let left_max = (ui.available_width() - actions_w - 16.0).max(120.0);
        ui.vertical(|ui| {
            ui.set_max_width(left_max);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(Text::page_title(cx.tr.t(page.title_key()), cx.pal));
            let label = egui::Label::new(Text::small(subtitle, cx.pal));
            ui.add(if page.is_superba() {
                label.wrap()
            } else {
                label.truncate()
            });
        });
        let used = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                actions(ui);
                ui.min_rect().width()
            })
            .inner;
        if (used - actions_w).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(actions_id, used));
            ui.ctx().request_repaint();
        }
    });
    let used = ui.cursor().min.y - top;
    ui.add_space((crate::design::tokens::size::PAGE_HEADER - used).max(0.0));
}

/// Section title above flat content (no card), with an optional subtitle.
pub fn section(ui: &mut Ui, cx: &Cx<'_>, title: &str, subtitle: Option<&str>) {
    section_with(ui, cx, title, subtitle, |_| {});
}

/// [`section`] with actions on the right of the title row.
pub fn section_with(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    subtitle: Option<&str>,
    actions: impl FnOnce(&mut Ui),
) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.horizontal(|ui| {
            ui.label(Text::section(title, cx.pal));
            ui.with_layout(Layout::right_to_left(Align::Center), actions);
        });
        // The subtitle wraps at the column's width under the title row (wrapped
        // in the row next to the actions it made the row wider than the column).
        if let Some(s) = subtitle {
            ui.add(egui::Label::new(Text::small(s, cx.pal)).wrap());
        }
    });
    ui.add_space(8.0);
}

/// A full-width hairline.
pub fn divider(ui: &mut Ui, cx: &Cx<'_>) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        r.x_range(),
        r.center().y,
        egui::Stroke::new(1.0, cx.pal.border),
    );
}

/// Space, a hairline and space again between two flat sections.
pub fn section_gap(ui: &mut Ui, cx: &Cx<'_>) {
    ui.add_space(20.0);
    divider(ui, cx);
    ui.add_space(20.0);
}

/// A label / value row inside a card: the label and the value's widgets share
/// one center line ([`FormRow`](crate::design::widgets::FormRow); a small button
/// in the value grows the row to its height).
pub fn kv(ui: &mut Ui, cx: &Cx<'_>, label: &str, value: impl FnOnce(&mut Ui)) {
    let label_w = (ui.available_width() * 0.3).clamp(90.0, 128.0);
    crate::design::widgets::FormRow::new(Text::small(label, cx.pal))
        .label_width(label_w)
        .gap(6.0)
        .height(24.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // Long values are truncated instead of widening the card.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            value(ui);
        });
}

/// Centered empty / error state inside a card.
pub fn empty_state(ui: &mut Ui, cx: &Cx<'_>, icon: Icon, title: &str, body: &str) {
    Card::new().padding(32).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), egui::Sense::hover());
            ui.painter().rect(
                rect,
                10,
                cx.pal.hover,
                egui::Stroke::new(1.0, cx.pal.border),
                egui::StrokeKind::Inside,
            );
            icons::paint(ui.painter(), rect.shrink(9.0), icon, cx.pal.text_muted);
            ui.add_space(8.0);
            ui.label(Text::strong(title, cx.pal));
            ui.label(Text::small(body, cx.pal));
        });
    });
}

/// Error card for a failed load (shows the localized error).
pub fn error_state(ui: &mut Ui, cx: &Cx<'_>, title: &str, error: &Error) {
    let body = error_text(cx.tr, error);
    empty_state(ui, cx, Icon::Warning, title, &body);
}

/// Localized error text (falls back to the English `Display`).
pub fn error_text(tr: &Tr, error: &Error) -> String {
    if let Error::Conflict { message } = error {
        // These conflicts originate in the GUI runtime, so their explanations
        // live in the GUI catalogs rather than the shared domain catalog.
        match message.as_str() {
            "site.remove_active" => return tr.t("site.remove_active"),
            "site.id_taken" => return tr.t("site.id_taken"),
            _ => {}
        }
    }
    if let Error::Blocked { reasons } = error
        && !reasons.is_empty()
    {
        return reasons
            .iter()
            .map(|r| tr.text(&r.text()))
            .collect::<Vec<_>>()
            .join(" ");
    }
    let rendered = tr.text(&error.to_text());
    if rendered.contains("{detail}") || rendered.is_empty() {
        error.to_string()
    } else {
        rendered
    }
}

/// A small caption above a value.
pub fn stat(ui: &mut Ui, cx: &Cx<'_>, label: &str, value: RichText) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(Text::caption(label, cx.pal));
        ui.label(value);
    });
}

/// Lay out `n` equally wide columns whose cards (built with
/// `Card::fill_height`) share the tallest natural height, measured in the
/// previous frame.
pub fn grid_row(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    n: usize,
    mut add: impl FnMut(&mut Ui, usize),
) {
    let id = ui.scope_id().with(("grid_row", id));
    let target: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    let gap = 16.0;
    let width = ui.available_width();
    let col_w = ((width - gap * (n as f32 - 1.0)) / n as f32).floor();
    let mut tallest: f32 = 0.0;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for i in 0..n {
            ui.allocate_ui_with_layout(vec2(col_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(col_w);
                let cell = ui.scope_id();
                ui.ctx()
                    .data_mut(|d| d.insert_temp(cell.with("grid_target"), target));
                let top = ui.cursor().min.y;
                add(ui, i);
                let measured = ui.min_rect().max.y - top;
                let natural = ui
                    .ctx()
                    .data(|d| d.get_temp::<f32>(cell.with("grid_natural")))
                    .unwrap_or(measured);
                tallest = tallest.max(natural);
            });
        }
    });
    if (tallest - target).abs() > 0.5 {
        ui.ctx().data_mut(|d| d.insert_temp(id, tallest));
        ui.ctx().request_repaint();
    }
}
