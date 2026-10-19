//! Superba's own update: check the GitHub releases of `krill-lab/superba`, and
//! install a newer release after the user chose to.
//!
//! - **Check** ([`SelfUpdater::check`]) on the update channel
//!   ([`UpdateChannel`]: the user's choice, else Preview on a pre-release build
//!   and Stable otherwise). Stable reads `releases/latest` (GitHub never lists
//!   a pre-release there); Preview reads one page of `releases` and takes the
//!   newest release by semver precedence (`alpha < beta < rc < final`), drafts
//!   skipped, and the newest final one for the page text. One request per
//!   check (Preview asks `releases/latest` too only when the page holds no
//!   final release). Every answer is cached with its ETag
//!   (`<data dir>/selfupdate.json`), 8 s deadline, no credentials. GitHub
//!   allows 60 requests an hour without an account: a rate-limited answer
//!   stores the reset time and no request goes out before it, the cached
//!   answer is shown meanwhile. A 404 / 401 / 403 (a private repository) ends
//!   in [`SelfUpdateStatus::PageUnavailable`], a calm state; nothing is retried
//!   before the next 24 h interval. Only a release newer than the running
//!   build is offered: never a downgrade (a preview build on the Stable channel
//!   waits for the next final release).
//! - **Install** ([`SelfUpdater::install`]; [`SelfUpdater::install_version`]
//!   for a release the user named, the only way to an older one): the package of this OS and
//!   architecture ([`release::asset_name`]) from
//!   `<downloads>/<tag>/<asset>` (GitHub, a configured mirror of exactly that
//!   path, or an opted-in third-party proxy of it), verified for size and
//!   SHA-256 against the `SHA256SUMS.txt` of the same release (always fetched
//!   from the GitHub path, never through a proxy), extracted with the same path
//!   and link rules as Codex packages, then swapped in place ([`apply`]):
//!   macOS replaces the app bundle (the old one stays as a backup until the new
//!   one has started), Windows hands the swap to the new `superba.exe`
//!   (`--finish-update`, [`finish`]), Linux replaces the binaries. A location
//!   that is not writable ends in [`SelfInstallOutcome::Manual`] with the
//!   verified package and short steps.
//!
//! Every install attempt is logged step by step in
//! `<data dir>/logs/selfupdate.log` ([`runlog`]; the last attempts are kept) and
//! recorded in `selfupdate.json` ([`record`]), so the next start can say when
//! an update failed ([`superba_core::SelfUpdateView::problem`]).
//!
//! The endpoints can be replaced for tests with [`ENV_SUPERBA_SELF_UPDATE_API`]
//! and [`ENV_SUPERBA_SELF_UPDATE_DOWNLOADS`]; like the other overrides they
//! must be https or loopback ([`crate::sources::override_base`]).

pub mod apply;
mod fetch;
pub mod finish;
mod install;
pub mod msi;
pub mod record;
pub mod release;
pub mod runlog;
pub mod state;
pub mod version;
pub mod zipx;

use std::path::PathBuf;
use std::time::Duration;

use semver::Version;
use superba_core::{
    DataDir, NetworkSettings, SelfRelease, SelfUpdateStatus, SelfUpdateView, Timestamp,
    UpdateChannel,
};

use crate::sources::checked_override;
use fetch::Fetched;
use release::Platform;
use state::{Outcome, State};

pub use install::{InstallResult, manual_steps};

/// Replaces the release API base (`https://api.github.com/repos/krill-lab/superba`).
/// Tests only; https or loopback http.
pub const ENV_SUPERBA_SELF_UPDATE_API: &str = "SUPERBA_SELF_UPDATE_API";

/// Replaces the download base
/// (`https://github.com/krill-lab/superba/releases/download`): packages and
/// `SHA256SUMS.txt` come from `<base>/<tag>/<file>`. Tests only; https or
/// loopback http.
pub const ENV_SUPERBA_SELF_UPDATE_DOWNLOADS: &str = "SUPERBA_SELF_UPDATE_DOWNLOADS";

/// Default API base.
pub const DEFAULT_API: &str = "https://api.github.com/repos/krill-lab/superba";

