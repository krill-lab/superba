//! Download, verify, extract and swap.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use superba_core::{
    Error, LocalizedText, Result, SelfInstallOutcome, SelfRelease, SelfUpdateStatus,
};

use super::apply::{self, InstallLayout};
use super::finish::{FinishArgs, FinishPlan};
use super::record::{self, Attempt, Stage};
use super::release::{Os, SUMS_FILE};
use super::runlog::RunLog;
use super::state::{Cleanup, State};
use super::{SelfUpdater, version};
use crate::host_of;
use crate::http::{self, Checksum};
use crate::sources::parse_sha256sums;
use crate::{MAX_PACKAGE_BYTES, METADATA_TIMEOUT, extract};

/// What [`SelfUpdater::install`] did.
#[derive(Clone, Debug)]
pub struct InstallResult {
    /// The version installed or staged.
    pub version: String,
    /// How it ended.
    pub outcome: SelfInstallOutcome,
    /// The verified package (kept for a manual install).
    pub package: Option<PathBuf>,
    /// Manual steps.
    pub steps: Vec<LocalizedText>,
    /// File names replaced.
    pub replaced: Vec<String>,
    /// The second stage to start once the caller is ready to quit (and has
    /// restarted the router, on Unix).
    pub finish: Option<FinishPlan>,
    /// The attempt's log ([`super::runlog`]).
    pub log: PathBuf,
}

impl SelfUpdater {
    /// The running executable, resolved.
    fn exe_path(&self) -> Result<PathBuf> {
        let exe = match &self.exe {
            Some(e) => e.clone(),
            None => std::env::current_exe().map_err(|e| Error::Process {
                message: format!("cannot locate the running Superba: {e}"),
            })?,
        };
        Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
    }

    /// Remove what an earlier update left behind: the macOS backup once the
    /// new version runs, interrupted `.superba-old` / `.superba-new` files.
    pub(super) fn cleanup(&self) {
        let path = self.state_path();
        let mut state = State::load(&path);
        if let Some(c) = state.cleanup.clone() {
            let running = version::parse_version(&c.version).is_some_and(|v| v == self.current);
            if !c.path.exists() || running {
                let _ = std::fs::remove_dir_all(&c.path);
                state.cleanup = None;
                let _ = state.save(&path);
            }
        }
        prune_downloads_upto(&self.data_dir.selfupdate_downloads_dir(), &self.current);
        if let Some(dir) = self
            .exe_path()
            .ok()
            .and_then(|e| e.parent().map(Path::to_owned))
        {
            apply::remove_leftovers(&dir);
        }
    }

    /// Candidate URLs of `<tag>/<file>`: the (override) download base first,
    /// then, when opted in, the proxies.
    fn urls(&self, tag: &str, file: &str) -> Vec<(String, String)> {
        let direct = format!(
            "{}/{tag}/{file}",
            self.endpoints.downloads.trim_end_matches('/')
        );
        let mut out = vec![("direct".to_owned(), direct.clone())];
        if self.network.third_party_proxies && !self.endpoints.downloads_overridden {
            for p in &self.endpoints.proxies {
                out.push((format!("proxy:{}", host_of(p)), format!("{p}{direct}")));
            }
        }
        out
    }

    /// The checksums of release `tag`, from its `SHA256SUMS.txt` (never through
    /// a proxy).
    pub async fn release_checksums(&self, tag: &str) -> Result<BTreeMap<String, String>> {
        let url = format!(
            "{}/{tag}/{SUMS_FILE}",
            self.endpoints.downloads.trim_end_matches('/')
        );
        let client = http::client(true)?;
        let text = http::get_text(&client, &url, METADATA_TIMEOUT).await?;
        Ok(parse_sha256sums(&text))
    }

    /// Download `asset` of `release` and verify its size and SHA-256 against the
    /// release's `SHA256SUMS.txt`. Returns the verified archive.
    pub async fn download(&self, release: &SelfRelease, asset: &str) -> Result<PathBuf> {
        self.download_logged(release, asset, None).await
    }

