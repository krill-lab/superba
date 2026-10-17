//! Installing Codex on a remote host without sudo (v0.3, spike B5).
//!
//! The desktop app starts the remote app-server with `codex` from the login
//! shell and `PATH="${CODEX_INSTALL_DIR:-$HOME/.local/bin}:$PATH"`, so a release in
//! the official standalone layout wins over npm, Homebrew and system installs
//! without touching any rc file:
//!
//! ```text
//! <CODEX_HOME>/packages/standalone/releases/<ver>-<target>/   extracted package
//! <CODEX_HOME>/packages/standalone/current -> releases/<ver>-<target>
//! ${CODEX_INSTALL_DIR:-~/.local/bin}/codex -> <root>/current/bin/codex
//! ```
//!
//! The verified package (`codex-package-<target>.tar.gz`, or the npm platform
//! tarball with the same layout under `package/vendor/<target>/`) is streamed on
//! the SSH session's stdin after the script ([`crate::ssh::RemoteShell::run_with_payload`]);
//! no path, checksum or key appears on a command line. The script checks free
//! space, size, sha256 (`sha256sum`, `shasum -a 256` or `openssl`), extracts with
//! `gzip -dc | tar -xf -`, requires `bin/codex --version` to print exactly
//! `codex-cli <version>`, swaps the links with `mv -T` / `mv -h` / remove + move
//! (a link to a directory is never a plain `mv` target), removes
//! `auto-update-version` (the release is pinned) and keeps the previous release
//! for [`rollback_script`]. Releases Superba installed earlier, other than the
//! current and the previous one, are removed; releases it did not install are
//! never touched. What `codex` was before the first install (a file or a link
//! elsewhere) is kept so a rollback can restore it.

use std::fmt::Write as _;

use superba_core::{CodexVersion, Error, Result};

use crate::records::Records;
use crate::script::{sh_quote, wrap, wrap_with_payload};

/// Minimum free space under the releases directory: an extracted package is
/// about 430 MB.
pub const MIN_FREE_KIB: u64 = 600 * 1024;

/// Free space an install of a `package_size`-byte archive needs (KiB): the
/// archive plus its extracted contents (about 2.7 times the archive), at least
/// [`MIN_FREE_KIB`].
pub fn required_kib(package_size: u64) -> u64 {
    (package_size.saturating_mul(4) / 1024).max(MIN_FREE_KIB)
}

/// Parameters of [`install_script`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallSpec {
    /// Remote Codex home (absolute).
    pub codex_home: String,
    /// `${CODEX_INSTALL_DIR:-$HOME/.local/bin}` of the login shell (absolute).
    pub bin_dir: String,
    /// Version to install.
    pub version: CodexVersion,
    /// Package target triple (`x86_64-unknown-linux-musl`).
    pub target: String,
    /// sha256 (hex) of the archive.
    pub sha256: String,
    /// Size of the archive in bytes.
    pub size: u64,
}

/// Helpers of the install and rollback scripts.
const FUNCS: &str = r#"sb_fail() { sb_put error "$1"; sb_put result error; }
sb_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{ print $1 }'
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | sed 's/^.*= *//'
  else echo none; fi
}
sb_link() {
  rm -f "$3" && ln -s "$2" "$3" || return 1
  mv -Tf "$3" "$1" 2>/dev/null || mv -hf "$3" "$1" 2>/dev/null || { rm -f "$1" && mv -f "$3" "$1"; }
}
sb_readlink() {
  if [ -L "$1" ]; then readlink "$1" 2>/dev/null || ls -ld "$1" 2>/dev/null | sed 's/^.* -> //'; fi
}
sb_version_of() {
  if [ -x "$1" ]; then "$1" --version </dev/null 2>/dev/null | head -n 1; fi
}
"#;

fn layout_vars(b: &mut String, codex_home: &str, bin_dir: &str) {
    let _ = writeln!(
        b,
        "ch={}\nbin={}\nroot=\"$ch/packages/standalone\"\nrel=\"$root/releases\"",
        sh_quote(codex_home),
        sh_quote(bin_dir)
    );
}

/// The install script (payload on fd 3). Records: `K result ok|error`, `K error
/// <code>` (`no_space`, `size_mismatch`, `sha256_mismatch`, `no_sha256_tool`,
/// `extract`, `no_codex_in_package`, `version_mismatch`, ...), `K got`, `K free`,
/// `K release` (new release dir), `K visible` (`<bin_dir>/codex`), `K previous`
/// (`--version` of the command it replaced), `K app_sees` (`command -v codex`
/// with the app's PATH prefix).
pub fn install_script(nonce: &str, spec: &InstallSpec) -> String {
    let mut b = String::new();
    layout_vars(&mut b, &spec.codex_home, &spec.bin_dir);
    let _ = writeln!(
        b,
        "v={}\nt={}\nwant_sha={}\nwant_size={}\nneed={}",
        sh_quote(&spec.version.to_string()),
        sh_quote(&spec.target),
        sh_quote(&spec.sha256.to_ascii_lowercase()),
        spec.size,
        required_kib(spec.size)
    );
    b.push_str(
        r#"name="$v-$t"
dest="$rel/$name"
if ! mkdir -p "$rel" "$bin"; then sb_fail mkdir; return 0; fi
sb_free=$(df -Pk "$rel" 2>/dev/null | awk 'NR == 2 { print $4 }')
case "$sb_free" in
  ''|*[!0-9]*) ;;
  *) if [ "$sb_free" -lt "$need" ]; then sb_put free "$sb_free"; sb_fail no_space; return 0; fi ;;
