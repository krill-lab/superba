//! Watchdog contract: when the router is disabled, uninstalled or found dead,
//! Superba writes the direct-mode Codex configuration back.
//!
//! Codex never fails a turn on a dead loopback port: it shows "Reconnecting…
//! waiting for network" and waits (spike Q3). The service manager restarts a
//! crashed router within seconds, so a short outage needs nothing. When the router
//! stays unreachable for [`WatchdogPolicy::grace`], the GUI or `spb` — whichever
//! runs the watchdog — calls [`DirectModeFallback::fall_back_to_direct`], which
//! `superba-switch` implements: write the direct configuration of the primary
//! member (its own key; PAT or API key as in a normal switch) and hot-restart the
//! app-server (a turn in flight is lost). Disabling or uninstalling the router
//! calls the same fallback *before* the service is stopped, so Codex never points
//! at a port nobody serves.
//!
//! [`Watchdog`] is a pure state machine over [`Liveness`] observations and
//! [`Instant`]s; [`check`] produces the observation.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use superba_core::{Result, async_trait};

use crate::client::AdminClient;
use crate::config::{LineConfig, MemberConfig, RouterConfig};
use crate::paths::RouterPaths;
use crate::status::RouterStatus;

/// Why Superba leaves router mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FallbackReason {
    /// The user turned the router off.
    Disabled,
    /// The router service was uninstalled (or its definition vanished).
    Uninstalled,
    /// The router stayed unreachable for the grace period.
    Dead {
        /// What the last check saw (no secrets).
        detail: String,
    },
}

/// Implemented by `superba-switch`: leave router mode for direct mode.
#[async_trait]
pub trait DirectModeFallback: Send + Sync {
    /// Write the direct-mode configuration for the primary member (see
    /// [`primary_target`]) and restart what reads it (hot restart of the local
    /// app-server; CLI processes pick it up at their next start). Must be
    /// idempotent: it may be called again after a partial failure.
    async fn fall_back_to_direct(&self, reason: &FallbackReason) -> Result<()>;
}

/// The member and line direct mode uses: the first member in priority order and
/// its first line. Callers may probe that member's lines and pick a better one.
pub fn primary_target(config: &RouterConfig) -> Option<(&MemberConfig, &LineConfig)> {
    let m = config.members.first()?;
    Some((m, m.lines.first()?))
}

/// One observation of the router.
#[derive(Clone, Debug, PartialEq)]
pub enum Liveness {
    /// The expected instance answers on the configured port.
    Up(Box<RouterStatus>),
    /// Nothing usable answers.
    Down {
        /// What failed (no secrets).
        detail: String,
    },
}

impl Liveness {
    /// True for [`Liveness::Up`].
    pub fn is_up(&self) -> bool {
        matches!(self, Liveness::Up(_))
    }
}

/// Observe the router described by `paths`: `runtime.json` must exist, name the
/// port of `config.json` (the one Codex points at), and the instance it names
/// must answer the authenticated status call.
pub async fn check(paths: &RouterPaths) -> Liveness {
    let down = |detail: String| Liveness::Down { detail };
    let config_port = match RouterConfig::load(paths) {
        Ok(c) => c.listen_port,
        Err(e) => return down(format!("router configuration: {e}")),
    };
    let client = match AdminClient::from_runtime(paths) {
        Ok(c) => c,
        Err(_) => return down("the router is not running (no runtime.json)".into()),
    };
    if config_port != 0 && client.port() != config_port {
        return down(format!(
            "the router listens on port {} but Codex is pointed at port {config_port}",
            client.port()
        ));
    }
    if let Err(e) = client.health().await {
        return down(e.to_string());
    }
    match client.status().await {
        Ok(s) => Liveness::Up(Box::new(s)),
        Err(e) => down(e.to_string()),
    }
}

/// Watchdog thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogPolicy {
    /// How long the router may stay down before Superba falls back. Long enough
    /// for the service manager to restart it (systemd 2 s, launchd 5 s throttle,
    /// supervisor backoff).
    pub grace: Duration,
}

impl Default for WatchdogPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(20),
        }
    }
}

/// Verdict of one observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The router is up.
    Healthy,
    /// Down for less than the grace period: wait (Codex waits too).
    Suspect {
        /// How long it has been down.
        down_for: Duration,
        /// Last failure.
        detail: String,
    },
    /// Fall back to direct mode now (reported once per outage).
    FallBack {
        /// Reason to pass to [`DirectModeFallback::fall_back_to_direct`].
        reason: FallbackReason,
    },
    /// Fallback already reported for this outage; nothing more to do.
    FellBack,
}

/// Turns observations into verdicts.
#[derive(Clone, Debug, Default)]
pub struct Watchdog {
    policy: WatchdogPolicy,
    down_since: Option<Instant>,
    fired: bool,
}

impl Watchdog {
    /// A watchdog with `policy`.
    pub fn new(policy: WatchdogPolicy) -> Self {
        Self {
            policy,
            down_since: None,
            fired: false,
        }
    }

    /// Feed one observation made at `now`.
    pub fn observe(&mut self, now: Instant, liveness: &Liveness) -> Verdict {
        match liveness {
            Liveness::Up(_) => {
                self.down_since = None;
                self.fired = false;
                Verdict::Healthy
            }
            Liveness::Down { detail } => {
                let since = *self.down_since.get_or_insert(now);
                let down_for = now.saturating_duration_since(since);
                if self.fired {
                    Verdict::FellBack
                } else if down_for >= self.policy.grace {
                    self.fired = true;
                    Verdict::FallBack {
                        reason: FallbackReason::Dead {
                            detail: detail.clone(),
                        },
                    }
                } else {
                    Verdict::Suspect {
                        down_for,
                        detail: detail.clone(),
                    }
                }
            }
        }
    }

    /// Forget the current outage (e.g. after the user re-enabled the router).
    pub fn reset(&mut self) {
        self.down_since = None;
        self.fired = false;
    }
}
