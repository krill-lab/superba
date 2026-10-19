//! # superba-tools
//!
//! Install, update and remove the coding-agent tools Superba configures —
//! Claude Code, dsh, opencode, pi, ZCode, WorkBuddy and WorkBuddy AI — with
//! each vendor's own artifacts, the way the research reports chose per OS
//! ([`descriptor`]):
//!
//! | Tool | Method | Newest version |
//! |---|---|---|
//! | Claude Code | native: `manifest.json` sha256 → `versions/<v>` → its own `install <v>` | `claude-code-releases/{latest,stable}` |
//! | dsh | npm into a Superba prefix (`--allow-scripts`); without Node ≥ 22.19 the PyPI runtime build | npm dist-tags / PyPI |
//! | opencode | standalone archive into `~/.opencode/bin` (OpenCode 2 by default; an install keeps its major) | `opencode.ai/update/api` / npm `opencode-ai` |
//! | pi | standalone archive + `SHA256SUMS` into `<data>/tools/pi` | `pi.dev/api/latest-version` |
//! | ZCode | Windows per-user silent installer; elsewhere a link | the release manifest |
//! | WorkBuddy (AI) | Windows per-user `/S` (version only, it updates itself); elsewhere a link | `/v2/update` |
//!
//! Rules every operation keeps: no administrator rights (nothing is run
//! elevated; system-wide installs are shown with their command), every
//! download verified (sha256 / sha512, Windows installers also by signature)
//! before it is used, progress and cancel through [`ToolProgressSink`], a log
//! per operation (`<data>/logs/tools/<tool>.log`), one operation per tool at a
//! time (a lock file), and the tools' settings and data are never changed by
//! an install, update or removal (opencode's data only on an explicit
//! `remove_data`). Turning a tool's own updater off ("Superba manages
//! updates") is a separate, explicit change of one key.
//!
//! WSL distributions: not yet (TODO: run the Linux flows inside a
//! distribution through `superba-remote`'s WSL plumbing); the view says so on
//! Windows.

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub mod archive;
pub mod descriptor;
pub mod fsx;
pub mod http;
pub mod jsonedit;
pub mod log;
pub mod pathenv;
pub mod platform;
pub mod procs;
pub mod run;
pub mod state;
pub mod tools;
pub mod version;
pub mod winapps;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use superba_core::{
    DataDir, EnvSource, Error, LocalContext, LocalizedText, NetworkSettings, Notice, PathHint,
    Result, Timestamp, ToolActions, ToolId, ToolInstall, ToolOp, ToolProgress, ToolProgressSink,
    ToolReport, ToolRequest, ToolRow, ToolSettingsChange, ToolStage, ToolSupport, ToolUpdater,
    ToolsView,
};

use crate::descriptor::{Descriptor, Level, descriptor};
use crate::log::OpLog;
use crate::platform::{Os, Platform};
use crate::state::{LatestCache, State, ToolLock, ToolState};

/// How long a looked-up newest version is used without asking again.
pub const LATEST_TTL: Duration = Duration::from_secs(24 * 3600);

/// The vendors' endpoints (tests point them at a local server).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sources {
    /// Claude Code releases (`…/claude-code-releases`).
    pub claude: String,
    /// npm registry.
    pub npm: String,
    /// PyPI JSON API (`…/pypi`).
    pub pypi: String,
    /// opencode.ai.
    pub opencode: String,
    /// github.com (release downloads).
    pub github: String,
    /// api.github.com.
    pub github_api: String,
    /// pi.dev.
    pub pi: String,
    /// zcode.z.ai.
    pub zcode: String,
    /// WorkBuddy (China) update host.
    pub workbuddy_cn: String,
    /// WorkBuddy AI (international) update host.
    pub workbuddy_intl: String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            claude: "https://downloads.claude.ai/claude-code-releases".into(),
            npm: "https://registry.npmjs.org".into(),
            pypi: "https://pypi.org/pypi".into(),
            opencode: "https://opencode.ai".into(),
            github: "https://github.com".into(),
            github_api: "https://api.github.com".into(),
            pi: "https://pi.dev".into(),
            zcode: "https://zcode.z.ai".into(),
            workbuddy_cn: "https://www.workbuddy.cn".into(),
            workbuddy_intl: "https://www.workbuddy.ai".into(),
        }
    }
}