/// Default download base.
pub const DEFAULT_DOWNLOADS: &str = "https://github.com/krill-lab/superba/releases/download";

/// Deadline of one API request.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

/// Automatic checks happen at most this often after an answer.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// A failed automatic check is retried after this long.
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(3600);

/// How many releases `/releases` is asked for (newest first by creation).
const LIST_PAGE: u32 = 30;

/// The longest wait a rate-limit answer can impose (a far reset time or a
/// wrong clock never blocks checks for longer).
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(3600);

/// Third-party GitHub proxies (prefixes of the download URL), opt-in.
const PROXIES: [&str; 2] = ["https://gh-proxy.com/", "https://ghfast.top/"];

/// Where releases and packages come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// API base (`<api>/releases/latest`).
    pub api: String,
    /// Download base (`<downloads>/<tag>/<file>`).
    pub downloads: String,
    /// Proxy prefixes, used only with [`NetworkSettings::third_party_proxies`].
    pub proxies: Vec<String>,
    /// [`Endpoints::downloads`] is an override: the only package source.
    pub downloads_overridden: bool,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: DEFAULT_API.into(),
            downloads: DEFAULT_DOWNLOADS.into(),
            proxies: PROXIES.iter().map(|s| (*s).to_owned()).collect(),
            downloads_overridden: false,
        }
    }
}

impl Endpoints {
    /// Defaults with the two environment overrides applied (`lookup` reads a
    /// variable). A value that is neither https nor loopback http is ignored.
    pub fn with_overrides(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut e = Self::default();
        if let Some(base) = checked_override(&lookup, ENV_SUPERBA_SELF_UPDATE_API) {
            e.api = base;
        }
        if let Some(base) = checked_override(&lookup, ENV_SUPERBA_SELF_UPDATE_DOWNLOADS) {
            e.downloads = base;
            e.downloads_overridden = true;
        }
        e
    }
}

/// The self-update engine for one data directory.
#[derive(Clone, Debug)]
pub struct SelfUpdater {
    control: std::sync::Arc<superba_core::UpdateControl>,
    data_dir: DataDir,
    network: NetworkSettings,
    auto_check: bool,
    channel: Option<UpdateChannel>,
    endpoints: Endpoints,
    platform: Platform,
    current: Version,
    now: Option<Timestamp>,
    exe: Option<PathBuf>,
}

impl SelfUpdater {
    /// An updater for the running build; endpoint overrides are read from the
    /// environment.
    pub fn new(data_dir: &DataDir, network: NetworkSettings) -> Self {
        Self {
            control: Default::default(),
            data_dir: data_dir.clone(),
            network,
            auto_check: true,
            channel: None,
            endpoints: Endpoints::with_overrides(|k| std::env::var(k).ok()),
            platform: Platform::current(),
            current: version::parse_version(env!("CARGO_PKG_VERSION"))
                .unwrap_or_else(|| Version::new(0, 0, 0)),
            now: None,
            exe: None,
        }
    }

    /// Cancellable preparation; replacing the running installation is atomic
    /// with respect to the final cancellation check.
    pub fn with_control(mut self, control: std::sync::Arc<superba_core::UpdateControl>) -> Self {
        self.control = control;
        self
    }

    /// The Preferences choices: automatic checks and the update channel
    /// (`None`: the running build decides, see [`UpdateChannel::for_version`]).
    pub fn with_preferences(mut self, auto_check: bool, channel: Option<UpdateChannel>) -> Self {
        self.auto_check = auto_check;
        self.channel = channel;
        self
    }

    /// Replace the endpoints (tests).
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Pretend to be another machine (tests).
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// Pretend to run as another version (tests).
    pub fn with_current(mut self, current: &str) -> Self {
        if let Some(v) = version::parse_version(current) {
            self.current = v;
        }
        self
    }

    /// Fix the clock (tests).
    pub fn with_now(mut self, now: Timestamp) -> Self {
        self.now = Some(now);
        self
    }

    /// Treat `exe` as the running executable when locating the install (tests).
    pub fn with_exe(mut self, exe: impl Into<PathBuf>) -> Self {
        self.exe = Some(exe.into());
        self
    }

