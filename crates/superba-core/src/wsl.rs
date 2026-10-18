//! WSL distributions of this Windows computer (user bug 45): `wsl.exe` output,
//! the desktop app's host ids, path mapping and networking facts.
//!
//! Superba manages a distribution like a remote host: scripts run inside it with
//! `wsl.exe -d <distro> --cd ~ --exec /bin/sh -s` (see `superba-remote`), as the
//! distribution's default user, so its own `CODEX_HOME` (`~/.codex`), its Codex
//! CLI and the desktop app's WSL app-server (`remote-wsl:<distro>`) are switched
//! with the same plan / apply / rollback rules as an SSH host.
//!
//! Facts (desktop app 26.1013 for Windows, `app.asar`):
//! - The app lists the default distribution and every other WSL 2 distribution
//!   as a connection `remote-wsl:<encodeURIComponent(distro)>` ("WSL: <distro>").
//! - It starts that distribution's app-server with `wsl.exe -d <distro> --
//!   /usr/bin/bash -lc '…; exec codex app-server …'` (the distribution's own Codex
//!   CLI, `${CODEX_INSTALL_DIR:-$HOME/.local/bin}` first, the environment of a
//!   non-interactive login bash), over stdio (no `--listen`).
//! - `wsl.exe` prints its own messages and `--list` output as UTF-16LE, with or
//!   without a byte order mark; programs inside the distribution write raw bytes.

use std::net::Ipv4Addr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;
use crate::ids::TargetId;
use crate::target::HOST_ID_PREFIX_WSL;

/// The WSL launcher on Windows (`%SystemRoot%\System32\wsl.exe`).
pub const WSL_EXE: &str = "wsl.exe";

/// The shell the desktop app starts a distribution's app-server with (`-lc`).
/// A distribution without it cannot host the app's Codex.
pub const APP_BASH: &str = "/usr/bin/bash";

/// Environment variable that makes `wsl.exe` print UTF-8 instead of UTF-16LE
/// (WSL 0.64+; older versions ignore it, [`decode_output`] handles both).
pub const ENV_WSL_UTF8: &str = "WSL_UTF8";

/// `WSLENV`: the Windows variables `wsl.exe` forwards into a distribution
/// (`NAME/flags` entries separated by `:`).
pub const ENV_WSLENV: &str = "WSLENV";

/// Distributions run by other programs (Docker Desktop, Rancher Desktop): never
/// listed or managed.
const UTILITY_DISTROS: &[&str] = &[
    "docker-desktop",
    "docker-desktop-data",
    "rancher-desktop",
    "rancher-desktop-data",
];

/// One distribution from `wsl.exe --list --verbose`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WslDistro {
    /// Distribution name (`Ubuntu-24.04`).
    pub name: String,
    /// WSL version (1 or 2), `None` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u8>,
    /// The default distribution (`*` in the list).
    #[serde(default)]
    pub default: bool,
    /// State as printed (`Running`, `Stopped`, …; localized by Windows).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// Networking of a WSL 2 distribution (`%USERPROFILE%\.wslconfig`; WSL 1
    /// shares Windows' network stack whatever it says).
    #[serde(default)]
    pub networking: WslNetworking,
}

/// [`WslDistro::state`] of a distribution `wsl.exe --list --running` names.
pub const STATE_RUNNING: &str = "Running";
/// [`WslDistro::state`] of a distribution `wsl.exe --list --running` leaves out.
pub const STATE_STOPPED: &str = "Stopped";

impl WslDistro {
    /// Record whether it runs ([`STATE_RUNNING`] / [`STATE_STOPPED`]).
    pub fn set_running(&mut self, running: bool) {
        self.state = if running {
            STATE_RUNNING
        } else {
            STATE_STOPPED
        }
        .to_owned();
    }

