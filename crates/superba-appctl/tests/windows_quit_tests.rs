//! Windows termination is asynchronous, even with permission to terminate.
#![cfg(windows)]

use std::os::windows::{io::AsRawHandle, process::CommandExt};
use std::process::{Child, Command};
use std::time::Duration;

use superba_appctl::{osproc, procs, quit};
use superba_core::{AppSession, EnvObservation, QuitOptions, QuitOutcome, QuitStep};
use windows_sys::Win32::Foundation::{CloseHandle, DBG_CONTINUE, LocalFree};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CREATE_PROCESS_DEBUG_EVENT, CREATE_THREAD_DEBUG_EVENT, ContinueDebugEvent, DEBUG_EVENT,
    DebugSetProcessKillOnExit, EXCEPTION_DEBUG_EVENT, EXIT_PROCESS_DEBUG_EVENT,
    LOAD_DLL_DEBUG_EVENT, WaitForDebugEvent,
};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(debug: bool) -> OwnedChild {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--ignored", "--exact", "fixture", "--nocapture"])
        .env("SUPERBA_QUIT_TEST_CHILD", "1");
    if debug {
        c.creation_flags(2); // DEBUG_ONLY_THIS_PROCESS
    }
    OwnedChild(c.spawn().unwrap())
}

#[test]
#[ignore = "a child process of these tests only"]
fn fixture() {
    if std::env::var("SUPERBA_QUIT_TEST_CHILD").as_deref() == Ok("1") {
        std::thread::sleep(Duration::from_secs(120));
    }
}

/// Impersonates this process's own token on the calling thread with
/// SeDebugPrivilege disabled, until dropped. Thread-local: tests running in
/// parallel keep the process token as it is.
struct WithoutDebugPrivilege;

impl WithoutDebugPrivilege {
    fn new() -> Self {
        use windows_sys::Win32::Foundation::LUID;
        use windows_sys::Win32::Security::{
            AdjustTokenPrivileges, ImpersonateSelf, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SecurityImpersonation, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
        let name: Vec<u16> = "SeDebugPrivilege\0".encode_utf16().collect();
        // SAFETY: valid out pointers, a NUL-terminated name, and a thread
        // token handle closed once after use; RevertToSelf runs on drop.
        unsafe {
            assert_ne!(ImpersonateSelf(SecurityImpersonation), 0);
            let mut token = std::ptr::null_mut();
            assert_ne!(
                OpenThreadToken(
                    GetCurrentThread(),
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                    0,
                    &mut token
                ),
                0
            );
            let mut luid = LUID {
                LowPart: 0,
                HighPart: 0,
            };
            if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) != 0 {
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: 0,
                    }],
                };
                AdjustTokenPrivileges(
                    token,
                    0,
                    &privileges,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
            CloseHandle(token);
        }
        Self
    }
}

impl Drop for WithoutDebugPrivilege {
    fn drop(&mut self) {
        // SAFETY: ends this thread's impersonation started in `new`.
        unsafe {
            windows_sys::Win32::Security::RevertToSelf();
        }
    }
}

fn session(pid: u32) -> AppSession {
    AppSession {
        install_id: None,
        main_pid: pid,
        started_at: osproc::facts(pid).created,
        executable: None,
        cdp: None,
        launched_by_us: false,
        authapi_env: EnvObservation::Unknown,
        home_override: None,
        codex_home: None,
        app_server_exe: None,
    }
}

