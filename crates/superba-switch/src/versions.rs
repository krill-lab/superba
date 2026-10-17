//! The Updates page and `spb codex|app` (v0.3 feedback 8): the desktop app's
//! installed and newest build, the Codex the app runs (`CODEX_CLI_PATH` pin),
//! local CLI binaries by install kind, remote hosts' Codex; installs into
//! Superba's verified store or onto a remote host; app package downloads.
//!
//! Pinning (`SwitchIntent::AppCodexPin`) is always a full app restart: the app's
//! main process reads `CODEX_CLI_PATH` when it starts. Every launch through
//! Superba sets it to the pinned executable, or removes an inherited value when
//! the app runs its bundled Codex, so nothing pins silently. After the relaunch
//! the app-server's process image is compared with the pin (a zsh rc export or
//! a bad path wins silently otherwise); the pin is stored only when it is not
//! contradicted. The pinned version is the app's consumer version everywhere
//! (settings catalog, PAT gate, model catalog) through [`Switcher::effective_installs`].
//! Not offered on Windows in 0.3 (sandbox identity, decision 10).

use std::path::{Path, PathBuf};

use superba_core::{
    AppCodexPin, AppCodexSection, AppDownloadReport, AppInstall, AppOs, AppPackage, AppPackageKind,
    AppRelease, AppUpdateAction, AppVersionsSection, BlockReason, CliUpdateAction, CliVersionRow,
    CodexCandidate, CodexInstallKind, CodexRelease, CodexVersion, CompatTier, EnvOp, Error,
    FeedsStatus, InstallReport, InstallRequest, InstallSource, InstallTarget, LaunchOptions,
    Notice, PlanDestination, ProfileSet, RemoteOs, RemoteVersionRow, RestartKind, Result,
    StoredCodex, SwitchIntent, SwitchPlan, SwitchRequest, TargetSelection, VersionsView,
    codex::{APP_SERVER_FLOOR_FALLBACK, ENV_CODEX_CLI_PATH},
};

use crate::Switcher;
use crate::messages::text;

/// Pinning the app's Codex is offered on this OS.
pub const PIN_SUPPORTED: bool = !cfg!(windows);

/// How the app's Codex is chosen at the next launch of a pin plan.
#[derive(Clone, Debug)]
pub struct PinPlan {
    /// The pin stored after a verified relaunch.
    pub pin: AppCodexPin,
    /// `CODEX_CLI_PATH` value (`None` = removed: the bundled Codex).
    pub exe: Option<PathBuf>,
    /// A store version to download and verify before the app is quit.
    pub ensure: Option<CodexVersion>,
    /// `config.toml` names a model catalog file made for another Codex: it is
    /// checked with the new one (and rebuilt or left out when refused) before
    /// the app restarts.
    pub check_catalog: bool,
}

/// How a Codex version fits the desktop app (spike B6; [`CompatTier::of`]).
pub fn compat_tier(
    bundled: Option<&CodexVersion>,
    floor: &CodexVersion,
    candidate: &CodexVersion,
) -> CompatTier {
    CompatTier::of(bundled, floor, candidate)
}

/// Put the pin into launch options: the executable, or the inherited value
/// removed (an unpinned launch must not pin silently).
pub fn apply_pin(options: &mut LaunchOptions, exe: Option<&Path>) {
    options.env.remove(ENV_CODEX_CLI_PATH);
    options.env_remove.retain(|v| v != ENV_CODEX_CLI_PATH);
    match exe {
        Some(p) => {
            options
                .env
                .insert(ENV_CODEX_CLI_PATH.to_owned(), p.display().to_string());
        }
        None => options.env_remove.push(ENV_CODEX_CLI_PATH.to_owned()),
    }
}

/// The store's executable for `version` on this machine (predicted layout
/// `<data dir>/codex/<version>-<target>/bin/codex`).
pub fn store_executable(
    data_dir: &superba_core::DataDir,
    version: &CodexVersion,
) -> Option<PathBuf> {
    let target = superba_core::local_codex_package_target()?;
    let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
    Some(
        data_dir
            .codex_store_dir()
            .join(format!("{version}-{target}"))
            .join("bin")
            .join(exe),
    )
}

