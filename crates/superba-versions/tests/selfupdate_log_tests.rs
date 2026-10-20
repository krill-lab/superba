//! The self-update log and the record of the last attempt (user bug 44): the
//! log of each attempt with rotation, the second stage (`--finish-update`)
//! logging every step against a scripted system (waiting for the old process,
//! `msiexec` and its exit code, the router restart, the relaunch), the outcome
//! it records, and the problem the next start shows.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use superba_core::{Catalog, DataDir, Locale, NetworkSettings, Timestamp};
use superba_versions::selfupdate::SelfUpdater;
use superba_versions::selfupdate::finish::{FinishArgs, FinishHost, Ran, run_with};
use superba_versions::selfupdate::record::{self, Attempt, Stage};
use superba_versions::selfupdate::runlog::{
    KEEP_PREVIOUS, MSIEXEC_KNOWN_CODES, RunLog, msiexec_meaning,
};
use superba_versions::selfupdate::state::State;

/// A program run: path, arguments, extra environment.
type Run = (PathBuf, Vec<String>, Vec<(String, String)>);

/// A scripted system: the old process lives for `alive_polls` looks, every
/// pause advances the clock by 100 ms, programs answer from `results` in
/// order, and every run and start is recorded.
#[derive(Default)]
struct Script {
    alive_polls: Cell<u32>,
    clock: Cell<Duration>,
    results: RefCell<VecDeque<Result<Ran, String>>>,
    runs: RefCell<Vec<Run>>,
    starts: RefCell<Vec<(PathBuf, Vec<String>)>>,
    start_fails: bool,
}

impl Script {
    fn answers(answers: impl IntoIterator<Item = Result<Ran, String>>) -> Self {
        Self {
            results: RefCell::new(answers.into_iter().collect()),
            ..Self::default()
        }
    }
}

fn ran(code: i32, output: &str) -> Result<Ran, String> {
    Ok(Ran {
        code: Some(code),
        output: output.into(),
    })
}

fn strings(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

impl FinishHost for Script {
    fn alive(&self, _pid: u32) -> bool {
        let left = self.alive_polls.get();
        self.alive_polls.set(left.saturating_sub(1));
        left > 0
    }
    fn pause(&self) {
        self.clock
            .set(self.clock.get() + Duration::from_millis(100));
    }
    fn elapsed(&self) -> Duration {
        self.clock.get()
    }
    fn run(&self, program: &Path, args: &[OsString], env: &[(&str, &str)]) -> std::io::Result<Ran> {
        self.runs.borrow_mut().push((
            program.to_owned(),
            strings(args),
            env.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ));
        self.results
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| ran(0, ""))
            .map_err(std::io::Error::other)
    }
    fn start(&self, program: &Path, args: &[OsString]) -> std::io::Result<u32> {
        self.starts
            .borrow_mut()
            .push((program.to_owned(), strings(args)));
        if self.start_fails {
            Err(std::io::Error::other("no such file"))
        } else {
            Ok(777)
        }
    }
}

/// A data directory with an attempt from 0.1.0 to 0.2.0 handed off to the
/// second stage, and the second stage's arguments for a Windows MSI install.
struct World {
    tmp: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let w = Self {
            tmp: tempfile::tempdir().unwrap(),
        };
        let log = RunLog::begin(w.log(), "Superba 0.1.0 -> 0.2.0 (v0.2.0)");
        log.line("the second stage installs the MSI once this process has exited");
        record::start(
            &w.state(),
            Attempt {
                stage: Stage::Handoff,
                ..Attempt::new("0.1.0", "0.2.0", &w.log(), Timestamp::now())
            },
        );
        w
    }

    fn data(&self) -> DataDir {
        DataDir::new(self.tmp.path())
    }

    fn log(&self) -> PathBuf {
        RunLog::path_in(&self.data())
    }

    fn state(&self) -> PathBuf {
        self.data().selfupdate_file()
    }

    fn install_dir(&self) -> PathBuf {
        self.tmp.path().join("Programs").join("Superba")
    }

    fn args(&self) -> FinishArgs {
        FinishArgs {
            wait_pid: Some(4242),
            msi: Some(
                self.tmp
                    .path()
                    .join("downloads/superba-0.2.0-windows-x86_64.msi"),
            ),
            restart_router: true,
            relaunch: Some(self.install_dir().join("superba.exe")),
            open_app: None,
            log: Some(self.log()),
            state: Some(self.state()),
        }
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap()
    }

    fn attempt(&self) -> Attempt {
        State::load(&self.state()).last_attempt.expect("an attempt")
    }

    fn updater(&self, current: &str) -> SelfUpdater {
        SelfUpdater::new(&self.data(), NetworkSettings::default()).with_current(current)
    }
}