    async fn download_logged(
        &self,
        release: &SelfRelease,
        asset: &str,
        log: Option<&RunLog>,
    ) -> Result<PathBuf> {
        let note = |line: String| {
            if let Some(log) = log {
                log.line(line);
            }
        };
        if asset.contains(['/', '\\']) || asset.starts_with('.') {
            return Err(Error::Invalid {
                message: format!("unexpected package name {asset}"),
            });
        }
        let sums = self.release_checksums(&release.tag).await?;
        let sum = sums.get(asset).cloned().ok_or_else(|| Error::NotFound {
            what: format!("a checksum for {asset} in {SUMS_FILE} of {}", release.tag),
        })?;
        let want = Checksum::Sha256(sum);
        let dir = self
            .data_dir
            .selfupdate_downloads_dir()
            .join(format!("superba-{}", release.version));
        let dest = dir.join(asset);
        // Older updates' downloads are of no use any more.
        prune_downloads(&self.data_dir.selfupdate_downloads_dir(), &dir);
        if dest.is_file() && http::verify_file(&dest, &want, release.asset_size).is_ok() {
            note(format!(
                "{} was downloaded and verified before (SHA-256 {})",
                dest.display(),
                want_hex(&want)
            ));
            return Ok(dest);
        }
        let client = http::client(true)?;
        let mut errors = Vec::new();
        for (source, url) in self.urls(&release.tag, asset) {
            tracing::info!(%source, version = %release.version, "downloading Superba");
            note(format!("downloading {url} ({source})"));
            match http::download(
                &client,
                &url,
                &dest,
                &want,
                release.asset_size,
                MAX_PACKAGE_BYTES,
            )
            .await
            {
                Ok(_) => {
                    note(format!(
                        "downloaded and verified {} (SHA-256 {}, as in {SUMS_FILE})",
                        dest.display(),
                        want_hex(&want)
                    ));
                    return Ok(dest);
                }
                Err(e) => {
                    tracing::warn!(%source, error = %e, "Superba download failed");
                    note(format!("download from {source} failed: {}", e.detail()));
                    errors.push(format!("{source}: {}", e.detail()));
                }
            }
        }
        Err(Error::NotFound {
            what: format!("a verified {asset} ({})", errors.join("; ")),
        })
    }

    /// Extract the verified `archive` into the new directory `into` and return
    /// the directory that holds the package (a single top-level folder is
    /// looked through).
    pub fn extract(&self, archive: &Path, into: &Path) -> Result<PathBuf> {
        let name = archive
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let name = name.unwrap_or_default();
        if name.ends_with(".zip") {
            super::zipx::extract_zip(archive, into)?;
        } else if name.ends_with(".tar.gz") {
            extract::extract(archive, crate::sources::Layout::Package, "", into)?;
        } else {
            return Err(Error::Unsupported {
                what: format!("package format of {name}"),
            });
        }
        let root = payload_root(into);
        let os = self.platform.os;
        let ok = if os == Os::Macos {
            root.join("Superba.app/Contents/MacOS/superba").is_file()
        } else {
            apply::BINARIES
                .iter()
                .any(|b| root.join(apply::bin_name(b, os)).is_file())
        };
        if !ok {
            return Err(Error::Invalid {
                message: format!("{name} does not hold a Superba package"),
            });
        }
        Ok(root)
    }

