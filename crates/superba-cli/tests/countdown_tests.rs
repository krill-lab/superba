#![cfg(unix)]

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use clap::Parser;
use common::*;
use superba_cli::{Capture, Cli, Output};
use superba_core::{Locale, RestartKind};

struct CountdownWriter {
    capture: Capture,
    ready: PathBuf,
}

impl Write for CountdownWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        // The first stderr write is the countdown banner. Its handler must
        // already be installed so an immediate signal is safely cancelled.
        std::fs::write(&self.ready, [])?;
        self.capture.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.capture.flush()
    }
}

#[tokio::test]
async fn ctrl_c_cancels_countdown_without_executing() {
    const CHILD: &str = "SUPERBA_CLI_TEST_COUNTDOWN_CHILD";
    if let Some(directory) = std::env::var_os(CHILD) {
        let directory = PathBuf::from(directory);
        let service = FakeService::new();
        service.recovery.lock().unwrap().entries.clear();
        service.script(|request| {
            let mut plan = plan(request);
            plan.primary = RestartKind::AppServerHot {
                host_id: "local".into(),
                kill_remote: false,
            };
            Ok(plan)
        });
        let stdout = Capture::default();
        let stderr = Capture::default();
        let mut out = Output::new(
            false,
            Locale::En,
            Box::new(stdout.clone()),
            Box::new(CountdownWriter {
                capture: stderr.clone(),
                ready: directory.join("ready"),
            }),
        );
        let cli = Cli::try_parse_from(["spb", "switch", "--countdown", "30"]).unwrap();
        let code = superba_cli::run(&cli, &runtime(service.clone()), &mut out).await;
        assert_eq!(code, 5);
        assert!(service.calls().executions.is_empty());
        assert!(stderr.contents().to_lowercase().contains("cancel"));
        assert!(!stdout.contents().contains("Switched"));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "ctrl_c_cancels_countdown_without_executing"])
        .env(CHILD, directory.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Signal only the fake-service child owned by this test, never another process.
    let pid = child.id().unwrap().to_string();
    assert!(
        tokio::process::Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .await
            .unwrap()
            .success()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn countdown_prints_warnings_once_and_redraws_only_on_a_terminal() {
    for terminal in [false, true] {
        let service = FakeService::new();
        service.recovery.lock().unwrap().entries.clear();
        service.script(|request| {
            let mut plan = plan(request);
            plan.primary = RestartKind::AppServerHot {
                host_id: "local".into(),
                kill_remote: false,
            };
            Ok(plan)
        });
        let (out, _, stderr) = Output::capture();
        let mut out = out.with_terminals(terminal, terminal);
        let cli = Cli::try_parse_from(["spb", "switch", "--countdown", "2"]).unwrap();
        assert_eq!(
            superba_cli::run(&cli, &runtime(service.clone()), &mut out).await,
            0
        );
        assert_eq!(service.calls().executions.len(), 1);
        let stderr = stderr.contents();
        assert_eq!(
            stderr.matches("The Codex core restarts in place").count(),
            1
        );
        assert_eq!(stderr.matches("MCP servers restart").count(), 1);
        assert_eq!(stderr.matches("Ctrl-C to cancel").count(), 2);
        assert!(stderr.contains("in 2 s") && stderr.contains("in 1 s"));
        assert_eq!(stderr.matches('\r').count(), if terminal { 2 } else { 0 });
        assert_eq!(
            stderr.matches("\x1b[K").count(),
            if terminal { 2 } else { 0 }
        );
        assert_eq!(stderr.lines().count(), if terminal { 5 } else { 6 });
        assert!(stderr.ends_with('\n'));
    }
}
