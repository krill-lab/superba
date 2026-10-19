//! POSIX `sh` scripts sent to remote hosts, and the encoding of data inside them.
//!
//! Protocol:
//! - A script is fed to `sh -s` on stdin. Its body is a `main` function that is
//!   parsed completely before it runs and is invoked with `</dev/null`, so no
//!   command inside can swallow the rest of the script from stdin.
//! - Results are lines `@@SB:<nonce>:<T>:<name>:<hex>` (`T`: `K` value, `F` file
//!   contents, `A` absent file, `U` unreadable, `L` too large); values are hex
//!   encoded with POSIX `od`, so arbitrary bytes and any noise printed by the
//!   user's shell startup files cannot be confused with results.
//! - Bytes written remotely (new `config.toml`, `auth.json`, rc files) are embedded
//!   as `printf '%b'` literals inside the script, i.e. they travel on stdin only.
//! - Only POSIX utilities are required (`od`, `tr`, `sed`, `awk`, `cmp`, `cp`,
//!   `mv`, `mkdir`, `ps`); non-POSIX extras (`stat -c|-f`, `find -mmin`, `grep -o`)
//!   are used only where a failure is detected and reported (turn activity, stale
//!   lock detection).
//! - The desktop app starts the remote app-server through the user's login shell
//!   (`$SHELL -l -i -c ...`, with `~/.local/bin` prepended to `PATH`); the probe
//!   runs its environment checks the same way so it sees the same `CODEX_HOME`,
//!   `CODEX_AUTHAPI_BASE_URL` and `codex` binary.
//!
//! Write protocol ([`write_script`]): take the per-home lock (`mkdir`), check every
//! precondition, capture first-write pre-images, back up, stage each new file in a
//! private staging directory created with `mkdir` next to its target (exclusive, so
//! nothing can be planted there), then per file re-check its precondition and
//! publish it (`mv`, or a verified write through a symlink). Any failure undoes the
//! files already published; an undo that fails is reported as `partial`.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::envrc::RC_FILES;

/// Prefix of every result record.
pub const RECORD_PREFIX: &str = "@@SB:";

/// Largest remote file Superba reads (bytes); bigger files are reported as `L`.
pub const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Name of Superba's directory inside a remote Codex home.
pub const REMOTE_DIR: &str = ".superba";

/// A lock older than this many minutes is considered stale and broken.
pub const STALE_LOCK_MINUTES: u32 = 10;

/// Rollouts examined by the turn check at most; more recent rollouts than this make
/// the result incomplete.
pub const MAX_ROLLOUTS: usize = 256;

/// A fresh per-script nonce (16 hex chars). It keeps result records apart from
/// unrelated output and makes remote staging names unpredictable.
pub fn new_nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = Sha256::new();
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .to_le_bytes(),
    );
    h.update(std::process::id().to_le_bytes());
    h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hex::encode(h.finalize())[..16].to_owned()
}

/// Quote `s` as one POSIX shell word (single quotes, `'` as `'\''`).
pub fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Source bytes per `printf` call, so even a non-builtin `printf` stays far below
/// `ARG_MAX`.
const PRINTF_CHUNK: usize = 2048;

/// A shell command (brace group) that writes exactly `bytes` to stdout.
///
/// Printable ASCII other than `'` and `\` is kept verbatim (as are `\n` and `\t`);
/// everything else becomes a `\0ooo` escape, which `printf '%b'` turns back into the
/// byte in dash, bash, zsh, busybox ash and the BSD/macOS sh. The script itself
/// stays pure ASCII.
pub fn printf_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return ":".to_owned();
    }
    let mut out = String::from("{ ");
    for chunk in bytes.chunks(PRINTF_CHUNK) {
        out.push_str("printf '%b' '");
        for &b in chunk {
            match b {
                b'\'' | b'\\' => {
                    let _ = write!(out, "\\0{b:03o}");
                }
                b'\n' | b'\t' | 0x20..=0x7e => out.push(char::from(b)),
                _ => {
                    let _ = write!(out, "\\0{b:03o}");
                }
            }
        }
        out.push_str("'; ");
    }
    out.push('}');
    out
}