fn assert_in_order(text: &str, parts: &[&str]) {
    let mut at = 0;
    for p in parts {
        match text[at..].find(p) {
            Some(i) => at += i + p.len(),
            None => panic!("{p:?} missing (or out of order) in:\n{text}"),
        }
    }
}

#[test]
fn a_good_msi_install_logs_every_step_and_records_done() {
    let w = World::new();
    let host = Script {
        alive_polls: Cell::new(3),
        ..Script::answers([ran(0, ""), ran(0, "{\"restarted\":true}")])
    };
    run_with(&w.args(), &host, Duration::from_secs(60)).unwrap();

    let log = w.log_text();
    assert_in_order(
        &log,
        &[
            "Superba 0.1.0 -> 0.2.0",
            "[finish ",
            "second stage started: superba --finish-update --msi",
            "--wait-pid 4242 --restart-router",
            "waiting for the old Superba (process 4242) to exit (at most 60 s)",
            "process 4242 has exited (after 0.3 s)",
            "running the installer: ",
            "msiexec",
            " /i ",
            "superba-0.2.0-windows-x86_64.msi /passive /L*V ",
            "selfupdate-msiexec.log",
            "msiexec exit code 0: success",
            "restarting the router: ",
            "spb",
            "self-update restart-router",
            "router restart: exit code 0 {\"restarted\":true}",
            "the update is done",
            "started Superba: ",
            "superba.exe (process 777)",
        ],
    );
    assert!(!log.contains("FAILED"), "{log}");

    let runs = host.runs.borrow();
    assert_eq!(runs.len(), 2);
    assert!(runs[0].0.to_string_lossy().contains("msiexec"));
    assert_eq!(
        runs[0].1[..3],
        [
            "/i".to_owned(),
            w.args().msi.unwrap().display().to_string(),
            "/passive".to_owned()
        ]
    );
    assert_eq!(runs[0].1[3], "/L*V");
    assert_eq!(runs[1].1, ["self-update", "restart-router"]);
    assert_eq!(
        runs[1].2,
        [("SUPERBA_FORCE_ROUTER_RESTART".to_owned(), "1".to_owned())]
    );
    assert_eq!(
        host.starts.borrow()[0].0,
        w.install_dir().join("superba.exe")
    );

    let attempt = w.attempt();
    assert_eq!(attempt.stage, Stage::Done);
    assert_eq!(attempt.problem, None);
    // The new version starts: nothing to say.
    assert_eq!(w.updater("0.2.0").view().problem, None);
}

#[test]
fn an_old_process_that_does_not_exit_fails_the_update_before_anything_runs() {
    let w = World::new();
    let host = Script {
        alive_polls: Cell::new(u32::MAX),
        ..Script::default()
    };
    let e = run_with(&w.args(), &host, Duration::from_secs(1)).unwrap_err();
    assert!(e.to_string().contains("4242"), "{e}");
    assert!(host.runs.borrow().is_empty(), "no msiexec, no router");
    assert!(host.starts.borrow().is_empty(), "the old one still runs");
    assert_in_order(
        &w.log_text(),
        &[
            "waiting for the old Superba (process 4242) to exit (at most 1 s)",
            "FAILED: process 4242 is still running after 1 s; nothing was installed",
        ],
    );
    let attempt = w.attempt();
    assert_eq!(attempt.stage, Stage::Failed);
    let reason = attempt.problem.unwrap();
    assert_eq!(reason.key, "selfupdate.problem.wait_timeout");
    let zh = Catalog::new(Locale::ZhCn).text(&reason);
    assert_eq!(zh, "Superba 未能在 1 秒内退出，所以没有安装 0.2.0。");
}

#[test]
fn a_failed_msiexec_is_recorded_with_its_meaning_and_the_old_version_restarts() {
    let w = World::new();
    let host = Script::answers([ran(1603, ""), ran(0, "")]);
    run_with(&w.args(), &host, Duration::from_secs(60)).unwrap();
    assert_in_order(
        &w.log_text(),
        &[
            "msiexec exit code 1603: a fatal error occurred during the installation",
            "restarting the router",
            "the update FAILED; the installed version was left as it was",
            "started Superba",
        ],
    );
    // The router the MSI stopped and the (old) Superba come back regardless.
    assert_eq!(host.runs.borrow().len(), 2);
    assert_eq!(host.starts.borrow().len(), 1);

    let attempt = w.attempt();
    assert_eq!(attempt.stage, Stage::Failed);
    assert!(!attempt.seen);

    // The next start (still 0.1.0) gets the problem, once.
    let u = w.updater("0.1.0");
    let problem = u.view().problem.expect("a problem");
    assert!(!problem.installed);
    assert!(!problem.seen);
    assert_eq!(problem.version, "0.2.0");
    assert_eq!(problem.log, w.log());
    let en = Catalog::new(Locale::En).text(&problem.reason);
    assert_eq!(
        en,
        "The installer (msiexec) ended with code 1603: a fatal error occurred during the installation"
    );
    let zh = Catalog::new(Locale::ZhCn).text(&problem.reason);
    assert_eq!(
        zh,
        "安装程序（msiexec）以代码 1603 结束：安装过程中发生严重错误"
    );
    let seen = u.acknowledge().problem.expect("still on the Updates page");
    assert!(seen.seen);
    assert!(u.view().problem.unwrap().seen);
}

