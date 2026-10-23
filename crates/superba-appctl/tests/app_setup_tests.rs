//! The desktop app's first-start Windows sandbox setup is seen as busy (user
//! bug 59): `codex-windows-sandbox-setup.exe` processes of this user.

#![allow(missing_docs)]

use std::path::PathBuf;

use superba_appctl::activity::app_setup_activity;
use superba_appctl::procs::{ProcEntry, Snapshot, app_setup_pids};
use superba_core::ActivitySignal;

fn entry(pid: u32, ppid: u32, exe: Option<&str>, argv: &[&str]) -> ProcEntry {
    ProcEntry {
        name: None,
        pid,
        ppid: Some(ppid),
        exe: exe.map(PathBuf::from),
        argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        started_at: None,
        env: None,
        zombie: false,
        foreign_user: false,
        cwd: None,
    }
}

#[test]
fn sandbox_setup_processes_of_this_user_are_found() {
    // As seen in the Windows VM: one started by the app-server, its elevated
    // child outliving it, and one started by the app's sandbox service.
    let bin = r"C:\Users\superba\AppData\Local\OpenAI\Codex\bin\896a0166b774037a";
    let setup = format!(r"{bin}\codex-windows-sandbox-setup.exe");
    let mut foreign = entry(30, 4460, Some(&setup), &[]);
    foreign.foreign_user = true;
    let mut gone = entry(31, 6592, Some(&setup), &[]);
    gone.zombie = true;
    let snap = Snapshot::classify_entries(
        vec![
            entry(
                6592,
                4208,
                Some(&format!(r"{bin}\codex.exe")),
                &["codex.exe", "app-server"],
            ),
            entry(11156, 6592, Some(&setup), &[]),
            // Exe unreadable: the command line names it (any case).
            entry(
                7900,
                11156,
                None,
                &[r"C:\x\Codex-Windows-Sandbox-Setup.EXE", "eyJ2"],
            ),
            entry(
                11204,
                4460,
                Some("/opt/codex/codex-windows-sandbox-setup"),
                &[],
            ),
            entry(40, 1, Some(r"C:\Windows\explorer.exe"), &[]),
            foreign,
            gone,
        ],
        &|_| false,
    );
    assert_eq!(app_setup_pids(&snap), vec![7900, 11156, 11204]);
    let empty = Snapshot::classify_entries(vec![entry(40, 1, None, &["explorer.exe"])], &|_| false);
    assert!(app_setup_pids(&empty).is_empty());
}

#[test]
fn the_setup_entry_is_local_and_marked() {
    let a = app_setup_activity();
    assert!(a.target_id.is_local());
    assert_eq!(a.signal, ActivitySignal::AppSetup);
    assert_eq!(a.thread_id, None);
}
