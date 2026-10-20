//! Building a [`Snapshot`] (and its public [`RemoteState`]) from probe records.

use std::path::Path;

use serde::{Deserialize, Serialize};
use superba_core::{
    AuthShape, CodexFiles, CodexInstallKind, CodexVersion, ConfigEngine, ConfigState,
    EnvObservation, Error, RemoteOs, RemoteState, Result, ShellKind, TargetId, Timestamp,
};

use crate::envrc::{RC_FILES, RcBases, RcFile, rc_path};
use crate::records::{FileRead, Records};
use crate::util::{rjoin, sha256_hex};

/// Everything one probe learned, including file bytes (kept in memory only).
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The public observation.
    pub state: RemoteState,
    /// Remote `$HOME`.
    pub home: String,
    /// `${CODEX_INSTALL_DIR:-$HOME/.local/bin}` of the login shell: where the app
    /// looks for `codex` first, so where an install goes.
    pub install_dir: Option<String>,
    /// Current `config.toml` / `auth.json`.
    pub files: CodexFiles,
    /// `config.toml` exists but cannot be read.
    pub config_read: FileRead,
    /// `auth.json` read status.
    pub auth_read: FileRead,
    /// First-write pre-images (`None` when Superba never wrote this host).
    pub first_write: Option<CodexFiles>,
    /// rc files.
    pub rc: Vec<RcFile>,
    /// The login shell ran (the app sees the same environment).
    pub login_ok: bool,
    /// `ConfigEngine::inspect_files` of the current files.
    pub inspected: std::result::Result<ConfigState, Error>,
    /// Effective Codex CA path and file, read inside the app's login shell.
    pub ca_path: Option<String>,
    pub ca_source: FileRead,
    /// Managed bundle under this Codex home.
    pub router_ca: FileRead,
}

impl Snapshot {
    /// Remote path of `config.toml`.
    pub fn config_path(&self) -> String {
        rjoin(&self.state.codex_home, "config.toml")
    }

    /// Remote path of `auth.json`.
    pub fn auth_path(&self) -> String {
        rjoin(&self.state.codex_home, "auth.json")
    }
}

/// Contents of the remote marker `<CODEX_HOME>/.superba/remote-state.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMarker {
    /// Schema version.
    pub schema: u32,
    /// Always `superba`.
    pub tool: String,
    /// Machine id of the host that wrote it.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// Target id used by the writer.
    #[serde(default)]
    pub target_id: Option<String>,
    /// Apply id.
    #[serde(default)]
    pub apply_id: Option<String>,
    /// When written (ms since epoch, writer's clock).
    #[serde(default)]
    pub written_at: Option<i64>,
}

fn os_of(uname: &str) -> RemoteOs {
    match uname.trim() {
        "Linux" => RemoteOs::Linux,
        "Darwin" => RemoteOs::Darwin,
        other => RemoteOs::Other(other.to_owned()),
    }
}

/// Machine architecture from `uname -m`; a shell running under Rosetta
/// (`sysctl.proc_translated` = 1) reports `x86_64` on Apple silicon.
fn arch_of(uname_m: Option<&str>, translated: Option<&str>) -> Option<String> {
    let arch = uname_m?.trim();
    if translated.map(str::trim) == Some("1") && arch == "x86_64" {
        return Some("arm64".to_owned());
    }
    (!arch.is_empty()).then(|| arch.to_owned())
}

fn file_bytes(read: &FileRead) -> Option<superba_core::SecretBytes> {
    read.bytes().cloned()
}

