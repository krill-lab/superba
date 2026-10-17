//! Targets: where a configuration is applied.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;
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

/// How the desktop app reaches a remote host (user bug 36). The app builds its
/// host ids as `remote-ssh-discovered:<alias>` / `remote-ssh-codex-managed:<uuid>`
/// (SSH), `remote-wsl:<distro>` (WSL), `remote-control:<environment id>` (ChatGPT
/// remote control) and `durable` / `chatgpt:<account>:<user>` (hosted by ChatGPT);
/// see [`HostKind::of_host_id`]. Only SSH hosts are managed by Superba (probe,
/// follow, settings sync, Codex installs, session scans, hot restart); the others
/// are shown read-only ([`HostKind::is_listed`]) or not at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostKind {
    /// Reached over SSH: the app's saved SSH connections, `remote-ssh…` ids, live
    /// `ssh … app-server` links and ssh config entries.
    #[default]
    Ssh,
    /// Another device signed in to the same ChatGPT account, controlled through
    /// the ChatGPT backend (`remote-control:<environment id>`); the app calls it a
    /// "Signed in device".
    RemoteControl,
    /// A WSL distribution of this Windows computer (`remote-wsl:<distro>`).
    Wsl,
    /// Hosted by ChatGPT: the app's "Long-lived" cloud app-server (`durable`) and
    /// the account's cloud thread catalog (`chatgpt:<account>:<user>`).
    Cloud,
    /// An id Superba does not recognise.
    Unknown,
}

/// `hostId` prefix of the app's remote-control environments.
pub const HOST_ID_PREFIX_REMOTE_CONTROL: &str = "remote-control:";
/// `hostId` prefix of the app's WSL distributions.
pub const HOST_ID_PREFIX_WSL: &str = "remote-wsl:";
/// `hostId` prefix of every SSH connection of the app (`remote-ssh-discovered:`,
/// `remote-ssh-codex-managed:`).
pub const HOST_ID_PREFIX_SSH: &str = "remote-ssh";

impl HostKind {
    /// The kind of an app `hostId`, as the app's own classifier decides it
    /// (`local` → local, `durable` → durable, `remote-control:` → remote control,
    /// `remote-wsl:` → WSL, `remote-ssh…` → SSH, anything else → remote). The
    /// local host and unrecognised ids are [`HostKind::Unknown`].
    pub fn of_host_id(id: &str) -> HostKind {
        let id = id.trim();
        if id == "durable" || id.starts_with("chatgpt:") {
            HostKind::Cloud
        } else if id.starts_with(HOST_ID_PREFIX_REMOTE_CONTROL) {
            HostKind::RemoteControl
        } else if id.starts_with(HOST_ID_PREFIX_WSL) {
            HostKind::Wsl
        } else if id.starts_with(HOST_ID_PREFIX_SSH) {
            HostKind::Ssh
        } else {
            HostKind::Unknown
        }
    }

    /// Superba manages hosts of this kind (only SSH).
    pub fn is_managed(self) -> bool {
        self == HostKind::Ssh
    }

    /// Hosts of this kind are shown (SSH; remote-control devices and WSL
    /// distributions read-only, as the app lists them among its connections).
    /// Cloud hosts and unknown ids are not: the app lists no connection for them
    /// and Superba has nothing to show.
    pub fn is_listed(self) -> bool {
        matches!(
            self,
            HostKind::Ssh | HostKind::RemoteControl | HostKind::Wsl
        )
    }

    /// Display name of the kind ("Remote-control device").
    pub fn text(self) -> LocalizedText {
        match self {
            HostKind::Ssh => LocalizedText::new("host_kind.ssh", "SSH host"),
            HostKind::RemoteControl => {
                LocalizedText::new("host_kind.remote_control", "Remote-control device")
            }
            HostKind::Wsl => LocalizedText::new("host_kind.wsl", "WSL distribution"),
            HostKind::Cloud => LocalizedText::new("host_kind.cloud", "ChatGPT cloud"),
            HostKind::Unknown => LocalizedText::new("host_kind.unknown", "Other connection"),
        }
    }

