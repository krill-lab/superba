//! The harness writers: apply Superba's provider into each harness's files
//! (only owned entries change; comments and unknown fields stay), read it
//! back, restore the files to what they were, take over or keep entries
//! someone changed, refuse what would break the files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_config::ENV_SUPERBA_SECRET_BACKEND;
use superba_config::harness::{HarnessWriter, jsonc, pi, yaml};
use superba_core::harness::{
    DshTelemetry, EndpointSource, HarnessId, HarnessModel, HarnessOptions, HarnessPlanKind,
    HarnessProtocol, HarnessRoute, HarnessTarget, OpencodeMajor, ProtocolEndpoint,
    WorkBuddyEdition,
};
use superba_core::{DataDir, LocalContext, MapEnv, ProfileId, Secret, Timestamp};
use url::Url;

const KEY: &str = "sk-test-key-1234567890abcd";

fn fixture(dir: &str, name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/harness")
            .join(dir)
            .join(name),
    )
    .unwrap()
}

struct Env {
    tmp: tempfile::TempDir,
    ctx: LocalContext,
}

impl Env {
    fn new() -> Self {
        Self::with_env(MapEnv::new())
    }

    fn with_env(env: MapEnv) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = LocalContext::new(
            tmp.path().join("home"),
            DataDir::new(tmp.path().join("data")),
            Arc::new(env.with(ENV_SUPERBA_SECRET_BACKEND, "file")),
        );
        std::fs::create_dir_all(&ctx.home_dir).unwrap();
        Self { tmp, ctx }
    }

    fn home(&self, rel: &str) -> PathBuf {
        self.ctx.home_dir.join(rel)
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.home(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.home(rel)).ok()
    }

    fn writer(&self, h: HarnessId) -> HarnessWriter {
        HarnessWriter::new(self.ctx.clone(), h)
    }
}

fn now() -> Timestamp {
    Timestamp::from_utc_date(2026, 10, 18).unwrap()
}

fn models() -> Vec<HarnessModel> {
    vec![
        HarnessModel {
            id: "gpt-5.6-luna".into(),
            name: Some("GPT-5.6 Luna".into()),
            context_window: Some(272_000),
            reasoning_levels: vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
            default_reasoning: Some("medium".into()),
            images: true,
        },
        HarnessModel::id_only("deepseek-v4"),
    ]
}

fn target(h: HarnessId, protocol: HarnessProtocol, base: &str) -> HarnessTarget {
    HarnessTarget {
        harness: h,
        provider: Some(ProfileId::new("krill-main")),
        provider_name: "Krill".into(),
        endpoint: ProtocolEndpoint {
            protocol,
            base_url: Url::parse(base).unwrap(),
            source: EndpointSource::Krill,
        },
        key: Secret::new(KEY),
        models: models(),
        default_model: None,
        route: HarnessRoute::Direct,
        options: HarnessOptions::default(),
    }
}

fn apply(w: &HarnessWriter, t: &HarnessTarget) -> superba_core::harness::HarnessApplyReport {
    let plan = w.plan(HarnessPlanKind::Apply, Some(t.clone())).unwrap();
    w.apply(&plan, false, now()).unwrap()
}