#[test]
fn an_installer_that_cannot_start_fails_the_update() {
    let w = World::new();
    let host = Script::answers([Err("program not found".into()), ran(0, "")]);
    run_with(&w.args(), &host, Duration::from_secs(60)).unwrap();
    assert!(
        w.log_text()
            .contains("FAILED: msiexec could not be started: program not found")
    );
    let reason = w.attempt().problem.unwrap();
    assert_eq!(reason.key, "selfupdate.problem.msiexec_start");
    assert_eq!(
        Catalog::new(Locale::En).text(&reason),
        "The installer (msiexec) could not be started: program not found"
    );
}

#[test]
fn installed_but_the_router_did_not_restart_is_a_problem_of_an_installed_update() {
    let w = World::new();
    let host = Script::answers([
        ran(0, ""),
        ran(1, "warning: x\nerror: the router did not answer\n"),
    ]);
    run_with(&w.args(), &host, Duration::from_secs(60)).unwrap();
    assert!(w.log_text().contains(
        "FAILED: router restart ended with exit code 1: warning: x\n    error: the router did not answer"
    ));
    let attempt = w.attempt();
    assert_eq!(attempt.stage, Stage::Done);
    let problem = w.updater("0.2.0").view().problem.expect("a problem");
    assert!(problem.installed);
    assert_eq!(
        Catalog::new(Locale::En).text(&problem.reason),
        "The update was installed, but the local router could not be restarted (error: the router did not answer). Start it again on the Router page."
    );
}

#[test]
fn a_restart_of_windows_pending_is_reported_after_a_successful_install() {
    for code in [3010, 1641] {
        let w = World::new();
        let host = Script::answers([ran(code, ""), ran(0, "")]);
        run_with(&w.args(), &host, Duration::from_secs(60)).unwrap();
        let attempt = w.attempt();
        assert_eq!(attempt.stage, Stage::Done, "{code}");
        assert_eq!(
            attempt.problem.unwrap().key,
            "selfupdate.problem.restart_pending"
        );
        assert!(
            w.log_text()
                .contains(&format!("msiexec exit code {code}: success;"))
        );
    }
}

#[test]
fn a_relaunch_that_fails_is_logged() {
    let w = World::new();
    let host = Script {
        start_fails: true,
        ..Script::answers([ran(0, ""), ran(0, "")])
    };
    run_with(&w.args(), &host, Duration::from_secs(60)).unwrap();
    assert!(w.log_text().contains("FAILED: could not start Superba ("));
    assert!(w.log_text().contains("): no such file"));
    // The update itself is in place: the next start (by hand) is the new version.
    assert_eq!(w.attempt().stage, Stage::Done);
}

#[test]
fn a_unix_second_stage_only_waits_and_relaunches_and_keeps_an_earlier_problem() {
    let w = World::new();
    record::update(&w.state(), Timestamp::now(), |a| {
        a.stage = Stage::Installed;
        a.problem = Some(record::router_reason("connection refused"));
    });
    let args = FinishArgs {
        msi: None,
        restart_router: false,
        relaunch: None,
        open_app: Some("/Applications/Superba.app".into()),
        ..w.args()
    };
    let host = Script::default();
    run_with(&args, &host, Duration::from_secs(60)).unwrap();
    assert!(host.runs.borrow().is_empty());
    assert_eq!(
        *host.starts.borrow(),
        [(
            PathBuf::from("open"),
            vec!["/Applications/Superba.app".to_owned()]
        )]
    );
    assert!(
        w.log_text()
            .contains("started Superba: open /Applications/Superba.app (process 777)")
    );
    let attempt = w.attempt();
    assert_eq!(attempt.stage, Stage::Done);
    assert_eq!(attempt.problem.unwrap().key, "selfupdate.problem.router");
}