/// Hold Windows' exit-debug event: TerminateProcess succeeds, but its handle
/// stays unsignalled until the debugger releases the event. No external app,
/// account, configuration or process is used by this regression.
fn delayed_exit(delay: Duration) -> (u32, std::thread::JoinHandle<()>) {
    let (send, recv) = std::sync::mpsc::sync_channel(1);
    let debugger = std::thread::spawn(move || {
        let child = spawn(true);
        let mut send = Some(send);
        // SAFETY: this thread is the debugger of the child it just created.
        unsafe { DebugSetProcessKillOnExit(0) };
        loop {
            // SAFETY: DEBUG_EVENT is plain zero-initializable storage.
            let mut event: DEBUG_EVENT = unsafe { std::mem::zeroed() };
            // SAFETY: valid writable event; only our child's events are read.
            assert_ne!(unsafe { WaitForDebugEvent(&mut event, 60_000) }, 0);
            // SAFETY: the event code identifies the active union field. Windows
            // gives the debugger these handles separately from std's Child.
            unsafe {
                match event.dwDebugEventCode {
                    CREATE_PROCESS_DEBUG_EVENT => {
                        let info = event.u.CreateProcessInfo;
                        if !info.hFile.is_null() {
                            CloseHandle(info.hFile);
                        }
                        CloseHandle(info.hProcess);
                        CloseHandle(info.hThread);
                    }
                    CREATE_THREAD_DEBUG_EVENT => {
                        CloseHandle(event.u.CreateThread.hThread);
                    }
                    LOAD_DLL_DEBUG_EVENT => {
                        let h = event.u.LoadDll.hFile;
                        if !h.is_null() {
                            CloseHandle(h);
                        }
                    }
                    _ => {}
                }
            }
            if event.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT {
                std::thread::sleep(delay);
            }
            // SAFETY: acknowledge the event just read by this debugger thread.
            assert_ne!(
                unsafe { ContinueDebugEvent(event.dwProcessId, event.dwThreadId, DBG_CONTINUE) },
                0
            );
            if event.dwDebugEventCode == EXCEPTION_DEBUG_EVENT
                && let Some(send) = send.take()
            {
                send.send(child.0.id()).unwrap();
            }
            if event.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT {
                break;
            }
        }
    });
    let pid = recv.recv_timeout(Duration::from_secs(30)).unwrap();
    (pid, debugger)
}

#[tokio::test]
async fn accepted_termination_waits_for_completion_without_reterminating() {
    let (pid, debugger) = delayed_exit(Duration::from_secs(12));
    let unrelated = spawn(false);
    let session = session(pid);
    let done = quit::attempt(
        &session,
        &QuitOptions {
            force: true,
            graceful_timeout_ms: 1,
        },
        None,
        vec![],
    )
    .await;
    debugger.join().unwrap();
    assert_eq!(done.outcome, QuitOutcome::Killed, "{done:?}");
    assert_eq!(
        done.steps
            .iter()
            .filter(|s| matches!(s, QuitStep::Ended { .. }))
            .count(),
        1,
        "{done:?}"
    );
    assert!(done.end_errors.is_empty(), "{done:?}");
    assert!(quit::report(&session, &done).left.is_empty());
    assert_eq!(
        procs::liveness(unrelated.0.id(), None),
        procs::Liveness::Alive
    );
}

#[tokio::test]
async fn pending_termination_times_out_honestly_without_an_access_denied() {
    let (pid, debugger) = delayed_exit(quit::TERMINATION_WAIT + Duration::from_secs(5));
    let session = session(pid);
    let done = quit::attempt(
        &session,
        &QuitOptions {
            force: true,
            graceful_timeout_ms: 1,
        },
        None,
        vec![],
    )
    .await;
    let report = quit::report(&session, &done);
    // Always release the debuggee before asserting, including on test failure.
    debugger.join().unwrap();
    assert_eq!(done.outcome, QuitOutcome::TimedOut, "{done:?}");
    assert!(done.end_errors.is_empty(), "{done:?}");
    assert_eq!(done.terminated, vec![pid]);
    assert_eq!(report.left.len(), 1, "{report:?}");
    assert_eq!(report.left[0].end_error.as_deref(), Some(quit::TERMINATING));
}

#[test]
fn an_exited_process_held_open_is_gone_and_omitted_from_the_report() {
    let mut child = spawn(false);
    let session = session(child.0.id());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(osproc::facts(child.0.id()).running, Some(false));
    assert_eq!(
        procs::liveness(child.0.id(), session.started_at),
        procs::Liveness::Gone
    );
    assert!(
        quit::report(
            &session,
            &quit::QuitAttempt::new(QuitOutcome::TimedOut, vec![])
        )
        .left
        .is_empty()
    );
}

