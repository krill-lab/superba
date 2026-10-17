//! Targets: where a configuration is applied.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::TargetId;

/// A place Superba configures.
///
/// `LocalApp` and `LocalCli` share the same local `CODEX_HOME` (one config, one
/// auth.json); they differ in how consumers are restarted (app-server hot restart
/// vs. next launch). A remote host has its own `CODEX_HOME`, reached over ssh.
// Few instances exist at a time; boxing the remote variant would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Target {
    /// The local desktop app (ChatGPT / Codex Electron app) and its app-server.
    LocalApp,
    /// Local Codex CLI processes (TUI, exec, daemon, IDE app-servers).
    LocalCli,
    /// A remote host the desktop app is connected to over SSH.
    Remote(RemoteHost),
}

impl Target {
    /// Stable id: `local` for both local variants, `ssh:<key>` for remotes.
    pub fn id(&self) -> TargetId {
        match self {
            Target::LocalApp | Target::LocalCli => TargetId::local(),
            Target::Remote(h) => h.target_id.clone(),
        }
    }

    /// True for the local machine.
    pub fn is_local(&self) -> bool {
        matches!(self, Target::LocalApp | Target::LocalCli)
    }

    /// True if both targets write the same `CODEX_HOME`.
    pub fn shares_codex_home_with(&self, other: &Target) -> bool {
        self.id() == other.id()
    }
}

/// A remote SSH host (plan 8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHost {
    /// `ssh:<host_id | alias | host>`.
    pub target_id: TargetId,
    /// Display label (alias or host).
    pub label: String,
    /// The desktop app's `hostId` for this host. Required for a remote hot restart
    /// (`codex-app-server-restart` with `killCodexProcess: true`); `None` means the
    /// user must reconnect the host in the app for changes to take effect.
    #[serde(default)]
    pub host_id: Option<String>,
    /// How to reach it with the system `ssh`.
    pub ssh: SshEndpoint,
    /// Where we learned about it.
    #[serde(default)]
    pub sources: Vec<HostSource>,
    /// Whether the app currently has a live connection (`None` = unknown).
    #[serde(default)]
    pub connected: Option<bool>,
}

impl RemoteHost {
    /// True when the desktop app knows the host (its global state, the renderer,
    /// or a live app-server link). Only these hosts are listed and synced; ssh
    /// config entries alone are never shown (v0.3).
    pub fn is_app_known(&self) -> bool {
        self.sources.iter().any(|s| {
            matches!(
                s,
                HostSource::GlobalState | HostSource::Cdp | HostSource::ProcessScan
            )
        })
    }
}

/// ssh connection parameters, mirroring the app's host structure (plan 8.3):
/// prefer `alias` (reuses `~/.ssh/config`), else `host` + `port` + `user`
/// (+ `identity` with `IdentitiesOnly=yes`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshEndpoint {
    /// `Host` alias from ssh config.
    #[serde(default)]
    pub alias: Option<String>,
    /// Host name or address.
    #[serde(default)]
    pub host: Option<String>,
    /// Port.
    #[serde(default)]
    pub port: Option<u16>,
    /// Login user.
    #[serde(default)]
    pub user: Option<String>,
    /// Identity file path (referenced only; private keys are never read).
    #[serde(default)]
    pub identity: Option<PathBuf>,
}

impl SshEndpoint {
    /// Best human label: alias, else `user@host:port`.
    pub fn label(&self) -> String {
        if let Some(a) = &self.alias {
            return a.clone();
        }
        let mut s = String::new();
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        s.push_str(self.host.as_deref().unwrap_or("?"));
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        s
    }
}

/// Discovery source of a remote host (plan 8.1, read order 1 -> 3 -> 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSource {
    /// `~/.codex/.codex-global-state.json` (`codex-managed-remote-connections`).
    GlobalState,
    /// The renderer's global-state via CDP.
    Cdp,
    /// Live `ssh ... codex app-server --listen` child processes of the app.
    ProcessScan,
    /// `~/.ssh/config` (suggestion only).
    SshConfig,
    /// Added by the user.
    Manual,
}