/// Helper functions shared by every script (and by the login-shell payload).
pub(crate) fn prelude(nonce: &str) -> String {
    format!(
        r#"umask 077
LC_ALL=C
export LC_ALL
SB_N='{nonce}'
sb_phase() {{ if [ "${{SB_PROGRESS:-}}" = 1 ]; then printf '@@SBPH:%s:%s\n' "$SB_N" "$1" >&4; fi; }}
sb_hex() {{ od -A n -v -t x1 | tr -d ' \t\n'; }}
sb_rec() {{ printf '@@SB:%s:%s:%s:' "$SB_N" "$1" "$2"; }}
sb_put() {{ sb_rec K "$1"; printf '%s' "$2" | sb_hex; printf '\n'; }}
sb_file() {{
  if [ -f "$2" ]; then
    if [ ! -r "$2" ]; then sb_rec U "$1"; printf '\n'; return 0; fi
    sb_sz=$(wc -c < "$2" 2>/dev/null | tr -d ' \t\n')
    case "$sb_sz" in ''|*[!0-9]*) sb_rec U "$1"; printf '\n'; return 0 ;; esac
    if [ "$sb_sz" -gt {max} ]; then sb_rec L "$1"; printf '\n'; return 0; fi
    sb_rec F "$1"; sb_hex < "$2"; printf '\n'
  elif [ -e "$2" ] || [ -L "$2" ]; then sb_rec U "$1"; printf '\n'
  else sb_rec A "$1"; printf '\n'
  fi
}}
"#,
        max = MAX_READ_BYTES
    )
}

/// How a script reaches the Codex the desktop app runs on a host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Flavor {
    /// An SSH host: the app starts `codex app-server --listen` through the user's
    /// login shell (`$SHELL -l -i -c`).
    #[default]
    Ssh,
    /// A WSL distribution (user bug 45): the app starts `codex … app-server`
    /// over stdio with `/usr/bin/bash -lc` (the environment of a non-interactive
    /// login bash) and the PATH of an interactive login bash.
    Wsl,
}

impl Flavor {
    /// The flavor of a host.
    pub fn of(host: &superba_core::RemoteHost) -> Self {
        if host.kind == superba_core::HostKind::Wsl {
            Flavor::Wsl
        } else {
            Flavor::Ssh
        }
    }
}

/// The host functions of a flavor: [`HOST_FUNCS`], with the WSL login view and
/// app-server pattern for [`Flavor::Wsl`].
fn host_funcs(flavor: Flavor) -> String {
    match flavor {
        Flavor::Ssh => HOST_FUNCS.to_owned(),
        Flavor::Wsl => format!("{HOST_FUNCS}{WSL_FUNCS}"),
    }
}