/// What the embedding application provides.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// Windows: check the Authenticode signature of `path` (chain to a
    /// trusted root) and return the signer's subject.
    async fn verify_signature(&self, path: &Path) -> Result<String> {
        let _ = path;
        Err(Error::Unsupported {
            what: "checking installer signatures on this platform".into(),
        })
    }
}

/// A host without signature checks (tests, platforms without installers).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

#[async_trait]
impl ToolHost for NoHost {}

/// Everything an operation reads: home, data dir, environment, platform,
/// endpoints, network settings, the host.
pub struct Ctx {
    /// The user's home.
    pub home: PathBuf,
    /// Superba's data dir.
    pub data: DataDir,
    /// Environment (`PATH`, `XDG_*`, the tools' variables).
    pub env: Arc<dyn EnvSource>,
    /// This computer.
    pub platform: Platform,
    /// Endpoints.
    pub sources: Sources,
    /// Network settings (online checks).
    pub network: NetworkSettings,
    /// The host.
    pub host: Arc<dyn ToolHost>,
    client: OnceLock<reqwest::Client>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("home", &self.home)
            .field("data", &self.data)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl Ctx {
    /// The HTTP client.
    pub fn client(&self) -> Result<&reqwest::Client> {
        if let Some(c) = self.client.get() {
            return Ok(c);
        }
        let c = http::client()?;
        Ok(self.client.get_or_init(|| c))
    }

    /// An absolute directory from `var`, else `default` under the home.
    pub fn xdg(&self, var: &str, default: &str) -> PathBuf {
        self.env
            .var_non_empty(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| default.split('/').fold(self.home.clone(), |p, c| p.join(c)))
    }

    /// `~/.local/bin`.
    pub fn local_bin(&self) -> PathBuf {
        self.home.join(".local").join("bin")
    }

    /// `<data>/tools/<tool>`.
    pub fn tool_dir(&self, tool: ToolId) -> PathBuf {
        state::tools_dir(&self.data).join(tool.as_str())
    }

    /// `PATH` directories.
    pub fn path_dirs(&self) -> Vec<PathBuf> {
        pathenv::path_dirs(self.env.as_ref())
    }

    /// The program `name` on `PATH`.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        run::which_in(
            name,
            self.env.var_os("PATH").as_deref(),
            self.platform.os == Os::Windows,
        )
    }

    /// A variable is set to something other than empty, `0` or `false`.
    pub fn flag(&self, var: &str) -> bool {
        self.env
            .var(var)
            .is_some_and(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false"))
    }

    /// A command with the environment Superba computed (home, `PATH`,
    /// `XDG_*`) so the tool sees the same places Superba looked at.
    pub fn cmd(&self, program: impl Into<PathBuf>) -> run::Cmd {
        let mut c = run::Cmd::new(program);
        if self.platform.os == Os::Windows {
            c = c.env("USERPROFILE", self.home.as_os_str());
        } else {
            c = c.env("HOME", self.home.as_os_str());
        }
        if let Some(p) = self.env.var_os("PATH") {
            c = c.env("PATH", p);
        }
        for var in [
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "CLAUDE_CONFIG_DIR",
        ] {
            match self.env.var_os(var) {
                Some(v) => c = c.env(var, v),
                None => c.remove.push(var.into()),
            }
        }
        c
    }

    /// A Unix `PATH` hint for `dir`; on Windows `None` (Superba adds it to
    /// the user `PATH` itself).
    pub fn path_hint(&self, dir: &Path) -> Option<PathHint> {
        match self.platform.os {
            Os::Windows => None,
            os => pathenv::unix_hint(self.env.as_ref(), &self.home, dir, os == Os::Macos),
        }
    }
}

