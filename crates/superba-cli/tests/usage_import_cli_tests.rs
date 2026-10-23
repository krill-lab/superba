//! `spb import usage ccs|ai-toolbox [--dry-run] [--remove]`, `spb usage log
//! --source` and `spb profile compaction` against the real switching service
//! and a temporary home with cc-switch's database (GitHub issues 1 and 3).

use std::path::Path;
use std::sync::Arc;

use clap::Parser;
use serde_json::Value;
use superba_cli::{Cli, Output, Runtime};
use superba_core::*;
use superba_switch::{Switcher, SwitcherDeps};

struct World {
    tmp: tempfile::TempDir,
}

fn fixture(rel: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../superba-import/tests/fixtures")
            .join(rel),
    )
    .unwrap()
}

impl World {
    fn new() -> Self {
        let w = Self {
            tmp: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(w.home().join(".cc-switch")).unwrap();
        std::fs::create_dir_all(w.data()).unwrap();
        w
    }

    fn home(&self) -> std::path::PathBuf {
        self.tmp.path().join("home")
    }

    fn data(&self) -> std::path::PathBuf {
        self.tmp.path().join("data")
    }

    fn ccs_db(&self) {
        let conn = rusqlite::Connection::open(self.home().join(".cc-switch/cc-switch.db")).unwrap();
        conn.execute_batch(&(fixture("ccs/schema-v20.sql") + &fixture("usage/ccs-v20.sql")))
            .unwrap();
    }

    fn runtime(&self) -> Runtime {
        let env: Arc<dyn EnvSource> = Arc::new(
            MapEnv::new()
                .with("SUPERBA_SECRET_BACKEND", "file")
                .with("SUPERBA_LOCALE", "en"),
        );
        let ctx = LocalContext::new(self.home(), DataDir::new(self.data()), env.clone());
        let service = Arc::new(Switcher::new(SwitcherDeps::with_defaults(ctx)));
        let mut rt = Runtime::with_service(service);
        rt.env = env;
        rt.read_stdin_line = Arc::new(|| panic!("stdin must not be read"));
        rt
    }

    async fn spb(&self, args: &[&str]) -> (u8, String, String) {
        let cli = Cli::try_parse_from(std::iter::once("spb").chain(args.iter().copied())).unwrap();
        let (mut out, stdout, stderr) = Output::capture();
        let code = superba_cli::run(&cli, &self.runtime(), &mut out).await;
        (code, stdout.contents(), stderr.contents())
    }
}

#[tokio::test]
async fn a_dry_run_previews_counts_range_and_cost_and_writes_nothing() {
    let w = World::new();
    w.ccs_db();
    let (code, stdout, stderr) = w.spb(&["import", "usage", "ccs", "--dry-run"]).await;
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    for expected in [
        "CC Switch: ~/.cc-switch/cc-switch.db (CC Switch database, schema 20)",
        "Would import 5 requests and 2 daily totals (52 requests) from 2026-08-01 to 2026-10-11",
        "1 of them has no cost recorded by CC Switch",
        "2 rows of other coding tools",
        "Import them: spb import usage ccs",
    ] {
        assert!(
            stdout.contains(&expected.replace('/', std::path::MAIN_SEPARATOR_STR))
                || stdout.contains(expected),
            "{expected:?} missing:\n{stdout}"
        );
    }
    assert!(!w.data().join("imported-usage.sqlite3").exists());
    // JSON: one document, the report.
    let (code, stdout, _) = w
        .spb(&["--json", "import", "usage", "ccs", "--dry-run"])
        .await;
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["source"], "ccs");
    assert_eq!(v["requests"], 5);
    assert_eq!(v["days"], 2);
    // The provider import still parses as before.
    let (code, _, _) = w.spb(&["import", "ccs", "--dry-run"]).await;
    assert_eq!(code, 0);
}

#[tokio::test]
async fn imported_rows_show_in_the_log_by_source_and_can_be_removed() {
    let w = World::new();
    w.ccs_db();
    let (code, stdout, stderr) = w.spb(&["import", "usage", "cc-switch"]).await;
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("Imported 5 requests and 2 daily totals"),
        "{stdout}"
    );
    let (code, stdout, _) = w.spb(&["import", "usage", "ccs"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Nothing new: everything CC Switch recorded")
            && stdout.contains("7 were imported before"),
        "{stdout}"
    );
    let (code, stdout, _) = w
        .spb(&["usage", "log", "--since", "3650d", "--source", "ccs"])
        .await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("PackyCode") && stdout.contains("CC Switch import"),
        "{stdout}"
    );
    let (code, stdout, _) = w
        .spb(&[
            "--json", "usage", "log", "--since", "3650d", "--source", "rollout",
        ])
        .await;
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert!(v["rows"].as_array().unwrap().is_empty());
    let (code, stdout, _) = w.spb(&["import", "usage", "ccs", "--remove"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Removed 5 imported requests and 2 daily totals of CC Switch."),
        "{stdout}"
    );
}

#[tokio::test]
async fn profile_compaction_shows_the_effective_value_and_the_choice_is_saved() {
    let w = World::new();
    let mut set = ProfileSet::default();
    set.profiles.push(Profile {
        id: ProfileId::new("relay"),
        name: "Relay".into(),
        builtin: None,
        site_id: SiteId::new(catalog::KRILL_SITE_ID),
        line_id: None,
        backup_lines: Vec::new(),
        disabled_lines: Default::default(),
        auth_mode: AuthMode::ApiKey,
        auth: AuthChoice::ApiKey,
        key_ref: None,
        base_url_override: None,
        model: Default::default(),
        codex_settings: Default::default(),
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        anthropic: None,
        model_source: None,
    });
    std::fs::write(
        w.data().join("profiles.json"),
        serde_json::to_string_pretty(&set).unwrap(),
    )
    .unwrap();
    let (code, stdout, _) = w.spb(&["profile", "compaction", "relay"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("Native remote compaction (known for this provider)"),
        "{stdout}"
    );
    let (code, _, _) = w
        .spb(&["profile", "edit", "relay", "--compaction", "unsupported"])
        .await;
    assert_eq!(code, 0);
    let (code, stdout, _) = w.spb(&["profile", "compaction", "relay"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("No remote compaction (set on the provider)"),
        "{stdout}"
    );
    assert!(stdout.contains("no setting to compact locally"), "{stdout}");
}