#[test]
fn unknown_and_reused_identity_never_terminate_a_live_process() {
    let mut child = spawn(false);
    let actual = osproc::facts(child.0.id()).created.unwrap();
    let snapshot = procs::snapshot_os(procs::ReadOpts::default());
    assert_eq!(
        snapshot
            .iter()
            .find(|e| e.pid == child.0.id())
            .unwrap()
            .started_at,
        Some(actual),
        "Windows keeps the native creation time, without second rounding"
    );
    assert!(matches!(
        osproc::end(child.0.id(), None, Duration::ZERO),
        osproc::EndResult::Failed {
            stage: "identity",
            ..
        }
    ));
    for elapsed in [1, 1_000, 60_000] {
        assert_eq!(
            osproc::end(
                child.0.id(),
                Some(superba_core::Timestamp(actual.0 - elapsed)),
                Duration::ZERO
            ),
            osproc::EndResult::Reused
        );
    }
    assert!(child.0.try_wait().unwrap().is_none());
}

#[test]
fn a_real_access_denied_is_preserved_and_the_process_is_left_running() {
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, SetKernelObjectSecurity};
    let mut child = spawn(false);
    let session = session(child.0.id());
    let sddl: Vec<u16> = "D:(D;;0x1;;;WD)(A;;GA;;;WD)\0".encode_utf16().collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: NUL-terminated SDDL and valid out pointer; the descriptor is freed
    // after applying it only to this test's own child. The retained Child handle
    // still has termination rights for RAII cleanup.
    unsafe {
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut()
            ),
            0
        );
        let ok = SetKernelObjectSecurity(
            child.0.as_raw_handle(),
            DACL_SECURITY_INFORMATION,
            descriptor,
        );
        LocalFree(descriptor);
        assert_ne!(ok, 0);
    }
    // An elevated runner (GitHub's Windows images) holds SeDebugPrivilege
    // enabled, which bypasses the deny ACE and turns this into an accepted
    // kill. Drop it (on this thread) so the open meets the DACL as an
    // ordinary user's would.
    let result = {
        let _plain = WithoutDebugPrivilege::new();
        osproc::end(child.0.id(), session.started_at, Duration::ZERO)
    };
    let osproc::EndResult::Failed { stage, error } = result else {
        panic!("{result:?}")
    };
    assert_eq!(stage, "open");
    assert!(error.contains("(os error 5)"), "{error}");
    let mut attempt = quit::QuitAttempt::new(QuitOutcome::TimedOut, vec![]);
    attempt
        .end_errors
        .insert(child.0.id(), format!("{stage}: {error}"));
    let report = quit::report(&session, &attempt);
    assert_eq!(report.left.len(), 1);
    assert!(
        report.left[0]
            .end_error
            .as_ref()
            .unwrap()
            .contains("(os error 5)")
    );
    assert!(child.0.try_wait().unwrap().is_none());
}

/// A disposable runner starts this same test executable's fixture as SYSTEM
/// and supplies its PID. Even an elevated caller with termination rights must
/// leave it alone. The runner owns and cleans up that fixture.
#[test]
#[ignore = "a disposable Windows runner with a separate-account fixture"]
fn another_users_fixture_is_excluded_even_when_elevated() {
    let pid = std::env::var("SUPERBA_QUIT_TEST_FOREIGN_PID")
        .expect("runner must supply its separate-account fixture PID")
        .parse::<u32>()
        .unwrap();
    let facts = osproc::facts(pid);
    assert_eq!(facts.image, Some(std::env::current_exe().unwrap()));
    assert_eq!(facts.running, Some(true));
    let entries = procs::snapshot_os(procs::ReadOpts::default());
    let entry = entries.iter().find(|e| e.pid == pid).unwrap();
    assert!(entry.foreign_user, "{entry:?}");
    assert!(matches!(
        osproc::end(pid, facts.created, Duration::ZERO),
        osproc::EndResult::Failed { stage: "identity", ref error }
            if error.contains("another Windows user")
    ));
    assert_eq!(osproc::facts(pid).running, Some(true));
}