esac
tmp="$rel/.superba-install-$SB_N"
if ! mkdir "$tmp"; then sb_fail tmpdir; return 0; fi
trap 'rm -rf "$tmp"' EXIT
trap 'rm -rf "$tmp"; exit 1' HUP INT TERM
if ! cat <&3 > "$tmp/pkg"; then sb_fail receive; return 0; fi
sb_got=$(wc -c < "$tmp/pkg" | tr -d ' \t\n')
if [ "$sb_got" != "$want_size" ]; then sb_put got "$sb_got"; sb_fail size_mismatch; return 0; fi
sb_got=$(sb_sha256 "$tmp/pkg")
if [ "$sb_got" = none ]; then sb_fail no_sha256_tool; return 0; fi
if [ "$sb_got" != "$want_sha" ]; then sb_put got "$sb_got"; sb_fail sha256_mismatch; return 0; fi
if ! mkdir "$tmp/x" || ! (cd "$tmp/x" && gzip -dc ../pkg | tar -xf -) >/dev/null 2>&1; then sb_fail extract; return 0; fi
rm -f "$tmp/pkg"
if [ -f "$tmp/x/bin/codex" ]; then pkg="$tmp/x"
elif [ -f "$tmp/x/package/vendor/$t/bin/codex" ]; then pkg="$tmp/x/package/vendor/$t"
else sb_fail no_codex_in_package; return 0; fi
sb_got=$(sb_version_of "$pkg/bin/codex")
if [ "$sb_got" != "codex-cli $v" ]; then sb_put got "$sb_got"; sb_fail version_mismatch; return 0; fi
sb_put previous "$(sb_version_of "$bin/codex")"
sb_prev=$(sb_readlink "$root/current")
sb_prev=${sb_prev##*/}
# What `codex` was before Superba's first install, for a rollback without an
# earlier release.
if [ ! -e "$root/.superba-original" ]; then
  sb_was=$(sb_readlink "$bin/codex")
  case "$sb_was" in
    "$root"/*) : > "$root/.superba-original" ;;
    ?*) printf '%s\n' "$sb_was" > "$root/.superba-original-link" && : > "$root/.superba-original" ;;
    *) if [ -f "$bin/codex" ]; then cp -p "$bin/codex" "$root/.superba-original-codex" && : > "$root/.superba-original"; else : > "$root/.superba-original"; fi ;;
  esac
fi
if [ -e "$dest" ] && ! mv "$dest" "$tmp/old"; then sb_fail replace; return 0; fi
if ! mv "$pkg" "$dest"; then
  if [ -e "$tmp/old" ]; then mv "$tmp/old" "$dest"; fi
  sb_fail move; return 0
fi
if ! sb_link "$root/current" "$dest" "$root/.current.$SB_N" || [ ! -L "$root/current" ]; then sb_fail link_current; return 0; fi
rm -f "$root/auto-update-version"
if ! sb_link "$bin/codex" "$root/current/bin/codex" "$bin/.codex.$SB_N"; then sb_fail link_bin; return 0; fi
if [ "$(uname -s 2>/dev/null)" = Darwin ] && [ -f "$dest/bin/codex-code-mode-host" ]; then
  sb_link "$bin/codex-code-mode-host" "$root/current/bin/codex-code-mode-host" "$bin/.cmh.$SB_N"
fi
if [ -n "$sb_prev" ] && [ "$sb_prev" != "$name" ]; then printf '%s\n' "$sb_prev" > "$root/.superba-previous"; fi
sb_keep=$(head -n 1 "$root/.superba-previous" 2>/dev/null)
# Superba's own releases other than the current and the previous one go away.
if [ -f "$root/.superba-installed" ]; then
  sort -u "$root/.superba-installed" | while IFS= read -r sb_n; do
    case "$sb_n" in ''|*/*|.|..) continue ;; esac
    if [ "$sb_n" != "$name" ] && [ "$sb_n" != "$sb_keep" ] && [ -d "$rel/$sb_n" ]; then rm -rf "$rel/$sb_n"; sb_put removed "$sb_n"; fi
  done
fi
{ printf '%s\n' "$name"; if [ -n "$sb_keep" ] && [ -f "$root/.superba-installed" ] && grep -qx "$sb_keep" "$root/.superba-installed"; then printf '%s\n' "$sb_keep"; fi; } > "$root/.superba-installed.$SB_N" && mv -f "$root/.superba-installed.$SB_N" "$root/.superba-installed"
sb_put release "$dest"
sb_put visible "$bin/codex"
sb_put app_sees "$(PATH="$bin:$PATH"; export PATH; command -v codex 2>/dev/null)"
sb_put result ok"#,
    );
    wrap_with_payload(nonce, FUNCS, &b)
}

