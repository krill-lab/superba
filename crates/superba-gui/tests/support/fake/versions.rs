//! Fake service, area "versions" (owner B7).
//! Updates page: versions, Codex installs, app downloads.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here, so the area's
//! owner edits only this file.

use std::time::Duration;

use superba_core::*;

use super::FakeService;

/// Fake state of the area (kept for the life of the fake world).
#[derive(Debug, Default)]
pub(super) struct State {
    view: Option<VersionsView>,
    installs: Vec<InstallRequest>,
    rollbacks: Vec<InstallTarget>,
    install_error: Option<Error>,
    download_error: Option<Error>,
    refreshes: usize,
}

impl FakeService {
    /// Replace the Updates page data.
    pub fn set_versions(&self, view: VersionsView) {
        self.areas.versions.lock().unwrap().view = Some(view);
    }

    /// The Updates page data the fake returns now.
    pub fn versions_view(&self) -> VersionsView {
        view(self)
    }

    /// Make every install fail with `error`.
    pub fn fail_installs(&self, error: Error) {
        self.areas.versions.lock().unwrap().install_error = Some(error);
    }

    /// Make the app download fail with `error`.
    pub fn fail_download(&self, error: Error) {
        self.areas.versions.lock().unwrap().download_error = Some(error);
    }

    /// Installs requested, in order.
    pub fn installs(&self) -> Vec<InstallRequest> {
        self.areas.versions.lock().unwrap().installs.clone()
    }

    /// Rollbacks requested, in order.
    pub fn rollbacks(&self) -> Vec<InstallTarget> {
        self.areas.versions.lock().unwrap().rollbacks.clone()
    }

    /// `versions(true)` calls.
    pub fn version_refreshes(&self) -> usize {
        self.areas.versions.lock().unwrap().refreshes
    }
}

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).expect("fixture version")
}

fn release(version: &str, prerelease: bool) -> CodexRelease {
    CodexRelease {
        version: v(version),
        tag: format!("rust-v{version}"),
        date: None,
        prerelease,
        bundled_by: Vec::new(),
        sha256: Default::default(),
    }
}

/// The fixture: Linux app 26.1013.2 (bundling the fake world's Codex) with
/// 26.1013.31256 available, pin candidates of every compatibility tier, a
/// standalone, an npm and the bundled CLI, and two remote hosts.
fn fixture_view(fake: &FakeService) -> VersionsView {
    let s = fake.state();
    let install = s.app.installs.first().cloned();
    let bundled = install
        .as_ref()
        .and_then(|i| i.bundled_codex_version.clone())
        .unwrap_or_else(|| v("0.162.0-alpha.17.2"));
    let now = Timestamp::now();
    let package = AppPackage {
        os: AppOs::Linux,
        kind: AppPackageKind::Deb,
        arch: "amd64".into(),
        url: "https://downloads.example.com/chatgpt_26.1013.31256_amd64.deb".into(),
        sha256: Some("ab".repeat(32)),
        size: Some(502_000_000),
        signature: None,
    };
    let candidate = |ver: &str, pre: bool, tier: CompatTier, stored: bool| CodexCandidate {
        release: release(ver, pre),
        tier,
        stored,
    };
    let remotes = s
        .remotes
        .iter()
        .map(|r| RemoteVersionRow {
            target_id: r.host.target_id.clone(),
            label: r.host.label.clone(),
            version: if r.host.label == "devbox" {
                r.state.as_ref().and_then(|st| st.codex_version.clone())
            } else {
                None
            },
            install_kind: (r.host.label == "devbox").then_some(CodexInstallKind::Standalone),
            tier: (r.host.label == "devbox").then_some(CompatTier::Compatible),
            error: (r.host.label != "devbox")
                .then(|| Error::other("ssh: connect to host gpu-01 port 22: timed out")),
        })
        .collect();
    VersionsView {
        checked_at: Some(now.saturating_sub(Duration::from_secs(300))),
        app: AppVersionsSection {
            install,
            latest: Some(AppRelease {
                version: "26.1013.31256".into(),
                date: Some(now.saturating_sub(Duration::from_secs(2 * 86_400))),
                bundled_codex: Some(v("0.163.0-alpha.10.2")),
                packages: vec![package.clone()],
                store_product_id: None,
            }),
            update_available: true,
            action: Some(AppUpdateAction::LinuxPackage {
                package,
                command: "sudo apt install ./chatgpt_26.1013.31256_amd64.deb".into(),
            }),
        },
        app_codex: AppCodexSection {
            supported: true,
            pin: AppCodexPin::Bundled,
            effective_version: Some(bundled.clone()),
            effective_exe: Some("/usr/lib/chatgpt/resources/codex".into()),
            floor: Some(v("0.141.0")),
            candidates: vec![
                candidate(&bundled.to_string(), true, CompatTier::Same, false),
                candidate("0.165.0", false, CompatTier::Newer, false),
                candidate("0.163.0", false, CompatTier::Compatible, true),
                candidate("0.162.0", false, CompatTier::Compatible, true),
                candidate("0.163.0-alpha.10.2", true, CompatTier::Compatible, false),
                candidate("0.160.0", false, CompatTier::Older, false),
                candidate("0.140.0", false, CompatTier::BelowFloor, false),
            ],
            stored: Vec::new(),
            mismatch: None,
        },
        cli: vec![
            CliVersionRow {
                binary: CodexBinary {
                    path: "/home/user/.local/bin/codex".into(),
                    version: Some(v("0.162.0")),
                    source: InstallSource::Unknown,
                    path_rank: Some(0),
                },
                kind: CodexInstallKind::Standalone,
                latest: Some(v("0.165.0")),
                action: CliUpdateAction::Install {
                    version: v("0.165.0"),
                },
            },
            CliVersionRow {
                binary: CodexBinary {
                    path: "/home/user/.npm-global/bin/codex".into(),
                    version: Some(v("0.160.0")),
                    source: InstallSource::Npm,
                    path_rank: Some(1),
                },
                kind: CodexInstallKind::Npm,
                latest: Some(v("0.165.0")),
                action: CliUpdateAction::Command {
                    command: "npm i -g @openai/codex@0.165.0".into(),
                },
            },
            CliVersionRow {
                binary: CodexBinary {
                    path: "/usr/lib/chatgpt/resources/codex".into(),
                    version: Some(bundled),
                    source: InstallSource::AppBundled,
                    path_rank: None,
                },
                kind: CodexInstallKind::AppBundled,
                latest: None,
                action: CliUpdateAction::None,
            },
        ],
        remote: remotes,
        notes: Vec::new(),
    }
}