    /// A distribution with only its name known.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            default: false,
            state: String::new(),
            networking: WslNetworking::Nat,
        }
    }

    /// `Some(true)` when Windows reports it running (English state names only;
    /// other languages give `None`).
    pub fn running(&self) -> Option<bool> {
        match self.state.trim().to_ascii_lowercase().as_str() {
            "running" => Some(true),
            "stopped" => Some(false),
            _ => None,
        }
    }

    /// The desktop app's host id of this distribution.
    pub fn host_id(&self) -> String {
        host_id(&self.name)
    }

    /// Superba's target id of this distribution.
    pub fn target_id(&self) -> TargetId {
        target_id(&self.name)
    }

    /// How this distribution's Codex reaches Superba's router
    /// ([`router_path`]).
    pub fn router_path(&self) -> RouterPath {
        router_path(self.version, self.networking)
    }

    /// "WSL 2", "WSL 1" or "WSL".
    pub fn version_text(&self) -> LocalizedText {
        match self.version {
            Some(v) => LocalizedText::new("wsl.version", "WSL {version}").arg("version", v),
            None => LocalizedText::new("wsl.version_unknown", "WSL"),
        }
    }
}

/// Decode output of `wsl.exe` itself the way the desktop app does: a UTF-16LE
/// byte order mark, a UTF-8 one, else UTF-16LE when most odd bytes of the first
/// 256 are zero, else UTF-8 — and, unlike the app, UTF-16LE for bytes that are
/// not UTF-8 at all (a message in a language without ASCII letters). A leading U+FEFF is dropped, CRLF becomes LF
/// and the text is trimmed.
pub fn decode_output(bytes: &[u8]) -> String {
    fn utf16(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    }
    let text = if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        let probe = bytes.len().min(256) & !1;
        let pairs = probe / 2;
        let zeros = (0..probe).step_by(2).filter(|&i| bytes[i + 1] == 0).count();
        if pairs > 0 && zeros * 10 > pairs * 6 {
            utf16(bytes)
        } else if let Ok(text) = std::str::from_utf8(bytes) {
            text.to_owned()
        } else if bytes.len().is_multiple_of(2) {
            // Not UTF-8 at all: UTF-16LE text without ASCII (a localized message).
            utf16(bytes)
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        }
    };
    text.trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\0', "")
        .trim()
        .to_owned()
}

/// Columns of one `--list --verbose` line: split on runs of two or more spaces
/// or on tabs (the app's `/\s{2,}|\t+/`).
fn columns(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let sep = bytes[i] == b'\t'
            || (bytes[i] == b' ' && bytes.get(i + 1).is_some_and(|b| *b == b' ' || *b == b'\t'));
        if sep {
            if let Some(s) = start.take() {
                out.push(line[s..i].trim());
            }
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            continue;
        }
        if start.is_none() {
            start = Some(i);
        }
        i += 1;
    }
    if let Some(s) = start {
        out.push(line[s..].trim());
    }
    out.retain(|c| !c.is_empty());
    out
}

/// Parse decoded `wsl.exe --list --verbose` output (header line first, the
/// default marked with `*`). Unparseable lines and utility distributions
/// (Docker / Rancher Desktop) are skipped; the default comes first.
pub fn parse_list_verbose(text: &str) -> Vec<WslDistro> {
    let mut out: Vec<WslDistro> = Vec::new();
    for line in text.lines().skip(1) {
        let trimmed = line.trim_start();
        let (default, rest) = match trimmed.strip_prefix('*') {
            Some(rest) => (true, rest.trim_start()),
            None => (false, trimmed),
        };
        let cols = columns(rest);
        let Some(name) = cols.first().copied().filter(|n| valid_name(n)) else {
            continue;
        };
        if is_utility_distro(name) || out.iter().any(|d| d.name == name) {
            continue;
        }
        let version = cols
            .get(2)
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| matches!(v, 1 | 2));
        out.push(WslDistro {
            name: name.to_owned(),
            version,
            default,
            state: cols.get(1).copied().unwrap_or_default().to_owned(),
            networking: WslNetworking::Nat,
        });
    }
    out.sort_by_key(|d| !d.default);
    out
}

/// Parse decoded `wsl.exe --list --quiet` output (one name per line).
pub fn parse_list_quiet(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let name = line.trim();
        if valid_name(name) && !is_utility_distro(name) && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out
}

