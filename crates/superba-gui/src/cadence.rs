//! What the shell asks the backend for on its own, and when (user request 34).
//!
//! The overview's fast facts (is the app running, CLI processes, the applied
//! configuration) are read every [`OVERVIEW_EVERY`] while the window is shown;
//! the switch service answers from cheap probes and the environment monitor's
//! snapshot, so this path never runs detection or starts a process. Slow facts
//! (installs, Codex binaries and versions) belong to the environment monitor of
//! the switch service: the shell only tells it about the window
//! ([`EnvironmentEvent`]) and reloads the views built from them when its
//! generation moves. The remote host list follows focus changes and a long
//! interval; Superba's own update check runs once a day while shown.
//!
//! While the window is hidden or minimised the router watchdog and notification
//! watcher keep ticking. Managed ChatGPT quotas have a separate one-minute
//! timer in the provider state, polled by the app alongside this cadence.
//! Showing the window again or giving it focus reads the overview at once.

use std::time::{Duration, Instant};

use superba_core::EnvironmentEvent;

/// Overview poll while the window is shown.
pub const OVERVIEW_EVERY: Duration = Duration::from_secs(5);
/// Router watchdog tick (also while hidden).
pub const ROUTER_WATCH_EVERY: Duration = Duration::from_secs(5);
/// Remote host list while the window is shown.
pub const HOSTS_EVERY: Duration = Duration::from_secs(5 * 60);
/// Remote host list when the focus comes back and the last load is this old.
pub const HOSTS_ON_FOCUS_AFTER: Duration = Duration::from_secs(60);
/// Superba's own update check (the backend also honours its preference).
pub const SELF_UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Look for failed Codex turns to notify about (GH#12; also while hidden).
pub const NOTIFY_WATCH_EVERY: Duration = Duration::from_secs(20);

/// What the shell can do right now (from its state at this frame).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    /// The window is shown (not minimised or hidden).
    pub visible: bool,
    /// The window has the keyboard focus.
    pub focused: bool,
    /// The first overview arrived (shell data loaded).
    pub entered: bool,
    /// A switch runs (no polls meanwhile; the switch reports itself).
    pub switching: bool,
    /// The router watchdog has work: router mode on, supported, no tick in flight,
    /// no switch planned or running.
    pub router_watch: bool,
    /// An overview load is in flight.
    pub overview_loading: bool,
    /// A host list load is in flight.
    pub hosts_loading: bool,
    /// A self-update check is in flight.
    pub selfupdate_loading: bool,
    /// Notifications want the failed-turn watcher and no scan is in flight.
    pub notify_watch: bool,
    /// The environment monitor's generation as the service reports it.
    pub environment_generation: u64,
}

/// Something due now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// Tell the environment monitor about the window.
    Environment(EnvironmentEvent),
    /// Read the overview.
    Overview,
    /// One router watchdog tick.
    RouterWatch,
    /// Reload the remote host list.
    Hosts,
    /// Look for a newer Superba.
    SelfUpdate,
    /// Look for failed Codex turns to notify about.
    NotifyWatch,
    /// The environment monitor saw a change: reload the views built from it.
    EnvironmentChanged,
}

/// The shell's timers and what it last told the environment monitor.
#[derive(Debug)]
pub struct Cadence {
    shown: Option<bool>,
    focused: bool,
    last_overview: Option<Instant>,
    last_router_watch: Option<Instant>,
    last_hosts: Option<Instant>,
    last_selfupdate: Instant,
    last_notify_watch: Option<Instant>,
    generation: Option<u64>,
}

impl Cadence {
    /// Timers started at `now` (the start-up loads count as the first reads).
    pub fn new(now: Instant) -> Self {
        Self {
            shown: None,
            focused: false,
            last_overview: Some(now),
            last_router_watch: Some(now),
            last_hosts: Some(now),
            last_selfupdate: now,
            last_notify_watch: Some(now),
            generation: None,
        }
    }

    /// The overview was just read (also on a reload the user asked for).
    pub fn overview_read(&mut self, now: Instant) {
        self.last_overview = Some(now);
    }