#[test]
fn the_next_start_tells_when_the_old_version_still_runs_or_the_attempt_never_finished() {
    let w = World::new();
    record::update(&w.state(), Timestamp::now(), |a| a.stage = Stage::Done);
    let p = w.updater("0.1.0").view().problem.expect("not applied");
    assert!(!p.installed);
    assert_eq!(
        Catalog::new(Locale::En).text(&p.reason),
        "The installer reported success, but this is still Superba 0.1.0."
    );
    // A newer version than the target (installed by hand later): fine.
    assert_eq!(w.updater("0.3.0").view().problem, None);

    // Handed off but never finished: quiet for a while, then reported.
    record::update(&w.state(), Timestamp::now(), |a| a.stage = Stage::Handoff);
    let now = Timestamp::now();
    assert_eq!(w.updater("0.1.0").with_now(now).view().problem, None);
    let later = now.saturating_add(record::STALE);
    let p = w
        .updater("0.1.0")
        .with_now(later)
        .view()
        .problem
        .expect("unfinished");
    assert_eq!(p.reason.key, "selfupdate.problem.unfinished");
    // Installed in place and waiting for a restart, or left for a manual
    // install: nothing to report.
    for stage in [Stage::Installed, Stage::Manual] {
        record::update(&w.state(), Timestamp::now(), |a| a.stage = stage);
        assert_eq!(w.updater("0.1.0").with_now(later).view().problem, None);
    }
}

#[test]
fn each_attempt_starts_a_new_log_and_keeps_the_previous_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("logs").join("selfupdate.log");
    for n in 0..(KEEP_PREVIOUS + 3) {
        let log = RunLog::begin(&path, &format!("attempt {n}"));
        log.line("step\nwith a second line");
    }
    let last = KEEP_PREVIOUS + 2;
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(&format!("attempt {last}")), "{text}");
    assert!(text.contains("] step\n    with a second line\n"), "{text}");
    let pid = std::process::id();
    assert!(
        text.contains(&format!(" [update {pid}] attempt {last}")),
        "{text}"
    );
    for k in 1..=KEEP_PREVIOUS {
        let old = std::fs::read_to_string(RunLog::previous(&path, k)).unwrap();
        assert!(old.contains(&format!("attempt {}", last - k)), "{k}: {old}");
        assert_eq!(old.matches("attempt").count(), 1, "one attempt per file");
    }
    assert!(!RunLog::previous(&path, KEEP_PREVIOUS + 1).exists());
    assert_eq!(
        RunLog::previous(&path, 2).file_name().unwrap(),
        "selfupdate.2.log"
    );
}

#[test]
fn every_msiexec_code_has_a_meaning_in_both_languages() {
    for code in MSIEXEC_KNOWN_CODES {
        let text = record::msiexec_meaning_text(code);
        assert_eq!(text.key, format!("selfupdate.msiexec.c{code}"));
        for locale in Locale::ALL {
            let catalog = Catalog::new(locale);
            assert!(catalog.get(&text.key).is_some(), "{} in {locale}", text.key);
        }
    }
    assert_eq!(msiexec_meaning(0), ("success", true));
    assert!(!msiexec_meaning(1603).1);
    assert_eq!(msiexec_meaning(4321), ("unknown error", false));
    assert_eq!(
        record::msiexec_meaning_text(4321).key,
        "selfupdate.msiexec.unknown"
    );
    for key in [
        "selfupdate.problem.failed",
        "selfupdate.problem.not_applied",
        "selfupdate.problem.unfinished",
        "selfupdate.problem.error",
        "selfupdate.problem.wait_timeout",
        "selfupdate.problem.msiexec_start",
        "selfupdate.problem.msiexec",
        "selfupdate.problem.restart_pending",
        "selfupdate.problem.router",
    ] {
        for locale in Locale::ALL {
            assert!(Catalog::new(locale).get(key).is_some(), "{key} in {locale}");
        }
    }
}

/// alpha.8 Windows acceptance (D1): the MSI path came from a canonicalized data
/// directory (`\\?\C:\…`) and msiexec refused it with 1619. The installer
/// is handed over in the plain form.
#[test]
fn msiexec_never_gets_a_verbatim_path() {
    let w = World::new();
    let host = Script::answers([ran(0, ""), ran(0, "{}")]);
    let mut args = w.args();
    args.msi = Some(PathBuf::from(
        r"\\?\C:\Users\me\AppData\Local\Superba\downloads\superba-0.2.0-windows-x86_64.msi",
    ));
    let _ = run_with(&args, &host, Duration::from_secs(60));
    let runs = host.runs.borrow();
    let msiexec = &runs[0].1;
    assert_eq!(
        msiexec[1],
        r"C:\Users\me\AppData\Local\Superba\downloads\superba-0.2.0-windows-x86_64.msi"
    );
    assert!(msiexec.iter().all(|a| !a.starts_with(r"\\?\")), "{msiexec:?}");
}
