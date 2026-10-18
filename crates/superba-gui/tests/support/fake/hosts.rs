//! Fake service, area "hosts" (owner B7).
//! Remote hosts page: app-known hosts, probes, follow toggle, sync plans.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here, so the area's
//! owner edits only this file.
//!
//! The hosts are the fake world's remote hosts (`devbox`, `gpu-01`, both known
//! to the app) plus `old-box`, a host known only from `~/.ssh/config`. The fake
//! returns it unfiltered on purpose: the page must never show it. With
//! [`FakeService::with_device_hosts`] the app also reports two ChatGPT
//! remote-control devices (one named "Studio Mac", one known only by its raw
//! id, as 0.1.0 showed it) and a WSL distribution (user bug 36).

use std::collections::BTreeMap;
use std::time::Duration;

use superba_core::*;

use super::FakeService;

/// Target id of the ssh-config-only host.
const SSH_CONFIG_ONLY: &str = "old-box";

/// App host id of the named remote-control device.
const DEVICE: &str = "remote-control:env_e_6a1b8b37df48832d9f0";
/// App host id of the remote-control device without a display name.
const DEVICE_UNNAMED: &str = "remote-control:env_e_00f1c2";
/// App host id of the WSL distribution.
const WSL: &str = "remote-wsl:Ubuntu";

/// Fake state of the area (kept for the life of the fake world).
#[derive(Debug, Default)]
pub(super) struct State {
    follow: BTreeMap<TargetId, bool>,
    probes: Vec<TargetId>,
    follow_calls: Vec<(TargetId, bool)>,
    plan: Option<HostSyncPlan>,
    hide_all: bool,
    devices: bool,
}

impl FakeService {
    /// Label of the host known only from `~/.ssh/config`.
    pub const SSH_CONFIG_ONLY_HOST: &str = SSH_CONFIG_ONLY;

    /// The fixture sync preview for `target`.
    pub fn fixture_sync_plan(target: &TargetId) -> HostSyncPlan {
        fixture_plan(target)
    }

    /// Hosts probed so far, in order.
    pub fn host_probes(&self) -> Vec<TargetId> {
        self.areas.hosts.lock().unwrap().probes.clone()
    }

    /// `set_host_follow` calls, in order.
    pub fn follow_calls(&self) -> Vec<(TargetId, bool)> {
        self.areas.hosts.lock().unwrap().follow_calls.clone()
    }

    /// Replace the sync preview returned for every host.
    pub fn set_sync_plan(&self, plan: HostSyncPlan) {
        self.areas.hosts.lock().unwrap().plan = Some(plan);
    }

    /// Pretend the app knows no remote host (empty state).
    pub fn without_hosts(&self) {
        self.areas.hosts.lock().unwrap().hide_all = true;
    }

    /// App host id of the remote-control device named "Studio Mac".
    pub const DEVICE_HOST_ID: &str = DEVICE;
    /// App host id of the remote-control device the app has no name for.
    pub const UNNAMED_DEVICE_HOST_ID: &str = DEVICE_UNNAMED;
    /// App host id of the WSL distribution.
    pub const WSL_HOST_ID: &str = WSL;

    /// The app also reports two remote-control devices and a WSL distribution.
    pub fn with_device_hosts(&self) {
        self.areas.hosts.lock().unwrap().devices = true;
    }
}

/// A host the app reports that Superba does not manage.
fn device(id: &str, label: &str, kind: HostKind) -> HostView {
    HostView {
        host: RemoteHost {
            target_id: TargetId::remote(id),
            label: label.into(),
            host_id: Some(id.into()),
            kind,
            ssh: SshEndpoint::default(),
            sources: vec![HostSource::Cdp],
            connected: None,
            wsl: None,
        },
        state: None,
        probe_error: None,
        settings: HostSettings::default(),
        pending_sync: None,
        hot_restart: false,
    }
}

/// The fixture sync preview for `target` (Codex 0.162.0): two writes, a skip
/// (needs 0.163), a removal and a conflict.
fn fixture_plan(target: &TargetId) -> HostSyncPlan {
    let row =
        |path: &str, local: Option<&str>, remote: Option<&str>, action, selected| HostSyncRow {
            path: path.into(),
            local: local.map(str::to_owned),
            remote: remote.map(str::to_owned),
            action,
            selected,
        };
    HostSyncPlan {
        target_id: target.clone(),
        profile_id: Some("krill".into()),
        host_version: Some(CodexVersion::new(0, 162, 0)),
        rows: vec![
            row(
                "approval_policy",
                Some("\"on-request\""),
                Some("\"never\""),
                HostSyncAction::Write,
                true,
            ),
            row(
                "model_reasoning_summary",
                Some("\"detailed\""),
                None,
                HostSyncAction::Write,
                true,
            ),
            row(
                "features.memories",
                Some("true"),
                None,
                HostSyncAction::Skip {
                    reason: settings::SkipReason::NotSupported {
                        consumers: vec!["devbox 0.162.0".into()],
                    },
                },
                false,
            ),
            row(
                "notify",
                None,
                Some("[\"notify-send\"]"),
                HostSyncAction::Remove,
                true,
            ),
            row(
                "tui.theme",
                Some("\"dark\""),
                Some("\"light\""),
                HostSyncAction::Conflict,
                false,
            ),
        ],
        blocked: Vec::new(),
        notes: Vec::new(),
    }
}