/// What detection found for one tool.
#[derive(Clone, Debug, Default)]
pub struct Detected {
    /// Installs; the first is the one in use.
    pub installs: Vec<ToolInstall>,
    /// The tool's own updater.
    pub updater: Option<ToolUpdater>,
    /// Notes.
    pub notes: Vec<Notice>,
    /// Install cautions.
    pub install_cautions: Vec<LocalizedText>,
    /// Update cautions.
    pub update_cautions: Vec<LocalizedText>,
    /// Removal cautions.
    pub uninstall_cautions: Vec<LocalizedText>,
    /// Data folders a removal can take too.
    pub data_dirs: Vec<PathBuf>,
    /// `PATH` hint.
    pub path_hint: Option<PathHint>,
    /// The channel the install itself follows (Claude Code's
    /// `autoUpdatesChannel`, `v1` for an OpenCode 1).
    pub channel_hint: Option<String>,
    /// Channels when they differ from the descriptor's (dsh's runtime: `pypi`).
    pub channels: Option<Vec<String>>,
    /// The install in use can be updated by Superba (its kind has a method).
    pub can_update: bool,
    /// The install in use can be removed by Superba.
    pub can_uninstall: bool,
    /// Superba may install although something is installed (Claude Code's
    /// native install over an npm one).
    pub install_beside: bool,
    /// Why nothing can be installed now (missing prerequisite).
    pub blocked: Option<LocalizedText>,
    /// A link replaces the install button (no verified method on this OS).
    pub link: Option<String>,
}

/// One operation in progress.
pub struct Op<'a> {
    /// Context.
    pub cx: &'a Ctx,
    /// The request.
    pub req: &'a ToolRequest,
    /// Detection before the operation.
    pub det: &'a Detected,
    /// The descriptor.
    pub desc: Descriptor,
    /// The channel in use.
    pub channel: String,
    /// The log.
    pub log: &'a OpLog,
    /// Progress and cancel.
    pub sink: &'a dyn ToolProgressSink,
}

impl Op<'_> {
    /// The tool.
    pub fn tool(&self) -> ToolId {
        self.req.tool
    }

    /// Report a stage.
    pub fn stage(&self, stage: ToolStage, detail: Option<String>) {
        self.sink.report(ToolProgress {
            detail,
            ..ToolProgress::stage(self.tool(), stage)
        });
    }

    /// Stop when the user asked to.
    pub fn check_cancel(&self) -> Result<()> {
        if self.sink.cancel_requested() {
            self.log.line("cancelled by the user");
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Download and verify (logged).
    pub async fn download(
        &self,
        url: &str,
        dest: &Path,
        checksum: Option<&http::Checksum>,
        size: Option<u64>,
    ) -> Result<String> {
        self.log.line(format!(
            "download: {url} ({}{})",
            checksum.map_or(
                "no published checksum; signature checked next".into(),
                |c| { format!("{} {}", c.algorithm(), c.value()) }
            ),
            size.map(|s| format!(", {s} bytes")).unwrap_or_default()
        ));
        let d = http::Download {
            tool: self.tool(),
            url,
            dest,
            checksum,
            size,
        };
        match http::download(self.cx.client()?, &d, self.sink).await {
            Ok(sha) => {
                self.log.line(format!("verified: sha256 {sha}"));
                Ok(sha)
            }
            Err(e) => {
                self.log.line(format!("download failed: {e}"));
                Err(e)
            }
        }
    }

    /// Run a command (logged; output lines become the progress detail).
    pub async fn run(&self, cmd: &run::Cmd, stage: ToolStage) -> Result<run::Outcome> {
        self.check_cancel()?;
        self.stage(stage, None);
        run::run(cmd, self.log, self.tool(), stage, self.sink).await
    }

    /// Run a command that must succeed.
    pub async fn run_ok(&self, cmd: &run::Cmd, stage: ToolStage) -> Result<run::Outcome> {
        let out = self.run(cmd, stage).await?;
        if !out.success() {
            return Err(Error::Process {
                message: format!(
                    "{} ended with {}: {}",
                    cmd.display(),
                    out.code
                        .map_or("a signal".into(), |c| format!("exit code {c}")),
                    out.tail(6)
                ),
            });
        }
        Ok(out)
    }

    /// The report of a finished operation.
    pub fn report(
        &self,
        version: Option<String>,
        path: Option<PathBuf>,
        notes: Vec<Notice>,
    ) -> ToolReport {
        ToolReport {
            tool: self.tool(),
            op: self.req.op,
            version,
            path,
            notes,
            log: self.log.path(),
        }
    }

    /// Update Superba's record of this tool.
    pub fn record(&self, f: impl FnOnce(&mut ToolState)) -> Result<()> {
        State::update(&self.cx.data, self.tool(), f)
    }
}

/// A program file: path, size, mtime.
type FileKey = (PathBuf, u64, i128);

/// Cached `--version` answers per program file.
static VERSION_CACHE: OnceLock<Mutex<HashMap<FileKey, Option<String>>>> = OnceLock::new();

/// `program --version` (or `args`), parsed; cached per file size and mtime.
pub async fn probe_version(cx: &Ctx, program: &Path, args: &[&str]) -> Option<String> {
    probe_version_with(cx.cmd(program).args(args.iter().copied())).await
}

/// A version probe with a tool-specific environment, using the same cache.
pub(crate) async fn probe_version_with(cmd: run::Cmd) -> Option<String> {
    let program = &cmd.program;
    let meta = std::fs::metadata(program).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    let key = (program.to_path_buf(), meta.len(), mtime);
    let cache = VERSION_CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return v.clone();
    }
    let cmd = cmd.timeout(Duration::from_secs(20));
    let found = run::output(&cmd)
        .await
        .ok()
        .filter(run::Outcome::success)
        .and_then(|o| version::from_output(&format!("{}\n{}", o.stdout, o.stderr)));
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, found.clone());
    found
}

