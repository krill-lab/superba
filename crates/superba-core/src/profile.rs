//! Profiles (what to switch to) and the persisted profile set + settings.
//!
//! A profile is a connection (site, ordered line priority, auth choice and key)
//! plus the Codex settings Superba manages for it. `auth_mode` mirrors
//! [`Profile::auth`] for key-based profiles.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::auth::AuthMode;
use crate::compaction::CompactionChoice;
use crate::i18n::Locale;
use crate::ids::{LineId, ProfileId, SiteId, TargetId};
use crate::models::{ModelOverride, ModelPicker, SpeedTier};
use crate::secret::KeyRef;
use crate::settings::{AuthChoice, CodexSettings};
use crate::site::Site;
use crate::time::Timestamp;
use crate::versions::AppCodexPin;

/// Current `profiles.json` schema.
pub const PROFILE_SET_SCHEMA: u32 = 1;

/// A named switch destination: connection (site, lines, auth, key) plus the Codex
/// settings Superba manages for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable id.
    pub id: ProfileId,
    /// User-visible name (user data, not translated; built-ins are translated by id).
    pub name: String,
    /// Set for the two built-in profiles. When set it decides the behaviour and the
    /// site/line/auth fields below are placeholders (the "original" profile restores
    /// whatever the baseline holds).
    #[serde(default)]
    pub builtin: Option<BuiltinProfile>,
    /// Site.
    pub site_id: SiteId,
    /// Primary line; `None` means "the site's default / best probed line" (Krill)
    /// or not applicable (official, compatible with `base_url_override`).
    #[serde(default)]
    pub line_id: Option<LineId>,
    /// Backup lines in priority order, used after [`Profile::line_id`] (see
    /// [`Profile::line_priority`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backup_lines: Vec<LineId>,
    /// Auth mode. For key-based profiles it mirrors [`Profile::auth`]
    /// ([`AuthChoice::representative_mode`]); built-in profiles use
    /// [`AuthMode::OfficialChatGpt`].
    pub auth_mode: AuthMode,
    /// Auth choice of a key-based profile: `Auto` (default) or a manual mode.
    #[serde(default)]
    pub auth: AuthChoice,
    /// Where the key is stored; required when `auth_mode.uses_key()`.
    #[serde(default)]
    pub key_ref: Option<KeyRef>,
    /// Full `openai_base_url` for compatible sites (may contain a path such as `/v1`).
    #[serde(default)]
    pub base_url_override: Option<Url>,
    /// Model defaults; `None` fields mean "do not manage that key".
    #[serde(default)]
    pub model: ModelDefaults,
    /// Codex settings this profile manages (catalog path -> value or removal).
    /// Superba owns exactly these keys while the profile is applied.
    #[serde(default, skip_serializing_if = "CodexSettings::is_empty")]
    pub codex_settings: CodexSettings,
    /// Per-model overrides by model slug (context window, compaction, display
    /// name, hidden, upstream name). Any override makes Superba write a
    /// `model_catalog_json` file for the profile.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_overrides: BTreeMap<String, ModelOverride>,
    /// Which list the app's model picker shows (custom sites).
    #[serde(default, skip_serializing_if = "ModelPicker::is_default")]
    pub model_picker: ModelPicker,
    /// Remote compaction of this provider: what Superba's check found
    /// (`Auto`, the default) or the user's override ([`crate::compaction`]).
    #[serde(default, skip_serializing_if = "CompactionChoice::is_auto")]
    pub compaction: CompactionChoice,
}

impl Profile {
    /// True for built-in profiles.
    pub fn is_builtin(&self) -> bool {
        self.builtin.is_some()
    }