    /// Stable lowercase name (`ssh`, `remote_control`, `wsl`, `cloud`, `unknown`),
    /// as serialized.
    pub fn as_str(self) -> &'static str {
        match self {
            HostKind::Ssh => "ssh",
            HostKind::RemoteControl => "remote_control",
            HostKind::Wsl => "wsl",
            HostKind::Cloud => "cloud",
            HostKind::Unknown => "unknown",
        }
    }
}

/// `hostId` prefix of an SSH alias the app discovered in ssh config.
pub const HOST_ID_PREFIX_SSH_DISCOVERED: &str = "remote-ssh-discovered:";

/// The ssh alias of a `remote-ssh-discovered:<encodeURIComponent(alias)>` host id
/// (JavaScript `decodeURIComponent`); `None` for other ids or a bad encoding.
pub fn discovered_ssh_alias(host_id: &str) -> Option<String> {
    let encoded = host_id.trim().strip_prefix(HOST_ID_PREFIX_SSH_DISCOVERED)?;
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut rest = encoded.bytes();
    while let Some(byte) = rest.next() {
        if byte == b'%' {
            let hex = [rest.next()?, rest.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    let alias = String::from_utf8(bytes).ok()?;
    let alias = alias.trim();
    (!alias.is_empty()).then(|| alias.to_owned())
}

/// What to call a remote target when only its id is at hand (`ssh:<key>`): the
/// ssh alias or host it is keyed by, the alias of a discovered SSH id, else the
/// kind of an app host id ("Remote-control device"); never a raw app `hostId`.
pub fn target_title(id: &TargetId) -> LocalizedText {
    let key = id.as_str().strip_prefix("ssh:").unwrap_or(id.as_str());
    if let Some(alias) = discovered_ssh_alias(key) {
        LocalizedText::new(String::new(), alias)
    } else if looks_like_host_id(key) {
        HostKind::of_host_id(key).text()
    } else {
        LocalizedText::new(String::new(), key)
    }
}

/// True when `text` is an app `hostId` rather than a name (`remote-control:…`,
/// `remote-ssh-…`, `durable`, …): such text is never shown as a host's title.
pub fn looks_like_host_id(text: &str) -> bool {
    let text = text.trim();
    text == "local" || HostKind::of_host_id(text) != HostKind::Unknown
}

/// A remote host the desktop app knows (plan 8); only [`HostKind::Ssh`] hosts are
/// reached (over SSH) and managed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteHost {
    /// `ssh:<host_id | alias | host>`.
    pub target_id: TargetId,
    /// Display label: the app's display name, else the ssh alias or host; empty
    /// when nothing better than the raw `hostId` is known (see
    /// [`RemoteHost::title`]).
    pub label: String,
    /// The desktop app's `hostId` for this host. Required for a remote hot restart
    /// (`codex-app-server-restart` with `killCodexProcess: true`); `None` means the
    /// user must reconnect the host in the app for changes to take effect.
    #[serde(default)]
    pub host_id: Option<String>,
    /// How the app reaches the host; only SSH hosts are managed.
    #[serde(default)]
    pub kind: HostKind,
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

    /// Superba manages this host (an SSH host): it may be probed, followed,
    /// synced, get a Codex install, have its sessions scanned and be switched.
    pub fn is_managed(&self) -> bool {
        self.kind.is_managed()
    }

    /// The host is shown in lists ([`HostKind::is_listed`]).
    pub fn is_listed(&self) -> bool {
        self.kind.is_listed()
    }

    /// The host's own name (the app's display name, an ssh alias or `user@host`),
    /// when one is known: never a raw app `hostId`.
    pub fn display_name(&self) -> Option<&str> {
        let label = self.label.trim();
        (!label.is_empty()
            && self.host_id.as_deref().map(str::trim) != Some(label)
            && !looks_like_host_id(label))
        .then_some(label)
    }

    /// What to call the host: its [`RemoteHost::display_name`], else its kind
    /// ("Remote-control device"). The raw `hostId` belongs in a tooltip or a copy
    /// action, never in a title.
    pub fn title(&self) -> LocalizedText {
        match self.display_name() {
            // No key: the name is shown as it is.
            Some(name) => LocalizedText::new(String::new(), name),
            None => self.kind.text(),
        }
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
