//! Real CLI invocations use an isolated Superba and Codex home throughout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for dir in [
            "home",
            "data",
            "codex/sessions",
            "tmp",
            "home/AppData/Local",
            "home/AppData/Roaming",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("data/sample.bin"), b"superba sample").unwrap();
        fs::write(
            root.join("codex/sessions/sample.jsonl"),
            b"codex session stays here",
        )
        .unwrap();
        Self { _temp: temp, root }
    }
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_spb"));
        command
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("LOCALAPPDATA", self.root.join("home/AppData/Local"))
            .env("APPDATA", self.root.join("home/AppData/Roaming"))
            .env("TEMP", self.root.join("tmp"))
            .env("TMP", self.root.join("tmp"))
            .env("SUPERBA_DATA_DIR", self.root.join("data"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env("SUPERBA_SECRET_BACKEND", "file")
            .env("SUPERBA_LOCALE", "en")
            .args(args);
        #[cfg(windows)]
        for name in ["SystemRoot", "SystemDrive", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.output().unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn move_where_and_usage_follow_a_fixed_pointer_across_processes() {
    let f = Fixture::new();
    let initial = f.json(&["--json", "data", "where"]);
    assert_eq!(
        Path::new(initial["directory"].as_str().unwrap()),
        f.root.join("data")
    );
    let target = f.root.join("会话 数据");
    let report = f.json(&["--json", "data", "move", target.to_str().unwrap()]);
    assert_eq!(
        Path::new(report["location"]["directory"].as_str().unwrap()),
        target.canonicalize().unwrap()
    );
    let current = f.json(&["--json", "data", "where"]);
    assert_eq!(current["directory"], report["location"]["directory"]);
    let usage = f.json(&["--json", "data", "usage"]);
    assert_eq!(
        usage["codex_sessions_bytes"],
        b"codex session stays here".len()
    );
    assert!(usage["total_bytes"].as_u64().unwrap() >= b"superba sample".len() as u64);
    assert_eq!(
        fs::read(target.join("sample.bin")).unwrap(),
        b"superba sample"
    );
    assert!(!f.root.join("data/sample.bin").exists());
    assert_eq!(
        fs::read(f.root.join("codex/sessions/sample.jsonl")).unwrap(),
        b"codex session stays here"
    );
}

#[test]
fn bad_destination_is_localized_and_keeps_existing_data() {
    let f = Fixture::new();
    let destination = f.root.join("taken");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("user"), b"keep").unwrap();
    let output = f.run(&[
        "--locale",
        "zh-CN",
        "data",
        "move",
        destination.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("空目录"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(destination.join("user")).unwrap(), b"keep");
    assert!(f.root.join("data/sample.bin").exists());
    assert!(!f.root.join("data/data-location.json").exists());
}

#[test]
fn data_help_is_bilingual_and_available_in_the_gui_command_dispatch() {
    let f = Fixture::new();
    for (locale, expected) in [("en", "Superba data"), ("zh-CN", "Superba 数据")] {
        let output = f.run(&["--locale", locale, "data", "--help"]);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
    }
    assert!(superba_cli::is_cli_invocation(&[
        "superba".into(),
        "data".into(),
        "where".into()
    ]));
}