    /// Lines in priority order: the primary line, then the backups (duplicates
    /// dropped). Empty = the site's default / best probed line.
    pub fn line_priority(&self) -> Vec<LineId> {
        let mut out: Vec<LineId> = Vec::new();
        for id in self.line_id.iter().chain(&self.backup_lines) {
            if !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    }

    /// Set the line priority (first = primary).
    pub fn set_line_priority(&mut self, lines: impl IntoIterator<Item = LineId>) {
        let mut unique: Vec<LineId> = Vec::new();
        for line in lines {
            if !unique.contains(&line) {
                unique.push(line);
            }
        }
        let mut iter = unique.into_iter();
        self.line_id = iter.next();
        self.backup_lines = iter.collect();
    }

    /// Set the auth choice and mirror it into the `auth_mode` field (key-based
    /// profiles only; built-in profiles are left alone). Writers should use this
    /// instead of assigning [`Profile::auth`] directly.
    pub fn set_auth(&mut self, choice: AuthChoice) {
        self.auth = choice;
        if self.builtin.is_none() && self.auth_mode.uses_key() {
            self.auth_mode = choice.representative_mode();
        }
    }

    /// The auth choice of the profile.
    pub fn auth_choice(&self) -> AuthChoice {
        self.auth
    }
}

/// Built-in profile kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinProfile {
    /// Restore the baseline captured before the first takeover.
    Original,
    /// The official ChatGPT login.
    Official,
}

/// Optional model keys a profile manages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDefaults {
    /// `model = "..."`; `None` leaves the user's value alone.
    #[serde(default)]
    pub model: Option<String>,
    /// `model_reasoning_effort = "..."`; `None` leaves the user's value alone.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Speed tier (`service_tier`, v0.3 section 11). Key-based profiles write
    /// Standard as `service_tier = "default"` when this is `None`, so the terminal
    /// Codex never switches to a catalog default tier on its own; the official
    /// login writes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<SpeedTier>,
    /// `model_context_window` for every model (the Models page's "all models"
    /// row); never written together with a per-model window for the same model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// `model_auto_compact_token_limit` for every model ("all models" row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact: Option<u64>,
}

/// Contents of `profiles.json` (owned by `superba-config`'s [`crate::ProfileStore`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSet {
    /// File schema version ([`PROFILE_SET_SCHEMA`]).
    #[serde(default = "schema_current")]
    pub schema: u32,
    /// User profiles plus the two built-ins.
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// User-created compatible sites (built-in sites are not stored).
    #[serde(default)]
    pub custom_sites: Vec<Site>,
    /// Last profile selected in the UI (the applied profile lives in config state).
    #[serde(default)]
    pub last_selected: Option<ProfileId>,
    /// App settings.
    #[serde(default)]
    pub settings: Settings,
}

fn schema_current() -> u32 {
    PROFILE_SET_SCHEMA
}

impl Default for ProfileSet {
    fn default() -> Self {
        Self {
            schema: PROFILE_SET_SCHEMA,
            profiles: crate::catalog::builtin_profiles(),
            custom_sites: Vec::new(),
            last_selected: None,
            settings: Settings::default(),
        }
    }
}

impl ProfileSet {
    /// Look up a profile.
    pub fn profile(&self, id: &ProfileId) -> Option<&Profile> {
        self.profiles.iter().find(|p| &p.id == id)
    }
}