fn view(fake: &FakeService) -> VersionsView {
    let custom = fake.areas.versions.lock().unwrap().view.clone();
    custom.unwrap_or_else(|| fixture_view(fake))
}

/// Body of `SwitchService::versions`.
pub(super) async fn versions(fake: &FakeService, refresh: bool) -> Result<VersionsView> {
    if refresh {
        fake.areas.versions.lock().unwrap().refreshes += 1;
    }
    Ok(view(fake))
}

/// Body of `SwitchService::install_codex`.
pub(super) async fn install_codex(
    fake: &FakeService,
    request: InstallRequest,
) -> Result<InstallReport> {
    let err = {
        let mut a = fake.areas.versions.lock().unwrap();
        a.installs.push(request.clone());
        a.install_error.clone()
    };
    if let Some(e) = err {
        return Err(e);
    }
    Ok(InstallReport {
        executable: Some(match &request.target {
            InstallTarget::Remote { .. } => "~/.local/bin/codex".into(),
            _ => "/home/user/.local/bin/codex".into(),
        }),
        previous: Some(v("0.162.0")),
        notes: Vec::new(),
        version: request.version,
        target: request.target,
    })
}

/// Body of `SwitchService::rollback_codex`: back to 0.162.0.
pub(super) async fn rollback_codex(
    fake: &FakeService,
    target: InstallTarget,
) -> Result<InstallReport> {
    let err = {
        let mut a = fake.areas.versions.lock().unwrap();
        a.rollbacks.push(target.clone());
        a.install_error.clone()
    };
    if let Some(e) = err {
        return Err(e);
    }
    Ok(InstallReport {
        executable: Some("/home/user/.local/bin/codex".into()),
        previous: None,
        notes: Vec::new(),
        version: v("0.162.0"),
        target,
    })
}

/// Body of `SwitchService::download_app_update`.
pub(super) async fn download_app_update(fake: &FakeService) -> Result<AppDownloadReport> {
    if let Some(e) = fake.areas.versions.lock().unwrap().download_error.clone() {
        return Err(e);
    }
    let view = view(fake);
    let Some(AppUpdateAction::LinuxPackage { package, command }) = view.app.action else {
        return Err(Error::other("no package for this system"));
    };
    Ok(AppDownloadReport {
        path: "/home/user/Downloads/chatgpt_26.1013.31256_amd64.deb".into(),
        sha256: package.sha256.clone().unwrap_or_default(),
        package,
        command: Some(command),
        windows: None,
    })
}
