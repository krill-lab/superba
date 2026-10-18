//! DesktopAppController against a fake CDP endpoint served by the test process.

mod common;

use std::sync::Arc;

use serde_json::{Value, json};
use superba_appctl::DesktopAppController;
use superba_core::{
    AppController, AppSession, BridgeStatus, CdpEndpoint, CodexHome, HotRestartRequest,
    LocalContext, ThreadRef,
};

fn session(port: Option<u16>) -> AppSession {
    AppSession {
        app_server_exe: None,
        install_id: None,
        main_pid: std::process::id(),
        started_at: None,
        executable: None,
        cdp: port.map(|port| CdpEndpoint {
            port,
            owner_verified: true,
        }),
        launched_by_us: true,
        authapi_env: Default::default(),
        home_override: None,
        codex_home: None,
    }
}

/// A loopback port nothing listens on, held until the returned socket is
/// dropped: bound but never listening, so a connection is refused and no other
/// socket can take the port meanwhile (a parallel test's fake renderer, an
/// outgoing connection). Picking a free port and letting it go raced with
/// those: the "dead" port could answer.
fn dead_port() -> (tokio::net::TcpSocket, u16) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket
        .bind(std::net::SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            0,
        )))
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    (socket, port)
}

fn controller(tmp: &tempfile::TempDir) -> DesktopAppController {
    DesktopAppController::new(LocalContext::for_tests(tmp.path()))
}

fn probe_value() -> Value {
    json!({
        "url": "app://-/index.html",
        "conversation": "01a143a6-f4e0-7ef2-93b7-ce25598382d2",
        "stop": true,
        "composer": true,
        "login": false,
        "rows": [
            {"id": "local:client-new-thread:e2", "host": "local", "title": "Count", "selected": true, "busy": true},
            {"id": "local:0199aaaa-0000-7000-8000-000000000001", "host": "local", "title": "Other", "selected": false, "busy": false},
            {"id": "devbox:abc", "host": "devbox-1", "title": "Remote work", "selected": false, "busy": true}
        ]
    })
}

fn responder() -> common::Responder {
    Arc::new(|expr: &str| {
        if expr.contains("codex-app-server-restart") {
            Value::Bool(true)
        } else if expr.contains("dispatchEvent") {
            json!("route")
        } else if expr.contains("data-app-action-sidebar-thread-row") {
            probe_value()
        } else if expr == superba_core::codex::BRIDGE_PROBE_JS {
            Value::Bool(true)
        } else {
            Value::Null
        }
    })
}

#[tokio::test]
async fn probe_bridge_states() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    assert_eq!(
        c.probe_bridge(&session(None)).await.unwrap(),
        BridgeStatus::NoCdp
    );

    let fake = common::serve(
        vec![
            "app://-/index.html?initialRoute=/avatar-overlay",
            "app://-/index.html",
        ],
        responder(),
    )
    .await;
    let st = c.probe_bridge(&session(Some(fake.port))).await.unwrap();
    assert_eq!(
        st,
        BridgeStatus::Ready {
            target_url: "app://-/index.html".into()
        }
    );

    // Only excluded targets -> no renderer target.
    let fake2 = common::serve(
        vec!["app://-/index.html?initialRoute=/avatar-overlay"],
        responder(),
    )
    .await;
    let st = c.probe_bridge(&session(Some(fake2.port))).await.unwrap();
    assert_eq!(st, BridgeStatus::NoRendererTarget);

    // Bridge function missing.
    let fake3 = common::serve(vec!["app://-/index.html"], Arc::new(|_| Value::Bool(false))).await;
    let st = c.probe_bridge(&session(Some(fake3.port))).await.unwrap();
    assert_eq!(st, BridgeStatus::BridgeMissing);
}

#[tokio::test]
async fn probe_bridge_unreachable_port() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let (_held, port) = dead_port();
    let st = c.probe_bridge(&session(Some(port))).await.unwrap();
    assert!(matches!(st, BridgeStatus::CdpUnreachable { .. }), "{st:?}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn probe_bridge_owner_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let fake = common::serve(vec!["app://-/index.html"], responder()).await;
    let mut s = session(Some(fake.port));
    // pid 1 does not hold our listener.
    s.main_pid = 1;
    let st = c.probe_bridge(&s).await.unwrap();
    assert_eq!(st, BridgeStatus::OwnerMismatch);
}

#[tokio::test]
async fn hot_restart_sends_ipc() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let fake = common::serve(vec!["app://-/index.html"], responder()).await;
    let s = session(Some(fake.port));
    let out = c
        .hot_restart(&s, &HotRestartRequest::local())
        .await
        .unwrap();
    assert!(out.sent_at.as_millis() > 0);
    let out = c
        .hot_restart(&s, &HotRestartRequest::remote("devbox-1"))
        .await
        .unwrap();
    assert!(out.sent_at.as_millis() > 0);
    let ev = fake.evaluated.lock().unwrap().clone();
    let restarts: Vec<&String> = ev
        .iter()
        .filter(|e| e.contains("codex-app-server-restart"))
        .collect();
    assert_eq!(restarts.len(), 2);
    assert!(restarts[0].contains("hostId:\"local\""));
    assert!(!restarts[0].contains("killCodexProcess"));
    assert!(restarts[1].contains("hostId:\"devbox-1\""));
    assert!(restarts[1].contains("killCodexProcess:true"));
}

