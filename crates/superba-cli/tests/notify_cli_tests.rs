//! `spb notify` end to end with the real `spb` binary in an isolated home
//! (temp HOME, CODEX_HOME, data dir, file secret backend) against local mock
//! servers (GH#12).
//!
//! The Codex part runs real Codex binaries and is opt-in:
//! `SUPERBA_NOTIFY_CODEX_BINS=/path/codex-0.140.0:/path/codex-0.163.0 cargo
//! test -p superba-cli --test notify_cli_tests` (a mock Responses server
//! stands in for the model; nothing leaves the machine).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// One request a mock server got.
#[derive(Clone, Debug)]
struct Hit {
    target: String,
    body: Value,
}

/// A threaded loopback HTTP/1.1 server: `respond` gives the status, content
/// type and body for each request.
struct Mock {
    base: String,
    hits: Arc<Mutex<Vec<Hit>>>,
}

impl Mock {
    fn start(
        respond: impl Fn(&Hit) -> (u16, &'static str, String) + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
        let recorded = hits.clone();
        let respond = Arc::new(respond);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let recorded = recorded.clone();
                let respond = respond.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut start = String::new();
                    if reader.read_line(&mut start).is_err() {
                        return;
                    }
                    let target = start.split(' ').nth(1).unwrap_or("").to_owned();
                    let mut len = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some((n, v)) = line.split_once(':')
                            && n.eq_ignore_ascii_case("content-length")
                        {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; len];
                    let _ = reader.read_exact(&mut body);
                    let hit = Hit {
                        target,
                        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                    };
                    let (status, kind, text) = respond(&hit);
                    recorded.lock().unwrap().push(hit);
                    let head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        text.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(text.as_bytes());
                });
            }
        });
        Self { base, hits }
    }

    fn sink() -> Self {
        Self::start(|_| (200, "application/json", r#"{"ok":true}"#.into()))
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    fn wait_for(&self, n: usize, timeout: Duration) -> Vec<Hit> {
        let end = Instant::now() + timeout;
        while Instant::now() < end && self.hits().len() < n {
            std::thread::sleep(Duration::from_millis(100));
        }
        self.hits()
    }
}

/// An isolated home for `spb` (and Codex).
struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for d in ["home", "codex", "data", "proj", "bin"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn p(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn env(&self, cmd: &mut Command) {
        cmd.env_clear()
            .env("HOME", self.p("home"))
            .env("USERPROFILE", self.p("home"))
            .env("CODEX_HOME", self.p("codex"))
            .env("SUPERBA_DATA_DIR", self.p("data"))
            .env("SUPERBA_SECRET_BACKEND", "file")
            .env("SUPERBA_LOCALE", "en")
            .env("PATH", format!("{}:/usr/bin:/bin", self.p("bin").display()));
    }

    fn spb(&self, args: &[&str], envs: &[(&str, &str)], stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_spb"));
        self.env(&mut cmd);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let Some(s) = stdin {
            child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        } else {
            drop(child.stdin.take());
        }
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str], envs: &[(&str, &str)]) -> String {
        let out = self.spb(args, envs, None);
        assert!(
            out.status.success(),
            "spb {args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.p("codex/config.toml")).unwrap_or_default()
    }
}

fn add_telegram(home: &Home, sink: &Mock) {
    home.ok(
        &[
            "notify",
            "channels",
            "add",
            "telegram",
            "--name",
            "phone",
            "--param",
            "chat_id=4242",
            "--param",
            &format!("api_base={}", sink.base),
            "--secret-env",
            "bot_token=TG_TOKEN",
        ],
        &[("TG_TOKEN", "123456:NOT-A-REAL-TOKEN")],
    );
    home.ok(&["notify", "set", "--enabled", "on"], &[]);
}

#[test]
fn channels_test_send_hooks_and_secrets() {
    let home = Home::new();
    let sink = Mock::sink();
    add_telegram(&home, &sink);
    // A secret never goes on the command line.
    let refused = home.spb(
        &[
            "notify",
            "channels",
            "add",
            "telegram",
            "--param",
            "bot_token=x",
        ],
        &[],
        None,
    );
    assert_eq!(refused.status.code(), Some(2));

    let out = home.ok(&["notify", "test"], &[]);
    assert!(out.contains("phone: sent"), "{out}");
    let hits = sink.hits();
    assert_eq!(hits[0].target, "/bot123456:NOT-A-REAL-TOKEN/sendMessage");
    assert!(
        hits[0].body["text"]
            .as_str()
            .unwrap()
            .contains("Test message")
    );

    // Claude Code's Stop hook on stdin: nothing on stdout, exit 0.
    let stop = json!({"session_id": "s1", "cwd": "/w/web-app", "hook_event_name": "Stop",
        "stop_hook_active": false, "last_assistant_message": "secret reply"});
    let out = home.spb(
        &["notify", "hook", "claude-code"],
        &[],
        Some(&stop.to_string()),
    );
    assert!(out.status.success() && out.stdout.is_empty(), "{out:?}");
    let hits = sink.wait_for(2, Duration::from_secs(5));
    let text = hits[1].body["text"].as_str().unwrap();
    assert!(
        text.contains("Task finished · Claude Code") && text.contains("Project: web-app"),
        "{text}"
    );
    assert!(!text.contains("secret reply"), "summary is off by default");
    // Broken data never fails the harness.
    let out = home.spb(&["notify", "hook", "claude-code"], &[], Some("not json"));
    assert!(out.status.success() && out.stdout.is_empty());

    // Quiet hours around the clock: a finished task is held back, the test is not.
    home.ok(&["notify", "set", "--quiet-hours", "00:00-23:59"], &[]);
    let stop2 = json!({"hook_event_name": "Stop", "session_id": "s2", "cwd": "/w"});
    home.spb(
        &["notify", "hook", "claude-code"],
        &[],
        Some(&stop2.to_string()),
    );
    let sent = home.ok(&["notify", "send", "--json", "hello from a script"], &[]);
    let report: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(
        report["skipped"],
        Value::Null,
        "plain messages are always sent"
    );
    assert_eq!(sink.wait_for(3, Duration::from_secs(5)).len(), 3);

    // The token is in the key store (0600 file), never in profiles.json.
    let profiles = std::fs::read_to_string(home.p("data/profiles.json")).unwrap();
    assert!(!profiles.contains("NOT-A-REAL-TOKEN"), "{profiles}");
    assert!(profiles.contains("••••OKEN"), "{profiles}");
    let log = std::fs::read_to_string(home.p("data/notify/log.jsonl")).unwrap();
    assert!(!log.contains("NOT-A-REAL-TOKEN") && !log.contains("web-app"));

    // Codex hook: written, chained with the user's program, removed again.
    std::fs::write(
        home.p("codex/config.toml"),
        "model = \"m\"\nnotify = [\"/usr/local/bin/toast\", \"--codex\"]\n",
    )
    .unwrap();
    home.ok(&["notify", "install", "codex"], &[]);
    let spb = env!("CARGO_BIN_EXE_spb");
    assert!(
        home.config().contains(&format!(
            "notify = [\"{spb}\", \"notify\", \"codex\", \"--\", \"/usr/local/bin/toast\", \"--codex\"]"
        )),
        "{}",
        home.config()
    );
    let status: Value =
        serde_json::from_str(&home.ok(&["notify", "status", "--json"], &[])).unwrap();
    assert_eq!(status["hooks"]["hooks"][0]["state"], "installed");
    assert_eq!(status["settings"]["channels"][0]["kind"], "telegram");
    home.ok(&["notify", "uninstall", "codex"], &[]);
    assert!(
        home.config()
            .contains("notify = [\"/usr/local/bin/toast\", \"--codex\"]")
    );
    // The editor's backups hold the file before each change.
    assert!(home.p("data").join("backups").exists());

    // Claude Code: the entries for settings.json.
    let fragment: Value =
        serde_json::from_str(&home.ok(&["notify", "install", "claude-code", "--json"], &[]))
            .unwrap();
    assert_eq!(fragment["hooks"]["Stop"][0]["hooks"][0]["async"], true);

    // Removing the channel removes its secret file.
    let id = status["settings"]["channels"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    home.ok(&["notify", "channels", "remove", &id], &[]);
    let secrets: Vec<_> = std::fs::read_dir(home.p("data/secrets"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("notify"))
        .collect();
    assert!(secrets.is_empty(), "{secrets:?}");
}

// ---------------------------------------------------------------------------
// Real Codex (opt-in)
// ---------------------------------------------------------------------------

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect()
}

/// A Responses API that answers with a message, or (approval mode, first
/// request of a turn) a shell call that needs an approval.
fn mock_model(approval: bool) -> Mock {
    Mock::start(move |hit| {
        let input = hit.body["input"].as_array().cloned().unwrap_or_default();
        let has_output = input.iter().any(|i| i["type"] == "function_call_output");
        let tools: Vec<String> = hit.body["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect();
        let created = json!({"type": "response.created", "response": {"id": "resp_1", "status": "in_progress"}});
        let usage = json!({"input_tokens": 1234, "input_tokens_details": {"cached_tokens": 1000},
            "output_tokens": 56, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 1290});
        let completed = json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": usage}});
        if approval && !has_output {
            let (name, args) = if tools.iter().any(|t| t == "exec_command") {
                (
                    "exec_command",
                    json!({"cmd": "touch /etc/superba-test", "sandbox_permissions": "require_escalated", "justification": "test"}),
                )
            } else {
                (
                    "shell",
                    json!({"command": ["touch", "/etc/superba-test"], "sandbox_permissions": "require_escalated", "justification": "test"}),
                )
            };
            let item = json!({"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": name,
                "arguments": args.to_string(), "status": "completed"});
            let events = [
                created,
                json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
                completed,
            ];
            return (200, "text/event-stream", sse(&events));
        }
        let text = "Done: the mock task finished.";
        let item = json!({"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}]});
        let events = [
            created,
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": []}}),
            json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
            completed,
        ];
        (200, "text/event-stream", sse(&events))
    })
}

fn codex_config(home: &Home, model: &Mock) {
    std::fs::write(
        home.p("codex/config.toml"),
        format!(
            "model = \"gpt-5.4\"\nmodel_provider = \"mock\"\n\n[model_providers.mock]\nname = \"mock\"\nbase_url = \"{}/v1\"\nwire_api = \"responses\"\nenv_key = \"MOCK_KEY\"\nrequest_max_retries = 0\nstream_max_retries = 0\n",
            model.base
        ),
    )
    .unwrap();
}

fn codex_version(codex: &Path) -> (u64, u64) {
    let out = Command::new(codex).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().last().unwrap_or("0.0.0");
    let mut parts = v.split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// One turn through `codex app-server` (as the desktop app runs it) with
/// on-request approvals; the approval request is declined.
fn app_server_turn(home: &Home, codex: &Path, prompt: &str) {
    let mut cmd = Command::new(codex);
    home.env(&mut cmd);
    let mut child = cmd
        .arg("app-server")
        .env("MOCK_KEY", "x")
        .current_dir(home.p("proj"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && tx.send(v).is_err()
            {
                return;
            }
        }
    });
    let mut send = |v: Value| writeln!(stdin, "{v}").unwrap();
    let wait = |id: i64| loop {
        let v = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("app-server answer");
        if v["id"] == id && v.get("method").is_none() {
            return v;
        }
    };
    send(
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "notify-test", "version": "0"}, "capabilities": {"experimentalApi": true}}}),
    );
    wait(1);
    send(json!({"method": "initialized"}));
    send(
        json!({"id": 2, "method": "thread/start", "params": {"cwd": home.p("proj"), "approvalPolicy": "on-request", "sandbox": "read-only"}}),
    );
    let thread = wait(2)["result"]["thread"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(
        json!({"id": 3, "method": "turn/start", "params": {"threadId": thread, "input": [{"type": "text", "text": prompt}]}}),
    );
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        let Ok(v) = rx.recv_timeout(Duration::from_secs(5)) else {
            continue;
        };
        let method = v["method"].as_str().unwrap_or("");
        if v.get("id").is_some() && method.to_ascii_lowercase().contains("approval") {
            send(json!({"id": v["id"], "result": {"decision": "decline"}}));
        }
        if method == "turn/completed" {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn real_codex_reports_finished_turns_and_approvals() {
    let Ok(bins) = std::env::var("SUPERBA_NOTIFY_CODEX_BINS") else {
        eprintln!("skipped: set SUPERBA_NOTIFY_CODEX_BINS to Codex binaries");
        return;
    };
    for bin in bins.split(':').filter(|b| !b.is_empty()) {
        let codex = PathBuf::from(bin);
        let version = codex_version(&codex);
        eprintln!("== {} ({version:?})", codex.display());
        let home = Home::new();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&codex, home.p("bin/codex")).unwrap();
        let sink = Mock::sink();
        let model = mock_model(false);
        codex_config(&home, &model);
        add_telegram(&home, &sink);
        home.ok(&["notify", "install", "codex"], &[]);

        // A finished turn through `codex exec`: Codex runs `spb notify codex`.
        let mut exec = Command::new(&codex);
        home.env(&mut exec);
        let out = exec
            .args(["exec", "--skip-git-repo-check", "do the task"])
            .env("MOCK_KEY", "x")
            .current_dir(home.p("proj"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let hits = sink.wait_for(1, Duration::from_secs(15));
        assert_eq!(hits.len(), 1, "no notification from {bin}");
        let text = hits[0].body["text"].as_str().unwrap().to_owned();
        eprintln!("{text}");
        assert!(text.contains("Task finished · Codex"), "{text}");
        assert!(text.contains("Project: proj"), "{text}");
        assert!(text.contains("in 1.2k (cached 1.0k) · out 56"), "{text}");

        // Approvals: the PermissionRequest hook, trusted with the hash Codex
        // computes, through the app-server (the desktop app's path).
        let mut approval = home.spb(
            &["notify", "install", "codex", "--approvals", "--trust"],
            &[],
            None,
        );
        if !approval.status.success() {
            approval = home.spb(
                &[
                    "notify",
                    "install",
                    "codex",
                    "--approvals",
                    "--trust",
                    "--verbose",
                ],
                &[],
                None,
            );
        }
        assert!(
            approval.status.success(),
            "{}",
            String::from_utf8_lossy(&approval.stderr)
        );
        let cfg = home.config();
        assert!(
            cfg.contains("[[hooks.PermissionRequest]]") && cfg.contains("trusted_hash = \"sha256:"),
            "{cfg}"
        );
        assert_eq!(cfg.contains("async = true"), version >= (0, 150), "{cfg}");
        let approvals = mock_model(true);
        codex_config(&home, &approvals);
        // codex_config rewrote the file: install and trust again.
        home.ok(&["notify", "install", "codex"], &[]);
        home.ok(
            &["notify", "install", "codex", "--approvals", "--trust"],
            &[],
        );
        app_server_turn(&home, &codex, "touch a file in /etc");
        let hits = sink.wait_for(2, Duration::from_secs(15));
        let texts: Vec<String> = hits
            .iter()
            .filter_map(|h| h.body["text"].as_str().map(str::to_owned))
            .collect();
        eprintln!("{texts:#?}");
        assert!(
            texts.iter().any(|t| t.contains("Needs approval · Codex")),
            "{texts:?}"
        );
    }
}
