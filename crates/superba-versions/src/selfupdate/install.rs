//! Download, verify, extract and swap.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use superba_core::{
    Error, LocalizedText, Result, SelfInstallOutcome, SelfRelease, SelfUpdateStatus,
};

use super::apply::{self, InstallLayout};
use super::finish::{FinishArgs, FinishPlan};
use super::release::{Os, SUMS_FILE};
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
            return Ok(dest);
        }
        let client = http::client(true)?;
        let mut errors = Vec::new();
        for (source, url) in self.urls(&release.tag, asset) {
            tracing::info!(%source, version = %release.version, "downloading Superba");
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
                Ok(_) => return Ok(dest),
                Err(e) => {
                    tracing::warn!(%source, error = %e, "Superba download failed");
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

    /// Download, verify and install the offered release.
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
        let asset = release.asset.clone().ok_or_else(|| Error::Unsupported {
            what: format!("a Superba package for this system in {}", release.tag),
        })?;
        let archive = self.download(&release, &asset).await?;
        let me = self.clone();
        let rel = release.clone();
        tokio::task::spawn_blocking(move || me.apply_blocking(&rel, &archive, relaunch))
            .await
            .map_err(|e| Error::other(format!("background task failed: {e}")))?
    }

    fn apply_blocking(
        &self,
        release: &SelfRelease,
        archive: &Path,
        relaunch: bool,
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
        };
        let Some(layout) = apply::detect_layout(&exe, os) else {
            return Ok(manual(manual_steps(os, archive, exe.parent())));
        };
        let nonce = crate::store::nonce();
        let pid = std::process::id();
        match layout {
            InstallLayout::Bundle { app, spb } => {
                let Some(parent) = app.parent().filter(|p| apply::writable_dir(p)) else {
                    return Ok(manual(manual_steps(os, archive, app.parent())));
                };
                let stage = parent.join(format!(".superba-update-{nonce}"));
                let result = (|| {
                    let root = self.extract(archive, &stage)?;
                    self.smoke_test(&root, &release.version)?;
                    let mut replaced = Vec::new();
                    if let Some(spb) = spb.filter(|s| s.parent().is_some_and(apply::writable_dir)) {
                        replaced.extend(apply::replace_files(&[(root.join("spb"), spb)])?);
                    }
                    let backup = apply::swap_bundle(&app, &root.join("Superba.app"))?;
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
                        log: Some(self.data_dir.root().join("selfupdate.log")),
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
                })
            }
            InstallLayout::Files { dir, names } => {
                if !apply::writable_dir(&dir) {
                    return Ok(manual(manual_steps(os, archive, Some(&dir))));
                }
                let stage = archive
                    .parent()
                    .unwrap_or(archive)
                    .join(format!("stage-{nonce}"));
                let result = (|| {
                    let root = self.extract(archive, &stage)?;
                    self.smoke_test(&root, &release.version)?;
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
                    if os == Os::Windows {
                        // A running .exe is swapped by the new superba.exe once
                        // this process has exited; keep the staged files.
                        return Ok((root, Vec::new(), true));
                    }
                    let replaced = apply::replace_files(&pairs)?;
                    Ok((root, replaced, false))
                })();
                let (root, replaced, handoff) = match result {
                    Ok(x) => x,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&stage);
                        return Err(e);
                    }
                };
                let gui = dir.join(apply::bin_name("superba", os));
                let log = Some(self.data_dir.root().join("selfupdate.log"));
                if handoff {
                    let exe = root.join("superba.exe");
                    if !exe.is_file() {
                        let _ = std::fs::remove_dir_all(&stage);
                        return Err(Error::Invalid {
                            message: "the package holds no superba.exe".into(),
                        });
                    }
                    return Ok(InstallResult {
                        version: release.version.clone(),
                        outcome: SelfInstallOutcome::Handoff,
                        package: None,
                        steps: Vec::new(),
                        replaced: names,
                        finish: Some(FinishPlan {
                            exe,
                            args: FinishArgs {
                                wait_pid: Some(pid),
                                swap_from: Some(root),
                                swap_to: Some(dir),
                                relaunch: (relaunch && gui.is_file()).then_some(gui),
                                log,
                                ..FinishArgs::default()
                            },
                        }),
                    });
                }
                let _ = std::fs::remove_dir_all(&stage);
                let _ = std::fs::remove_file(archive);
                let finish = (relaunch && gui.is_file()).then(|| FinishPlan {
                    exe: gui.clone(),
                    args: FinishArgs {
                        wait_pid: Some(pid),
                        relaunch: Some(gui),
                        log,
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
                })
            }
        }
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
            "Quit Superba and extract the zip into {dir}, overwriting the old files.",
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
