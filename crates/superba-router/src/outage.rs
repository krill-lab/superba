//! Member outages for notifications (GH#12): a member none of whose lines has
//! been usable for [`DOWN_FOR`] is reported once per outage by starting
//! `spb notify event router_member_down` next to this binary (`spb` holds the
//! preferences and the channel secrets; the router has neither). The breaker
//! opens and closes within seconds on a single failure, so only a long outage
//! is reported, and only once until the member serves again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::state::{Generation, lock};

/// How long every line of a member must be unusable before it is reported.
pub const DOWN_FOR: Duration = Duration::from_secs(120);

/// A member to report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outage {
    /// Member id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The last error of its lines.
    pub error: Option<String>,
    /// Unique id of this outage (member id + start), for deduplication.
    pub key: String,
}

/// Per-member outage state between health ticks.
#[derive(Debug, Default)]
pub struct OutageTracker {
    /// Member id -> (down since, already reported, wall-clock start in ms).
    down: HashMap<String, (Instant, bool, u128)>,
}

/// One member as the tracker sees it at a tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberHealth {
    /// Member id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// At least one line is usable.
    pub usable: bool,
    /// The last error of its lines.
    pub error: Option<String>,
}

impl OutageTracker {
    /// Look at every member at `now` (`wall_ms` = the wall clock, for the
    /// outage key); returns the members whose outage just passed
    /// [`DOWN_FOR`]. Members missing from `members` are forgotten.
    pub fn observe(
        &mut self,
        members: &[MemberHealth],
        now: Instant,
        wall_ms: u128,
    ) -> Vec<Outage> {
        let mut out = Vec::new();
        for m in members {
            if m.usable {
                self.down.remove(&m.id);
                continue;
            }
            let entry = self
                .down
                .entry(m.id.clone())
                .or_insert((now, false, wall_ms));
            if !entry.1 && now.saturating_duration_since(entry.0) >= DOWN_FOR {
                entry.1 = true;
                out.push(Outage {
                    id: m.id.clone(),
                    name: if m.name.is_empty() {
                        m.id.clone()
                    } else {
                        m.name.clone()
                    },
                    error: m.error.clone(),
                    key: format!("{}@{}", m.id, entry.2),
                });
            }
        }
        self.down
            .retain(|id, _| members.iter().any(|m| &m.id == id));
        out
    }

    /// [`OutageTracker::observe`] over a router generation.
    pub(crate) fn tick(&mut self, generation: &Generation, now: Instant) -> Vec<Outage> {
        let members: Vec<MemberHealth> = generation
            .members
            .iter()
            .filter(|m| !m.lines.is_empty())
            .map(|m| {
                let mut error = None;
                let usable = m.lines.iter().any(|l| {
                    let h = lock(&l.health);
                    if error.is_none() {
                        error = h.last_error().map(str::to_owned);
                    }
                    h.is_available(now)
                });
                MemberHealth {
                    id: m.cfg.id.clone(),
                    name: m.cfg.name.clone(),
                    usable,
                    error,
                }
            })
            .collect();
        let wall_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        self.observe(&members, now, wall_ms)
    }
}

/// `spb` next to the running router (`spb.exe` on Windows); the GUI binary
/// `superba` runs the same commands.
pub fn spb_next_to(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let names: &[&str] = if cfg!(windows) {
        &["spb.exe", "superba.exe"]
    } else {
        &["spb", "superba"]
    };
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Start `spb notify event` for `outage` (detached, no output, never waited
/// for; failures are only logged).
pub fn report(data_dir: &Path, outage: &Outage) {
    let Some(spb) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(spb_next_to)
    else {
        tracing::debug!(
            "no spb next to the router; outage of {} not reported",
            outage.id
        );
        return;
    };
    let mut cmd = superba_core::process::command(spb);
    cmd.args(["notify", "event", "router_member_down", "--detail"])
        .arg(match &outage.error {
            Some(e) => format!("{} ({e})", outage.name),
            None => outage.name.clone(),
        })
        .args(["--id", &outage.key])
        .env("SUPERBA_DATA_DIR", data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match cmd.spawn() {
        // Reap it in the background so it never stays a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::warn!(error = %e, "cannot start spb to report a router outage"),
    }
}
