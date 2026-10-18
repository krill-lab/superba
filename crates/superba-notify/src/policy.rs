//! Which channels get an event: triggers, muted harnesses, the minimum task
//! length, quiet hours, deduplication and the per-channel rate limit.
//!
//! Hooks run as separate short-lived processes (Codex starts one per finished
//! task, Claude Code one per hook), so the counters live in
//! `<data>/notify/state.json` and every decision takes `state.lock` first.
//! The state holds ids and counters only, never message text.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{EventKind, NotifyChannel, NotifyEvent, NotifySettings, NotifySkip};

/// How long a sent turn is remembered for deduplication.
pub const DEDUPE_WINDOW_MS: i64 = 6 * 3600 * 1000;
/// Rate-limit window.
pub const RATE_WINDOW_MS: i64 = 60 * 1000;

/// Counters of one channel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelCounters {
    /// Start of the current window (Unix ms).
    pub window_start: i64,
    /// Messages sent in it.
    pub sent: u32,
    /// Messages dropped since the last one sent.
    pub dropped: u32,
}

/// `state.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyState {
    /// Per channel id.
    pub channels: BTreeMap<String, ChannelCounters>,
    /// Dedupe key -> when it was sent (Unix ms).
    pub sent: BTreeMap<String, i64>,
    /// Watcher bookmarks (rollout path -> byte offset read).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub watch: BTreeMap<String, u64>,
    /// When the watcher last scanned (Unix ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_scanned_ms: Option<i64>,
}

/// The decision for one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Dropped before any channel.
    pub skipped: Option<NotifySkip>,
    /// Per channel: send (with the number of messages dropped before it) or skip.
    pub channels: Vec<(NotifyChannel, Result<u32, NotifySkip>)>,
}

/// Inputs besides the event and the settings.
#[derive(Clone, Debug, Default)]
pub struct PolicyInput {
    /// Unix ms.
    pub now_ms: i64,
    /// Local minute of the day (0..1440) for quiet hours; `None` = unknown
    /// (quiet hours are then ignored).
    pub local_minute: Option<u16>,
    /// Only this channel.
    pub only_channel: Option<String>,
    /// Ignore everything but the channel selection (test button).
    pub force: bool,
}

/// Decide for `event` and update `state` (counters, dedupe keys).
pub fn decide(
    event: &NotifyEvent,
    settings: &NotifySettings,
    input: &PolicyInput,
    state: &mut PolicyState,
) -> Decision {
    let skip = |reason| Decision {
        skipped: Some(reason),
        channels: Vec::new(),
    };
    let always = input.force || event.kind.always_sent();
    prune(state, input.now_ms);
    if !input.force && !settings.enabled {
        return skip(NotifySkip::Disabled);
    }
    if !always {
        if !settings.triggers.wants(event.kind) {
            return skip(NotifySkip::TriggerOff);
        }
        if settings.triggers.muted_harnesses.contains(&event.harness) {
            return skip(NotifySkip::HarnessMuted);
        }
        let min_ms = u64::from(settings.triggers.min_turn_secs) * 1000;
        if event.kind == EventKind::TurnComplete
            && min_ms > 0
            && event.duration_ms.is_some_and(|d| d < min_ms)
        {
            return skip(NotifySkip::TooShort);
        }
        let quiet = &settings.quiet_hours;
        if let Some(minute) = input.local_minute
            && quiet.contains(minute)
            && !(event.kind.urgent() && quiet.allow_urgent)
        {
            return skip(NotifySkip::QuietHours);
        }
        if let Some(key) = event.dedupe_key() {
            if state.sent.contains_key(&key) {
                return skip(NotifySkip::Duplicate);
            }
            state.sent.insert(key, input.now_ms);
        }
    }
    let mut channels = Vec::new();
    for ch in &settings.channels {
        if input.only_channel.as_deref().is_some_and(|id| id != ch.id) {
            continue;
        }
        let verdict = if !input.force && !ch.enabled {
            Err(NotifySkip::ChannelOff)
        } else if !always && !ch.only.is_empty() && !ch.only.contains(&event.kind) {
            Err(NotifySkip::TriggerOff)
        } else if always {
            Ok(0)
        } else {
            rate(state, &ch.id, settings.rate_limit.per_minute, input.now_ms)
        };
        channels.push((ch.clone(), verdict));
    }
    Decision {
        skipped: None,
        channels,
    }
}

fn rate(state: &mut PolicyState, id: &str, per_minute: u32, now: i64) -> Result<u32, NotifySkip> {
    let c = state.channels.entry(id.to_owned()).or_default();
    if now - c.window_start >= RATE_WINDOW_MS || now < c.window_start {
        c.window_start = now;
        c.sent = 0;
    }
    if per_minute > 0 && c.sent >= per_minute {
        c.dropped = c.dropped.saturating_add(1);
        return Err(NotifySkip::RateLimited);
    }
    c.sent += 1;
    Ok(std::mem::take(&mut c.dropped))
}

/// An update is announced once per version (a month), other events once
/// per [`DEDUPE_WINDOW_MS`].
fn window_of(key: &str) -> i64 {
    if key.contains(":update_available:") {
        30 * 24 * 3600 * 1000
    } else {
        DEDUPE_WINDOW_MS
    }
}

fn prune(state: &mut PolicyState, now: i64) {
    state.sent.retain(|key, at| now - *at < window_of(key));
    if state.watch.len() > 512 {
        // Bookmarks of files that are long gone: keep the newest half (the
        // watcher only reads recent rollouts anyway).
        let drop = state.watch.len() - 256;
        let keys: Vec<String> = state.watch.keys().take(drop).cloned().collect();
        for k in keys {
            state.watch.remove(&k);
        }
    }
}

/// `<dir>/state.json` under `<dir>/state.lock`.
#[derive(Clone, Debug)]
pub struct StateFile {
    dir: PathBuf,
}

impl StateFile {
    /// The state in `dir` (`<data>/notify`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Read, change and write the state under the lock. A missing or corrupt
    /// file starts empty (the counters are only a courtesy).
    pub fn update<T>(&self, f: impl FnOnce(&mut PolicyState) -> T) -> std::io::Result<T> {
        std::fs::create_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("state.lock"))?;
        // std's advisory lock (flock / LockFileEx); released when `lock` drops.
        lock.lock()?;
        let path = self.dir.join("state.json");
        let mut state: PolicyState = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let out = f(&mut state);
        let tmp = self.dir.join("state.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&state)?)?;
        std::fs::rename(&tmp, &path)?;
        lock.unlock()?;
        Ok(out)
    }
}