fn restore(w: &HarnessWriter) -> superba_core::harness::HarnessApplyReport {
    let plan = w.plan(HarnessPlanKind::Restore, None).unwrap();
    w.apply(&plan, false, now()).unwrap()
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

// ---------------------------------------------------------------------------
// pi

#[test]
fn pi_into_absent_files_and_back_to_nothing() {
    let e = Env::new();
    let w = e.writer(HarnessId::Pi);
    let t = target(
        HarnessId::Pi,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    let report = apply(&w, &t);
    assert_eq!(report.files.len(), 2);
    let models: Value = serde_json::from_str(&e.read(".pi/agent/models.json").unwrap()).unwrap();
    let p = &models["providers"]["superba"];
    assert_eq!(p["api"], "openai-responses");
    assert_eq!(p["baseUrl"], "https://line.example/codex/v1");
    assert_eq!(p["apiKey"], KEY);
    assert_eq!(p["name"], "Superba · Krill");
    let luna = &p["models"][0];
    assert_eq!(luna["reasoning"], true);
    assert_eq!(
        luna["thinkingLevelMap"],
        json!({ "off": null, "minimal": null, "xhigh": "xhigh" })
    );
    assert_eq!(luna["input"], json!(["text", "image"]));
    assert_eq!(luna["contextWindow"], 272000);
    // No reasoning declared for a model without efforts.
    assert!(p["models"][1].get("reasoning").is_none());
    let settings: Value =
        serde_json::from_str(&e.read(".pi/agent/settings.json").unwrap()).unwrap();
    assert_eq!(
        settings,
        json!({ "defaultProvider": "superba", "defaultModel": "gpt-5.6-luna" })
    );
    #[cfg(unix)]
    assert_eq!(mode(&e.home(".pi/agent/models.json")), 0o600);
    // Read back: owned and not drifted.
    let applied = w.applied().unwrap().unwrap();
    assert!(applied.drift.is_empty(), "{:?}", applied.drift);
    assert_eq!(applied.owned.len(), 3);
    assert!(w.pending(&t).unwrap().is_empty());
    // Restore removes both files again (Superba created them).
    restore(&w);
    assert_eq!(e.read(".pi/agent/models.json"), None);
    assert_eq!(e.read(".pi/agent/settings.json"), None);
    assert!(w.state().unwrap().is_none());
    // The directories Superba created for them are gone too.
    assert!(!e.home(".pi").exists());
}

const PI_MODELS: &str = r#"{
  // my providers
  "providers": {
    "ollama": {
      "baseUrl": "http://localhost:11434/v1",
      "api": "openai-completions",
      "apiKey": "ollama",
      "models": [{ "id": "qwen3", "futureField": 1 }],
    },
  },
  "unknownTopLevel": { "kept": true }
}
"#;

const PI_SETTINGS: &str = "{\n  \"defaultProvider\": \"ollama\",\n  \"defaultModel\": \"qwen3\",\n  \"theme\": \"dark\",\n  \"retry\": { \"maxRetries\": 5 }\n}\n";

#[test]
fn pi_keeps_comments_trailing_commas_and_other_providers_and_restores_bytes() {
    let e = Env::new();
    e.put(".pi/agent/models.json", PI_MODELS);
    e.put(".pi/agent/settings.json", PI_SETTINGS);
    let w = e.writer(HarnessId::Pi);
    let mut t = target(
        HarnessId::Pi,
        HarnessProtocol::Anthropic,
        "https://line.example/codex",
    );
    t.default_model = Some("deepseek-v4".into());
    apply(&w, &t);
    let text = e.read(".pi/agent/models.json").unwrap();
    assert!(text.starts_with("{\n  // my providers\n"), "{text}");
    assert!(text.contains("\"futureField\": 1"));
    let v = jsonc::to_value(&text, jsonc::JsonFlavor::Jsonc).unwrap();
    assert_eq!(v["providers"]["ollama"]["apiKey"], "ollama");
    assert_eq!(v["unknownTopLevel"], json!({ "kept": true }));
    assert_eq!(v["providers"]["superba"]["api"], "anthropic-messages");
    assert_eq!(
        v["providers"]["superba"]["baseUrl"],
        "https://line.example/codex"
    );
    let s: Value = serde_json::from_str(&e.read(".pi/agent/settings.json").unwrap()).unwrap();
    assert_eq!(s["defaultProvider"], "superba");
    assert_eq!(s["defaultModel"], "deepseek-v4");
    assert_eq!(s["theme"], "dark");
    restore(&w);
    assert_eq!(e.read(".pi/agent/models.json").unwrap(), PI_MODELS);
    assert_eq!(e.read(".pi/agent/settings.json").unwrap(), PI_SETTINGS);
}

#[test]
fn pi_escapes_dollar_and_bang_in_keys() {
    assert_eq!(pi::escape_value("sk-$HOME!x"), "sk-$$HOME$!x");
    assert_eq!(pi::escape_value("!cmd"), "$!cmd");
    assert_eq!(pi::escape_value("plain-key"), "plain-key");
    let e = Env::new();
    let w = e.writer(HarnessId::Pi);
    let mut t = target(
        HarnessId::Pi,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    t.key = Secret::new("!rm-${X}-$Y");
    apply(&w, &t);
    let v: Value = serde_json::from_str(&e.read(".pi/agent/models.json").unwrap()).unwrap();
    assert_eq!(v["providers"]["superba"]["apiKey"], "$!rm-$${X}-$$Y");
}

#[test]
fn pi_refuses_a_models_file_its_schema_rejects() {
    let e = Env::new();
    // The user's own provider is already invalid (contextWindow 0): pi shows
    // no custom provider at all, so Superba's would be hidden too.
    let bad = r#"{ "providers": { "mine": { "models": [{ "id": "x", "contextWindow": 0 }] } } }"#;
    e.put(".pi/agent/models.json", bad);
    let w = e.writer(HarnessId::Pi);
    let t = target(
        HarnessId::Pi,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    let err = w
        .plan(HarnessPlanKind::Apply, Some(t))
        .unwrap_err()
        .to_string();
    assert!(err.contains("pi would reject the whole file"), "{err}");
    assert!(err.contains("contextWindow"), "{err}");
    assert_eq!(e.read(".pi/agent/models.json").unwrap(), bad);
}

#[test]
fn pi_schema_accepts_what_superba_writes_for_every_protocol() {
    let schema: Value = serde_json::from_str(pi::MODELS_SCHEMA).unwrap();
    for p in HarnessProtocol::ALL {
        let t = target(HarnessId::Pi, p, "https://line.example/codex/v1");
        let doc = json!({ "providers": { "superba": pi::provider_entry(&t) } });
        superba_config::harness::schema::check(&schema, &doc).unwrap();
    }
}

// ---------------------------------------------------------------------------
// opencode

#[test]
fn opencode_v2_shape_by_default_v1_on_request_and_restore() {
    let original = fixture("opencode", "research-v2-mock.jsonc");
    for major in [OpencodeMajor::V2, OpencodeMajor::V1] {
        let e = Env::new();
        e.put(".config/opencode/opencode.jsonc", &original);
        let w = e.writer(HarnessId::OpenCode);
        let mut t = target(
            HarnessId::OpenCode,
            HarnessProtocol::Anthropic,
            "https://line.example/codex",
        );
        t.options.opencode_major = Some(major);
        apply(&w, &t);
        let text = e.read(".config/opencode/opencode.jsonc").unwrap();
        let v = jsonc::to_value(&text, jsonc::JsonFlavor::Jsonc).unwrap();
        assert_eq!(v["model"], "superba/gpt-5.6-luna");
        // The user's providers, unknown fields and compact formatting stay.
        assert!(text.contains(r#""models": { "mock-gpt": { "name": "Mock GPT", "limit": { "context": 400000, "output": 32000 } }, "hot-added": { "name": "Hot added" } }"#), "{text}");
        assert_eq!(v["share"], "disabled");
        match major {
            OpencodeMajor::V2 => {
                let p = &v["providers"]["superba"];
                assert_eq!(p["package"], "@opencode/ai/providers/anthropic-compatible");
                assert_eq!(p["settings"]["baseURL"], "https://line.example/codex/v1");
                assert_eq!(p["settings"]["apiKey"], KEY);
                assert_eq!(
                    p["models"]["gpt-5.6-luna"]["capabilities"],
                    json!({ "reasoning": { "supported": true, "effort": ["low", "medium", "high", "xhigh"] }, "input": ["text", "image"] })
                );
                assert_eq!(
                    p["models"]["gpt-5.6-luna"]["limit"],
                    json!({ "context": 272000 })
                );
                assert!(v.get("provider").is_none());
            }
            OpencodeMajor::V1 => {
                let p = &v["provider"]["superba"];
                assert_eq!(p["npm"], "@ai-sdk/anthropic");
                assert_eq!(p["options"]["baseURL"], "https://line.example/codex/v1");
                assert_eq!(
                    p["models"]["gpt-5.6-luna"]["reasoning_options"],
                    json!([{ "type": "effort", "values": ["low", "medium", "high", "xhigh"] }])
                );
                assert_eq!(
                    p["models"]["gpt-5.6-luna"]["limit"],
                    json!({ "context": 272000, "output": 32000 })
                );
                assert!(v["providers"].get("superba").is_none());
            }
        }
        restore(&w);
        assert_eq!(e.read(".config/opencode/opencode.jsonc").unwrap(), original);
    }
}

#[test]
fn opencode_switching_major_moves_the_entry() {
    let e = Env::new();
    e.put(
        ".config/opencode/opencode.json",
        &fixture("opencode", "research-v1-mock.json"),
    );
    let w = e.writer(HarnessId::OpenCode);
    let mut t = target(
        HarnessId::OpenCode,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    t.options.opencode_major = Some(OpencodeMajor::V1);
    apply(&w, &t);
    t.options.opencode_major = Some(OpencodeMajor::V2);
    apply(&w, &t);
    let v: Value =
        serde_json::from_str(&e.read(".config/opencode/opencode.json").unwrap()).unwrap();
    assert!(v["provider"].get("superba").is_none());
    assert_eq!(
        v["providers"]["superba"]["package"],
        "@opencode/ai/providers/openai"
    );
    // v1's provider block kept the user's providers.
    assert!(v["provider"].get("mockchat").is_some());
    restore(&w);
    assert_eq!(
        e.read(".config/opencode/opencode.json").unwrap(),
        fixture("opencode", "research-v1-mock.json")
    );
}

#[test]
fn opencode_new_file_names_the_schema_and_is_removed_on_restore() {
    let e = Env::new();
    let w = e.writer(HarnessId::OpenCode);
    let t = target(
        HarnessId::OpenCode,
        HarnessProtocol::Chat,
        "https://line.example/codex/v1",
    );
    apply(&w, &t);
    let v: Value =
        serde_json::from_str(&e.read(".config/opencode/opencode.json").unwrap()).unwrap();
    assert_eq!(v["$schema"], "https://opencode.ai/config.json");
    assert_eq!(
        v["providers"]["superba"]["package"],
        "@opencode/ai/providers/openai-compatible"
    );
    restore(&w);
    assert_eq!(e.read(".config/opencode/opencode.json"), None);
}

#[test]
fn opencode_follows_xdg_and_config_dir_overrides() {
    let e = Env::with_env(MapEnv::new().with("XDG_CONFIG_HOME", "/nonexistent-xdg"));
    let files = superba_config::harness::spec(HarnessId::OpenCode)
        .files(&e.ctx, &HarnessOptions::default())
        .unwrap();
    assert_eq!(
        files[0].path,
        Path::new("/nonexistent-xdg/opencode/opencode.json")
    );
    let e = Env::with_env(MapEnv::new().with("OPENCODE_CONFIG_DIR", "/elsewhere"));
    let files = superba_config::harness::spec(HarnessId::OpenCode)
        .files(&e.ctx, &HarnessOptions::default())
        .unwrap();
    assert_eq!(files[0].path, Path::new("/elsewhere/opencode.json"));
}

#[test]
fn opencode_refuses_substitution_markers_in_keys() {
    let e = Env::new();
    let w = e.writer(HarnessId::OpenCode);
    let mut t = target(
        HarnessId::OpenCode,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    t.key = Secret::new("abc{env:HOME}");
    assert!(w.plan(HarnessPlanKind::Apply, Some(t)).is_err());
    assert_eq!(e.read(".config/opencode/opencode.json"), None);
}

// ---------------------------------------------------------------------------
// ZCode

#[test]
fn zcode_entries_in_lists_and_restore() {
    let original = fixture("zcode", "research-provider_config.json");
    let e = Env::new();
    e.put(".zcode/v2/provider_config.json", &original);
    let w = e.writer(HarnessId::ZCode);
    let t = target(
        HarnessId::ZCode,
        HarnessProtocol::Responses,
        "http://127.0.0.1:18731/openai/v1",
    );
    apply(&w, &t);
    let v: Value =
        serde_json::from_str(&e.read(".zcode/v2/provider_config.json").unwrap()).unwrap();
    let c = &v["config"];
    assert_eq!(c["providerOrder"][0], "superba");
    assert_eq!(c["providerOrder"].as_array().unwrap().len(), 4);
    let rules = c["providerConfigRules"]["providerRules"]
        .as_array()
        .unwrap();
    assert_eq!(rules.len(), 4);
    let mine = rules.iter().find(|r| r["providerId"] == "superba").unwrap();
    assert_eq!(
        mine["config"]["access"],
        json!({ "type": "api-key", "apiKey": KEY })
    );
    assert_eq!(mine["config"]["api"]["type"], "openai-responses");
    assert_eq!(
        mine["config"]["personalModelIds"],
        json!(["gpt-5.6-luna", "deepseek-v4"])
    );
    // One model rule for the model Superba knows something about.
    let mrules = c["modelConfigRules"]["providerModelRules"]
        .as_array()
        .unwrap();
    assert_eq!(mrules.len(), 2);
    assert_eq!(
        c["defaultModelSelection"],
        json!({ "providerId": "superba", "modelId": "gpt-5.6-luna" })
    );
    assert_eq!(c["modelConfigRules"]["manualProviderModelRules"], json!([]));
    restore(&w);
    assert_eq!(e.read(".zcode/v2/provider_config.json").unwrap(), original);
}

#[test]
fn zcode_fresh_file_has_the_schema_version() {
    let e = Env::new();
    let w = e.writer(HarnessId::ZCode);
    apply(
        &w,
        &target(
            HarnessId::ZCode,
            HarnessProtocol::Anthropic,
            "https://line.example/codex",
        ),
    );
    let v: Value =
        serde_json::from_str(&e.read(".zcode/v2/provider_config.json").unwrap()).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(
        v["config"]["providerConfigRules"]["providerRules"][0]["config"]["api"]["baseUrl"],
        "https://line.example/codex"
    );
    restore(&w);
    assert_eq!(e.read(".zcode/v2/provider_config.json"), None);
}

// ---------------------------------------------------------------------------
// WorkBuddy

#[test]
fn workbuddy_both_editions_bare_array_file_and_restore() {
    let e = Env::new();
    e.put(".workbuddy/models.json", "[]");
    e.put(
        ".workbuddy-ai/models.json",
        &fixture("workbuddy", "research-models.json"),
    );
    let w = e.writer(HarnessId::WorkBuddy);
    let t = target(
        HarnessId::WorkBuddy,
        HarnessProtocol::Chat,
        "https://line.example/codex/v1",
    );
    apply(&w, &t);
    let cn: Value = serde_json::from_str(&e.read(".workbuddy/models.json").unwrap()).unwrap();
    assert_eq!(
        cn["availableModels"],
        json!(["gpt-5.6-luna", "deepseek-v4"])
    );
    let m = &cn["models"][0];
    assert_eq!(m["url"], "https://line.example/codex/v1");
    assert_eq!(m["apiKey"], KEY);
    assert_eq!(
        m["reasoning"],
        json!({ "defaultEffort": "medium", "supportedEfforts": ["low", "medium", "high", "xhigh"] })
    );
    assert_eq!(m["maxInputTokens"], 272000);
    let intl: Value = serde_json::from_str(&e.read(".workbuddy-ai/models.json").unwrap()).unwrap();
    assert_eq!(intl["models"].as_array().unwrap().len(), 7);
    assert_eq!(intl["availableModels"].as_array().unwrap().len(), 7);
    restore(&w);
    assert_eq!(e.read(".workbuddy/models.json").unwrap(), "[]");
    assert_eq!(
        e.read(".workbuddy-ai/models.json").unwrap(),
        fixture("workbuddy", "research-models.json")
    );
}

#[test]
fn workbuddy_takes_chat_only_and_refuses_env_references_in_keys() {
    let e = Env::new();
    let w = e.writer(HarnessId::WorkBuddy);
    let t = target(
        HarnessId::WorkBuddy,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    assert!(w.plan(HarnessPlanKind::Apply, Some(t)).is_err());
    let mut t = target(
        HarnessId::WorkBuddy,
        HarnessProtocol::Chat,
        "https://line.example/codex/v1",
    );
    t.key = Secret::new("abc${HOME}");
    assert!(w.plan(HarnessPlanKind::Apply, Some(t)).is_err());
    // Only the China build without any directory.
    let files = superba_config::harness::spec(HarnessId::WorkBuddy)
        .files(&e.ctx, &HarnessOptions::default())
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].key, "cn/models.json");
    let opts = HarnessOptions {
        workbuddy_editions: vec![WorkBuddyEdition::Intl],
        ..HarnessOptions::default()
    };
    let files = superba_config::harness::spec(HarnessId::WorkBuddy)
        .files(&e.ctx, &opts)
        .unwrap();
    assert_eq!(files[0].key, "intl/models.json");
}

// ---------------------------------------------------------------------------
// dsh

#[test]
fn dsh_every_profile_the_env_file_and_restore_bytes() {
    let e = Env::new();
    let fresh = fixture("dsh", "fresh-profile-patch.yml");
    let mine = format!(
        "# my overrides\n{}",
        fixture("dsh", "research-js-patch.yml")
    );
    e.put(".dsh/profiles/headless/cordis.patch.yml", &fresh);
    e.put(".dsh/profiles/tui/cordis.patch.yml", &mine);
    e.put(".dsh/.env", "# keys\nDEEPSEEK_API_KEY=sk-other\n");
    let w = e.writer(HarnessId::Dsh);
    let mut t = target(
        HarnessId::Dsh,
        HarnessProtocol::Chat,
        "https://line.example/codex/v1",
    );
    t.options.dsh_telemetry = DshTelemetry::Off;
    let report = apply(&w, &t);
    // headless, tui, web (shipped, created) and .env.
    assert_eq!(report.files.len(), 4, "{:?}", report.files);
    for p in ["headless", "tui", "web"] {
        let text = e
            .read(&format!(".dsh/profiles/{p}/cordis.patch.yml"))
            .unwrap();
        let v = yaml::to_value(&text).unwrap();
        let llm = v
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == "llm-pi-ai")
            .unwrap();
        let s = &llm["config"]["providers"]["superba"];
        assert_eq!(s["api"], "openai-completions");
        assert_eq!(s["apiKeyEnv"], "SUPERBA_API_KEY");
        assert_eq!(
            s["models"][0]["reasoningEfforts"],
            json!({ "low": "low", "medium": "medium", "high": "high", "xhigh": "xhigh" })
        );
        assert!(s["models"][1].get("reasoningEfforts").is_none());
        assert!(
            v.as_array()
                .unwrap()
                .iter()
                .any(|x| x == &json!({ "id": "session-telemetry-otel", "disabled": true }))
        );
        assert!(!text.contains(KEY));
    }
    let tui = e.read(".dsh/profiles/tui/cordis.patch.yml").unwrap();
    assert!(tui.contains("baseURL: !!js process.env.KRILL_BASE + '/codex/v1'"));
    assert!(tui.starts_with("# my overrides\n"));
    let env = e.read(".dsh/.env").unwrap();
    assert_eq!(
        env,
        format!("# keys\nDEEPSEEK_API_KEY=sk-other\nSUPERBA_API_KEY={KEY}\n")
    );
    #[cfg(unix)]
    assert_eq!(mode(&e.home(".dsh/.env")), 0o600);
    // The plan shows the key masked.
    let plan = w.plan(HarnessPlanKind::Restore, None).unwrap();
    let key_change = plan
        .changes
        .iter()
        .find(|c| c.slot == "SUPERBA_API_KEY")
        .unwrap();
    assert!(key_change.secret);
    assert!(!key_change.before.as_deref().unwrap().contains("1234567890"));
    restore(&w);
    assert_eq!(
        e.read(".dsh/profiles/headless/cordis.patch.yml").unwrap(),
        fresh
    );
    assert_eq!(e.read(".dsh/profiles/tui/cordis.patch.yml").unwrap(), mine);
    assert_eq!(e.read(".dsh/profiles/web/cordis.patch.yml"), None);
    assert!(!e.home(".dsh/profiles/web").exists());
    assert!(e.home(".dsh/profiles/headless").exists());
    assert_eq!(
        e.read(".dsh/.env").unwrap(),
        "# keys\nDEEPSEEK_API_KEY=sk-other\n"
    );
}

#[test]
fn dsh_telemetry_back_to_unmanaged_gives_the_users_entries_back() {
    let e = Env::new();
    let original = "- id: session-log-deepseek\n  config:\n    enabled: true\n";
    e.put(".dsh/profiles/web/cordis.patch.yml", original);
    let w = e.writer(HarnessId::Dsh);
    let mut t = target(
        HarnessId::Dsh,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    t.options.dsh_telemetry = DshTelemetry::Off;
    apply(&w, &t);
    let v = yaml::to_value(&e.read(".dsh/profiles/web/cordis.patch.yml").unwrap()).unwrap();
    assert_eq!(
        v[0],
        json!({ "id": "session-log-deepseek", "config": { "enabled": false } })
    );
    t.options.dsh_telemetry = DshTelemetry::Unmanaged;
    apply(&w, &t);
    let v = yaml::to_value(&e.read(".dsh/profiles/web/cordis.patch.yml").unwrap()).unwrap();
    assert_eq!(
        v[0],
        json!({ "id": "session-log-deepseek", "config": { "enabled": true } })
    );
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .all(|x| x["id"] != "session-telemetry-otel")
    );
}

#[test]
fn dsh_home_patch_override_is_diagnosed_and_never_written() {
    let e = Env::new();
    let home_patch = "- id: llm-pi-ai\n  config:\n    providers: {}\n";
    e.put(".dsh/cordis.patch.yml", home_patch);
    let w = e.writer(HarnessId::Dsh);
    let t = target(
        HarnessId::Dsh,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    let plan = w.plan(HarnessPlanKind::Apply, Some(t.clone())).unwrap();
    assert!(
        plan.notes
            .iter()
            .any(|n| n.text.key == "config.harness.dsh.home_patch")
    );
    w.apply(&plan, false, now()).unwrap();
    assert_eq!(e.read(".dsh/cordis.patch.yml").unwrap(), home_patch);
}

// ---------------------------------------------------------------------------
// engine behaviour shared by all harnesses

#[test]
fn drift_is_kept_unless_taken_over_and_kept_on_restore() {
    let e = Env::new();
    let w = e.writer(HarnessId::Pi);
    let t = target(
        HarnessId::Pi,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    apply(&w, &t);
    // Someone changes Superba's default model.
    let mut s: Value = serde_json::from_str(&e.read(".pi/agent/settings.json").unwrap()).unwrap();
    s["defaultModel"] = json!("deepseek-v4");
    e.put(
        ".pi/agent/settings.json",
        &serde_json::to_string_pretty(&s).unwrap(),
    );
    let applied = w.applied().unwrap().unwrap();
    assert_eq!(applied.drift, vec!["settings.json defaultModel".to_owned()]);
    // A new apply keeps it (reported as kept) ...
    let mut t2 = t.clone();
    t2.provider_name = "Krill 2".into();
    let plan = w.plan(HarnessPlanKind::Apply, Some(t2.clone())).unwrap();
    assert!(
        plan.changes
            .iter()
            .any(|c| c.drift && c.slot == "defaultModel")
    );
    let report = w.apply(&plan, false, now()).unwrap();
    assert_eq!(report.kept, vec!["settings.json defaultModel".to_owned()]);
    let s: Value = serde_json::from_str(&e.read(".pi/agent/settings.json").unwrap()).unwrap();
    assert_eq!(s["defaultModel"], "deepseek-v4");
    // ... a restore keeps it too and lets go of everything else.
    let report = restore(&w);
    assert_eq!(report.kept, vec!["settings.json defaultModel".to_owned()]);
    assert_eq!(e.read(".pi/agent/models.json"), None);
    assert!(
        e.read(".pi/agent/settings.json")
            .unwrap()
            .contains("deepseek-v4")
    );
    // Taken over, it is Superba's again.
    let plan = w.plan(HarnessPlanKind::Apply, Some(t.clone())).unwrap();
    w.apply(&plan, true, now()).unwrap();
    let s: Value = serde_json::from_str(&e.read(".pi/agent/settings.json").unwrap()).unwrap();
    assert_eq!(s["defaultModel"], "gpt-5.6-luna");
}

#[test]
fn a_file_changed_since_the_plan_is_not_written() {
    let e = Env::new();
    e.put(
        ".zcode/v2/provider_config.json",
        "{\"schemaVersion\": 1, \"config\": {}}",
    );
    let w = e.writer(HarnessId::ZCode);
    let t = target(
        HarnessId::ZCode,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    let plan = w.plan(HarnessPlanKind::Apply, Some(t)).unwrap();
    e.put(
        ".zcode/v2/provider_config.json",
        "{\"schemaVersion\": 1, \"config\": {\"x\": 1}}",
    );
    let err = w.apply(&plan, false, now()).unwrap_err();
    assert!(matches!(err, superba_core::Error::Conflict { .. }), "{err}");
    assert_eq!(
        e.read(".zcode/v2/provider_config.json").unwrap(),
        "{\"schemaVersion\": 1, \"config\": {\"x\": 1}}"
    );
}

#[test]
fn invalid_files_are_refused() {
    let e = Env::new();
    e.put(".config/opencode/opencode.json", "{ \"model\": ");
    let w = e.writer(HarnessId::OpenCode);
    let t = target(
        HarnessId::OpenCode,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    let err = w
        .plan(HarnessPlanKind::Apply, Some(t))
        .unwrap_err()
        .to_string();
    assert!(err.contains("not valid"), "{err}");
    e.put(
        ".zcode/v2/provider_config.json",
        "{ // no comments in ZCode\n}",
    );
    let w = e.writer(HarnessId::ZCode);
    let t = target(
        HarnessId::ZCode,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    assert!(w.plan(HarnessPlanKind::Apply, Some(t)).is_err());
}

#[test]
fn keys_with_quotes_or_control_characters_are_refused() {
    for h in [
        HarnessId::Pi,
        HarnessId::OpenCode,
        HarnessId::ZCode,
        HarnessId::Dsh,
    ] {
        let e = Env::new();
        let w = e.writer(h);
        let mut t = target(
            h,
            HarnessProtocol::Responses,
            "https://line.example/codex/v1",
        );
        for bad in ["a\"b", "a b", "a\\b", "a\nb"] {
            t.key = Secret::new(bad);
            assert!(
                w.plan(HarnessPlanKind::Apply, Some(t.clone())).is_err(),
                "{h} {bad:?}"
            );
        }
    }
    // dsh: a single quote cannot go into the .env file either.
    let e = Env::new();
    let mut t = target(
        HarnessId::Dsh,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    t.key = Secret::new("a'b");
    assert!(
        e.writer(HarnessId::Dsh)
            .plan(HarnessPlanKind::Apply, Some(t))
            .is_err()
    );
}

#[test]
fn backups_keep_the_original_and_a_backup_restores_a_whole_file() {
    let e = Env::new();
    e.put(".pi/agent/settings.json", PI_SETTINGS);
    let w = e.writer(HarnessId::Pi);
    let t = target(
        HarnessId::Pi,
        HarnessProtocol::Responses,
        "https://line.example/codex/v1",
    );
    apply(&w, &t);
    apply(&w, &{
        let mut t = t.clone();
        t.default_model = Some("deepseek-v4".into());
        t
    });
    let backups = w.backups().unwrap();
    let originals: Vec<_> = backups
        .iter()
        .filter(|b| b.source == superba_core::BackupSource::Original)
        .collect();
    assert_eq!(originals.len(), 1);
    assert_eq!(originals[0].file, "settings.json");
    w.restore_backup(&originals[0].id, now()).unwrap();
    assert_eq!(e.read(".pi/agent/settings.json").unwrap(), PI_SETTINGS);
    // Superba no longer owns anything in settings.json; models.json stays.
    let state = w.state().unwrap().unwrap();
    assert!(!state.files.contains_key("settings.json"));
    assert!(state.files.contains_key("models.json"));
    let _ = &e.tmp;
}

#[test]
fn restore_with_nothing_owned_is_empty() {
    let e = Env::new();
    let w = e.writer(HarnessId::ZCode);
    let plan = w.plan(HarnessPlanKind::Restore, None).unwrap();
    assert!(plan.is_empty());
    assert!(
        plan.notes
            .iter()
            .any(|n| n.text.key == "config.harness.nothing_owned")
    );
}