/// Forget cached `--version` answers.
pub fn forget_versions() {
    if let Some(c) = VERSION_CACHE.get() {
        c.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

/// Installs, updates and removes the tools.
#[derive(Clone)]
pub struct ToolManager {
    cx: Arc<Ctx>,
}

impl std::fmt::Debug for ToolManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cx.fmt(f)
    }
}

impl ToolManager {
    /// For `ctx` (home, data dir, environment) on this computer.
    pub fn new(ctx: &LocalContext, network: NetworkSettings, host: Arc<dyn ToolHost>) -> Self {
        Self::with(ctx, network, host, Platform::detect(), Sources::default())
    }

    /// With an explicit platform and endpoints (tests).
    pub fn with(
        ctx: &LocalContext,
        network: NetworkSettings,
        host: Arc<dyn ToolHost>,
        platform: Platform,
        sources: Sources,
    ) -> Self {
        Self {
            cx: Arc::new(Ctx {
                home: ctx.home_dir.clone(),
                data: ctx.data_dir.clone(),
                env: ctx.env.clone(),
                platform,
                sources,
                network,
                host,
                client: OnceLock::new(),
            }),
        }
    }

    /// The context.
    pub fn ctx(&self) -> &Ctx {
        &self.cx
    }

    /// Every tool. `refresh` looks the newest versions up now (else a day-old
    /// answer is used, and a missing one is looked up when online checks are
    /// on).
    pub async fn view(&self, refresh: bool) -> Result<ToolsView> {
        if refresh {
            forget_versions();
        }
        let mut rows = Vec::new();
        let futures = ToolId::ALL.map(|id| self.row(id, refresh));
        for r in futures_join(futures).await {
            rows.push(r?);
        }
        let mut notes = Vec::new();
        if self.cx.platform.os == Os::Windows {
            notes.push(Notice::info(LocalizedText::new(
                "tools.note.wsl",
                "Tools inside WSL distributions are not managed here yet; install them in the distribution with their own installers.",
            )));
        }
        if !self.cx.network.online_checks && !refresh {
            notes.push(Notice::info(LocalizedText::new(
                "tools.note.offline",
                "Online checks are off: newest versions are looked up only when you check now.",
            )));
        }
        Ok(ToolsView { rows, notes })
    }