/// Two paths are the same Codex: the same file (links resolved, Unix `(dev,
/// inode)`), or a launcher and the program it execs (macOS `codex-cli/bin/codex`
/// and `CodexCLI.app/Contents/MacOS/codex`), a directory and its `codex`, a
/// desktop app's `codex.exe` and its WSL twin ([`superba_appctl::codexid`]).
pub fn same_file(a: &Path, b: &Path) -> bool {
    superba_appctl::codexid::same_codex(a, b)
}

/// Facts behind a pin mismatch warning.
pub(crate) struct PinCheck<'a> {
    /// The app-server's process image.
    pub actual: &'a Path,
    /// The Codex Superba expects: the pinned one, else the bundled one.
    pub expected: &'a Path,
    /// A pin is configured (`expected` is not the bundled Codex).
    pub pinned: bool,
    /// The install the app runs from (raw, not pin-adjusted).
    pub install: Option<&'a AppInstall>,
    /// Superba's stored Codex versions.
    pub stored: &'a [StoredCodex],
    /// The app was opened by Superba (it passed `CODEX_CLI_PATH` itself).
    pub launched_by_us: bool,
    /// `CODEX_CLI_PATH` is also exported for apps opened elsewhere.
    pub persist_env: bool,
    /// The user's home (`~` in short paths).
    pub home: &'a Path,
}

/// The warning when the running app-server is not the expected Codex (`None`
/// when it is, whatever path names it): names both sides briefly (the bundled
/// Codex, a stored version or a short path; the GUI shows full paths on hover)
/// and the likely cause.
pub(crate) fn pin_mismatch(c: &PinCheck<'_>) -> Option<superba_core::LocalizedText> {
    if same_file(c.actual, c.expected) {
        return None;
    }
    let bundled = c.install.and_then(|i| i.bundled_codex.as_deref());
    let runs_bundled = bundled.is_some_and(|b| same_file(c.actual, b));
    let cause = if c.pinned && runs_bundled {
        if c.launched_by_us || c.persist_env {
            "switch.pin.cause_fallback"
        } else {
            "switch.pin.cause_outside"
        }
    } else {
        "switch.pin.cause_override"
    };
    Some(
        text("switch.pin.mismatch")
            .arg_text("actual", codex_label(c.actual, c))
            .arg_text("expected", codex_label(c.expected, c))
            .arg_text("cause", text(cause)),
    )
}

/// How a pin message names a Codex: the app's bundled one, a stored version, or
/// a short path.
fn codex_label(path: &Path, c: &PinCheck<'_>) -> superba_core::LocalizedText {
    if let Some(i) = c.install
        && i.bundled_codex
            .as_deref()
            .is_some_and(|b| same_file(path, b))
    {
        return match &i.bundled_codex_version {
            Some(v) => text("switch.pin.who_bundled_version").arg("version", v),
            None => text("switch.pin.who_bundled"),
        };
    }
    if let Some(s) = c.stored.iter().find(|s| same_file(path, &s.executable)) {
        return text("switch.pin.who_stored").arg("version", &s.version);
    }
    let short = superba_appctl::codexid::short_path(
        path,
        c.install.map(|i| i.path.as_path()),
        Some(c.home),
    );
    text("switch.pin.who_path").arg("path", short)
}

/// Dotted numeric compare of app versions (`26.1013.31256`).
fn app_version_newer(latest: &str, installed: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    parse(latest) > parse(installed)
}

/// The install kind of a CLI binary from its detected source (fallback when the
/// app controller cannot tell).
fn kind_from_source(source: InstallSource) -> CodexInstallKind {
    match source {
        InstallSource::Npm => CodexInstallKind::Npm,
        InstallSource::Brew => CodexInstallKind::Homebrew,
        InstallSource::AppBundled => CodexInstallKind::AppBundled,
        _ => CodexInstallKind::Other,
    }
}