    /// The running version.
    pub fn current(&self) -> &Version {
        &self.current
    }

    /// The update channel in use: the user's choice, else Preview on a
    /// pre-release build and Stable otherwise.
    pub fn channel(&self) -> UpdateChannel {
        self.channel.unwrap_or(if self.current.pre.is_empty() {
            UpdateChannel::Stable
        } else {
            UpdateChannel::Preview
        })
    }

    /// The endpoints in use.
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    fn now(&self) -> Timestamp {
        self.now.unwrap_or_else(Timestamp::now)
    }

    fn state_path(&self) -> PathBuf {
        self.data_dir.selfupdate_file()
    }

    /// The cached answer; never touches the network.
    pub fn view(&self) -> SelfUpdateView {
        let state = State::load(&self.state_path());
        self.compose(&state)
    }

    /// The newest final release and the newest release of all (Preview only)
    /// the cache knows for `channel`.
    fn known(
        &self,
        state: &State,
        channel: UpdateChannel,
    ) -> (Option<SelfRelease>, Option<SelfRelease>) {
        match channel {
            UpdateChannel::Stable => (state.stable.release.clone(), None),
            UpdateChannel::Preview => {
                let stable = state
                    .list
                    .newest_stable
                    .clone()
                    .or_else(|| state.stable.release.clone());
                let newest = [state.list.newest.clone(), stable.clone()]
                    .into_iter()
                    .flatten()
                    .filter_map(|r| version::parse_version(&r.version).map(|v| (v, r)))
                    .max_by(|(a, _), (b, _)| version::cmp(a, b))
                    .map(|(_, r)| r);
                (stable, newest)
            }
        }
    }

    /// The release a check on `channel` offers when it is newer than the
    /// running build (never an older one).
    fn candidate(&self, state: &State, channel: UpdateChannel) -> Option<SelfRelease> {
        let (stable, newest) = self.known(state, channel);
        match channel {
            UpdateChannel::Stable => stable,
            UpdateChannel::Preview => newest,
        }
    }

    /// GitHub's rate limit is in force at `now`.
    fn limited(&self, state: &State) -> bool {
        state
            .limited_until
            .is_some_and(|t| t.as_millis() > self.now().as_millis())
    }

    /// The state as the UIs see it.
    fn compose(&self, state: &State) -> SelfUpdateView {
        let channel = self.channel();
        let (latest_stable, latest_preview) = self.known(state, channel);
        let best = self
            .candidate(state, channel)
            .and_then(|r| version::parse_version(&r.version).map(|v| (v, r)));
        let newer = best
            .as_ref()
            .is_some_and(|(v, _)| version::is_newer(v, &self.current));
        // The last round asked for another channel: only this channel's own
        // cache speaks (Preview never answers from the Stable cache alone).
        let answered = state.checked_channel == Some(channel);
        let cached = match channel {
            UpdateChannel::Stable => state.stable.release.is_some(),
            UpdateChannel::Preview => state.list.newest.is_some(),
        };
        let limited = self.limited(state);
        let status = match state.outcome {
            Outcome::None if !self.network.online_checks => SelfUpdateStatus::OnlineChecksOff,
            Outcome::None => SelfUpdateStatus::NotChecked,
            _ if best.is_none() && limited => SelfUpdateStatus::CheckFailed,
            _ if !answered && !cached => SelfUpdateStatus::NotChecked,
            _ if !answered && newer => SelfUpdateStatus::Available,
            _ if !answered => SelfUpdateStatus::UpToDate,
            Outcome::Unavailable => SelfUpdateStatus::PageUnavailable,
            Outcome::Failed if best.is_none() => SelfUpdateStatus::CheckFailed,
            Outcome::Ok | Outcome::Failed if newer => SelfUpdateStatus::Available,
            Outcome::Ok | Outcome::Failed => SelfUpdateStatus::UpToDate,
        };
        let latest: Option<SelfRelease> = best.map(|(_, r)| r);
        SelfUpdateView {
            current: self.current.to_string(),
            status,
            no_package: status == SelfUpdateStatus::Available
                && latest.as_ref().is_some_and(|l| l.asset.is_none()),
            latest,
            channel,
            channel_chosen: self.channel.is_some(),
            latest_stable,
            latest_preview,
            checked_at: state.checked_at,
            detail: state.detail.clone(),
            limited_until: state.limited_until.filter(|_| limited),
            skipped: state.skipped.clone(),
            auto_check: self.auto_check,
            problem: state
                .last_attempt
                .as_ref()
                .and_then(|a| a.problem(&self.current, self.now())),
        }
    }