    /// One tool.
    pub async fn row(&self, tool: ToolId, refresh: bool) -> Result<ToolRow> {
        let cx = &*self.cx;
        let desc = descriptor(tool, &cx.platform);
        let st = State::load(&cx.data).tool(tool);
        let det = if desc.level == Level::Unavailable {
            Detected::default()
        } else {
            tools::detect(cx, &desc, &st).await
        };
        let channels: Vec<String> = det
            .channels
            .clone()
            .unwrap_or_else(|| desc.channels.iter().map(|c| (*c).to_owned()).collect());
        let channel = pick_channel(
            &channels,
            st.channel.as_deref(),
            det.channel_hint.as_deref(),
        );
        let (latest, checked_at, latest_error) = match (&channel, desc.level) {
            (_, Level::Unavailable) => (None, None, None),
            (Some(ch), _) => self.latest(tool, &desc, ch, refresh, &det).await,
            (None, _) => (None, None, None),
        };
        Ok(compose(
            tool,
            &desc,
            det,
            st,
            channels,
            channel,
            latest,
            checked_at,
            latest_error,
        ))
    }

    async fn latest(
        &self,
        tool: ToolId,
        desc: &Descriptor,
        channel: &str,
        refresh: bool,
        det: &Detected,
    ) -> (Option<String>, Option<Timestamp>, Option<String>) {
        let cx = &*self.cx;
        let mut cache = LatestCache::load(&cx.data);
        let now = Timestamp::now();
        let cached = cache.get(tool, channel).cloned();
        let fresh = cached
            .as_ref()
            .is_some_and(|c| (now.as_millis() - c.at.as_millis()) < LATEST_TTL.as_millis() as i64);
        if !refresh && (fresh || (!cx.network.online_checks && cached.is_some())) {
            let c = cached.expect("checked");
            return (Some(c.version), Some(c.at), None);
        }
        if !refresh && !cx.network.online_checks {
            return (None, None, None);
        }
        match tools::latest(cx, desc, channel, det).await {
            Ok(v) => {
                cache.set(tool, channel, v.clone(), now);
                let _ = cache.save(&cx.data);
                (Some(v), Some(now), None)
            }
            Err(e) => match cached {
                Some(c) => (Some(c.version), Some(c.at), Some(e.to_string())),
                None => (None, None, Some(e.to_string())),
            },
        }
    }

    /// Change the channel or "Superba manages updates".
    pub async fn settings(&self, tool: ToolId, change: ToolSettingsChange) -> Result<ToolRow> {
        let cx = &*self.cx;
        let desc = descriptor(tool, &cx.platform);
        if let Some(ch) = &change.channel {
            let st = State::load(&cx.data).tool(tool);
            let det = tools::detect(cx, &desc, &st).await;
            let channels = det
                .channels
                .unwrap_or_else(|| desc.channels.iter().map(|c| (*c).to_owned()).collect());
            if !channels.iter().any(|c| c == ch) {
                return Err(Error::invalid(format!(
                    "{} has no channel `{ch}` (channels: {})",
                    tool.display_name(),
                    channels.join(", ")
                )));
            }
            State::update(&cx.data, tool, |s| s.channel = Some(ch.clone()))?;
        }
        if let Some(on) = change.manage_updates {
            let _lock = ToolLock::acquire(&cx.data, tool)?;
            tools::manage_updates(cx, &desc, on).await?;
        }
        self.row(tool, false).await
    }