/// User settings (plan 11.1 settings page). All fields default sensibly so that
/// new fields never break old files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Hot-switch countdown in seconds: 0, 3, 5 (default) or 10. `spb` uses 0.
    pub countdown_secs: u8,
    /// Safe mode: never launch the app with a CDP port (no hot switching).
    pub safe_mode: bool,
    /// Default auth mode for new Krill profiles. New profiles take
    /// [`AuthChoice::from_mode`] of it, so the default `Pat` gives `Auto`
    /// (decision 3: auth is Auto unless changed under Advanced); UIs should not
    /// offer it as a separate preference any more.
    pub default_auth_mode: AuthMode,
    /// Offer to relaunch a manually opened app in CDP mode (plan P1 #25).
    pub takeover_app: bool,
    /// UI language; `None` follows the system.
    pub locale: Option<Locale>,
    /// UI theme.
    pub theme: Theme,
    /// Window background colour (each has a light and a dark variant).
    pub background: Background,
    /// Probe lines automatically when the app opens.
    pub auto_probe: bool,
    /// What closing the main window does (item 70). `None` until the user
    /// chose: Windows and Linux then ask on the first close, macOS keeps
    /// running in the menu bar. Without a tray (a Linux desktop without a
    /// StatusNotifierItem host) closing always quits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_action: Option<CloseAction>,
    /// Start with the window hidden in the tray (menu bar on macOS) when a
    /// tray is available. Off by default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub start_in_tray: bool,
    /// Desktop app install to use when several are present (`AppInstall::id`).
    pub preferred_app_id: Option<String>,
    /// Router mode (opt-in, off by default).
    pub router: crate::router::RouterSettings,
    /// Per remote host choices (follow switches, last settings sync).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hosts: BTreeMap<TargetId, HostSettings>,
    /// Online checks, download and feed sources.
    #[serde(skip_serializing_if = "NetworkSettings::is_default")]
    pub network: NetworkSettings,
    /// Which Codex the desktop app runs.
    #[serde(skip_serializing_if = "AppCodexSettings::is_default")]
    pub app_codex: AppCodexSettings,
    /// Check for a newer Superba at start-up and every 24 hours (installing
    /// always needs a click). On by default.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub auto_update_check: bool,
    /// Which Superba releases are offered. `None` until the user chooses:
    /// then the running build decides ([`crate::UpdateChannel::for_version`]:
    /// Preview on a pre-release build, else Stable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_channel: Option<crate::UpdateChannel>,
    /// Reduce motion: animated scenes (the session migration dialog) show a
    /// still frame. Off by default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reduce_motion: bool,
    /// Open ChatGPT through Superba when the Superba window starts and ChatGPT
    /// is not running yet (GH#7), so it can be hot-switched. Off by default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub open_app_on_start: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl Settings {
    /// The update channel in effect for a build of version `running`: the
    /// user's choice, else Preview on a pre-release build and Stable otherwise.
    pub fn update_channel_for(&self, running: &str) -> crate::UpdateChannel {
        self.update_channel
            .unwrap_or_else(|| crate::UpdateChannel::for_version(running))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            countdown_secs: 5,
            safe_mode: false,
            default_auth_mode: AuthMode::Pat,
            takeover_app: false,
            locale: None,
            theme: Theme::System,
            background: Background::Neutral,
            auto_probe: true,
            close_action: None,
            start_in_tray: false,
            preferred_app_id: None,
            router: crate::router::RouterSettings::default(),
            hosts: BTreeMap::new(),
            network: NetworkSettings::default(),
            app_codex: AppCodexSettings::default(),
            auto_update_check: true,
            update_channel: None,
            reduce_motion: false,
            open_app_on_start: false,
        }
    }
}

/// What closing Superba's main window does ([`Settings::close_action`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseAction {
    /// Quit Superba (the router keeps running as its own service).
    Quit,
    /// Hide the window and keep running in the tray (the menu bar on macOS).
    Tray,
}

/// UI theme preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the OS.
    #[default]
    System,
    /// Light.
    Light,
    /// Dark.
    Dark,
}

/// Window background colour of the GUI (Preferences → Appearance): a soft tint
/// behind the sidebar and the content, drawn in a light and a dark variant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Background {
    /// Light gray (dark: graphite).
    #[default]
    Neutral,
    /// Mint green.
    Mint,
    /// Sky blue.
    Sky,
    /// Warm sand.
    Sand,
    /// Lavender.
    Lavender,
}

impl Background {
    /// Every background, in the order Preferences lists them.
    pub const ALL: [Background; 5] = [
        Background::Neutral,
        Background::Mint,
        Background::Sky,
        Background::Sand,
        Background::Lavender,
    ];
}

/// Choices for one remote host (Remote hosts page).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSettings {
    /// A local switch also changes this host's connection (routing and auth keys
    /// only, always direct). Off by default for SSH hosts: switches leave the host
    /// alone. A WSL distribution follows unless the user turned it off
    /// ([`HostSettings::follows`]).
    pub follow_switches: bool,
    /// The user set [`HostSettings::follow_switches`] (a WSL distribution without
    /// a choice follows this computer).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub follow_chosen: bool,
    /// The last settings sync to the host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<HostSync>,
}