/// The update action of a CLI row.
pub fn cli_action(
    kind: CodexInstallKind,
    installed: Option<&CodexVersion>,
    latest: Option<&CodexVersion>,
) -> CliUpdateAction {
    let Some(latest) = latest else {
        return CliUpdateAction::None;
    };
    if installed.is_some_and(|v| v >= latest) {
        return CliUpdateAction::None;
    }
    match kind {
        CodexInstallKind::Standalone => CliUpdateAction::Install {
            version: latest.clone(),
        },
        CodexInstallKind::Npm => CliUpdateAction::Command {
            command: format!("npm install -g @openai/codex@{latest}"),
        },
        CodexInstallKind::Homebrew => CliUpdateAction::Command {
            command: "brew upgrade codex".into(),
        },
        _ => CliUpdateAction::None,
    }
}

/// The package of `release` for this OS and architecture (`deb` before `rpm`
/// unless only `rpm` tools exist).
pub fn package_for_here(release: &AppRelease) -> Option<&AppPackage> {
    let os = if cfg!(target_os = "linux") {
        AppOs::Linux
    } else if cfg!(target_os = "macos") {
        AppOs::Macos
    } else {
        AppOs::Windows
    };
    let arch_ok = |a: &str| {
        let a = a.to_ascii_lowercase();
        match std::env::consts::ARCH {
            "x86_64" => matches!(a.as_str(), "amd64" | "x86_64" | "x64"),
            "aarch64" => matches!(a.as_str(), "arm64" | "aarch64"),
            _ => false,
        }
    };
    let rpm_only = os == AppOs::Linux
        && !Path::new("/usr/bin/dpkg").exists()
        && Path::new("/usr/bin/rpm").exists();
    let mut candidates: Vec<&AppPackage> = release
        .packages
        .iter()
        .filter(|p| p.os == os && arch_ok(&p.arch))
        .collect();
    candidates.sort_by_key(|p| match (p.kind, rpm_only) {
        (AppPackageKind::Deb, false) | (AppPackageKind::Rpm, true) => 0,
        _ => 1,
    });
    candidates.into_iter().next()
}

/// The command shown to install a downloaded Linux package (never run). A bare
/// file name (before the download, the package is named only) gets `./`: apt
/// reads an argument without a slash as a package name.
pub fn install_command(package: &AppPackage, path: &Path) -> Option<String> {
    let bare = path.is_relative() && path.parent().is_none_or(|p| p.as_os_str().is_empty());
    let shown = if bare {
        format!("./{}", path.display())
    } else {
        path.display().to_string()
    };
    match package.kind {
        AppPackageKind::Deb => Some(format!("sudo apt install {shown}")),
        AppPackageKind::Rpm => Some(format!("sudo dnf install {shown}")),
        _ => None,
    }
}

/// The per-OS update action (`source`: where the Windows package comes from).
fn update_action(latest: &AppRelease, source: superba_core::WindowsAppSource) -> AppUpdateAction {
    if cfg!(target_os = "linux") {
        match package_for_here(latest) {
            Some(p) => AppUpdateAction::LinuxPackage {
                command: install_command(p, Path::new(p.url.rsplit('/').next().unwrap_or("")))
                    .unwrap_or_default(),
                package: p.clone(),
            },
            None => AppUpdateAction::None,
        }
    } else if cfg!(target_os = "macos") {
        AppUpdateAction::MacSparkle {
            package: package_for_here(latest).cloned(),
        }
    } else {
        // The package is downloaded and opened with App Installer; the Store
        // page and winget stay as alternatives.
        let id = latest.store_product_id.as_deref();
        AppUpdateAction::WindowsPackage {
            package: package_for_here(latest).cloned(),
            source,
            store_url: id.map(|id| format!("ms-windows-store://pdp/?ProductId={id}")),
            command: id.map(|id| format!("winget upgrade --id {id} --source msstore")),
        }
    }
}

impl Switcher {
    fn version_store(&self, set: &ProfileSet) -> std::sync::Arc<dyn superba_core::VersionStore> {
        (self.deps.versions)(&set.settings.network)
    }

    /// The executable `pin` launches the app with (`None` = bundled). A store
    /// version that is not in the store yields its predicted path.
    pub(crate) async fn pin_exe(&self, set: &ProfileSet, pin: &AppCodexPin) -> Option<PathBuf> {
        match pin {
            AppCodexPin::Path { path } => Some(path.clone()),
            AppCodexPin::Version { version } => {
                let stored = self.version_store(set).stored().await.unwrap_or_default();
                stored
                    .into_iter()
                    .find(|s| &s.version == version)
                    .map(|s| s.executable)
                    .or_else(|| store_executable(&self.deps.ctx.data_dir, version))
            }
            _ => None,
        }
    }

