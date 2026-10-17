//! A small self-supervisor for platforms without a persistent service manager.

use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use superba_core::Result;

use super::autostart::read_pid;
use super::system::{atomic_write, remove_file, validate_spec};
use super::{CommandRunner, ServiceSpec, SystemRunner};

/// Missing or invalid router configuration: stop instead of restarting.
pub const EXIT_CONFIG: i32 = 78;

/// Restart delays, health threshold and graceful shutdown timing.
#[derive(Clone, Debug)]
pub struct SupervisorPolicy {
    /// Delay before retrying the first short-lived child or failed spawn.
    pub initial_backoff: Duration,
    /// Upper bound on the exponentially increasing restart delay.
    pub max_backoff: Duration,
    /// Running this long resets the restart delay to the initial value.
    pub healthy_after: Duration,
    /// Interval for child checks and interruptible backoff sleeps.
    pub poll: Duration,
    /// Maximum time between graceful termination and forced killing.
    pub stop_grace: Duration,
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            healthy_after: Duration::from_secs(60),
            poll: Duration::from_millis(200),
            stop_grace: Duration::from_secs(5),
        }
    }
}

/// A child process that can be polled and stopped without blocking indefinitely.
pub trait SupervisedChild: Send {
    /// `None` means running; `Some(None)` means exited by signal, otherwise code.
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>>;
    /// Request a graceful stop.
    fn terminate(&mut self);
    /// Force the child to exit and reap it.
    fn kill(&mut self);
}

/// Why supervision ended.
#[derive(Debug, PartialEq)]
pub enum SuperviseEnd {
    /// The caller requested a stop, or another supervisor owns this instance.
    Stopped,
    /// The child reported missing or invalid configuration.
    ConfigInvalid,
    /// Ten consecutive attempts to spawn the child failed; contains the last error.
    SpawnFailed(String),
}

fn stopped(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Acquire)
}

fn poll(policy: &SupervisorPolicy) -> Duration {
    // Even a zero poll interval must let an injected clock make progress.
    policy.poll.max(Duration::from_millis(1))
}

fn stop_child(
    child: &mut dyn SupervisedChild,
    policy: &SupervisorPolicy,
    now: &dyn Fn() -> Instant,
    sleep: &dyn Fn(Duration),
) {
    child.terminate();
    let start = now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Err(_) => break,
            Ok(None) => {}
        }
        let remaining = policy
            .stop_grace
            .saturating_sub(now().saturating_duration_since(start));
        if remaining.is_zero() {
            break;
        }
        sleep(poll(policy).min(remaining));
    }
    child.kill();
}

fn backoff_sleep(
    duration: Duration,
    stop: &AtomicBool,
    policy: &SupervisorPolicy,
    now: &dyn Fn() -> Instant,
    sleep: &dyn Fn(Duration),
) -> bool {
    let start = now();
    loop {
        if stopped(stop) {
            return true;
        }
        let remaining = duration.saturating_sub(now().saturating_duration_since(start));
        if remaining.is_zero() {
            return false;
        }
        sleep(poll(policy).min(remaining));
    }
}

/// Supervise using injected child creation, monotonic time and sleep operations.
///
/// Short-lived children and spawn failures share exponential backoff. A healthy
/// child resets it. Stop requests interrupt polling and backoff; a running child
/// gets a graceful stop followed by a forced kill after `stop_grace`.
pub fn supervise_with(
    spawn: &mut dyn FnMut() -> io::Result<Box<dyn SupervisedChild>>,
    stop: &AtomicBool,
    policy: &SupervisorPolicy,
    now: &dyn Fn() -> Instant,
    sleep: &dyn Fn(Duration),
) -> SuperviseEnd {
    let initial = policy.initial_backoff.min(policy.max_backoff);
    let mut backoff = initial;
    let mut failures = 0;
    loop {
        if stopped(stop) {
            return SuperviseEnd::Stopped;
        }
        match spawn() {
            Err(error) => {
                failures += 1;
                if failures == 10 {
                    return SuperviseEnd::SpawnFailed(error.to_string());
                }
                tracing::warn!(%error, failures, "Router spawn failed; retrying");
            }
            Ok(mut child) => {
                failures = 0;
                let started = now();
                loop {
                    if stopped(stop) {
                        stop_child(child.as_mut(), policy, now, sleep);
                        return SuperviseEnd::Stopped;
                    }
                    match child.try_wait() {
                        Ok(Some(Some(EXIT_CONFIG))) => return SuperviseEnd::ConfigInvalid,
                        Ok(Some(_)) => break,
                        Ok(None) => sleep(poll(policy)),
                        Err(error) => {
                            tracing::warn!(%error, "Cannot poll router; stopping it before retrying");
                            stop_child(child.as_mut(), policy, now, sleep);
                            break;
                        }
                    }
                }
                if now().saturating_duration_since(started) >= policy.healthy_after {
                    backoff = initial;
                }
                tracing::warn!(delay = ?backoff, "Router exited; restarting");
            }
        }
        if backoff_sleep(backoff, stop, policy, now, sleep) {
            return SuperviseEnd::Stopped;
        }
        backoff = backoff.saturating_mul(2).min(policy.max_backoff);
    }
}

struct RouterChild(Child);

impl SupervisedChild for RouterChild {
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        self.0.try_wait().map(|status| {
            status.map(|status| {
                tracing::info!(pid = self.0.id(), code = ?status.code(), "Router exited");
                status.code()
            })
        })
    }

    fn terminate(&mut self) {
        #[cfg(unix)]
        let _ = SystemRunner.terminate(self.0.id());
        #[cfg(windows)]
        let _ = self.0.kill();
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
        if let Ok(status) = self.0.wait() {
            tracing::info!(pid = self.0.id(), code = ?status.code(), "Router exited");
        }
    }
}

/// Run the real supervisor, guarding its pid file against an existing instance.
///
/// The child inherits stdout/stderr and receives `spec.env`. The pid file is
/// removed on exit only when it still belongs to this supervisor.
pub fn supervise(spec: &ServiceSpec, stop: &AtomicBool) -> Result<SuperviseEnd> {
    validate_spec(spec)?;
    let our_pid = std::process::id();
    if let Some(pid) = read_pid(&spec.pid_file)?
        && pid != our_pid
        && SystemRunner.is_router_process(pid)
    {
        tracing::info!(pid, "A router supervisor is already running");
        return Ok(SuperviseEnd::Stopped);
    }
    atomic_write(&spec.pid_file, &format!("{our_pid}\n"), 0o600)?;
    let mut spawn = || -> io::Result<Box<dyn SupervisedChild>> {
        let mut command = Command::new(&spec.exe);
        command
            .args(spec.run_args())
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn()?;
        tracing::info!(pid = child.id(), "Router started");
        Ok(Box::new(RouterChild(child)))
    };
    let end = supervise_with(
        &mut spawn,
        stop,
        &SupervisorPolicy::default(),
        &Instant::now,
        &std::thread::sleep,
    );
    if read_pid(&spec.pid_file)? == Some(our_pid) {
        remove_file(&spec.pid_file)?;
    }
    Ok(end)
}