/// A plausible distribution name (`wsl --import` accepts letters, digits and
/// `. - _`; anything with spaces or separators is a message line, not a name).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Distributions of Docker Desktop / Rancher Desktop (never managed).
pub fn is_utility_distro(name: &str) -> bool {
    UTILITY_DISTROS
        .iter()
        .any(|u| u.eq_ignore_ascii_case(name.trim()))
}

/// JavaScript `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// JavaScript `decodeURIComponent`; `None` for a bad escape or invalid UTF-8.
pub fn decode_uri_component(s: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(s.len());
    let mut rest = s.bytes();
    while let Some(byte) = rest.next() {
        if byte == b'%' {
            let hex = [rest.next()?, rest.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

/// The desktop app's host id of a distribution: `remote-wsl:<encodeURIComponent(distro)>`.
pub fn host_id(distro: &str) -> String {
    format!(
        "{HOST_ID_PREFIX_WSL}{}",
        encode_uri_component(distro.trim())
    )
}

/// The distribution named by an app host id `remote-wsl:<…>`.
pub fn distro_of_host_id(host_id: &str) -> Option<String> {
    let encoded = host_id.trim().strip_prefix(HOST_ID_PREFIX_WSL)?;
    let name = decode_uri_component(encoded)?;
    let name = name.trim();
    valid_name(name).then(|| name.to_owned())
}

/// Superba's target id of a distribution (`ssh:remote-wsl:<distro>`, keyed by the
/// app's host id like every remote host).
pub fn target_id(distro: &str) -> TargetId {
    TargetId::remote(&host_id(distro))
}

/// The distribution of a target id made by [`target_id`].
pub fn distro_of_target(id: &TargetId) -> Option<String> {
    distro_of_host_id(id.as_str().strip_prefix("ssh:")?)
}

/// The Windows path of a Linux path inside a distribution
/// (`\\wsl.localhost\<distro>\home\u\.codex`). `None` for a relative path.
pub fn unc_path(distro: &str, linux_path: &str) -> Option<PathBuf> {
    let rest = linux_path.strip_prefix('/')?;
    let mut s = format!(r"\\wsl.localhost\{distro}");
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        s.push('\\');
        s.push_str(part);
    }
    Some(PathBuf::from(s))
}

/// The path a distribution sees for a Windows drive path with the default
/// automount root (`C:\Users\u\x` → `/mnt/c/Users/u/x`, `wslpath -u`). `None`
/// for paths without a drive letter (UNC, relative).
pub fn drvfs_path(windows_path: &str) -> Option<String> {
    let p = windows_path
        .strip_prefix(r"\\?\")
        .unwrap_or(windows_path)
        .replace('\\', "/");
    let mut chars = p.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let rest = chars.as_str().trim_start_matches('/');
    let mut out = format!("/mnt/{}", drive.to_ascii_lowercase());
    if !rest.is_empty() {
        out.push('/');
        out.push_str(rest.trim_end_matches('/'));
    }
    Some(out)
}

/// WSL 2 networking mode (`%USERPROFILE%\.wslconfig`, `[wsl2] networkingMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WslNetworking {
    /// The default: the VM sits behind NAT; Linux `127.0.0.1` is the VM itself
    /// and Windows is reached at the VM's default gateway.
    #[default]
    Nat,
    /// Mirrored: Linux shares Windows' interfaces; `127.0.0.1` reaches Windows.
    Mirrored,
    /// No network.
    None,
    /// `virtioproxy`: like NAT for reaching Windows.
    VirtioProxy,
}

impl WslNetworking {
    /// Stable name (`nat`, `mirrored`, `none`, `virtio_proxy`).
    pub fn as_str(self) -> &'static str {
        match self {
            WslNetworking::Nat => "nat",
            WslNetworking::Mirrored => "mirrored",
            WslNetworking::None => "none",
            WslNetworking::VirtioProxy => "virtio_proxy",
        }
    }
}

/// The networking mode configured in a `.wslconfig` (`[wsl2] networkingMode`,
/// or the preview location `[experimental] networkingMode`); NAT when unset or
/// unknown (WSL falls back to NAT for values it does not know).
pub fn parse_wslconfig_networking(text: &str) -> WslNetworking {
    let mut section = String::new();
    let mut found: Option<(bool, WslNetworking)> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("networkingMode") {
            continue;
        }
        let value = value
            .split(['#', ';'])
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('"')
            .to_ascii_lowercase();
        let mode = match value.as_str() {
            "mirrored" => WslNetworking::Mirrored,
            "none" => WslNetworking::None,
            "virtioproxy" => WslNetworking::VirtioProxy,
            _ => WslNetworking::Nat,
        };
        let primary = section == "wsl2";
        if section == "wsl2" || section == "experimental" {
            // `[wsl2]` wins over the preview `[experimental]` location.
            if found.is_none_or(|(p, _)| !p || primary) {
                found = Some((primary, mode));
            }
        }
    }
    found.map(|(_, m)| m).unwrap_or_default()
}

/// How a distribution's Codex can reach Superba's local router.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RouterPath {
    /// `127.0.0.1` reaches Windows (WSL 1 shares its network stack; WSL 2 with
    /// mirrored networking forwards loopback).
    Loopback,
    /// WSL 2 behind NAT: Windows is the VM's default gateway, so the router needs
    /// a listener on that address (Windows Firewall may block it).
    HostAddress,
    /// No network inside the VM.
    Unreachable,
}