    /// Whether an automatic check has nothing to do now.
    fn fresh(&self, state: &State) -> bool {
        let Some(at) = state.checked_at else {
            return false;
        };
        if state.checked_channel != Some(self.channel()) {
            return false;
        }
        if state.api.as_deref() != Some(self.endpoints.api.as_str()) {
            return false;
        }
        let window = match state.outcome {
            Outcome::Failed | Outcome::None => RETRY_AFTER_FAILURE,
            _ => CHECK_INTERVAL,
        };
        let age = self.now().as_millis() - at.as_millis();
        age >= 0 && (age as u128) < window.as_millis()
    }

    /// When a rate-limited answer allows the next request.
    fn limit_end(&self, reset_at: Option<u64>, retry_after: Option<u64>) -> Option<Timestamp> {
        let now = self.now().as_millis();
        let max = now + MAX_RATE_LIMIT_WAIT.as_millis() as i64;
        let end = match (reset_at, retry_after) {
            (_, Some(secs)) => now.saturating_add((secs as i64).saturating_mul(1000)),
            (Some(at), None) => (at as i64).saturating_mul(1000),
            (None, None) => return None,
        };
        (end > now).then(|| Timestamp::from_millis(end.min(max)))
    }

    /// Check the release page. `manual` ignores the "automatic checks"
    /// preference, the interval and "online checks" being off in the network
    /// preferences (those are about checking on its own; the user asked).
    /// Nothing is sent while GitHub's rate limit is in force.
    pub async fn check(&self, manual: bool) -> SelfUpdateView {
        self.cleanup();
        let path = self.state_path();
        let mut state = State::load(&path);
        // Online checks off: only a check the user asked for goes out.
        if !manual && (!self.network.online_checks || !self.auto_check || self.fresh(&state)) {
            return self.compose(&state);
        }
        if state.api.as_deref() != Some(self.endpoints.api.as_str()) {
            state.stable = state::Channel::default();
            state.list = state::List::default();
            state.limited_until = None;
            state.api = Some(self.endpoints.api.clone());
        }
        if self.limited(&state) {
            return self.compose(&state);
        }
        state.limited_until = None;
        let api = self.endpoints.api.trim_end_matches('/');
        let channel = self.channel();
        let mut results = Vec::new();
        let mut limits = Vec::new();
        let mut note = |fetched: &Fetched| {
            if let Fetched::Limited {
                reset_at,
                retry_after,
            } = fetched
            {
                limits.push((*reset_at, *retry_after));
            }
        };

        let mut need_latest = channel == UpdateChannel::Stable;
        if channel == UpdateChannel::Preview {
            let list = fetch::get(
                &format!("{api}/releases?per_page={LIST_PAGE}"),
                state.list.etag.as_deref(),
                CHECK_TIMEOUT,
            )
            .await;
            note(&list);
            let step = absorb_list(&mut state.list, list, &self.platform);
            // A page of pre-releases only: the newest final release is asked
            // for separately (it may be older than the page).
            need_latest = matches!(step, Step::Ok) && state.list.newest_stable.is_none();
            results.push(step);
        }
        if need_latest {
            let stable = fetch::get(
                &format!("{api}/releases/latest"),
                state.stable.etag.as_deref(),
                CHECK_TIMEOUT,
            )
            .await;
            note(&stable);
            results.push(absorb(&mut state.stable, stable, |body| {
                release::parse_latest(body, &self.platform).map(Some)
            }));
        }
        state.limited_until = limits
            .into_iter()
            .filter_map(|(r, a)| self.limit_end(r, a))
            .max_by_key(|t| t.as_millis());
        state.checked_channel = Some(channel);
        state.checked_at = Some(self.now());
        state.detail = None;
        state.outcome = if results.iter().any(|r| matches!(r, Step::Ok)) {
            Outcome::Ok
        } else if let Some(Step::Failed(why)) =
            results.iter().find(|r| matches!(r, Step::Failed(_)))
        {
            state.detail = Some(why.clone());
            Outcome::Failed
        } else {
            Outcome::Unavailable
        };
        if let Err(e) = state.save(&path) {
            tracing::warn!(error = %e, "could not save the self-update state");
        }
        self.compose(&state)
    }

