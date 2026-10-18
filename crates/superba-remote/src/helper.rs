//! Superba's Linux helper inside a WSL distribution (user bug 45).
//!
//! Migrating sessions needs the sessions engine (SQLite updates in one
//! transaction, rollout rewrites with their byte offsets, Codex's own writer
//! locks and process checks), and none of that works from Windows over
//! `\\wsl.localhost` (SQLite cannot lock there, Linux `flock` locks are
//! invisible to Windows). So the distribution runs Superba's own Linux `spb`:
//! the file is streamed in once per checksum to
//! `~/.local/share/superba/wsl-helper/<sha256>/spb` (verified there, 0755),
//! then run as the distribution's user with the host's `CODEX_HOME`; its data
//! (backups, migration ledger) stays inside the distribution, like a user
//! running `spb` there.

use std::fmt::Write as _;

use crate::script::{sh_quote, wrap, wrap_with_payload};

/// Directory of the helper copies, relative to `$HOME`.
pub const HELPER_DIR: &str = ".local/share/superba/wsl-helper";

/// Most output lines a run keeps for its report.
pub const TAIL_LINES: usize = 40;

/// `$HOME/<HELPER_DIR>/<sha256>/spb` as a shell expression.
fn helper_path(sha256: &str) -> String {
    format!("\"$HOME/{HELPER_DIR}/{sha256}/spb\"")
}

/// Records `K present 1` when the helper with this checksum is in place.
pub fn check_script(nonce: &str, sha256: &str) -> String {
    let mut b = String::new();
    let _ = writeln!(
        b,
        "h={}\nwant={}",
        helper_path(sha256),
        sh_quote(&sha256.to_ascii_lowercase())
    );
    b.push_str(
        r#"if [ -x "$h" ] && [ "$(sb_sha256 "$h")" = "$want" ]; then sb_put present 1; fi
sb_put done 1"#,
    );
    wrap(nonce, crate::install::FUNCS, &b)
}

/// Receive the helper on fd 3, check size and checksum, publish it 0755.
/// Records `K result ok|error` and `K error <code>`.
pub fn upload_script(nonce: &str, sha256: &str, size: u64) -> String {
    let mut b = String::new();
    let _ = writeln!(
        b,
        "dir=\"$HOME/{HELPER_DIR}/{sha}\"\nwant={want}\nwant_size={size}",
        sha = sha256.to_ascii_lowercase(),
        want = sh_quote(&sha256.to_ascii_lowercase()),
    );
    b.push_str(
        r#"if ! mkdir -p "$dir"; then sb_fail mkdir; return 0; fi
tmp="$dir/.spb.$SB_N"
if ! cat <&3 > "$tmp"; then rm -f "$tmp"; sb_fail receive; return 0; fi
sb_got=$(wc -c < "$tmp" | tr -d ' \t\n')
if [ "$sb_got" != "$want_size" ]; then rm -f "$tmp"; sb_put got "$sb_got"; sb_fail size_mismatch; return 0; fi
sb_got=$(sb_sha256 "$tmp")
if [ "$sb_got" != "$want" ]; then rm -f "$tmp"; sb_put got "$sb_got"; sb_fail sha256_mismatch; return 0; fi
chmod 755 "$tmp" && mv -f "$tmp" "$dir/spb" || { rm -f "$tmp"; sb_fail publish; return 0; }
sb_put result ok"#,
    );
    wrap_with_payload(nonce, crate::install::FUNCS, &b)
}

/// Run the helper with `args` (each one shell-quoted) and `CODEX_HOME`, its
/// output passing through; the exit code as `K exit <n>` at the end.
pub fn run_script(nonce: &str, sha256: &str, args: &[String], codex_home: &str) -> String {
    let mut b = String::new();
    let quoted: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
    let _ = writeln!(
        b,
        "h={}\nCODEX_HOME={}\nexport CODEX_HOME\n# Only the helper's own output: no colours, no log lines.\nNO_COLOR=1\nRUST_LOG=off\nexport NO_COLOR RUST_LOG",
        helper_path(sha256),
        sh_quote(codex_home)
    );
    let _ = writeln!(
        b,
        "if [ ! -x \"$h\" ]; then sb_put exit 127; sb_put done 1; return 0; fi\n\
         \"$h\" {} 2>&1\nsb_put exit \"$?\"\nsb_put done 1",
        quoted.join(" ")
    );
    wrap(nonce, "", &b)
}

/// The last [`TAIL_LINES`] output lines (records and blank lines left out).
pub fn tail(output: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(output);
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty() && !l.starts_with(crate::script::RECORD_PREFIX))
        .map(str::to_owned)
        .collect();
    let skip = lines.len().saturating_sub(TAIL_LINES);
    lines.into_iter().skip(skip).collect()
}