    /// Launch options with the configured pin applied.
    pub(crate) async fn with_pin(&self, mut options: LaunchOptions) -> LaunchOptions {
        if !PIN_SUPPORTED {
            return options;
        }
        let set = self.deps.profiles.load().await.unwrap_or_default();
        let exe = self.pin_exe(&set, &set.settings.app_codex.pin).await;
        apply_pin(&mut options, exe.as_deref());
        options
    }

    /// Installs with the app's Codex replaced by the pinned one (path and
    /// version), so every version rule sees what the app actually runs.
    pub(crate) async fn effective_installs(
        &self,
        mut installs: Vec<AppInstall>,
        set: &ProfileSet,
    ) -> Vec<AppInstall> {
        if !PIN_SUPPORTED {
            return installs;
        }
        let pin = self
            .pin_override
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(|| set.settings.app_codex.pin.clone());
        let (exe, version) = match &pin {
            AppCodexPin::Version { version } => {
                (self.pin_exe(set, &pin).await, Some(version.clone()))
            }
            AppCodexPin::Path { path } => {
                let binaries = self.deps.app.codex_binaries().await.unwrap_or_default();
                let version = binaries
                    .iter()
                    .find(|b| same_file(&b.path, path))
                    .and_then(|b| b.version.clone());
                (Some(path.clone()), version)
            }
            _ => return installs,
        };
        for i in &mut installs {
            i.bundled_codex = exe.clone();
            i.bundled_codex_version = version.clone();
        }
        installs
    }

    async fn app_floor(&self, install: Option<&AppInstall>) -> CodexVersion {
        match install {
            Some(i) => self
                .deps
                .app
                .app_server_floor(i)
                .await
                .unwrap_or(APP_SERVER_FLOOR_FALLBACK),
            None => APP_SERVER_FLOOR_FALLBACK,
        }
    }