/// The rollback script: switch `current` back to the release kept by the last
/// install (and remember the one switched away from, so a second rollback
/// returns); without one, restore what `codex` was before Superba's first
/// install. Records as [`install_script`] plus `K version` (`--version` of the
/// command now visible) and `K error nothing_to_roll_back`.
pub fn rollback_script(nonce: &str, codex_home: &str, bin_dir: &str) -> String {
    let mut b = String::new();
    layout_vars(&mut b, codex_home, bin_dir);
    b.push_str(
        r#"sb_prev=$(head -n 1 "$root/.superba-previous" 2>/dev/null)
sb_cur=$(sb_readlink "$root/current")
sb_cur=${sb_cur##*/}
sb_put previous "$(sb_version_of "$bin/codex")"
case "$sb_prev" in */*|.|..) sb_prev= ;; esac
if [ -n "$sb_prev" ] && [ -f "$rel/$sb_prev/bin/codex" ]; then
  if ! sb_link "$root/current" "$rel/$sb_prev" "$root/.current.$SB_N"; then sb_fail link_current; return 0; fi
  if ! sb_link "$bin/codex" "$root/current/bin/codex" "$bin/.codex.$SB_N"; then sb_fail link_bin; return 0; fi
  if [ -n "$sb_cur" ]; then printf '%s\n' "$sb_cur" > "$root/.superba-previous"; fi
  sb_put release "$rel/$sb_prev"
elif [ -f "$root/.superba-original-link" ]; then
  if ! sb_link "$bin/codex" "$(head -n 1 "$root/.superba-original-link")" "$bin/.codex.$SB_N"; then sb_fail link_bin; return 0; fi
elif [ -f "$root/.superba-original-codex" ]; then
  if ! cp -p "$root/.superba-original-codex" "$bin/.codex.$SB_N" || ! mv -f "$bin/.codex.$SB_N" "$bin/codex"; then sb_fail restore; return 0; fi
else
  sb_fail nothing_to_roll_back; return 0
fi
sb_put visible "$bin/codex"
sb_put version "$(sb_version_of "$bin/codex")"
sb_put result ok"#,
    );
    wrap(nonce, FUNCS, &b)
}

/// What an install or rollback reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    /// The visible command (`<bin_dir>/codex`).
    pub visible: String,
    /// Version of the command it replaced, when it ran.
    pub previous: Option<CodexVersion>,
    /// Version now visible (rollback), when it ran.
    pub version: Option<CodexVersion>,
    /// `command -v codex` with the app's PATH prefix (install).
    pub app_sees: Option<String>,
}

/// Turn script records into an outcome or an error naming the failed check.
pub fn parse_outcome(recs: &Records, host: &str) -> Result<InstallOutcome> {
    match recs.get("result") {
        Some("ok") => Ok(InstallOutcome {
            visible: recs.get_nonempty("visible").unwrap_or_default().to_owned(),
            previous: recs
                .get_nonempty("previous")
                .and_then(|v| CodexVersion::parse(v).ok()),
            version: recs
                .get_nonempty("version")
                .and_then(|v| CodexVersion::parse(v).ok()),
            app_sees: recs.get_nonempty("app_sees").map(str::to_owned),
        }),
        Some("error") => Err(install_error(
            host,
            recs.get("error").unwrap_or("unknown"),
            recs.get_nonempty("got"),
            recs.get_nonempty("free"),
        )),
        _ => Err(Error::Ssh {
            host: host.to_owned(),
            message: "the install script stopped without a result".into(),
        }),
    }
}

fn install_error(host: &str, code: &str, got: Option<&str>, free: Option<&str>) -> Error {
    let detail = match code {
        "no_space" => format!(
            "not enough free space on {host} ({} KiB free)",
            free.unwrap_or("?")
        ),
        "size_mismatch" => {
            "the package arrived incomplete (size differs); nothing was installed".to_owned()
        }
        "sha256_mismatch" => {
            "the package's sha256 differs on the host; nothing was installed".to_owned()
        }
        "no_sha256_tool" => {
            "the host has no sha256sum, shasum or openssl to verify the package".to_owned()
        }
        "extract" => "the package could not be extracted on the host (gzip/tar)".to_owned(),
        "no_codex_in_package" => "the package holds no bin/codex".to_owned(),
        "version_mismatch" => format!(
            "the packaged codex reports {:?}, not the requested version",
            got.unwrap_or("")
        ),
        "nothing_to_roll_back" => {
            "no earlier Codex release or original command is kept on the host".to_owned()
        }
        other => format!("the install step {other:?} failed"),
    };
    match code {
        "sha256_mismatch" | "size_mismatch" | "version_mismatch" | "no_codex_in_package" => {
            Error::Invalid { message: detail }
        }
        "nothing_to_roll_back" => Error::NotFound { what: detail },
        _ => Error::Ssh {
            host: host.to_owned(),
            message: detail,
        },
    }
}
