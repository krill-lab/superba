//! Read-only session counts of a remote host (v0.3 Sessions page, per host).
//!
//! One POSIX script reads the first line (`session_meta`) of every rollout under
//! `<CODEX_HOME>/sessions` and `<CODEX_HOME>/archived_sessions` (`.jsonl`, and
//! `.jsonl.zst` when the host has `zstd`) and aggregates it remotely with `awk`,
//! so the output stays small: one line per provider (live rollouts, archived,
//! compressed, distinct session ids) plus the counts it could not read. No
//! sqlite is needed and nothing is written. A rollout without `model_provider`
//! counts as `openai` (Codex's default), as in the local scan.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use superba_core::codex::PROVIDER_OPENAI;
use superba_core::{LocalizedText, Notice, ProviderSessions, Result, SessionReport, Timestamp};

use crate::records::Records;
use crate::script::{sh_quote, wrap};

/// Rollouts read at most per scan; more make the result incomplete (a warning).
pub const MAX_SCANNED: usize = 20_000;

/// Provider id used in the script for an unreadable head.
const UNKNOWN: &str = "?";

/// The scan script. Records: `K codex_home`, `K sqlite` (state database path,
/// when present), `K truncated <n>`, `K table` (tab-separated lines `provider,
/// live, archived, compressed, distinct` and a final `#, unreadable compressed,
/// no session_meta`), `K done`. `codex_home = None` uses
/// `${CODEX_HOME:-$HOME/.codex}` of the non-login shell.
pub fn scan_script(nonce: &str, codex_home: Option<&str>) -> String {
    let ch = codex_home.map_or_else(|| "\"${CODEX_HOME:-$HOME/.codex}\"".to_owned(), sh_quote);
    let mut b = String::new();
    let _ = writeln!(b, "ch={ch}");
    let _ = writeln!(
        b,
        r##"sb_put codex_home "$ch"
if [ -f "$ch/state_5.sqlite" ]; then sb_put sqlite "$ch/state_5.sqlite"; fi
sb_z=
for sb_c in zstd unzstd; do if command -v "$sb_c" >/dev/null 2>&1; then sb_z=$sb_c; break; fi; done
sb_list() {{
  if [ -d "$1" ]; then find "$1" -type f \( -name 'rollout-*.jsonl' -o -name 'rollout-*.jsonl.zst' \) 2>/dev/null | sed "s|^|$2 |"; fi
}}
sb_all=$(sb_list "$ch/sessions" s; sb_list "$ch/archived_sessions" a)
sb_n=$(printf '%s\n' "$sb_all" | grep -c . 2>/dev/null)
if [ "${{sb_n:-0}}" -gt {max} ]; then sb_put truncated "$sb_n"; fi
sb_t=$(printf '%s\n' "$sb_all" | head -n {max} | while IFS= read -r sb_e; do
  [ -n "$sb_e" ] || continue
  sb_k=${{sb_e%% *}}
  sb_f=${{sb_e#* }}
  case "$sb_f" in
    (*.zst)
      if [ -n "$sb_z" ]; then sb_c=1; sb_l=$("$sb_z" -dcq < "$sb_f" 2>/dev/null | head -n 1)
      else sb_c=2; sb_l=; fi ;;
    (*) sb_c=0; sb_l=$(head -n 1 "$sb_f" 2>/dev/null) ;;
  esac
  printf '%s\t%s\t%s\t%s\n' "$sb_k" "$sb_c" "${{sb_f##*/}}" "$sb_l"
done | awk -F '\t' '
{{
  line = $0; sub(/^[^\t]*\t[^\t]*\t[^\t]*\t/, "", line)
  if ($2 == 2) {{ unread++; next }}
  if (line ~ /"type": ?"session_meta"/) {{
    p = "{openai}"
    if (match(line, /"model_provider": ?"[^"]*"/)) {{
      v = substr(line, RSTART, RLENGTH); sub(/^"model_provider": ?"/, "", v); sub(/"$/, "", v); p = v
    }}
  }} else {{ nohead++; next }}
  id = ""
  if (match(line, /"id": ?"[^"]*"/)) {{ id = substr(line, RSTART, RLENGTH); sub(/^"id": ?"/, "", id); sub(/"$/, "", id) }}
  if (id == "") id = $3
  seen_p[p] = 1
  if ($1 == "a") arch[p]++; else live[p]++
  if ($2 == 1) zst[p]++
  if (!((p, id) in seen)) {{ seen[p, id] = 1; dist[p]++ }}
}}
END {{
  for (p in seen_p) printf "%s\t%d\t%d\t%d\t%d\n", p, live[p], arch[p], zst[p], dist[p]
  printf "#\t%d\t%d\n", unread, nohead
}}')
sb_put table "$sb_t"
sb_put done 1"##,
        max = MAX_SCANNED,
        openai = PROVIDER_OPENAI,
    );
    wrap(nonce, "", &b)
}

/// Build a [`SessionReport`] from the scan records.
pub fn parse_report(recs: &Records, now: Timestamp) -> Result<SessionReport> {
    recs.require_done("session scan")?;
    let codex_home = PathBuf::from(recs.get_raw_nonempty("codex_home").unwrap_or_default());
    let mut by_provider: BTreeMap<String, ProviderSessions> = BTreeMap::new();
    let mut warnings = Vec::new();
    let (mut unread, mut nohead) = (0_u32, 0_u32);
    for line in recs.get("table").unwrap_or_default().lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        let num = |i: usize| {
            cols.get(i)
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(0)
        };
        match cols.first().copied() {
            Some("#") => {
                unread = num(1);
                nohead = num(2);
            }
            Some(p) if !p.is_empty() && p != UNKNOWN && cols.len() >= 5 => {
                by_provider.insert(
                    p.to_owned(),
                    ProviderSessions {
                        threads: 0,
                        rollouts: num(1),
                        archived: num(2),
                        compressed: num(3),
                        distinct: num(4),
                    },
                );
            }
            _ => {}
        }
    }
    if let Some(n) = recs.get_nonempty("truncated") {
        warnings.push(Notice::warning(
            LocalizedText::new(
                "host_sessions.truncated",
                "The host has {count} session files; only the first {max} were counted.",
            )
            .arg("count", n)
            .arg("max", MAX_SCANNED),
        ));
    }
    if unread > 0 {
        warnings.push(Notice::warning(
            LocalizedText::new(
                "host_sessions.no_zstd",
                "{count} compressed session files were not counted: the host has no zstd.",
            )
            .arg("count", unread),
        ));
    }
    if nohead > 0 {
        warnings.push(Notice::warning(
            LocalizedText::new(
                "host_sessions.no_meta",
                "{count} session files have no readable session header and were not counted.",
            )
            .arg("count", nohead),
        ));
    }
    Ok(SessionReport {
        codex_home,
        scanned_at: now,
        sqlite_paths: recs
            .get_raw_nonempty("sqlite")
            .map(PathBuf::from)
            .into_iter()
            .collect(),
        by_provider,
        inconsistent: 0,
        warnings,
    })
}

/// Sample texts with arguments (catalog coverage tests).
pub fn all_samples() -> Vec<LocalizedText> {
    vec![
        LocalizedText::new(
            "host_sessions.truncated",
            "The host has {count} session files; only the first {max} were counted.",
        )
        .arg("count", 1)
        .arg("max", 2),
        LocalizedText::new(
            "host_sessions.no_zstd",
            "{count} compressed session files were not counted: the host has no zstd.",
        )
        .arg("count", 1),
        LocalizedText::new(
            "host_sessions.no_meta",
            "{count} session files have no readable session header and were not counted.",
        )
        .arg("count", 1),
    ]
}
