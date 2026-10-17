//! The MSI's side of the router: the Windows installer
//! (`packaging/windows/superba.wxs`) runs `spb router uninstall --quiet` when
//! Superba is removed and `... --keep-service` when it is upgraded, which stops
//! the `superba-router.exe` of the install ([`stop_router_processes`]) so its
//! file can be replaced or deleted. After an upgrade the update's second stage
//! ([`super::finish`], `--msi`) starts the router service again if it was
//! running ([`ENV_FORCE_ROUTER_RESTART`]).

use std::path::Path;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Set (to `1`) for `spb self-update restart-router` after an MSI update: the
/// installer stopped the router, so "restart" must start it although it is
/// not running.
pub const ENV_FORCE_ROUTER_RESTART: &str = "SUPERBA_FORCE_ROUTER_RESTART";

/// Stop every running `superba-router` whose executable is in `dir` (the
/// install being removed or replaced; a router of another install is left
/// alone). Returns how many were killed.
pub fn stop_router_processes(dir: &Path) -> usize {
    // A supervisor may respawn its child once: go again until nothing is left.
    let mut total = 0;
    for _ in 0..4 {
        let killed = stop_router_processes_once(dir);
        if killed == 0 {
            break;
        }
        total += killed;
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    total
}

fn stop_router_processes_once(dir: &Path) -> usize {
    let want = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let mut killed = 0;
    for process in sys.processes().values() {
        let Some(exe) = process.exe() else { continue };
        let stem = exe.file_stem().map(|s| s.to_string_lossy().to_lowercase());
        if stem.as_deref() != Some("superba-router") {
            continue;
        }
        let Some(parent) = exe.parent() else { continue };
        let parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_owned());
        if same_dir(&parent, &want) && process.kill() {
            killed += 1;
        }
    }
    killed
}

fn same_dir(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}