    /// The new `spb` must run and report the version being installed.
    fn smoke_test(&self, root: &Path, version: &str) -> Result<()> {
        if self.platform != super::release::Platform::current() {
            return Ok(());
        }
        let spb = root.join(apply::bin_name("spb", self.platform.os));
        if !spb.is_file() {
            return Err(Error::Invalid {
                message: "the package holds no spb".into(),
            });
        }
        let out = superba_core::process::command(&spb)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| Error::Process {
                message: format!("the new spb does not start on this system: {e}"),
            })?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || !text.split_whitespace().any(|w| w == version) {
            return Err(Error::Invalid {
                message: format!("the new spb reports {:?}, not {version}", text.trim()),
            });
        }
        Ok(())
    }

    /// Download, verify and install the release offered on the update
    /// channel (always newer than the running build).
    pub async fn install(&self, relaunch: bool) -> Result<InstallResult> {
        let view = self.check(true).await;
        if view.status == SelfUpdateStatus::OnlineChecksOff {
            return Err(Error::Invalid {
                message: "online checks are off in the network preferences".into(),
            });
        }
        let release = match (view.status, view.latest) {
            (SelfUpdateStatus::Available, Some(r)) => r,
            _ => {
                return Err(Error::NotFound {
                    what: "a newer Superba release".into(),
                });
            }
        };
        self.install_chosen(release, relaunch).await
    }

    /// The release of `version` (`0.1.0` or `v0.1.0`), read from
    /// `releases/tags/<tag>`; any channel.
    pub async fn release_of(&self, version: &str) -> Result<SelfRelease> {
        let tag = format!("v{}", version.trim().trim_start_matches('v'));
        if version::parse_tag(&tag).is_none() {
            return Err(Error::invalid(format!(
                "{version} is not a Superba release version (like 0.1.0 or 0.2.0-rc.1)"
            )));
        }
        let api = self.endpoints.api.trim_end_matches('/');
        let url = format!("{api}/releases/tags/{tag}");
        match super::fetch::get(&url, None, super::CHECK_TIMEOUT).await {
            super::fetch::Fetched::Fresh { body, .. } => {
                super::release::parse_latest(&body, &self.platform)
                    .filter(|r| r.tag == tag)
                    .ok_or_else(|| Error::NotFound {
                        what: format!("a published Superba release {tag}"),
                    })
            }
            super::fetch::Fetched::Unavailable | super::fetch::Fetched::NotModified => {
                Err(Error::NotFound {
                    what: format!("a published Superba release {tag}"),
                })
            }
            super::fetch::Fetched::Limited { .. } => Err(Error::other(
                "GitHub's rate limit for requests without an account is reached; try again later",
            )),
            super::fetch::Fetched::Failed(why) => Err(Error::other(format!(
                "could not read the release {tag}: {why}"
            ))),
        }
    }

    /// Download, verify and install release `version`, which the user chose
    /// by name: the only way to an older version than the running one (the
    /// Windows installer only upgrades, so an older version is refused there).
    pub async fn install_version(&self, version: &str, relaunch: bool) -> Result<InstallResult> {
        let release = self.release_of(version).await?;
        let wanted = version::parse_version(&release.version)
            .ok_or_else(|| Error::invalid(format!("{} is not a version", release.version)))?;
        if version::cmp(&wanted, &self.current) == std::cmp::Ordering::Equal {
            return Err(Error::invalid(format!(
                "Superba {} is the running version",
                release.version
            )));
        }
        if self.platform.os == Os::Windows && !version::is_newer(&wanted, &self.current) {
            return Err(Error::Unsupported {
                what: format!(
                    "installing the older Superba {} over {} on Windows (the installer only \
                     upgrades: uninstall Superba, then install the older MSI from {})",
                    release.version, self.current, release.url
                ),
            });
        }
        self.install_chosen(release, relaunch).await
    }

    async fn install_chosen(&self, release: SelfRelease, relaunch: bool) -> Result<InstallResult> {
        let asset = release.asset.clone().ok_or_else(|| Error::Unsupported {
            what: format!("a Superba package for this system in {}", release.tag),
        })?;
        let log = self.begin_attempt(&release);
        let result = self.install_release(&release, &asset, relaunch, &log).await;
        match &result {
            Ok(done) => {
                let stage = match done.outcome {
                    SelfInstallOutcome::Handoff => Stage::Handoff,
                    SelfInstallOutcome::Installed => Stage::Installed,
                    _ => Stage::Manual,
                };
                log.line(format!(
                    "{:?}: replaced {:?}; second stage: {}",
                    done.outcome,
                    done.replaced,
                    done.finish.as_ref().map_or_else(
                        || "none".to_owned(),
                        |f| super::finish::shown_command(
                            &f.exe.to_string_lossy(),
                            &f.args.to_args()
                        )
                    )
                ));
                record::update(&self.state_path(), self.now(), |a| a.stage = stage);
            }
            Err(Error::Cancelled) => {
                log.line("cancelled before replacing files");
                let mut state = State::load(&self.state_path());
                state.last_attempt = None;
                let _ = state.save(&self.state_path());
            }
            Err(e) => self.attempt_failed(e),
        }
        result
    }

    /// Start the log and the record of an attempt to install `release`.
    fn begin_attempt(&self, release: &SelfRelease) -> RunLog {
        let path = RunLog::path_in(&self.data_dir);
        let exe = self
            .exe_path()
            .map_or_else(|e| e.detail(), |p| superba_core::paths::display_path(&p));
        let log = RunLog::begin(
            &path,
            &format!(
                "Superba {} -> {} ({}), {} on {:?} {}",
                self.current,
                release.version,
                release.tag,
                exe,
                self.platform.os,
                self.platform.arch
            ),
        );
        record::start(
            &self.state_path(),
            Attempt::new(
                &self.current.to_string(),
                &release.version,
                &path,
                self.now(),
            ),
        );
        log
    }

    async fn install_release(
        &self,
        release: &SelfRelease,
        asset: &str,
        relaunch: bool,
        log: &RunLog,
    ) -> Result<InstallResult> {
        self.control.check()?;
        let download = self.download_logged(release, asset, Some(log));
        tokio::pin!(download);
        let archive = loop {
            tokio::select! {
                result = &mut download => break result?,
                _ = tokio::time::sleep(std::time::Duration::from_millis(80)) => self.control.check()?,
            }
        };
        self.control.begin_commit()?;
        let me = self.clone();
        let rel = release.clone();
        let log = log.clone();
        tokio::task::spawn_blocking(move || me.apply_blocking(&rel, &archive, relaunch, &log))
            .await
            .map_err(|e| Error::other(format!("background task failed: {e}")))?
    }

    /// Windows: the release ships an MSI (per-user install with a fixed
    /// UpgradeCode, so it upgrades in place). The verified MSI is not run while
    /// Superba is running: a copy of `superba.exe` outside the install folder
    /// waits for this process to exit, runs `msiexec /i <msi> /passive`
    /// (`finish::install_msi`), restarts the router and starts the new Superba.
    /// The one place the Windows install is decided.
    fn windows_msi_handoff(
        &self,
        release: &SelfRelease,
        msi: &Path,
        relaunch: bool,
        exe: &Path,
        log: &RunLog,
    ) -> Result<InstallResult> {
        let dir = exe.parent();
        let gui = dir.map(|d| d.join("superba.exe")).filter(|g| g.is_file());
        let manual = |steps| InstallResult {
            version: release.version.clone(),
            outcome: SelfInstallOutcome::Manual,
            package: Some(msi.to_owned()),
            steps,
            replaced: Vec::new(),
            finish: None,
            log: log.path().to_owned(),
        };
        let Some(gui) = gui else {
            log.line(format!(
                "no superba.exe next to {}: the MSI is left for a manual install",
                exe.display()
            ));
            return Ok(manual(manual_steps(Os::Windows, msi, dir)));
        };
        // The finisher must not live in the folder the MSI replaces.
        let helper = msi.with_file_name(format!("finish-update-{}.exe", crate::store::nonce()));
        if let Err(e) = std::fs::copy(&gui, &helper) {
            log.line(format!(
                "could not copy {} to {}: {e}; the MSI is left for a manual install",
                gui.display(),
                helper.display()
            ));
            return Ok(manual(manual_steps(Os::Windows, msi, dir)));
        }
        log.line(format!(
            "the second stage ({}) installs {} once this process ({}) has exited",
            helper.display(),
            msi.display(),
            std::process::id()
        ));
        Ok(InstallResult {
            version: release.version.clone(),
            outcome: SelfInstallOutcome::Handoff,
            package: Some(msi.to_owned()),
            steps: Vec::new(),
            replaced: vec!["Superba".into()],
            finish: Some(FinishPlan {
                exe: helper,
                args: FinishArgs {
                    wait_pid: Some(std::process::id()),
                    msi: Some(msi.to_owned()),
                    relaunch: relaunch.then_some(gui),
                    log: Some(log.path().to_owned()),
                    state: Some(self.state_path()),
                    ..FinishArgs::default()
                },
            }),
            log: log.path().to_owned(),
        })
    }

    fn apply_blocking(
        &self,
        release: &SelfRelease,
        archive: &Path,
        relaunch: bool,
        log: &RunLog,
    ) -> Result<InstallResult> {
        let exe = self.exe_path()?;
        let os = self.platform.os;
        let manual = |steps| InstallResult {
            version: release.version.clone(),
            outcome: SelfInstallOutcome::Manual,
            package: Some(archive.to_owned()),
            steps,
            replaced: Vec::new(),
            finish: None,
            log: log.path().to_owned(),
        };
        if os == Os::Windows {
            return self.windows_msi_handoff(release, archive, relaunch, &exe, log);
        }
        let Some(layout) = apply::detect_layout(&exe, os) else {
            log.line(format!(
                "{} is not in a layout Superba can replace: the package is left for a manual install",
                exe.display()
            ));
            return Ok(manual(manual_steps(os, archive, exe.parent())));
        };
        log.line(format!("install layout: {layout:?}"));
        let nonce = crate::store::nonce();
        let pid = std::process::id();
        match layout {
            InstallLayout::Bundle { app, spb } => {
                let Some(parent) = app.parent().filter(|p| apply::writable_dir(p)) else {
                    log.line(format!(
                        "{} is not writable: the package is left for a manual install",
                        app.parent().unwrap_or(&app).display()
                    ));
                    return Ok(manual(manual_steps(os, archive, app.parent())));
                };
                let stage = parent.join(format!(".superba-update-{nonce}"));
                let result = (|| {
                    let root = self.extract(archive, &stage)?;
                    log.line(format!("extracted into {}", root.display()));
                    self.smoke_test(&root, &release.version)?;
                    log.line(format!("the new spb reports {}", release.version));
                    let mut replaced = Vec::new();
                    if let Some(spb) = spb.filter(|s| s.parent().is_some_and(apply::writable_dir)) {
                        replaced.extend(apply::replace_files(&[(root.join("spb"), spb.clone())])?);
                        log.line(format!("replaced {}", spb.display()));
                    }
                    let backup = apply::swap_bundle(&app, &root.join("Superba.app"))?;
                    log.line(format!(
                        "replaced {} (the old bundle is kept as {} until the new version runs)",
                        app.display(),
                        backup.display()
                    ));
                    let path = self.state_path();
                    let mut state = State::load(&path);
                    state.cleanup = Some(Cleanup {
                        path: backup,
                        version: release.version.clone(),
                    });
                    let _ = state.save(&path);
                    replaced.push(app.file_name().map_or_else(
                        || "Superba.app".into(),
                        |n| n.to_string_lossy().into_owned(),
                    ));
                    Ok(replaced)
                })();
                let _ = std::fs::remove_dir_all(&stage);
                let replaced = result?;
                let finish = relaunch.then(|| FinishPlan {
                    exe: app.join("Contents/MacOS/superba"),
                    args: FinishArgs {
                        wait_pid: Some(pid),
                        open_app: Some(app.clone()),
                        log: Some(log.path().to_owned()),
                        state: Some(self.state_path()),
                        ..FinishArgs::default()
                    },
                });
                let _ = std::fs::remove_file(archive);
                Ok(InstallResult {
                    version: release.version.clone(),
                    outcome: SelfInstallOutcome::Installed,
                    package: None,
                    steps: Vec::new(),
                    replaced,
                    finish,
                    log: log.path().to_owned(),
                })
            }
            InstallLayout::Files { dir, names } => {
                if !apply::writable_dir(&dir) {
                    log.line(format!(
                        "{} is not writable: the package is left for a manual install",
                        dir.display()
                    ));
                    return Ok(manual(manual_steps(os, archive, Some(&dir))));
                }
                let stage = archive
                    .parent()
                    .unwrap_or(archive)
                    .join(format!("stage-{nonce}"));
                let result = (|| {
                    let root = self.extract(archive, &stage)?;
                    log.line(format!("extracted into {}", root.display()));
                    self.smoke_test(&root, &release.version)?;
                    log.line(format!("the new spb reports {}", release.version));
                    let pairs: Vec<(PathBuf, PathBuf)> = names
                        .iter()
                        .filter(|n| root.join(n).is_file())
                        .map(|n| (root.join(n), dir.join(n)))
                        .collect();
                    if pairs.is_empty() {
                        return Err(Error::Invalid {
                            message: "the package holds none of the installed programs".into(),
                        });
                    }
                    let replaced = apply::replace_files(&pairs)?;
                    log.line(format!("replaced {replaced:?} in {}", dir.display()));
                    Ok(replaced)
                })();
                let replaced = match result {
                    Ok(x) => x,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&stage);
                        return Err(e);
                    }
                };
                let gui = dir.join(apply::bin_name("superba", os));
                let _ = std::fs::remove_dir_all(&stage);
                let _ = std::fs::remove_file(archive);
                let finish = (relaunch && gui.is_file()).then(|| FinishPlan {
                    exe: gui.clone(),
                    args: FinishArgs {
                        wait_pid: Some(pid),
                        relaunch: Some(gui),
                        log: Some(log.path().to_owned()),
                        state: Some(self.state_path()),
                        ..FinishArgs::default()
                    },
                });
                Ok(InstallResult {
                    version: release.version.clone(),
                    outcome: SelfInstallOutcome::Installed,
                    package: None,
                    steps: Vec::new(),
                    replaced,
                    finish,
                    log: log.path().to_owned(),
                })
            }
        }
    }
}