impl HostSettings {
    /// Whether a local switch also switches the host: the user's choice, else
    /// (no choice made) on for a WSL distribution of this computer and off for an
    /// SSH host. A setting stored before the choice was recorded counts as a
    /// choice when it turned following on.
    pub fn follows(&self, kind: crate::HostKind) -> bool {
        if self.follow_chosen || self.follow_switches {
            self.follow_switches
        } else {
            kind == crate::HostKind::Wsl
        }
    }

    /// Record the user's follow choice.
    pub fn choose_follow(&mut self, follow: bool) {
        self.follow_switches = follow;
        self.follow_chosen = true;
    }
}

/// A completed settings sync to a host (conflict detection, "unsynced" badge).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSync {
    /// When.
    pub at: Timestamp,
    /// sha256 (hex) over every synced path and value; equal hashes = nothing to
    /// sync.
    pub values_hash: String,
    /// Paths Superba wrote on the host (candidates for Remove rows later).
    pub paths: Vec<String>,
    /// sha256 (hex) of each value written, by path (a different host value later
    /// is a Conflict row).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub value_hashes: BTreeMap<String, String>,
}

/// Where Codex packages are downloaded from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DownloadSource {
    /// GitHub release package, then npm, then npmmirror.
    #[default]
    Auto,
    /// GitHub releases only.
    #[serde(rename = "github")]
    GitHub,
    /// registry.npmjs.org only.
    Npm,
    /// registry.npmmirror.com only.
    NpmMirror,
}

/// Where the signed feeds are fetched from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeedSourceChoice {
    /// Race every built-in mirror.
    #[default]
    Auto,
    /// raw.githubusercontent.com only.
    #[serde(rename = "github_raw")]
    GitHubRaw,
    /// jsDelivr mirrors only.
    #[serde(rename = "jsdelivr")]
    JsDelivr,
}

/// Where the ChatGPT desktop app package is downloaded from on Windows
/// (Preferences -> Network; `spb app download --source`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WindowsAppSource {
    /// OpenAI's own download (`persistent.oaistatic.com`); the Microsoft Store
    /// mirror only when that fails.
    #[default]
    OfficialFirst,
    /// OpenAI's own download only.
    OfficialOnly,
    /// The Microsoft Store CDN only, its temporary links resolved by
    /// store.rg-adguard.net (a third-party link resolver; the files still come
    /// from Microsoft's CDN and are verified).
    StoreMirrorOnly,
}

impl WindowsAppSource {
    /// The sources tried, in order.
    pub fn order(self) -> &'static [crate::versions::AppPackageSource] {
        use crate::versions::AppPackageSource::{Official, StoreMirror};
        match self {
            WindowsAppSource::OfficialFirst => &[Official, StoreMirror],
            WindowsAppSource::OfficialOnly => &[Official],
            WindowsAppSource::StoreMirrorOnly => &[StoreMirror],
        }
    }
}

/// Network preferences (Preferences -> Network).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    /// Check feeds and versions online (at most daily). On by default.
    pub online_checks: bool,
    /// Codex package source.
    pub download_source: DownloadSource,
    /// Feed source.
    pub feed_source: FeedSourceChoice,
    /// Also use third-party GitHub proxies (gh-proxy.com, ghfast.top); every
    /// download stays checksum-verified. Off by default.
    pub third_party_proxies: bool,
    /// Extra mirror base URLs (https).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_mirrors: Vec<String>,
    /// Windows: where the ChatGPT desktop app package comes from.
    pub windows_app_source: WindowsAppSource,
}

impl NetworkSettings {
    /// True for the defaults (serde skip helper).
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            online_checks: true,
            download_source: DownloadSource::Auto,
            feed_source: FeedSourceChoice::Auto,
            third_party_proxies: false,
            extra_mirrors: Vec::new(),
            windows_app_source: WindowsAppSource::OfficialFirst,
        }
    }
}

/// Which Codex the desktop app runs (`CODEX_CLI_PATH` at launch).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppCodexSettings {
    /// The pin (default: the bundled Codex).
    pub pin: AppCodexPin,
    /// Also persist `CODEX_CLI_PATH` for apps started outside Superba (shell rc /
    /// launchctl).
    pub persist_env: bool,
}

impl AppCodexSettings {
    /// True for the defaults (serde skip helper).
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}