    fn preferred_install<'a>(
        set: &ProfileSet,
        installs: &'a [AppInstall],
    ) -> Option<&'a AppInstall> {
        set.settings
            .preferred_app_id
            .as_ref()
            .and_then(|id| installs.iter().find(|i| &i.id == id))
            .or_else(|| installs.first())
    }

    /// Plan a pin change (`SwitchIntent::AppCodexPin`): a full app restart with
    /// `CODEX_CLI_PATH` set or removed; the store download happens first at
    /// execution.
    pub(crate) async fn plan_pin(&self, request: SwitchRequest) -> Result<SwitchPlan> {
        let SwitchIntent::AppCodexPin { pin } = request.intent.clone() else {
            return Err(Error::invalid("not a pin change"));
        };
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let set = d.profiles.load().await?;
        let state = d.config.read_state(&home).await?;
        let mut blocks: Vec<BlockReason> = Vec::new();
        let mut warnings: Vec<Notice> = Vec::new();
        if crate::planner::elevated(d.ctx.env.as_ref()) {
            blocks.push(BlockReason::Elevated);
        }
        if !PIN_SUPPORTED {
            blocks.push(BlockReason::UnsupportedTarget {
                detail: "pinning the app's Codex is not offered on Windows".into(),
            });
        }
        let installs = d.app.detect_installs().await.unwrap_or_default();
        let install = Self::preferred_install(&set, &installs).cloned();
        if install.is_none() {
            blocks.push(BlockReason::UnsupportedTarget {
                detail: "no ChatGPT desktop app is installed".into(),
            });
        }
        let floor = self.app_floor(install.as_ref()).await;
        let bundled = install
            .as_ref()
            .and_then(|i| i.bundled_codex_version.clone());
        let mut ensure = None;
        let exe = match &pin {
            AppCodexPin::Version { version } => {
                let tier = compat_tier(bundled.as_ref(), &floor, version);
                if tier.is_blocked() {
                    blocks.push(BlockReason::UnsupportedTarget {
                        detail: format!(
                            "Codex {version} is below the app's minimum version {floor}"
                        ),
                    });
                } else if matches!(tier, CompatTier::Older | CompatTier::Newer) {
                    warnings.push(Notice::warning(
                        text("switch.pin.compat")
                            .arg("version", version)
                            .arg_text("tier", tier.text()),
                    ));
                }
                let stored = self.version_store(&set).stored().await.unwrap_or_default();
                match stored.into_iter().find(|s| &s.version == version) {
                    Some(s) => Some(s.executable),
                    None => {
                        warnings.push(Notice::info(
                            text("switch.pin.download").arg("version", version),
                        ));
                        ensure = Some(version.clone());
                        store_executable(&d.ctx.data_dir, version)
                    }
                }
            }
            AppCodexPin::Path { path } => {
                let ok = std::fs::metadata(path).is_ok_and(|m| m.is_file());
                if !ok {
                    blocks.push(BlockReason::UnsupportedTarget {
                        detail: format!("{} is not an executable file", path.display()),
                    });
                }
                warnings.push(Notice::info(
                    text("switch.pin.custom_path").arg("path", path.display()),
                ));
                Some(path.clone())
            }
            _ => None,
        };
        let check_catalog = blocks.is_empty()
            && self
                .pin_catalog_check(&pin, install.as_ref(), &mut warnings)
                .await;
        let session = d.app.running_session().await.ok().flatten();
        let primary = if !blocks.is_empty() {
            RestartKind::Blocked {
                reasons: blocks.clone(),
            }
        } else if session.is_some() {
            RestartKind::FullAppRestart {
                reason: superba_core::FullRestartReason::Requested,
            }
        } else {
            warnings.push(Notice::info(text("switch.pin.next_launch")));
            RestartKind::None
        };
        // Optional persistence for apps started outside Superba. Without it, a
        // value Superba persisted earlier is removed (it would outlive the pin).
        let env = if blocks.is_empty() {
            let op = match &exe {
                Some(p) if set.settings.app_codex.persist_env => EnvOp::Set {
                    value: p.display().to_string(),
                    take_over_foreign: false,
                },
                _ => EnvOp::Clear,
            };
            match d.env.plan(ENV_CODEX_CLI_PATH, op).await {
                Ok(p) if p.is_noop() => None,
                Ok(p) => Some(p),
                Err(e) if e.is_not_implemented() => None,
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        let processes = d.app.processes(&home).await.unwrap_or_default();
        let restarts =
            crate::rules::consumer_restarts(&primary, true, &processes, home.root(), &[]);
        let label = match &pin {
            AppCodexPin::Version { version } => {
                text("switch.destination.pin_version").arg("version", version)
            }
            AppCodexPin::Path { path } => {
                text("switch.destination.pin_path").arg("path", path.display())
            }
            _ => text("switch.destination.pin_bundled"),
        };
        let mut request = request;
        request.targets = TargetSelection {
            local: true,
            remotes: Vec::new(),
        };
        let id = uuid::Uuid::new_v4().simple().to_string();
        let plan = SwitchPlan {
            id: id.clone(),
            created_at: d.clock.now(),
            request,
            from: state.active.clone(),
            to: PlanDestination {
                profile_id: None,
                site_id: None,
                line_id: None,
                auth_mode: None,
                label,
            },
            primary,
            restarts,
            config: None,
            env,
            migration: None,
            remotes: Vec::new(),
            active_threads: Vec::new(),
            model_check: None,
            choices: Vec::new(),
            alternatives: Vec::new(),
            warnings,
            cleanup: Vec::new(),
        };
        let mut pc = crate::planner::PlanContext::bare(home, set.settings.safe_mode);
        pc.install_id = install.map(|i| i.id);
        pc.launch_authapi = session
            .as_ref()
            .and_then(|s| s.authapi_env.value())
            .and_then(|o| superba_core::Origin::parse(o).ok());
        pc.session = session;
        pc.pin = Some(PinPlan {
            pin,
            exe,
            ensure,
            check_catalog,
        });
        self.remember_plan(id, pc);
        Ok(plan)
    }

    pub(crate) async fn versions_impl(&self, refresh: bool) -> Result<VersionsView> {
        let d = &self.deps;
        let set = d.profiles.load().await?;
        let store = self.version_store(&set);
        let mut notes = Vec::new();
        let mut online_ok = false;
        // "Check now" also looks at this machine again.
        if refresh {
            self.monitor.invalidate();
        }
        let facts = self.monitor.facts().await;
        let installs = facts.installs.clone();
        let install = Self::preferred_install(&set, &installs).cloned();

        // The ChatGPT app.
        let latest = match store.app_latest(refresh).await {
            Ok(l) => {
                online_ok = true;
                l
            }
            Err(e) => {
                notes.push(Notice::info(
                    text("switch.versions.check_failed")
                        .arg("what", "app")
                        .arg_text("reason", e.to_text()),
                ));
                None
            }
        };
        let update_available = match (
            &latest,
            install.as_ref().and_then(|i| i.app_version.as_ref()),
        ) {
            (Some(l), Some(v)) => app_version_newer(&l.version, v),
            _ => false,
        };
        // Windows also offers the package when the app is not installed.
        let offer = update_available || (cfg!(windows) && install.is_none());
        let source = set.settings.network.windows_app_source;
        let app = AppVersionsSection {
            action: latest
                .as_ref()
                .filter(|_| offer)
                .map(|l| update_action(l, source)),
            install: install.clone(),
            latest,
            update_available,
        };

        // The Codex the app runs.
        let releases: Vec<CodexRelease> = match store.codex_releases(refresh).await {
            Ok(r) => {
                online_ok = true;
                r
            }
            Err(e) => {
                notes.push(Notice::info(
                    text("switch.versions.check_failed")
                        .arg("what", "codex")
                        .arg_text("reason", e.to_text()),
                ));
                Vec::new()
            }
        };
        let floor = self.app_floor(install.as_ref()).await;
        let bundled = install
            .as_ref()
            .and_then(|i| i.bundled_codex_version.clone());
        let pin = set.settings.app_codex.pin.clone();
        let pinned_exe = self.pin_exe(&set, &pin).await;
        let session = d.app.running_session().await.ok().flatten();
        let effective_exe = match &session {
            Some(s) => d.app.app_server_exe(s).await.ok().flatten(),
            None => None,
        };
        let mut stored: Vec<StoredCodex> = store.stored().await.unwrap_or_default();
        let binaries = facts.binaries.clone();
        for s in &mut stored {
            s.pinned = matches!(&pin, AppCodexPin::Version { version } if version == &s.version);
            s.in_use = effective_exe
                .as_deref()
                .is_some_and(|e| same_file(e, &s.executable));
        }
        let effective_version = effective_exe.as_deref().and_then(|e| {
            if install
                .as_ref()
                .and_then(|i| i.bundled_codex.as_deref())
                .is_some_and(|b| same_file(b, e))
            {
                bundled.clone()
            } else {
                stored
                    .iter()
                    .find(|s| same_file(&s.executable, e))
                    .map(|s| s.version.clone())
                    .or_else(|| {
                        // A custom path: the version its CLI row probed.
                        binaries
                            .iter()
                            .find(|b| same_file(&b.path, e))
                            .and_then(|b| b.version.clone())
                    })
            }
        });
        let expected_exe = pinned_exe
            .clone()
            .or_else(|| install.as_ref().and_then(|i| i.bundled_codex.clone()));
        let mismatch = match (&effective_exe, &expected_exe) {
            (Some(actual), Some(expected)) if PIN_SUPPORTED => pin_mismatch(&PinCheck {
                actual,
                expected,
                pinned: pinned_exe.is_some(),
                install: install.as_ref(),
                stored: &stored,
                launched_by_us: session.as_ref().is_some_and(|s| s.launched_by_us),
                persist_env: set.settings.app_codex.persist_env,
                home: &d.ctx.home_dir,
            }),
            _ => None,
        };
        let candidates = releases
            .iter()
            .map(|r| CodexCandidate {
                tier: compat_tier(bundled.as_ref(), &floor, &r.version),
                stored: stored.iter().any(|s| s.version == r.version),
                release: r.clone(),
            })
            .collect();
        if let (AppCodexPin::Version { version }, Some(b)) = (&pin, &bundled)
            && b.triple() > version.triple()
        {
            notes.push(Notice::warning(
                text("switch.pin.bundled_ahead")
                    .arg("bundled", b)
                    .arg("pinned", version),
            ));
        }
        let app_codex = AppCodexSection {
            supported: PIN_SUPPORTED,
            pin,
            effective_version,
            effective_exe,
            floor: Some(floor.clone()),
            candidates,
            stored,
            mismatch,
        };

        // Local CLI binaries.
        let latest_stable = releases
            .iter()
            .filter(|r| !r.prerelease && r.version.pre.is_none())
            .map(|r| r.version.clone())
            .max();
        let mut cli = Vec::new();
        for b in binaries {
            let kind = match d.app.cli_install_kind(&b.path).await {
                Ok(k) => k,
                Err(_) => kind_from_source(b.source),
            };
            let action = cli_action(kind, b.version.as_ref(), latest_stable.as_ref());
            cli.push(CliVersionRow {
                binary: b,
                kind,
                latest: latest_stable.clone(),
                action,
            });
        }

        // Remote hosts (last probe; the Remote hosts page probes).
        let mut remote = Vec::new();
        if let Ok(hosts) = self.app_hosts(session.as_ref(), None).await {
            let cache = self.cached_hosts();
            for h in hosts {
                let c = cache.get(&h.target_id);
                let state = c.and_then(|c| c.state.as_ref());
                let version = state.and_then(|s| s.codex_version.clone());
                remote.push(RemoteVersionRow {
                    tier: version
                        .as_ref()
                        .map(|v| compat_tier(bundled.as_ref(), &floor, v)),
                    install_kind: state.and_then(|s| s.codex_install),
                    error: c.and_then(|c| c.error.clone()),
                    target_id: h.target_id,
                    label: h.label,
                    version,
                });
            }
        }
        Ok(VersionsView {
            checked_at: online_ok.then(|| d.clock.now()),
            app,
            app_codex,
            cli,
            remote,
            notes,
        })
    }

    pub(crate) async fn install_codex_impl(
        &self,
        request: InstallRequest,
    ) -> Result<InstallReport> {
        let d = &self.deps;
        let set = d.profiles.load().await?;
        let store = self.version_store(&set);
        match &request.target {
            InstallTarget::Store => {
                let s = store.ensure(&request.version).await?;
                Ok(InstallReport {
                    version: s.version,
                    target: request.target,
                    executable: Some(s.executable.display().to_string()),
                    previous: None,
                    notes: Vec::new(),
                })
            }
            InstallTarget::Remote { target_id } => {
                let (host, session) = self.app_host(target_id).await?;
                let installs = d.app.detect_installs().await.unwrap_or_default();
                let install = Self::preferred_install(&set, &installs).cloned();
                let floor = self.app_floor(install.as_ref()).await;
                if request.version.triple() < floor.triple() {
                    return Err(Error::invalid(format!(
                        "Codex {} is below the app's minimum version {floor}; the app would refuse it on {}",
                        request.version, host.label
                    )));
                }
                let state = match self
                    .cached_hosts()
                    .get(target_id)
                    .and_then(|c| c.state.clone())
                {
                    Some(s) if s.arch.is_some() => s,
                    _ => d.remote.probe(&host).await?,
                };
                let os = match &state.os {
                    RemoteOs::Linux => "linux",
                    RemoteOs::Darwin => "darwin",
                    RemoteOs::Other(o) => {
                        return Err(Error::Unsupported {
                            what: format!("Codex packages for {o}"),
                        });
                    }
                };
                let arch = state.arch.clone().unwrap_or_default();
                let target = superba_core::codex_package_target(os, &arch).ok_or_else(|| {
                    Error::Unsupported {
                        what: format!("Codex packages for {os}/{arch}"),
                    }
                })?;
                let package = store.package_path(&request.version, target).await?;
                let report = d
                    .remote
                    .install_codex(&host, &package, &request.version)
                    .await?;
                Ok(self
                    .after_remote_install(
                        target_id,
                        &host,
                        session.as_ref(),
                        report,
                        request.target.clone(),
                    )
                    .await)
            }
            InstallTarget::Local => {
                store.ensure(&request.version).await?;
                store
                    .install_standalone(&request.version, &self.standalone_layout())
                    .await
            }
            _ => Err(Error::Unsupported {
                what: format!("install target {:?}", request.target),
            }),
        }
    }

    /// [`superba_core::SwitchService::rollback_codex`].
    pub(crate) async fn rollback_codex_impl(&self, target: InstallTarget) -> Result<InstallReport> {
        let d = &self.deps;
        match &target {
            InstallTarget::Local => {
                let set = d.profiles.load().await?;
                self.version_store(&set)
                    .rollback_standalone(&self.standalone_layout())
                    .await
            }
            InstallTarget::Remote { target_id } => {
                let (host, session) = self.app_host(target_id).await?;
                let report = d.remote.rollback_codex(&host).await?;
                Ok(self
                    .after_remote_install(
                        target_id,
                        &host,
                        session.as_ref(),
                        report,
                        target.clone(),
                    )
                    .await)
            }
            _ => Err(Error::Unsupported {
                what: format!("rolling back install target {target:?}"),
            }),
        }
    }

    /// The local standalone CLI layout (`$CODEX_INSTALL_DIR`, else
    /// `~/.local/bin`, and the Codex home's `packages/standalone`).
    fn standalone_layout(&self) -> superba_core::StandaloneLayout {
        let d = &self.deps;
        let bin_dir = d
            .ctx
            .env
            .var_non_empty(superba_core::codex::ENV_CODEX_INSTALL_DIR)
            .map(PathBuf::from)
            .unwrap_or_else(|| d.ctx.home_dir.join(".local").join("bin"));
        superba_core::StandaloneLayout {
            codex_home: d.ctx.codex_home().root().to_path_buf(),
            bin_dir,
        }
    }

    /// After a remote install or rollback: hot restart of the host's app-server
    /// (so the app runs the new codex), re-probe, cache the result.
    async fn after_remote_install(
        &self,
        target_id: &superba_core::TargetId,
        host: &superba_core::RemoteHost,
        session: Option<&superba_core::AppSession>,
        report: superba_core::RemoteInstallReport,
        target: InstallTarget,
    ) -> InstallReport {
        let d = &self.deps;
        let mut notes = Vec::new();
        match (&host.host_id, session.filter(|s| s.cdp.is_some())) {
            (Some(host_id), Some(s)) => {
                if let Err(e) = d
                    .app
                    .hot_restart(s, &superba_core::HotRestartRequest::remote(host_id))
                    .await
                {
                    notes.push(Notice::warning(
                        text("switch.versions.remote_restart_failed")
                            .arg("host", &host.label)
                            .arg_text("reason", e.to_text()),
                    ));
                }
            }
            _ => notes.push(Notice::info(
                text("switch.hosts.no_hot_restart").arg("host", &host.label),
            )),
        }
        let probed = d.remote.probe(host).await;
        let report_copy = report.clone();
        self.cache_host(target_id, |c| {
            c.last_install = Some(report_copy);
            match probed {
                Ok(s) => {
                    c.state = Some(s);
                    c.error = None;
                }
                Err(e) => c.error = Some(e),
            }
        });
        InstallReport {
            version: report.version,
            target,
            executable: Some(report.executable),
            previous: report.previous,
            notes,
        }
    }

    pub(crate) async fn download_app_update_impl(&self) -> Result<AppDownloadReport> {
        let set = self.deps.profiles.load().await?;
        let store = self.version_store(&set);
        let latest = store
            .app_latest(false)
            .await?
            .ok_or_else(|| Error::NotFound {
                what: "a newer ChatGPT app build for this system".into(),
            })?;
        let package = package_for_here(&latest)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                what: "a ChatGPT app package for this system".into(),
            })?;
        let mut report = store.download_app(&package).await?;
        if report.command.is_none() {
            report.command = install_command(&report.package, &report.path);
        }
        Ok(report)
    }

    pub(crate) async fn feeds_status_impl(&self, refresh: bool) -> Result<FeedsStatus> {
        let set = self.deps.profiles.load().await?;
        let feeds = (self.deps.feeds)(&set.settings.network);
        if refresh {
            feeds.snapshot(true).await?;
        }
        feeds.status().await
    }
}