/// Turn probe records into a snapshot. `engine.inspect_files` provides the parsed
/// view of the Codex files (provider, base URL, auth shape), as the contract asks
/// remote probes not to parse TOML/JSON themselves.
pub fn snapshot_from_records(
    target: &TargetId,
    recs: &Records,
    now: Timestamp,
    engine: &dyn ConfigEngine,
) -> Result<Snapshot> {
    recs.require_done("probe")?;
    let os = os_of(recs.get("os").unwrap_or_default());
    let shell_path = recs.get_nonempty("shell").unwrap_or_default();
    let shell = ShellKind::from_shell_path(shell_path);
    let home = recs
        .get_raw_nonempty("home")
        .ok_or_else(|| Error::parse("remote probe", "the host reported no $HOME"))?
        .to_owned();
    let login_ok = recs.has("login_done");
    let codex_home = recs
        .get_raw_nonempty("codex_home_effective")
        .map(str::to_owned)
        .unwrap_or_else(|| rjoin(&home, ".codex"));
    // The script passes CODEX_HOME through a plain text line; a value that does
    // not survive that unchanged (control characters) is refused, not truncated.
    if let Some(login_home) = recs.get_raw_nonempty("codex_home")
        && login_ok
        && login_home != codex_home
    {
        return Err(Error::Unsupported {
            what: format!("remote CODEX_HOME {login_home:?} (control characters in the path)"),
        });
    }
    for (what, value) in [("HOME", &home), ("CODEX_HOME", &codex_home)] {
        if value.chars().any(char::is_control) || !value.starts_with('/') {
            return Err(Error::Unsupported {
                what: format!(
                    "remote {what} {value:?} (must be an absolute path without control characters)"
                ),
            });
        }
    }
    let bases = RcBases {
        home: home.clone(),
        zdotdir: recs
            .get_raw_nonempty("zdotdir")
            .filter(|d| d.starts_with('/') && !d.chars().any(char::is_control))
            .map(str::to_owned),
        xdg_config_home: recs
            .get_raw_nonempty("xdg_config_home")
            .filter(|d| d.starts_with('/') && !d.chars().any(char::is_control))
            .map(str::to_owned),
    };

    let config_read = recs.file("config").cloned().unwrap_or(FileRead::Absent);
    let auth_read = recs.file("auth").cloned().unwrap_or(FileRead::Absent);
    let files = CodexFiles {
        config_toml: file_bytes(&config_read),
        auth_json: file_bytes(&auth_read),
    };
    let first_write = {
        let c = recs.file("fw_config");
        let a = recs.file("fw_auth");
        let recorded = matches!(c, Some(FileRead::Bytes(_)))
            || matches!(a, Some(FileRead::Bytes(_)))
            || recs.has("fw_config_absent")
            || recs.has("fw_auth_absent");
        recorded.then(|| CodexFiles {
            config_toml: c.and_then(FileRead::bytes).cloned(),
            auth_json: a.and_then(FileRead::bytes).cloned(),
        })
    };
    let rc = RC_FILES
        .iter()
        .map(|spec| {
            let read = recs
                .file(&format!("rc.{}", spec.name))
                .cloned()
                .unwrap_or(FileRead::Absent);
            RcFile {
                spec: *spec,
                path: rc_path(&bases, spec),
                unreadable: read.is_unavailable(),
                content: read.bytes().map(|b| b.expose().to_vec()),
            }
        })
        .collect();

    let authapi_env = if login_ok {
        if let Some(v) = recs.get("authapi") {
            EnvObservation::Present(v.to_owned())
        } else if recs.has("authapi_absent") {
            EnvObservation::Absent
        } else {
            EnvObservation::Unknown
        }
    } else if let Some(v) = recs.get("plain_authapi") {
        EnvObservation::Present(v.to_owned())
    } else if recs.has("plain_authapi_absent") {
        EnvObservation::Absent
    } else {
        EnvObservation::Unknown
    };

    let marker_machine_id = recs
        .file("marker")
        .and_then(FileRead::bytes)
        .and_then(|b| serde_json::from_slice::<RemoteMarker>(b.expose()).ok())
        .and_then(|m| m.machine_id)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    let inspected = engine.inspect_files(Path::new(&codex_home), &files);
    let (auth, model_provider, openai_base_url) = match &inspected {
        Ok(cs) => (
            cs.auth.clone(),
            cs.model_provider.clone(),
            cs.openai_base_url.clone(),
        ),
        Err(e) => (
            match &files.auth_json {
                None => AuthShape::Missing,
                Some(_) => AuthShape::Unknown {
                    detail: format!("not inspected: {}", e.code()),
                },
            },
            None,
            None,
        ),
    };

    let codex_resolved_path = recs
        .get_raw_nonempty("codex_resolved")
        .filter(|p| p.starts_with('/') && !p.chars().any(char::is_control))
        .map(str::to_owned);
    let state = RemoteState {
        disk_free_kib: recs.get_nonempty("disk_free").and_then(|v| v.parse().ok()),
        kernel: recs.get_nonempty("kernel").map(str::to_owned),
        arch: arch_of(recs.get_nonempty("arch"), recs.get_nonempty("translated")),
        codex_install: codex_resolved_path
            .as_deref()
            .map(CodexInstallKind::from_resolved_path),
        codex_resolved_path,
        target_id: target.clone(),
        probed_at: now,
        os,
        shell,
        codex_home,
        codex_path: recs.get_nonempty("codex_path").map(str::to_owned),
        codex_version: recs
            .get_nonempty("codex_version")
            .and_then(|v| CodexVersion::parse(v).ok()),
        config_sha256: files.config_toml.as_ref().map(|b| sha256_hex(b.expose())),
        auth_sha256: files.auth_json.as_ref().map(|b| sha256_hex(b.expose())),
        auth,
        model_provider,
        openai_base_url,
        machine_id: recs.get_nonempty("machine_id").map(str::to_owned),
        marker_machine_id,
        authapi_env,
        app_server_pids: parse_pids(recs.get("pids").unwrap_or_default()),
        owned_paths: Vec::new(),
        config_toml: files.config_toml.clone(),
        gateway: match recs.file("route") {
            Some(FileRead::Bytes(b)) => {
                superba_core::wsl::default_gateway(&String::from_utf8_lossy(b.expose()))
                    .map(|ip| ip.to_string())
            }
            _ => None,
        },
        app_bash: recs.get_nonempty("app_bash").map(|v| v == "1"),
    };
    let install_dir = recs
        .get_raw_nonempty("install_dir")
        .filter(|d| d.starts_with('/') && !d.chars().any(char::is_control))
        .map(str::to_owned);
    Ok(Snapshot {
        state,
        install_dir,
        home,
        files,
        config_read,
        auth_read,
        first_write,
        rc,
        login_ok,
        inspected,
        ca_path: recs.get_raw_nonempty("ca_path").map(str::to_owned),
        ca_source: recs.file("ca_source").cloned().unwrap_or(FileRead::Absent),
        router_ca: recs.file("router_ca").cloned().unwrap_or(FileRead::Absent),
    })
}

/// Parse a whitespace separated pid list.
pub fn parse_pids(s: &str) -> Vec<u32> {
    let mut v: Vec<u32> = s
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}