    /// Install, update or remove.
    pub async fn run(
        &self,
        req: ToolRequest,
        sink: Arc<dyn ToolProgressSink>,
    ) -> Result<ToolReport> {
        let cx = &*self.cx;
        let tool = req.tool;
        let desc = descriptor(tool, &cx.platform);
        let _lock = ToolLock::acquire(&cx.data, tool)?;
        forget_versions();
        let st = State::load(&cx.data).tool(tool);
        let det = tools::detect(cx, &desc, &st).await;
        let channels: Vec<String> = det
            .channels
            .clone()
            .unwrap_or_else(|| desc.channels.iter().map(|c| (*c).to_owned()).collect());
        if let Some(ch) = &req.channel
            && !channels.iter().any(|c| c == ch)
        {
            return Err(Error::invalid(format!(
                "{} has no channel `{ch}` (channels: {})",
                tool.display_name(),
                channels.join(", ")
            )));
        }
        let channel = req
            .channel
            .clone()
            .or_else(|| {
                pick_channel(
                    &channels,
                    st.channel.as_deref(),
                    det.channel_hint.as_deref(),
                )
            })
            .unwrap_or_else(|| "latest".into());
        let what = match req.op {
            ToolOp::Install => "install",
            ToolOp::Update => "update",
            ToolOp::Uninstall => "uninstall",
            _ => "operation",
        };
        let log = OpLog::begin(
            &cx.data,
            tool,
            &format!(
                "{what} {} (channel {channel}{}) on {:?} {} — Superba {}",
                tool.display_name(),
                req.version
                    .as_deref()
                    .map(|v| format!(", version {v}"))
                    .unwrap_or_default(),
                cx.platform.os,
                cx.platform.arch_name(),
                env!("CARGO_PKG_VERSION")
            ),
        );
        for i in &det.installs {
            log.line(format!(
                "found: {:?} {} at {}{}",
                i.kind,
                i.version.as_deref().unwrap_or("?"),
                i.path.display(),
                if i.by_superba { " (Superba)" } else { "" }
            ));
        }
        let check = preflight(tool, &desc, &det, &req);
        if let Err(e) = check {
            log.line(format!("refused: {e}"));
            return Err(e);
        }
        if req.channel.is_some() && req.op != ToolOp::Uninstall {
            let _ = State::update(&cx.data, tool, |s| s.channel = Some(channel.clone()));
        }
        let op = Op {
            cx,
            req: &req,
            det: &det,
            desc,
            channel,
            log: &log,
            sink: sink.as_ref(),
        };
        let result = tools::run(&op).await;
        match &result {
            Ok(r) => log.line(format!(
                "done: {}",
                r.version.as_deref().unwrap_or("removed")
            )),
            Err(Error::Cancelled) => log.line("stopped: cancelled"),
            Err(e) => log.line(format!("failed: {e}")),
        }
        forget_versions();
        result
    }
}

/// Run futures one after another (the detections are cheap; the network
/// lookups are few and cached).
async fn futures_join<F: std::future::Future>(fs: [F; 7]) -> Vec<F::Output> {
    let mut out = Vec::with_capacity(7);
    for f in fs {
        out.push(f.await);
    }
    out
}

/// The channel to follow: the user's choice, else the install's own, else
/// the first offered.
fn pick_channel(channels: &[String], chosen: Option<&str>, hint: Option<&str>) -> Option<String> {
    chosen
        .filter(|c| channels.iter().any(|x| x == c))
        .or_else(|| hint.filter(|c| channels.iter().any(|x| x == c)))
        .map(str::to_owned)
        .or_else(|| channels.first().cloned())
}