/// WSL overrides (defined after [`HOST_FUNCS`], so they replace `sb_login` and
/// `sb_pids`):
/// - `sb_login`: what the app's WSL app-server sees: `/usr/bin/bash -lc` (a
///   non-interactive login bash: `/etc/profile`, the first of `~/.bash_profile`,
///   `~/.bash_login`, `~/.profile`) with the `PATH` an interactive login bash
///   prints (the app reads it the same way, between markers). Without
///   `/usr/bin/bash` the app cannot use the distribution; the user's login shell
///   (the CLI's view) is used instead.
/// - `sb_pids`: the app's stdio app-server (`app-server` with
///   `features.code_mode_host`, which the app always passes; IDE app-servers of
///   the same user are not the app's). The app's "Run Codex in WSL" app-server
///   runs the app's own Linux Codex from the Windows Codex home
///   (`/mnt/c/Users/<u>/.codex/bin/wsl/<16 hex>/codex`, under a `bash` wrapper
///   that sets `CODEX_SQLITE_HOME`; user bug 54 (b), seen in the test VM): it
///   reads that home, not this distribution's, so neither is counted.
const WSL_FUNCS: &str = r#"sb_user_login() {
  sb_ls=$(sb_shell)
  if [ -z "$sb_ls" ] || [ ! -x "$sb_ls" ]; then return 127; fi
  SB_PAYLOAD="$1"
  export SB_PAYLOAD
  case "${sb_ls##*/}" in
    csh|tcsh) "$sb_ls" -i -c 'set loginsh=1; if ( -r /etc/csh.login ) source /etc/csh.login; if ( -r ~/.login ) source ~/.login; exec /bin/sh -c "$SB_PAYLOAD"' ;;
    nu) "$sb_ls" -l -i -c 'exec /bin/sh -c $env.SB_PAYLOAD' ;;
    fish|xonsh) "$sb_ls" -l -i -c 'exec /bin/sh -c "$SB_PAYLOAD"' ;;
    *) "$sb_ls" -l -i -c 'CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"; export CODEX_HOME; exec /bin/sh -c "$SB_PAYLOAD"' ;;
  esac
}
sb_login() {
  if [ ! -x /usr/bin/bash ]; then sb_user_login "$1"; return $?; fi
  SB_PAYLOAD="$1"
  SB_IPC='printf "%s
" __SB_P0__ "$PATH" __SB_P1__'
  SB_WRAP='sb_ip=$(/usr/bin/bash -ilc "$SB_IPC" </dev/null 2>/dev/null | sed -n "/^__SB_P0__$/,/^__SB_P1__$/p" | sed "1d;\$d"); if [ -n "$sb_ip" ]; then PATH=$sb_ip; export PATH; fi; CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"; export CODEX_HOME; exec /bin/sh -c "$SB_PAYLOAD"'
  export SB_PAYLOAD SB_IPC SB_WRAP
  /usr/bin/bash -lc "$SB_WRAP"
}
sb_pids() {
  sb_u=$(id -u 2>/dev/null)
  if sb_ps=$(ps -A -o pid= -o uid= -o args= 2>/dev/null) && [ -n "$sb_ps" ]; then
    printf '%s
' "$sb_ps" | awk -v u="$sb_u" '$2 == u && /[a]pp-server/ && /code_mode_host/ && !/\/bin\/wsl\/([^ \/]+\/)?codex / && !/CODEX_SQLITE_HOME=/ { printf "%s ", $1 }'
    return 0
  fi
  for sb_d in /proc/[0-9]*; do
    [ -r "$sb_d/cmdline" ] || continue
    sb_pu=$(awk '/^Uid:/ { print $2; exit }' "$sb_d/status" 2>/dev/null)
    [ "$sb_pu" = "$sb_u" ] || continue
    sb_a=$(tr '\000' ' ' < "$sb_d/cmdline" 2>/dev/null)
    case "$sb_a" in */bin/wsl/codex\ *|*/bin/wsl/*/codex\ *|*CODEX_SQLITE_HOME=*) ;; *code_mode_host*app-server*) printf '%s ' "${sb_d#/proc/}" ;; esac
  done
}
"#;

/// Functions used by scripts that inspect the host (shell, login shell, pids).
const HOST_FUNCS: &str = r#"sb_shell() {
  sb_s="${SHELL:-}"
  if [ -z "$sb_s" ] && command -v getent >/dev/null 2>&1; then
    sb_s=$(getent passwd "$(id -un 2>/dev/null)" 2>/dev/null | cut -d: -f7)
  fi
  printf '%s' "$sb_s"
}
sb_login() {
  sb_ls=$(sb_shell)
  if [ -z "$sb_ls" ] || [ ! -x "$sb_ls" ]; then return 127; fi
  SB_PAYLOAD="$1"
  export SB_PAYLOAD
  case "${sb_ls##*/}" in
    csh|tcsh) "$sb_ls" -i -c 'set loginsh=1; if ( -r /etc/csh.login ) source /etc/csh.login; if ( -r ~/.login ) source ~/.login; exec /bin/sh -c "$SB_PAYLOAD"' ;;
    nu) "$sb_ls" -l -i -c 'exec /bin/sh -c $env.SB_PAYLOAD' ;;
    fish|xonsh) "$sb_ls" -l -i -c 'exec /bin/sh -c "$SB_PAYLOAD"' ;;
    *) "$sb_ls" -l -i -c 'CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"; export CODEX_HOME; exec /bin/sh -c "$SB_PAYLOAD"' ;;
  esac
}
sb_pids() {
  sb_u=$(id -u 2>/dev/null)
  if sb_ps=$(ps -A -o pid= -o uid= -o args= 2>/dev/null) && [ -n "$sb_ps" ]; then
    printf '%s\n' "$sb_ps" | awk -v u="$sb_u" '$2 == u && /[a]pp-server/ && /--listen/ { printf "%s ", $1 }'
    return 0
  fi
  # Linux without a full ps (busybox truncates user names): scan /proc by uid.
  for sb_d in /proc/[0-9]*; do
    [ -r "$sb_d/cmdline" ] || continue
    sb_pu=$(awk '/^Uid:/ { print $2; exit }' "$sb_d/status" 2>/dev/null)
    [ "$sb_pu" = "$sb_u" ] || continue
    sb_a=$(tr '\000' ' ' < "$sb_d/cmdline" 2>/dev/null)
    case "$sb_a" in *app-server*--listen*) printf '%s ' "${sb_d#/proc/}" ;; esac
  done
}
sb_machine_id() {
  for sb_f in /etc/machine-id /var/lib/dbus/machine-id; do
    if [ -s "$sb_f" ]; then head -n 1 "$sb_f"; return 0; fi
  done
  if command -v ioreg >/dev/null 2>&1; then
    ioreg -rd1 -c IOPlatformExpertDevice 2>/dev/null | awk -F'"' '/IOPlatformUUID/ { print $4; exit }'
    return 0
  fi
  if command -v hostid >/dev/null 2>&1; then hostid 2>/dev/null; fi
}
sb_env_of() {
  if [ -r "/proc/$1/environ" ]; then
    if tr '\000' '\n' < "/proc/$1/environ" 2>/dev/null | grep -q '^CODEX_AUTHAPI_BASE_URL='; then
      sb_put "env.$1" "$(tr '\000' '\n' < "/proc/$1/environ" | sed -n 's/^CODEX_AUTHAPI_BASE_URL=//p' | head -n 1)"
    else
      sb_put "envabsent.$1" 1
    fi
  elif [ "$(uname -s 2>/dev/null)" = Darwin ]; then
    sb_o=$(ps -E -ww -o command= -p "$1" 2>/dev/null)
    case "$sb_o" in
      *" CODEX_AUTHAPI_BASE_URL="*) sb_v=${sb_o#*" CODEX_AUTHAPI_BASE_URL="}; sb_put "env.$1" "${sb_v%%" "*}" ;;
      *) sb_put "envunknown.$1" 1 ;;
    esac
  else
    sb_put "envunknown.$1" 1
  fi
}
"#;

