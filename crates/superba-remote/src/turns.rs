//! Remote turn activity from rollout files (plan 7.4): the newest turn event in a
//! rollout's tail decides; without one, a rollout written in the last
//! [`RECENT_WRITE_SECS`] seconds counts as busy (may false-positive).
//!
//! Ages are computed on the remote host (`date +%s` minus the file mtime), so the
//! two machines' clocks are never compared.

use superba_core::{ActivitySignal, TargetId, ThreadActivity, Timestamp};

use crate::records::Records;

/// A rollout written this recently with no conclusive tail counts as busy.
pub const RECENT_WRITE_SECS: i64 = 20;

const STARTED: &[&str] = &["task_started", "turn_started"];
const ENDED: &[&str] = &["task_complete", "turn_complete", "turn_aborted"];

/// Name of the event in a `"type":"<name>"` fragment.
fn event_name(fragment: &str) -> Option<&str> {
    let after = fragment.split_once(':')?.1.trim();
    after.strip_prefix('"')?.strip_suffix('"')
}

/// True if the turn script examined every recent rollout (no truncation, no
/// `find` / `stat` / `date` failure) and finished.
pub fn scan_complete(recs: &Records) -> bool {
    recs.has("done")
        && !recs.has("scan_failed")
        && !recs.has("truncated")
        && !recs.has("stat_failed")
}

/// Thread id from a rollout file name (`rollout-<timestamp>-<uuid>.jsonl`).
pub fn thread_id_from_path(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    // A UUID is the last 36 characters: 8-4-4-4-12 hex digits. Names come from
    // the host, so the slice is checked (it may fall inside a multi-byte char).
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    let groups: Vec<&str> = id.split('-').collect();
    let ok = groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()));
    ok.then(|| id.to_owned())
}

/// Busy threads from turn-script records, observed at local time `now`.
pub fn busy_threads(target: &TargetId, recs: &Records, now: Timestamp) -> Vec<ThreadActivity> {
    let mut out = Vec::new();
    for rec in recs.all("rollout") {
        let mut parts = rec.splitn(3, '|');
        let (Some(age), Some(last), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(age) = age.trim().parse::<i64>() else {
            continue;
        };
        let signal = match event_name(last) {
            Some(e) if STARTED.contains(&e) => Some(ActivitySignal::RolloutTail),
            Some(e) if ENDED.contains(&e) => None,
            _ if (0..=RECENT_WRITE_SECS).contains(&age) => Some(ActivitySignal::RolloutMtime),
            _ => None,
        };
        let Some(signal) = signal else { continue };
        out.push(ThreadActivity {
            target_id: target.clone(),
            thread_id: thread_id_from_path(path),
            title: None,
            rollout_path: Some(path.to_owned()),
            last_event_at: Some(Timestamp::from_millis(
                now.as_millis()
                    .saturating_sub(age.max(0).saturating_mul(1000)),
            )),
            signal,
        });
    }
    out
}