    /// The host list was just loaded.
    pub fn hosts_loaded(&mut self, now: Instant) {
        self.last_hosts = Some(now);
    }

    /// Superba's update state was just asked for.
    pub fn selfupdate_checked(&mut self, now: Instant) {
        self.last_selfupdate = now;
    }

    /// What is due at `now`, in order. Marks it as done.
    pub fn tick(&mut self, now: Instant, inputs: Inputs) -> Vec<Due> {
        let mut due = Vec::new();
        let older = |at: Option<Instant>, age: Duration| {
            at.is_none_or(|at| now.saturating_duration_since(at) >= age)
        };
        // Window events for the environment monitor.
        let mut came_back = false;
        if self.shown != Some(inputs.visible) {
            due.push(Due::Environment(if inputs.visible {
                EnvironmentEvent::Shown
            } else {
                EnvironmentEvent::Hidden
            }));
            came_back = inputs.visible && self.shown.is_some();
            self.shown = Some(inputs.visible);
        }
        let focus_gained = inputs.visible && inputs.focused && !self.focused;
        if focus_gained {
            due.push(Due::Environment(EnvironmentEvent::FocusGained));
        }
        self.focused = inputs.visible && inputs.focused;
        // Coming back reads at once, but not more than once a second.
        let back = (came_back || focus_gained) && older(self.last_overview, Duration::from_secs(1));

        // The watchdog runs whatever the window does.
        if inputs.router_watch && older(self.last_router_watch, ROUTER_WATCH_EVERY) {
            self.last_router_watch = Some(now);
            due.push(Due::RouterWatch);
        }
        if inputs.notify_watch && older(self.last_notify_watch, NOTIFY_WATCH_EVERY) {
            self.last_notify_watch = Some(now);
            due.push(Due::NotifyWatch);
        }
        if !inputs.visible {
            return due;
        }
        // Shown: the fast facts, at once after the window comes back.
        if !inputs.switching
            && !inputs.overview_loading
            && (back || older(self.last_overview, OVERVIEW_EVERY))
        {
            self.last_overview = Some(now);
            due.push(Due::Overview);
        }
        if inputs.entered
            && !inputs.switching
            && !inputs.hosts_loading
            && (older(self.last_hosts, HOSTS_EVERY)
                || ((came_back || focus_gained) && older(self.last_hosts, HOSTS_ON_FOCUS_AFTER)))
        {
            self.last_hosts = Some(now);
            due.push(Due::Hosts);
        }
        if inputs.entered
            && !inputs.selfupdate_loading
            && older(Some(self.last_selfupdate), SELF_UPDATE_EVERY)
        {
            self.last_selfupdate = now;
            due.push(Due::SelfUpdate);
        }
        match self.generation {
            Some(g) if g != inputs.environment_generation => {
                due.push(Due::EnvironmentChanged);
            }
            _ => {}
        }
        self.generation = Some(inputs.environment_generation);
        due
    }

    /// How long the window may sleep after a tick at `now` before the next
    /// timer is due (`None`: only an event or a backend reply wakes it).
    pub fn next_wake(&self, now: Instant, inputs: Inputs) -> Option<Duration> {
        let left = |at: Option<Instant>, every: Duration| {
            at.map_or(Duration::ZERO, |at| {
                (at + every).saturating_duration_since(now)
            })
        };
        let router = inputs
            .router_watch
            .then(|| left(self.last_router_watch, ROUTER_WATCH_EVERY));
        let notify = inputs
            .notify_watch
            .then(|| left(self.last_notify_watch, NOTIFY_WATCH_EVERY));
        let watch = match (router, notify) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if !inputs.visible {
            return watch;
        }
        let shown = left(self.last_overview, OVERVIEW_EVERY)
            .min(left(self.last_hosts, HOSTS_EVERY))
            .min(left(Some(self.last_selfupdate), SELF_UPDATE_EVERY));
        Some(watch.map_or(shown, |w| w.min(shown)))
    }
}
