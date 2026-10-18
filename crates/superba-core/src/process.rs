//! The one place a child process is created.
//!
//! The GUI is a Windows `windows` subsystem program, so a child that is created
//! without `CREATE_NO_WINDOW` gets a console window of its own: every probe
//! (`codex --version`, `ssh`, `powershell`, ...) would flash a window. Every
//! crate therefore builds its [`std::process::Command`] through [`command`]
//! (or [`detached_command`]); a test in this crate (`tests/process_tests.rs`)
//! fails when `Command::new` appears anywhere else in `crates/*/src`.
//!
//! For tokio use `tokio::process::Command::from(command(program))`; the
//! creation flags are carried over by the conversion.
//!
//! Every command built here is counted ([`commands_built`]): tests use the count
//! to prove that periodic paths (the GUI's overview poll, the environment
//! monitor's checks) start no process at all.

use std::ffi::OsStr;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Commands built through this module since the process started.
static BUILT: AtomicU64 = AtomicU64::new(0);

/// How many child-process commands were built through [`command`] and
/// [`detached_command`] since the process started (each is built right before
/// it is spawned). Monotonic; compare two readings.
pub fn commands_built() -> u64 {
    BUILT.load(Ordering::Relaxed)
}

/// `CREATE_NO_WINDOW`: a console program gets no console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `DETACHED_PROCESS`: the child has no console and does not inherit ours.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: Ctrl+C aimed at us does not reach the child.
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// A command for a short-lived child (a probe, a helper, a supervised service)
/// that never shows a console window. Nothing is changed outside Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    BUILT.fetch_add(1, Ordering::Relaxed);
    let mut command = Command::new(program);
    no_window(&mut command);
    command
}

/// Hide the console window of `command` (`CREATE_NO_WINDOW`); a no-op outside Windows.
///
/// Use this only for a command that must be built another way; prefer [`command`].
pub fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// A command for a child that works in our terminal and that we wait for: the
/// user's text editor of `spb config edit` (vi, nano, notepad). It keeps our
/// console and standard streams (a hidden console would leave a terminal
/// editor without one); nothing else is changed.
pub fn terminal_command(program: impl AsRef<OsStr>) -> Command {
    BUILT.fetch_add(1, Ordering::Relaxed);
    Command::new(program)
}

/// A command for a child that must outlive us and stay out of our console and
/// Ctrl+C group (the Codex desktop app, a background daemon).
///
/// Windows uses `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` and not
/// `CREATE_NO_WINDOW`: the two are mutually exclusive for a console program
/// (`CREATE_NO_WINDOW` is ignored when `DETACHED_PROCESS` is set), and a detached
/// child has no console to show in the first place. Nothing is changed outside Windows.
pub fn detached_command(program: impl AsRef<OsStr>) -> Command {
    BUILT.fetch_add(1, Ordering::Relaxed);
    let mut command = Command::new(program);
    detach(&mut command);
    command
}

/// Apply the [`detached_command`] flags to a command that is already configured.
pub fn detach(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command
}

/// Hide the console window of a long-running background child and give it its
/// own process group (`CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`), so a Ctrl+C
/// aimed at our console does not reach it. For the router service, which is not a
/// GUI and keeps its stdio redirected to a log file instead of a console.
pub fn no_window_new_group(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    command
}
