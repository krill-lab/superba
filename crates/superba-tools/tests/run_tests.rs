//! A failed exec may be retried only while Linux reports a busy executable.

#![cfg(target_os = "linux")]

mod support;

use std::future::{Future as _, poll_fn};
use std::task::Poll;
use std::time::Duration;

use superba_core::{Error, SharedToolProgress, ToolId, ToolStage};
use superba_tools::log::OpLog;
use superba_tools::run::{self, Cmd};

#[tokio::test]
async fn other_spawn_errors_return_without_waiting() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let denied = root.path().join("denied");
    support::write(&denied, b"#!/bin/sh\nexit 0\n");
    for (path, message) in [(missing, "No such file"), (denied, "Permission denied")] {
        let cmd = Cmd::new(&path);
        let output = run::output(&cmd);
        tokio::pin!(output);
        let err = poll_fn(|cx| match output.as_mut().poll(cx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => panic!("a non-ETXTBSY error must not wait for a retry"),
        })
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Process { .. }), "{err:?}");
        assert!(err.to_string().contains(message), "{err}");
        assert!(err.to_string().contains(&*path.to_string_lossy()), "{err}");
    }
}

#[tokio::test]
async fn a_persistent_writer_is_still_an_error() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("busy");
    support::script(&path, "exit 0");
    let _writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let cmd = Cmd::new(&path);
    let err = tokio::time::timeout(Duration::from_secs(2), run::output(&cmd))
        .await
        .expect("retries must be bounded")
        .unwrap_err();
    assert!(matches!(err, Error::Process { .. }), "{err:?}");
    assert!(err.to_string().contains("Text file busy"), "{err}");
    assert!(err.to_string().contains(&*path.to_string_lossy()), "{err}");
}

#[tokio::test]
async fn a_busy_executable_obeys_the_command_deadline() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("busy");
    support::script(&path, "exit 0");
    let _writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let cmd = Cmd::new(&path).timeout(Duration::ZERO);
    let err = run::output(&cmd).await.unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
}

#[tokio::test]
async fn cancelling_a_busy_spawn_does_not_start_it_when_the_writer_closes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("busy");
    support::script(&path, "exit 0");
    let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let sink = SharedToolProgress::default();
    let cmd = Cmd::new(&path);
    let log = OpLog::none();
    let output = run::run(&cmd, &log, ToolId::ClaudeCode, ToolStage::Installing, &sink);
    tokio::pin!(output);
    poll_fn(|cx| {
        assert!(output.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    sink.cancel();
    drop(writer);
    let err = output.await.unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

#[tokio::test]
async fn a_started_program_is_never_retried_on_a_failed_exit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fails");
    support::script(&path, "echo started >> attempts; echo failure >&2; exit 26");
    let output = run::output(&Cmd::new(path).cwd(root.path())).await.unwrap();
    assert_eq!(output.code, Some(26));
    assert_eq!(output.stderr.trim(), "failure");
    assert_eq!(
        std::fs::read_to_string(root.path().join("attempts")).unwrap(),
        "started\n"
    );
}