/// Body run inside the user's login shell (as `/bin/sh -c`): what the app-server
/// would see when the app starts it.
fn login_payload(nonce: &str, with_version: bool) -> String {
    let mut s = prelude(nonce);
    s.push_str(RESOLVE_FUNC);
    s.push_str(
        r#"PATH="${CODEX_INSTALL_DIR:-$HOME/.local/bin}:$PATH"
export PATH
sb_put login 1
printf '@@SBCH:%s:%s\n' "$SB_N" "${CODEX_HOME:-$HOME/.codex}"
sb_put codex_home "${CODEX_HOME:-$HOME/.codex}"
if [ "${CODEX_AUTHAPI_BASE_URL+set}" = set ]; then sb_put authapi "$CODEX_AUTHAPI_BASE_URL"; else sb_put authapi_absent 1; fi
if sb_cx=$(command -v codex 2>/dev/null); then
  sb_put codex_path "$sb_cx"
"#,
    );
    if with_version {
        s.push_str(
            "  sb_phase version\n  sb_put codex_version \"$(codex --version </dev/null 2>/dev/null | head -n 1)\"\n",
        );
    }
    // v0.3: where that `codex` really lives (install kind) and where an install
    // goes (the app's PATH prefix).
    s.push_str(
        "  sb_put codex_resolved \"$(sb_resolve \"$sb_cx\")\"\n\
         fi\n\
         sb_put install_dir \"${CODEX_INSTALL_DIR:-$HOME/.local/bin}\"\n\
         sb_put login_done 1\n",
    );
    s
}