fn ssh_config_only() -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote(SSH_CONFIG_ONLY),
        label: SSH_CONFIG_ONLY.into(),
        host_id: None,
        kind: superba_core::HostKind::Ssh,
        ssh: SshEndpoint {
            alias: Some(SSH_CONFIG_ONLY.into()),
            host: Some("old-box.example.com".into()),
            port: Some(22),
            user: Some("me".into()),
            identity: None,
        },
        sources: vec![HostSource::SshConfig],
        connected: Some(false),
        wsl: None,
    }
}

fn views(fake: &FakeService) -> Vec<HostView> {
    let area = fake.areas.hosts.lock().unwrap();
    if area.hide_all {
        return Vec::new();
    }
    let s = fake.state();
    let mut out: Vec<HostView> = s
        .remotes
        .iter()
        .map(|r| {
            let id = r.host.target_id.clone();
            let devbox = r.host.label == "devbox";
            let mut state = r.state.clone();
            if let Some(st) = state.as_mut() {
                st.arch = Some(if devbox { "x86_64" } else { "aarch64" }.into());
                st.kernel = Some("6.1.0".into());
                st.disk_free_kib = Some(40 * 1024 * 1024);
                st.codex_install = Some(CodexInstallKind::Standalone);
                st.codex_resolved_path = Some(format!(
                    "{}/packages/standalone/current/bin/codex",
                    st.codex_home
                ));
                st.codex_path = Some("~/.local/bin/codex".into());
            }
            let mut host = r.host.clone();
            if !devbox {
                host.connected = Some(false);
            }
            HostView {
                state,
                probe_error: None,
                settings: HostSettings {
                    follow_switches: area.follow.get(&id).copied().unwrap_or(false),
                    last_sync: devbox.then(|| HostSync {
                        at: Timestamp::now().saturating_sub(Duration::from_secs(20 * 3600)),
                        values_hash: "0".repeat(64),
                        paths: vec!["approval_policy".into(), "notify".into()],
                        value_hashes: Default::default(),
                    }),
                    follow_chosen: false,
                },
                pending_sync: devbox.then_some(3),
                hot_restart: host.host_id.is_some(),
                host,
            }
        })
        .collect();
    out.push(HostView {
        host: ssh_config_only(),
        state: None,
        probe_error: None,
        settings: HostSettings::default(),
        pending_sync: None,
        hot_restart: false,
    });
    if area.devices {
        out.push(device(DEVICE, "Studio Mac", HostKind::RemoteControl));
        // Labelled with its raw id, as 0.1.0 did: the page must not show it.
        out.push(device(
            DEVICE_UNNAMED,
            DEVICE_UNNAMED,
            HostKind::RemoteControl,
        ));
        out.push(device(WSL, "WSL: Ubuntu", HostKind::Wsl));
    }
    out
}

/// Body of `SwitchService::hosts` (deliberately unfiltered, see the module doc).
pub(super) async fn hosts(fake: &FakeService) -> Result<Vec<HostView>> {
    Ok(views(fake))
}

/// Body of `SwitchService::host_probe`.
pub(super) async fn host_probe(fake: &FakeService, target_id: &TargetId) -> Result<HostView> {
    fake.areas
        .hosts
        .lock()
        .unwrap()
        .probes
        .push(target_id.clone());
    let mut v = views(fake)
        .into_iter()
        .find(|h| &h.host.target_id == target_id)
        .ok_or_else(|| Error::NotFound {
            what: format!("host {target_id}"),
        })?;
    if let Some(s) = v.state.as_mut() {
        s.probed_at = Timestamp::now();
    }
    Ok(v)
}

/// Body of `SwitchService::set_host_follow`.
pub(super) async fn set_host_follow(
    fake: &FakeService,
    target_id: &TargetId,
    follow: bool,
) -> Result<HostView> {
    {
        let mut a = fake.areas.hosts.lock().unwrap();
        a.follow.insert(target_id.clone(), follow);
        a.follow_calls.push((target_id.clone(), follow));
    }
    views(fake)
        .into_iter()
        .find(|h| &h.host.target_id == target_id)
        .ok_or_else(|| Error::NotFound {
            what: format!("host {target_id}"),
        })
}

/// Body of `SwitchService::host_sync_plan`.
pub(super) async fn host_sync_plan(
    fake: &FakeService,
    target_id: &TargetId,
) -> Result<HostSyncPlan> {
    let custom = fake.areas.hosts.lock().unwrap().plan.clone();
    Ok(custom.unwrap_or_else(|| fixture_plan(target_id)))
}