#[tokio::test]
async fn hot_restart_refused_and_no_cdp() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let err = c
        .hot_restart(&session(None), &HotRestartRequest::local())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cdp");
    let fake = common::serve(vec!["app://-/index.html"], Arc::new(|_| Value::Bool(false))).await;
    let err = c
        .hot_restart(&session(Some(fake.port)), &HotRestartRequest::local())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cdp");
}

#[tokio::test]
async fn renderer_state_and_threads() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let fake = common::serve(vec!["app://-/index.html"], responder()).await;
    let s = session(Some(fake.port));
    let st = c.renderer_state(&s).await.unwrap();
    assert!(!st.on_login_page);
    assert!(!st.on_home);
    let cur = st.current_thread.unwrap();
    assert_eq!(cur.thread_id, "01a143a6-f4e0-7ef2-93b7-ce25598382d2");
    assert_eq!(cur.title.as_deref(), Some("Count"));
    let cur2 = c.current_thread(&s).await.unwrap().unwrap();
    assert_eq!(cur2, cur);

    c.return_to_thread(&s, &cur).await.unwrap();
    let bad = ThreadRef {
        thread_id: "x'); alert(1); ('".into(),
        title: None,
    };
    assert_eq!(
        c.return_to_thread(&s, &bad).await.unwrap_err().code(),
        "invalid"
    );

    let remotes = c.remote_connections(&s).await.unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].host_id.as_deref(), Some("devbox-1"));
    assert_eq!(remotes[0].target_id.as_str(), "ssh:devbox-1");
}

#[tokio::test]
async fn return_to_thread_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let fake = common::serve(vec!["app://-/index.html"], Arc::new(|_| json!("failed"))).await;
    let s = session(Some(fake.port));
    let t = ThreadRef {
        thread_id: "0199aaaa-0000-7000-8000-000000000001".into(),
        title: Some("Other".into()),
    };
    assert_eq!(c.return_to_thread(&s, &t).await.unwrap_err().code(), "cdp");
}

#[tokio::test]
async fn turn_activity_merges_renderer_and_rollouts() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let home = CodexHome::new(tmp.path().join("codex"));
    // No rollouts and no session: idle and complete.
    let a = c.turn_activity(&home, None).await.unwrap();
    assert!(a.complete);
    assert!(!a.is_busy());

    let fake = common::serve(vec!["app://-/index.html"], responder()).await;
    let s = session(Some(fake.port));
    let a = c.turn_activity(&home, Some(&s)).await.unwrap();
    assert!(a.complete);
    let ids: Vec<_> = a
        .busy
        .iter()
        .map(|t| (t.target_id.as_str().to_owned(), t.thread_id.clone()))
        .collect();
    assert!(ids.contains(&(
        "local".into(),
        Some("01a143a6-f4e0-7ef2-93b7-ce25598382d2".into())
    )));
    assert!(ids.contains(&("ssh:devbox-1".into(), Some("abc".into()))));
    // The stop button and the selected busy row describe the same thread.
    assert_eq!(a.busy.len(), 2);

    // Renderer unreachable -> incomplete.
    let (_held, port) = dead_port();
    let a = c
        .turn_activity(&home, Some(&session(Some(port))))
        .await
        .unwrap();
    assert!(!a.complete);
}

#[tokio::test]
async fn javascript_exception_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let fake = common::serve(vec!["app://-/index.html"], Arc::new(|_| json!("__throw__"))).await;
    let err = c
        .renderer_state(&session(Some(fake.port)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "cdp");
    assert!(err.to_string().contains("boom"), "{err}");
}

#[tokio::test]
async fn processes_only_for_home() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let home = CodexHome::new(tmp.path().join("nobody-uses-this-home"));
    let ps = c.processes(&home).await.unwrap();
    for p in ps {
        assert!(
            p.codex_home.is_none(),
            "{:?} has codex home {:?}",
            p.pid,
            p.codex_home
        );
    }
}

#[tokio::test]
async fn quit_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let mut s = session(None);
    // A pid that cannot exist on Linux (above pid_max default) and a start time.
    s.main_pid = u32::MAX - 7;
    let out = c.quit(&s, &Default::default()).await.unwrap();
    assert_eq!(out, superba_core::QuitOutcome::NotRunning);
}

#[cfg(unix)]
#[tokio::test]
async fn quit_terminates_a_process_we_started() {
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    // Reap in the background so the exit is observable (no zombie).
    let reaper = std::thread::spawn(move || child.wait());
    let mut s = session(None);
    s.main_pid = pid;
    let out = c
        .quit(
            &s,
            &superba_core::QuitOptions {
                graceful_timeout_ms: 5_000,
                force: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(out, superba_core::QuitOutcome::Exited);
    let _ = reaper.join();
}

#[tokio::test]
async fn running_session_none_without_app() {
    // The test environment never runs the desktop app under the temp home; any
    // session found must not claim our temp Codex home.
    let tmp = tempfile::tempdir().unwrap();
    let c = controller(&tmp);
    if let Some(s) = c.running_session().await.unwrap() {
        assert!(!s.launched_by_us);
    }
}