/// `sb_resolve <path>`: the path with every symbolic link resolved (`readlink
/// -f`, `realpath`, else by hand with POSIX tools), printed without a newline.
const RESOLVE_FUNC: &str = r#"sb_resolve() {
  if sb_r=$(readlink -f "$1" 2>/dev/null) && [ -n "$sb_r" ]; then printf '%s' "$sb_r"; return 0; fi
  if sb_r=$(realpath "$1" 2>/dev/null) && [ -n "$sb_r" ]; then printf '%s' "$sb_r"; return 0; fi
  sb_p=$1; sb_i=0
  while [ -L "$sb_p" ] && [ "$sb_i" -lt 40 ]; do
    sb_t=$(ls -ld "$sb_p" 2>/dev/null | sed 's/^.* -> //')
    case "$sb_t" in /*) sb_p=$sb_t ;; *) sb_p=$(dirname "$sb_p")/$sb_t ;; esac
    sb_i=$((sb_i + 1))
  done
  if sb_d=$(cd "$(dirname "$sb_p")" 2>/dev/null && pwd -P); then printf '%s/%s' "$sb_d" "$(basename "$sb_p")"; else printf '%s' "$sb_p"; fi
}
"#;

/// Wrap a `main` body into a complete script.
pub(crate) fn wrap(nonce: &str, funcs: &str, body: &str) -> String {
    let mut s = String::with_capacity(body.len() + 4096);
    s.push_str(&prelude(nonce));
    s.push_str(funcs);
    s.push_str("main() {\n");
    s.push_str(body);
    s.push_str("\n}\nmain </dev/null\nexit $?\n");
    s
}

/// [`wrap`] without extra functions, for scripts built in other modules.
pub fn wrap_public(nonce: &str, body: &str) -> String {
    wrap(nonce, "", body)
}

/// [`wrap`] for a script that receives a payload after itself on stdin
/// ([`crate::ssh::RemoteShell::run_with_payload`]): stdin is kept as fd 3 before
/// `main` runs with `</dev/null`, so the payload is read with `<&3` explicitly.
pub(crate) fn wrap_with_payload(nonce: &str, funcs: &str, body: &str) -> String {
    let mut s = String::with_capacity(body.len() + 4096);
    s.push_str(&prelude(nonce));
    s.push_str(funcs);
    s.push_str("main() {\n");
    s.push_str(body);
    s.push_str("\n}\nexec 3<&0\nmain </dev/null\nexit $?\n");
    s
}

/// The probe / snapshot script: OS, shell, login-shell view, Codex files, rc
/// files, marker, first-write pre-images, machine id and app-server pids.
pub fn probe_script(nonce: &str, with_version: bool) -> String {
    probe_script_for(nonce, with_version, Flavor::Ssh)
}

/// [`probe_script`] for a host of `flavor`; a WSL distribution also reports
/// whether `/usr/bin/bash` exists (`K app_bash`) and its routing table
/// (`F route`, for the default gateway).
pub fn probe_script_for(nonce: &str, with_version: bool, flavor: Flavor) -> String {
    let mut body = String::new();
    if flavor == Flavor::Wsl {
        body.push_str("exec 4>&1\nSB_PROGRESS=1\nexport SB_PROGRESS\nsb_phase system\n");
    }
    let _ = writeln!(
        body,
        "SB_LP={}",
        sh_quote(&login_payload(nonce, with_version))
    );
    body.push_str(
        r#"sb_put os "$(uname -s 2>/dev/null)"
sb_put shell "$(sb_shell)"
sb_put home "${HOME:-}"
sb_put zdotdir "${ZDOTDIR:-}"
sb_put xdg_config_home "${XDG_CONFIG_HOME:-}"
sb_phase login
sb_lo=$(sb_login "$SB_LP" 2>/dev/null)
sb_put login_status "$?"
printf '%s\n' "$sb_lo"
ch=$(printf '%s\n' "$sb_lo" | sed -n "s/^@@SBCH:$SB_N://p" | tail -n 1)
if [ -z "$ch" ]; then
  ch="${CODEX_HOME:-$HOME/.codex}"
  if [ "${CODEX_AUTHAPI_BASE_URL+set}" = set ]; then sb_put plain_authapi "$CODEX_AUTHAPI_BASE_URL"; else sb_put plain_authapi_absent 1; fi
fi
sb_put codex_home_effective "$ch"
sb_phase files
sb_file config "$ch/config.toml"
sb_file auth "$ch/auth.json"
"#,
    );
    let _ = writeln!(
        body,
        "sb_file marker \"$ch/{REMOTE_DIR}/remote-state.json\"\n\
         sb_file fw_config \"$ch/{REMOTE_DIR}/first-write/config.toml\"\n\
         sb_file fw_auth \"$ch/{REMOTE_DIR}/first-write/auth.json\"\n\
         if [ -e \"$ch/{REMOTE_DIR}/first-write/config.toml.absent\" ]; then sb_put fw_config_absent 1; fi\n\
         if [ -e \"$ch/{REMOTE_DIR}/first-write/auth.json.absent\" ]; then sb_put fw_auth_absent 1; fi"
    );
    for rc in RC_FILES {
        let _ = writeln!(
            body,
            "sb_file rc.{} \"{}/{}\"",
            rc.name,
            rc.base.shell_expr(),
            rc.rel
        );
    }
    body.push_str(
        r#"sb_phase system
sb_put machine_id "$(sb_machine_id)"
sb_put pids "$(sb_pids)"
sb_put arch "$(uname -m 2>/dev/null)"
sb_put kernel "$(uname -r 2>/dev/null)"
if [ "$(uname -s 2>/dev/null)" = Darwin ]; then sb_put translated "$(sysctl -in sysctl.proc_translated 2>/dev/null)"; fi
sb_put disk_free "$(df -Pk "${HOME:-/}" 2>/dev/null | awk 'NR == 2 { print $4 }')"
"#,
    );
    if flavor == Flavor::Wsl {
        body.push_str(
            "if [ -x /usr/bin/bash ]; then sb_put app_bash 1; else sb_put app_bash 0; fi\n\
             sb_file route /proc/net/route\n",
        );
    }
    body.push_str("sb_put done 1");
    let script = wrap(nonce, &host_funcs(flavor), &body);
    if flavor == Flavor::Wsl {
        format!("# superba-wsl-probe\n{script}")
    } else {
        script
    }
}

/// What a write operation expects to find before replacing a file (compare and
/// swap against the bytes read just before).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// The file must not exist.
    Absent,
    /// The file must contain exactly these bytes.
    Bytes(Vec<u8>),
}

/// File permissions of a written file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Keep the existing file's mode (new files get 0600 from `umask 077`).
    Preserve,
    /// Always 0600, also on the referent of a symlinked target (`auth.json`).
    Private,
}

/// One file operation of a write script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOp {
    /// Remote absolute path.
    pub path: String,
    /// Precondition.
    pub expect: Expect,
    /// New contents (`None` = remove the file).
    pub new: Option<Vec<u8>>,
    /// Permissions.
    pub mode: Mode,
    /// Back up the current file into the backup directory first.
    pub backup: bool,
}

/// Backup file name of `path` inside a backup directory: the path with `/`
/// replaced by `%`, so rollback can find it again from the path alone.
pub fn backup_name(path: &str) -> String {
    path.trim_start_matches('/').replace('/', "%")
}

/// Parameters of a write script.
#[derive(Clone, Debug, Default)]
pub struct WriteSpec {
    /// Remote Codex home (lock, first-write pre-images and the marker live in
    /// `<codex_home>/.superba`). Empty = no lock, no first-write, no marker.
    pub codex_home: String,
    /// Backup directory (created if any operation backs up).
    pub backup_dir: String,
    /// Keep first-write pre-images of `config.toml` / `auth.json` (once per host).
    pub first_write: bool,
    /// Operations, all-or-nothing (see the module docs).
    pub ops: Vec<FileOp>,
    /// Marker contents for `<codex_home>/.superba/remote-state.json`, if any.
    pub marker: Option<Vec<u8>>,
}

/// Results of [`write_script`]: `K result` is one of
/// - `ok`: everything published;
/// - `conflict`: a precondition failed (records `K conflict <path>`); nothing
///   changed;
/// - `locked`: another writer holds the per-home lock; nothing changed;
/// - `error`: a step failed (`K error <message>`); every published file was put back;
/// - `partial`: a step failed and putting a file back failed too (`K undo_failed
///   <path>`): the host is half written.
///
/// Backups are reported as `K backup.<i> <path>`.
pub fn write_script(nonce: &str, spec: &WriteSpec) -> String {
    let mut b = String::new();
    let ch = sh_quote(&spec.codex_home);
    let bd = sh_quote(&spec.backup_dir);
    let n = spec.ops.len();
    let _ = writeln!(b, "ch={ch}\nbd={bd}\nsb_fail=0");
    for (i, op) in spec.ops.iter().enumerate() {
        let _ = writeln!(b, "p{i}={}", sh_quote(&op.path));
    }

    // Per-home lock (Superba writers on any machine sharing this home).
    if !spec.codex_home.is_empty() {
        let _ = writeln!(
            b,
            r#"sb_lock="$ch/{REMOTE_DIR}/lock"
if ! mkdir -p "$ch/{REMOTE_DIR}"; then sb_put error "cannot create $ch/{REMOTE_DIR}"; sb_put result error; return 0; fi
if ! mkdir "$sb_lock" 2>/dev/null; then
  if [ -n "$(find "$sb_lock" -prune -mmin +{STALE_LOCK_MINUTES} 2>/dev/null)" ] && rmdir "$sb_lock" 2>/dev/null && mkdir "$sb_lock" 2>/dev/null; then :; else sb_put result locked; return 0; fi
fi
trap 'rmdir "$sb_lock" 2>/dev/null' EXIT
trap 'rmdir "$sb_lock" 2>/dev/null; exit 1' HUP INT TERM"#
        );
    }

    // Phase 1: every precondition (nothing changes when one fails).
    let check = |b: &mut String, i: usize, op: &FileOp, on_fail: &str| match &op.expect {
        Expect::Absent => {
            let _ = writeln!(
                b,
                "if [ -e \"$p{i}\" ] || [ -L \"$p{i}\" ]; then sb_put conflict \"$p{i}\"; {on_fail}; fi"
            );
        }
        Expect::Bytes(bytes) => {
            let _ = writeln!(
                b,
                "if [ -f \"$p{i}\" ] && {} | cmp -s - \"$p{i}\"; then :; else sb_put conflict \"$p{i}\"; {on_fail}; fi",
                printf_bytes(bytes)
            );
        }
    };
    for (i, op) in spec.ops.iter().enumerate() {
        check(&mut b, i, op, "sb_fail=1");
    }
    b.push_str("if [ \"$sb_fail\" != 0 ]; then sb_put result conflict; return 0; fi\n");

    // First-write pre-images (once per host); any failure aborts before changes.
    if spec.first_write && !spec.codex_home.is_empty() {
        let _ = writeln!(
            b,
            "fw=\"$ch/{REMOTE_DIR}/first-write\"\nmkdir -p \"$fw\" || sb_fail=1"
        );
        for name in ["config.toml", "auth.json"] {
            let _ = writeln!(
                b,
                "if [ \"$sb_fail\" = 0 ] && [ ! -e \"$fw/{name}\" ] && [ ! -e \"$fw/{name}.absent\" ]; then\n\
                 \x20 if [ -f \"$ch/{name}\" ]; then cp -p \"$ch/{name}\" \"$fw/.{name}.$SB_N\" && mv -f \"$fw/.{name}.$SB_N\" \"$fw/{name}\" || sb_fail=1\n\
                 \x20 elif [ -e \"$ch/{name}\" ] || [ -L \"$ch/{name}\" ]; then sb_fail=1\n\
                 \x20 else : > \"$fw/{name}.absent\" || sb_fail=1; fi\n\
                 fi"
            );
        }
        b.push_str("if [ \"$sb_fail\" != 0 ]; then sb_put error \"cannot record the original files\"; sb_put result error; return 0; fi\n");
    }

    // Backups.
    if spec.ops.iter().any(|o| o.backup) {
        b.push_str("if ! mkdir -p \"$bd\"; then sb_put error \"cannot create the backup directory\"; sb_put result error; return 0; fi\n");
    }
    for (i, op) in spec.ops.iter().enumerate() {
        if op.backup {
            let bn = sh_quote(&backup_name(&op.path));
            let _ = writeln!(
                b,
                "if [ -f \"$p{i}\" ]; then if cp -p \"$p{i}\" \"$bd/\"{bn}; then sb_put backup.{i} \"$bd/\"{bn}; else sb_fail=1; sb_put error \"cannot back up $p{i}\"; fi; fi"
            );
        }
    }

    // Staging: an exclusive private directory next to each target.
    b.push_str("sb_clean() {\n  :\n");
    for i in 0..n {
        let _ = writeln!(
            b,
            "  if [ -n \"${{s{i}:-}}\" ]; then rm -f \"$s{i}/f\" 2>/dev/null; rmdir \"$s{i}\" 2>/dev/null; fi"
        );
    }
    b.push_str("}\n");
    for (i, op) in spec.ops.iter().enumerate() {
        let Some(new) = &op.new else { continue };
        let _ = writeln!(
            b,
            "if [ \"$sb_fail\" = 0 ]; then\n\
             \x20 d{i}=$(dirname \"$p{i}\")\n\
             \x20 if mkdir -p \"$d{i}\" && mkdir \"$d{i}/.superba-stage-$SB_N-{i}\"; then s{i}=\"$d{i}/.superba-stage-$SB_N-{i}\"; else sb_fail=1; sb_put error \"cannot stage $p{i}\"; fi\n\
             fi"
        );
        let copy_mode = match op.mode {
            Mode::Preserve => format!(
                "if [ -f \"$p{i}\" ]; then cp -p \"$p{i}\" \"$s{i}/f\" || sb_fail=1; fi\n  "
            ),
            Mode::Private => String::new(),
        };
        let _ = writeln!(
            b,
            "if [ \"$sb_fail\" = 0 ]; then\n\
             \x20 {copy_mode}{} > \"$s{i}/f\" || {{ sb_fail=1; sb_put error \"cannot stage $p{i}\"; }}\n\
             fi",
            printf_bytes(new)
        );
        if op.mode == Mode::Private {
            let _ = writeln!(
                b,
                "if [ \"$sb_fail\" = 0 ]; then chmod 600 \"$s{i}/f\" || sb_fail=1; fi"
            );
        }
    }
    b.push_str("if [ \"$sb_fail\" != 0 ]; then sb_clean; sb_put result error; return 0; fi\n");

    // Undo: put back the bytes found before (embedded again) for every file that
    // may have changed; report what could not be restored.
    b.push_str("sb_undo() {\n  sb_partial=0\n");
    for (i, op) in spec.ops.iter().enumerate() {
        let restore = match &op.expect {
            Expect::Absent => format!("rm -f \"$p{i}\""),
            Expect::Bytes(bytes) => format!("{} > \"$p{i}\"", printf_bytes(bytes)),
        };
        let _ = writeln!(
            b,
            "  if [ \"${{c{i}:-0}}\" = 1 ]; then if ! {restore}; then sb_partial=1; sb_put undo_failed \"$p{i}\"; fi; fi"
        );
    }
    b.push_str(
        "  sb_clean\n  if [ \"$sb_partial\" = 0 ]; then sb_put result \"$1\"; else sb_put result partial; fi\n}\n",
    );

    // Phase 2: per file, re-check the precondition right before publishing it.
    for (i, op) in spec.ops.iter().enumerate() {
        check(&mut b, i, op, "sb_undo conflict; return 0");
        let _ = writeln!(b, "c{i}=1");
        match &op.new {
            Some(_) => {
                let chmod = if op.mode == Mode::Private {
                    format!(" && chmod 600 \"$p{i}\"")
                } else {
                    String::new()
                };
                let _ = writeln!(
                    b,
                    "if [ -L \"$p{i}\" ]; then\n\
                     \x20 cat \"$s{i}/f\" > \"$p{i}\" && cmp -s \"$s{i}/f\" \"$p{i}\"{chmod}\n\
                     else\n\
                     \x20 mv -f \"$s{i}/f\" \"$p{i}\"\n\
                     fi\n\
                     if [ $? -ne 0 ]; then sb_put error \"cannot replace $p{i}\"; sb_undo error; return 0; fi"
                );
            }
            None => {
                let _ = writeln!(
                    b,
                    "if ! rm -f \"$p{i}\"; then sb_put error \"cannot remove $p{i}\"; sb_undo error; return 0; fi"
                );
            }
        }
    }
    b.push_str("sb_clean\n");
    if let (Some(marker), false) = (&spec.marker, spec.codex_home.is_empty()) {
        let _ = writeln!(
            b,
            "{} > \"$ch/{REMOTE_DIR}/.remote-state.$SB_N\" && mv -f \"$ch/{REMOTE_DIR}/.remote-state.$SB_N\" \"$ch/{REMOTE_DIR}/remote-state.json\" || {{ rm -f \"$ch/{REMOTE_DIR}/.remote-state.$SB_N\"; sb_put marker_failed 1; }}",
            printf_bytes(marker)
        );
    }
    b.push_str("sb_put result ok");
    format!("# superba-write\n{}", wrap(nonce, "", &b))
}

/// Read a list of files (records `F|A|U|L` named `f.<index>`).
pub fn read_script(nonce: &str, paths: &[String]) -> String {
    let mut b = String::from(":\n");
    for (i, p) in paths.iter().enumerate() {
        let _ = writeln!(b, "sb_file f.{i} {}", sh_quote(p));
    }
    b.push_str("sb_put done 1");
    wrap(nonce, "", &b)
}

/// Turn activity: rollouts under `<codex_home>/sessions` modified in the last two
/// hours, each as `K rollout <age-seconds>|<last turn event>|<path>`. Records
/// `scan_failed`, `truncated` or `stat_failed` mark an incomplete answer.
/// `codex_home = None` uses `${CODEX_HOME:-$HOME/.codex}` of the non-login shell.
pub fn turn_script(nonce: &str, codex_home: Option<&str>) -> String {
    let ch = codex_home.map_or_else(|| "\"${CODEX_HOME:-$HOME/.codex}\"".to_owned(), sh_quote);
    let body = format!(
        r#"ch={ch}
sb_now=$(date +%s)
case "$sb_now" in ''|*[!0-9]*) sb_put scan_failed date; sb_put done 1; return 0 ;; esac
sb_put now "$sb_now"
if [ ! -d "$ch/sessions" ]; then sb_put sessions absent; sb_put done 1; return 0; fi
if ! sb_list=$(find "$ch/sessions" -type f -name 'rollout-*.jsonl' -mmin -120 2>/dev/null); then sb_put scan_failed find; fi
sb_count=$(printf '%s\n' "$sb_list" | grep -c . 2>/dev/null)
if [ "${{sb_count:-0}}" -gt {max} ]; then sb_put truncated "$sb_count"; fi
printf '%s\n' "$sb_list" | head -n {max} | while IFS= read -r sb_f; do
  [ -n "$sb_f" ] || continue
  sb_m=$(stat -c %Y "$sb_f" 2>/dev/null || stat -f %m "$sb_f" 2>/dev/null)
  case "$sb_m" in ''|*[!0-9]*) sb_put stat_failed "$sb_f"; continue ;; esac
  sb_last=$(tail -c 65536 "$sb_f" 2>/dev/null | grep -o -E '"type" ?: ?"(task_started|task_complete|turn_started|turn_complete|turn_aborted)"' 2>/dev/null | tail -n 1)
  sb_put rollout "$((sb_now - sb_m))|$sb_last|$sb_f"
done
sb_put done 1"#,
        max = MAX_ROLLOUTS
    );
    wrap(nonce, "", &body)
}

/// Verification: app-server pids, the `CODEX_AUTHAPI_BASE_URL` of those not in
/// `previous` (only new app-servers are inspected), and the current contents of
/// `paths` (records `f.<index>`).
pub fn verify_script(nonce: &str, previous: &[u32], paths: &[String]) -> String {
    verify_script_for(nonce, previous, paths, Flavor::Ssh)
}

/// [`verify_script`] for a host of `flavor` (the app-server pattern differs).
pub fn verify_script_for(
    nonce: &str,
    previous: &[u32],
    paths: &[String],
    flavor: Flavor,
) -> String {
    let prev: Vec<String> = previous.iter().map(u32::to_string).collect();
    let mut b = format!(
        "sb_prev=' {} '\nsb_p=$(sb_pids)\nsb_put pids \"$sb_p\"\n\
         for sb_i in $sb_p; do case \"$sb_prev\" in *\" $sb_i \"*) ;; *) sb_env_of \"$sb_i\" ;; esac; done\n",
        prev.join(" ")
    );
    for (i, p) in paths.iter().enumerate() {
        let _ = writeln!(b, "sb_file f.{i} {}", sh_quote(p));
    }
    b.push_str("sb_put done 1");
    wrap(nonce, &host_funcs(flavor), &b)
}