/// The router path a distribution of `version` should use with `networking`.
/// Unknown versions are treated as WSL 2 (the default since 2020).
pub fn router_path(version: Option<u8>, networking: WslNetworking) -> RouterPath {
    match (version, networking) {
        (Some(1), _) => RouterPath::Loopback,
        (_, WslNetworking::Mirrored) => RouterPath::Loopback,
        (_, WslNetworking::None) => RouterPath::Unreachable,
        _ => RouterPath::HostAddress,
    }
}

/// The IPv4 default gateway in `/proc/net/route` contents (destination
/// `00000000`, flags with `RTF_GATEWAY`; addresses are little-endian hex):
/// inside WSL 2 behind NAT this is the Windows host's WSL adapter address.
pub fn default_gateway(proc_net_route: &str) -> Option<Ipv4Addr> {
    for line in proc_net_route.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 || f[1] != "00000000" {
            continue;
        }
        let flags = u16::from_str_radix(f[3], 16).ok()?;
        if flags & 0x2 == 0 {
            continue;
        }
        let raw = u32::from_str_radix(f[2], 16).ok()?;
        let ip = Ipv4Addr::from(raw.swap_bytes());
        if !ip.is_unspecified() {
            return Some(ip);
        }
    }
    None
}

/// Router source id of a distribution (`wsl:<distro>`), the label request-log
/// rows carry for it.
pub fn router_source(distro: &str) -> String {
    format!("wsl:{}", distro.trim())
}

/// The distribution named by a router source id ([`router_source`]).
pub fn distro_of_router_source(source: &str) -> Option<&str> {
    source.strip_prefix("wsl:").filter(|d| valid_name(d))
}

/// WSL on this computer (`spb wsl list`, the Remote hosts page header).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WslOverview {
    /// `%USERPROFILE%\.wslconfig` `networkingMode` (WSL 2).
    #[serde(default)]
    pub networking: WslNetworking,
    /// Why WSL could not be listed (not installed, `wsl.exe` failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The distributions, as hosts (the default first).
    #[serde(default)]
    pub distros: Vec<crate::remote::HostView>,
    /// The router's WSL listeners currently bound (`<address>:<port>`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub router_wsl_listen: Vec<String>,
}

/// Result of turning on router access for a WSL distribution
/// ([`crate::SwitchService::wsl_router_access`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WslAccessReport {
    /// The distribution.
    pub target_id: TargetId,
    /// How it reaches the router.
    pub path: RouterPath,
    /// The address it uses (`http://<address>:<port>`), when it reaches it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// It reaches the router now.
    pub reachable: bool,
    /// A Windows Firewall rule was added or updated.
    #[serde(default)]
    pub firewall_changed: bool,
    /// What happened.
    #[serde(default)]
    pub notes: Vec<crate::i18n::Notice>,
}