    /// The last attempt's problem has been shown: do not announce it again.
    pub fn acknowledge(&self) -> SelfUpdateView {
        let path = self.state_path();
        record::update(&path, self.now(), |a| a.seen = true);
        self.view()
    }

    /// The log of the current (or last) install attempt.
    pub fn attempt_log(&self) -> runlog::RunLog {
        runlog::RunLog::resume(runlog::RunLog::path_in(&self.data_dir), "update")
    }

    /// Note a problem of the current attempt found after the files were in
    /// place (the router could not be restarted). Shown at the next start.
    pub fn attempt_problem(&self, reason: superba_core::LocalizedText) {
        record::update(&self.state_path(), self.now(), |a| {
            a.problem = Some(reason);
            a.seen = false;
        });
    }

    /// The current attempt failed with `error`, already shown to the user.
    pub fn attempt_failed(&self, error: &superba_core::Error) {
        self.attempt_log().line(format!("FAILED: {error}"));
        record::update(&self.state_path(), self.now(), |a| {
            a.stage = record::Stage::Failed;
            a.problem = Some(record::error_reason(&error.detail()));
            a.seen = true;
        });
    }

    /// Skip `version` (or clear the skip with `None`).
    pub fn skip(&self, version: Option<String>) -> SelfUpdateView {
        let path = self.state_path();
        let mut state = State::load(&path);
        state.skipped = version.map(|v| v.trim_start_matches('v').to_owned());
        if let Err(e) = state.save(&path) {
            tracing::warn!(error = %e, "could not save the skipped version");
        }
        self.compose(&state)
    }
}

/// One request's effect on its channel.
enum Step {
    Ok,
    Unavailable,
    Failed(String),
}

fn absorb(
    channel: &mut state::Channel,
    fetched: Fetched,
    parse: impl FnOnce(&[u8]) -> Option<Option<SelfRelease>>,
) -> Step {
    match fetched {
        Fetched::NotModified => Step::Ok,
        Fetched::Fresh { body, etag } => match parse(&body) {
            Some(release) => {
                channel.etag = etag;
                channel.release = release;
                Step::Ok
            }
            None => Step::Failed("unexpected answer".into()),
        },
        Fetched::Unavailable => {
            *channel = state::Channel::default();
            Step::Unavailable
        }
        Fetched::Limited { .. } => Step::Failed("rate limited".into()),
        Fetched::Failed(why) => Step::Failed(why),
    }
}

/// A `/releases` answer into the list cache. A page without any release we
/// accept is "no release page" ([`Step::Unavailable`]).
fn absorb_list(list: &mut state::List, fetched: Fetched, platform: &Platform) -> Step {
    match fetched {
        Fetched::NotModified => Step::Ok,
        Fetched::Fresh { body, etag } => match release::parse_list(&body, platform) {
            Some(answer) if answer.newest.is_none() => {
                *list = state::List::default();
                Step::Unavailable
            }
            Some(answer) => {
                *list = state::List {
                    etag,
                    newest: answer.newest,
                    newest_stable: answer.newest_stable,
                };
                Step::Ok
            }
            None => Step::Failed("unexpected answer".into()),
        },
        Fetched::Unavailable => {
            *list = state::List::default();
            Step::Unavailable
        }
        Fetched::Limited { .. } => Step::Failed("rate limited".into()),
        Fetched::Failed(why) => Step::Failed(why),
    }
}