/// A checksum as hex, for the log.
fn want_hex(want: &Checksum) -> String {
    match want {
        Checksum::Sha256(hex) | Checksum::Sha512Integrity(hex) => hex.clone(),
    }
}

/// The folder the package is in: the single top-level directory of `dir`, else
/// `dir` itself.
fn payload_root(dir: &Path) -> PathBuf {
    let mut entries = match std::fs::read_dir(dir) {
        Ok(r) => r.flatten().collect::<Vec<_>>(),
        Err(_) => return dir.to_owned(),
    };
    if entries.len() == 1 {
        let only = entries.remove(0).path();
        if only.is_dir() && only.extension().is_none_or(|e| e != "app") {
            return only;
        }
    }
    dir.to_owned()
}

/// Remove the download folders of versions up to `current` (installed already).
fn prune_downloads_upto(root: &Path, current: &semver::Version) {
    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    for e in read.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let old = name
            .strip_prefix("superba-")
            .and_then(version::parse_version)
            .is_some_and(|v| !version::is_newer(&v, current));
        if old && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Remove the other versions' download folders.
fn prune_downloads(root: &Path, keep: &Path) {
    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    for e in read.flatten() {
        if e.path() != keep && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Short manual steps for a package that cannot be installed in place.
pub fn manual_steps(os: Os, package: &Path, dir: Option<&Path>) -> Vec<LocalizedText> {
    let dir = dir.map_or_else(String::new, |d| d.display().to_string());
    let path = package.display().to_string();
    let (key, fallback) = match os {
        Os::Macos => (
            "selfupdate.manual.macos",
            "Quit Superba, unzip the package and drag Superba.app into {dir}, replacing the old one.",
        ),
        Os::Windows => (
            "selfupdate.manual.windows",
            "Quit Superba and run the installer {path}.",
        ),
        Os::Linux => (
            "selfupdate.manual.linux",
            "Quit Superba and extract the archive over {dir} (use sudo if it belongs to root): tar -xzf {path} --strip-components=1 -C {dir}",
        ),
    };
    vec![
        LocalizedText::new(
            "selfupdate.manual.package",
            "The update was downloaded and verified: {path}",
        )
        .arg("path", &path),
        LocalizedText::new(key, fallback)
            .arg("dir", &dir)
            .arg("path", &path),
    ]
}