/// Refuse an operation the row would not offer.
fn preflight(tool: ToolId, desc: &Descriptor, det: &Detected, req: &ToolRequest) -> Result<()> {
    let name = tool.display_name();
    match desc.level {
        Level::Unavailable => {
            return Err(Error::Unsupported {
                what: format!("{name} on this computer"),
            });
        }
        Level::Link => {
            return Err(Error::Unsupported {
                what: format!(
                    "installing {name} on this OS through Superba (download it from {})",
                    det.link.as_deref().unwrap_or(desc.page)
                ),
            });
        }
        _ => {}
    }
    let installed = det.installs.first();
    match req.op {
        ToolOp::Install => {
            if let Some(b) = &det.blocked {
                return Err(Error::Invalid {
                    message: b.fallback.clone(),
                });
            }
            if installed.is_some() && !det.install_beside && req.version.is_none() {
                return Err(Error::Invalid {
                    message: format!("{name} is already installed; update it instead"),
                });
            }
            if !det.install_cautions.is_empty() && !req.confirmed {
                return Err(Error::Invalid {
                    message: format!(
                        "confirm first: {}",
                        det.install_cautions
                            .iter()
                            .map(|c| c.fallback.clone())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                });
            }
        }
        ToolOp::Update => {
            if installed.is_none() {
                return Err(Error::Invalid {
                    message: format!("{name} is not installed"),
                });
            }
            if desc.level == Level::InstallOnly {
                return Err(Error::Unsupported {
                    what: format!("updating {name} (it updates itself)"),
                });
            }
            if !det.can_update {
                return Err(Error::Unsupported {
                    what: format!("updating this {name} install through Superba"),
                });
            }
            if !det.update_cautions.is_empty() && !req.confirmed {
                return Err(Error::Invalid {
                    message: format!(
                        "confirm first: {}",
                        det.update_cautions
                            .iter()
                            .map(|c| c.fallback.clone())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                });
            }
        }
        ToolOp::Uninstall => {
            if installed.is_none() {
                return Err(Error::Invalid {
                    message: format!("{name} is not installed"),
                });
            }
            if !det.can_uninstall {
                return Err(Error::Unsupported {
                    what: format!("removing this {name} install through Superba"),
                });
            }
            if !det.uninstall_cautions.is_empty() && !req.confirmed {
                return Err(Error::Invalid {
                    message: format!(
                        "confirm first: {}",
                        det.uninstall_cautions
                            .iter()
                            .map(|c| c.fallback.clone())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                });
            }
        }
        _ => {
            return Err(Error::Unsupported {
                what: "this operation".into(),
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compose(
    tool: ToolId,
    desc: &Descriptor,
    det: Detected,
    st: ToolState,
    channels: Vec<String>,
    channel: Option<String>,
    latest: Option<String>,
    checked_at: Option<Timestamp>,
    latest_error: Option<String>,
) -> ToolRow {
    let support = match desc.level {
        Level::Full => ToolSupport::Full,
        Level::InstallOnly => ToolSupport::InstallOnly,
        Level::Link => ToolSupport::Link {
            url: det.link.clone().unwrap_or_else(|| desc.page.to_owned()),
        },
        Level::Unavailable => ToolSupport::Unavailable,
    };
    let primary = det.installs.first();
    let update_available = match (primary.and_then(|i| i.version.as_deref()), &latest) {
        (Some(have), Some(want)) => version::is_older(have, want),
        _ => false,
    };
    let managed = matches!(desc.level, Level::Full | Level::InstallOnly);
    let actions = ToolActions {
        install: managed && det.blocked.is_none() && (primary.is_none() || det.install_beside),
        update: desc.level == Level::Full
            && primary.is_some()
            && det.can_update
            && (update_available || primary.is_some_and(|i| i.version.is_none())),
        uninstall: managed && primary.is_some() && det.can_uninstall,
        manage_updates: desc.switch.is_some() && primary.is_some(),
    };
    let updater = det.updater.clone().unwrap_or(match desc.updater {
        descriptor::OwnUpdater::None => ToolUpdater::None,
        descriptor::OwnUpdater::Notifies => ToolUpdater::Notifies,
        descriptor::OwnUpdater::Automatic => ToolUpdater::Automatic,
        descriptor::OwnUpdater::Forced => ToolUpdater::Forced,
    });
    let _ = st;
    ToolRow {
        id: tool,
        name: tool.display_name().to_owned(),
        support,
        installs: det.installs,
        channel,
        channels,
        latest,
        checked_at,
        latest_error,
        update_available,
        updater,
        actions,
        install_cautions: det.install_cautions,
        update_cautions: det.update_cautions,
        uninstall_cautions: det.uninstall_cautions,
        data_dirs: det.data_dirs,
        path_hint: det.path_hint,
        notes: det.notes,
    }
}
