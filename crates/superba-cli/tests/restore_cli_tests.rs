//! Real `spb switch` / `spb restore` in isolated HOME and CODEX_HOME, against a
//! loopback model endpoint. Linux only: native macOS/Windows env stores are
//! exercised with injected stores by superba-switch's restore tests.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

struct Models {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Models {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0; 4096];
                if stream.read(&mut request).unwrap_or(0) == 0 {
                    continue;
                }
                let body = r#"{"data":[{"id":"gpt-5.5","object":"model","owned_by":"test"}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

impl Drop for Models {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}

struct Home(tempfile::TempDir);

impl Home {
    fn new(original: &str) -> Self {
        let home = Self(tempfile::tempdir().unwrap());
        for dir in ["home", "codex", "data", "tmp", "claude"] {
            std::fs::create_dir_all(home.path(dir)).unwrap();
        }
        std::fs::write(home.path("codex/config.toml"), original).unwrap();
        std::fs::write(home.path("home/.profile"), "# original shell\n").unwrap();
        home
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }

    fn spb(&self, args: &[&str]) {
        let out = Command::new(env!("CARGO_BIN_EXE_spb"))
            .env_clear()
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("CODEX_HOME", self.path("codex"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude"))
            .env("SUPERBA_DATA_DIR", self.path("data"))
            .env("SUPERBA_SECRET_BACKEND", "file")
            .env("SUPERBA_LOCALE", "en")
            .env("TMPDIR", self.path("tmp"))
            .env("PATH", "/usr/bin:/bin")
            .env("RESTORE_TEST_KEY", "sk-placeholder-test-only")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "spb {args:?}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn round_trip(original_url: bool, custom_provider: bool, repeated: bool) {
    let server = Models::new();
    let mut original = "# original\nmodel = \"gpt-5.5\"\n".to_owned();
    if original_url {
        original.push_str(&format!(
            "openai_base_url = \"{}\"\n",
            server.url("/original/v1")
        ));
    }
    if custom_provider {
        original.push_str(&format!("model_provider = \"legacy\"\n\n[model_providers.legacy]\nname = \"Legacy\"\nbase_url = \"{}\"\nwire_api = \"responses\"\nenv_key = \"ORIGINAL_KEY\"\n", server.url("/legacy/v1")));
    }
    original
        .push_str("\n[mcp_servers.docs]\ncommand = \"npx\"\nenv = { DOCS_TOKEN = \"fixture\" }\n");
    let h = Home::new(&original);
    for id in ["first", "second"] {
        h.spb(&[
            "site",
            "add",
            id,
            "--base-url",
            &server.url(&format!("/{id}/v1")),
        ]);
        h.spb(&[
            "profile",
            "add",
            id,
            "--site",
            id,
            "--auth",
            "apikey",
            "--key-env",
            "RESTORE_TEST_KEY",
        ]);
    }
    let switches: &[&str] = if repeated {
        &["first", "second", "first"]
    } else {
        &["first"]
    };
    for id in switches {
        h.spb(&[
            "switch",
            "--profile",
            id,
            "--write-only",
            "--yes",
            "--accept-unknown-version",
            "--skip-legacy-sessions",
        ]);
        let cfg = std::fs::read_to_string(h.path("codex/config.toml")).unwrap();
        assert!(cfg.contains(&server.url(&format!("/{id}/v1"))), "{cfg}");
    }
    // No --provider override: the normal command restores missing routing keys
    // as missing, and existing keys/tables as their original values.
    h.spb(&["restore", "--write-only", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(h.path("codex/config.toml")).unwrap(),
        original
    );
    assert!(!h.path("codex/auth.json").exists());
    assert_eq!(
        std::fs::read_to_string(h.path("home/.profile")).unwrap(),
        "# original shell\n"
    );
}

#[test]
fn restore_removes_added_url_from_plain_config() {
    round_trip(false, false, false);
}

#[test]
fn restore_removes_added_url_from_custom_provider_after_multiple_switches() {
    round_trip(false, true, true);
}

#[test]
fn restore_preserves_original_url_after_multiple_switches() {
    round_trip(true, true, true);
}
