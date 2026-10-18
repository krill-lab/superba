//! `spb import ccs|ai-toolbox` against the real switching service and a
//! temporary home with cc-switch's files: the dry-run table and summary, JSON
//! output without keys, `--select` and `--on-conflict`, unknown names, and a
//! tool that is not installed. Nothing here writes (dry runs only), so the tests
//! run unelevated on every OS.

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

    fn ccs_v2(&self) {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../superba-import/tests/fixtures/ccs/config.v2.json");
        std::fs::copy(fixture, self.home().join(".cc-switch/config.json")).unwrap();
    }

    /// A custom site provider named "My Gateway" already in Superba (another
    /// address than cc-switch's).
    fn existing(&self) {
        let mut set = ProfileSet::default();
        let base = url::Url::parse("https://old-gw.example.org/v1").unwrap();
        let site =
            superba_switch::new_compatible_site(SiteId::new("my-gateway"), "My Gateway", &base)
                .unwrap();
        set.profiles.push(Profile {
            id: ProfileId::new("my-gateway"),
            name: "My Gateway".into(),
            builtin: None,
            site_id: site.id.clone(),
            line_id: None,
            backup_lines: Vec::new(),
            auth_mode: AuthMode::ApiKey,
            auth: AuthChoice::ApiKey,
            key_ref: None,
            base_url_override: None,
            model: Default::default(),
            codex_settings: Default::default(),
            model_overrides: Default::default(),
            model_picker: Default::default(),
            compaction: Default::default(),
        });
        set.custom_sites.push(site);
        std::fs::write(
            self.data().join("profiles.json"),
            serde_json::to_string_pretty(&set).unwrap(),
        )
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
async fn dry_run_lists_every_provider_with_what_would_happen() {
    let w = World::new();
    w.ccs_v2();
    w.existing();
    let profiles_before = std::fs::read(w.data().join("profiles.json")).unwrap();
    let (code, stdout, stderr) = w.spb(&["import", "ccs", "--dry-run"]).await;
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    for expected in [
        "cc-switch config.json v2",
        "Provider | In Superba | Model | Key | Conflict | Import",
        "PackyCode (current) | custom site codex-api.packycode.example, 3 lines | gpt-5-codex | ••••0000 | - | would add as PackyCode",
        "My Gateway | custom site gw.example.org | gpt-5 | ••••0000 | same name: My Gateway | skipped: My Gateway already exists in Superba; it was left as it is.",
        "OpenAI Official | - | - | - | - | not importable: The official ChatGPT login",
        "(Claude Code) | - | - | - | - | not importable: A Claude Code provider: Superba sets up Codex only.",
        "PackyCode: Not imported from its config.toml: disable_response_storage.",
        "Dry run: 1 provider would be imported; nothing was written.",
    ] {
        assert!(stdout.contains(expected), "{expected:?} missing:\n{stdout}");
    }
    assert!(
        !stdout.contains("sk-test") && !stderr.contains("sk-test"),
        "no key is printed"
    );
    assert_eq!(
        std::fs::read(w.data().join("profiles.json")).unwrap(),
        profiles_before,
        "a dry run writes nothing"
    );
    assert!(!w.home().join(".codex").exists(), "Codex is not touched");
    // Chinese.
    let (code, stdout, _) = w
        .spb(&["--locale", "zh-CN", "import", "ccs", "--dry-run"])
        .await;
    assert_eq!(code, 0);
    assert!(stdout.contains("将添加为 PackyCode"), "{stdout}");
    assert!(
        stdout.contains("试运行：将导入 1 个供应商；没有写入任何内容。"),
        "{stdout}"
    );
}

#[tokio::test]
async fn select_and_on_conflict_choose_what_is_imported() {
    let w = World::new();
    w.ccs_v2();
    w.existing();
    let (code, stdout, _) = w
        .spb(&[
            "import",
            "cc-switch",
            "--dry-run",
            "--select",
            "my gateway",
            "--on-conflict",
            "rename",
        ])
        .await;
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("would add as My Gateway (CC Switch)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("PackyCode (current)") && stdout.contains("not selected"),
        "{stdout}"
    );
    let (code, stdout, _) = w
        .spb(&[
            "import",
            "ccs",
            "--dry-run",
            "--select",
            "My Gateway",
            "--on-conflict",
            "update",
        ])
        .await;
    assert_eq!(code, 0);
    assert!(stdout.contains("would update My Gateway"), "{stdout}");
    // Unknown names are a usage error that lists what can be imported.
    let (code, _, stderr) = w.spb(&["import", "ccs", "--select", "Nope"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("No provider named Nope"), "{stderr}");
    assert!(stderr.contains("PackyCode, My Gateway"), "{stderr}");
}

#[tokio::test]
async fn json_is_one_document_with_stores_candidates_and_report() {
    let w = World::new();
    w.ccs_v2();
    let (code, stdout, _) = w.spb(&["--json", "import", "ccs", "--dry-run"]).await;
    assert_eq!(code, 0);
    let mut docs = serde_json::Deserializer::from_str(&stdout).into_iter::<Value>();
    let v = docs.next().unwrap().unwrap();
    assert!(docs.next().is_none(), "exactly one JSON document");
    assert_eq!(v["source"], "ccs");
    assert_eq!(v["stores"][0]["status"], "read");
    assert_eq!(v["report"]["dry_run"], true);
    let candidates = v["candidates"].as_array().unwrap();
    let packy = candidates
        .iter()
        .find(|c| c["name"] == "PackyCode")
        .unwrap();
    assert_eq!(packy["key"]["kind"], "present");
    assert_eq!(packy["target"]["kind"], "compatible");
    assert_eq!(packy["target"]["lines"].as_array().unwrap().len(), 3);
    assert_eq!(v["report"]["items"].as_array().unwrap().len(), 2);
    assert!(!stdout.contains("sk-test"));
}

#[tokio::test]
async fn a_tool_that_is_not_installed_is_reported_calmly() {
    let w = World::new();
    let (code, stdout, stderr) = w.spb(&["import", "ai-toolbox", "--dry-run"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("No AI Toolbox configuration found"),
        "{stdout}"
    );
    let (code, _, _) = w.spb(&["import", "aitoolbox", "--dry-run"]).await;
    assert_eq!(code, 0);
}
