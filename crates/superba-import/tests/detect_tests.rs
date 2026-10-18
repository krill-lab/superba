//! GH#5: who changed Codex's configuration. The live files in
//! `fixtures/live/` reproduce each tool's write over
//! `superba-krill-pat.config.toml` (what Superba writes for a Krill PAT
//! profile), from the tools' sources (2026-10-18 HEADs):
//!
//! - `ccs-kimi.config.toml`: cc-switch 4.0.15 switching Codex to its provider
//!   "Kimi" (`services/provider/codex_direct.rs` `write_direct` →
//!   `live/project/codex.rs` `CodexConfigPatch::apply_to`): the key fields it
//!   manages are cleared or replaced in place (`openai_base_url` removed,
//!   `model_provider = "custom"`), new ones appended, `[model_providers.custom]`
//!   appended with the key as `experimental_bearer_token`; everything else (our
//!   `chatgpt_base_url`, `service_tier`, comments, MCP servers) stays.
//!   `auth.json` is deleted (`codex_login::plan`, preservation off).
//! - `aitb-relay.config.toml` / `.auth.json`: AI Toolbox HEAD (schema 29)
//!   applying "Example Relay" (`coding/codex/commands.rs`
//!   `apply_config_internal_with_events`): `#:schema none` prepended, its
//!   managed TOML merged over the file (`model`, `model_provider = "custom"`,
//!   `[model_providers.custom]` with `requires_openai_auth = true`), keys it
//!   does not manage (our `openai_base_url`, `chatgpt_base_url`) left;
//!   `auth.json` merged (`OPENAI_API_KEY` replaced in place, `auth_mode =
//!   "apikey"` appended, pretty JSON without a trailing newline).
//!
//! The stores are the import fixtures (cc-switch schema 20 with "Kimi" current,
//! AI Toolbox schema 29 with "Example Relay" applied). All keys are fake.

mod support;

use superba_core::{ChangeSource, ImportSource};
use superba_import::Os;
use superba_import::detect::{Attribution, Changed, LiveFiles, attribute_with};
use support::{Home, fixture};

const AITB_DB: &str = ".local/share/com.ai-toolbox/ai-toolbox.db";

fn keys(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

fn with_stores(h: &Home) {
    h.db(
        ".cc-switch/cc-switch.db",
        &fixture("ccs/schema-v20.sql"),
        false,
    );
    h.write(".cc-switch/settings.json", &fixture("ccs/settings.json"));
    h.db(AITB_DB, &fixture("ai-toolbox/schema-v29.sql"), true);
}

fn run(h: &Home, config: Option<&str>, auth: Option<&str>, changed: &Changed<'_>) -> Attribution {
    let codex = h.path(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    if let Some(c) = config {
        std::fs::write(codex.join("config.toml"), c).unwrap();
    }
    let reads = [
        h.read(ImportSource::Ccs, Os::Linux),
        h.read(ImportSource::AiToolbox, Os::Linux),
    ];
    let live = LiveFiles {
        codex_home: &codex,
        config_toml: config.map(str::as_bytes),
        auth_json: auth.map(str::as_bytes),
    };
    attribute_with(&live, changed, &reads, &h.home())
}

fn evidence_keys(a: &Attribution) -> Vec<&str> {
    a.evidence.iter().map(|e| e.key.as_str()).collect()
}

#[test]
fn cc_switch_is_named_with_its_current_provider() {
    let h = Home::new();
    with_stores(&h);
    let k = keys(&["model_provider", "openai_base_url", "model"]);
    let a = run(
        &h,
        Some(&fixture("live/ccs-kimi.config.toml")),
        None,
        &Changed {
            keys: &k,
            auth: true,
        },
    );
    assert_eq!(
        a.source,
        ChangeSource::CcSwitch {
            provider: Some("Kimi".into())
        }
    );
    let ev = evidence_keys(&a);
    assert!(ev.contains(&"ownership.evidence.store_current"), "{ev:?}");
    assert!(ev.contains(&"ownership.evidence.marker_bearer"), "{ev:?}");
}

#[test]
fn ai_toolbox_is_named_with_its_applied_provider() {
    let h = Home::new();
    with_stores(&h);
    let k = keys(&["model_provider"]);
    let a = run(
        &h,
        Some(&fixture("live/aitb-relay.config.toml")),
        Some(&fixture("live/aitb-relay.auth.json")),
        &Changed {
            keys: &k,
            auth: true,
        },
    );
    assert_eq!(
        a.source,
        ChangeSource::AiToolbox {
            provider: Some("Example Relay".into())
        }
    );
    let ev = evidence_keys(&a);
    assert!(ev.contains(&"ownership.evidence.store_current"), "{ev:?}");
    assert!(ev.contains(&"ownership.evidence.marker_schema"), "{ev:?}");
}

#[test]
fn markers_name_the_tool_when_its_store_is_not_readable() {
    // No stores at all: AI Toolbox's schema line and cc-switch's first-write
    // backup still name the writer, without a provider.
    let h = Home::new();
    let k = keys(&["model_provider"]);
    let changed = Changed {
        keys: &k,
        auth: false,
    };
    let a = run(
        &h,
        Some(&fixture("live/aitb-relay.config.toml")),
        None,
        &changed,
    );
    assert_eq!(a.source, ChangeSource::AiToolbox { provider: None });

    let h = Home::new();
    let codex = h.path(".codex");
    h.write(
        ".cc-switch/backups/live-first-write/0a1b2c3d4e5f-config.toml.source",
        &codex.join("config.toml").display().to_string(),
    );
    h.write(
        ".cc-switch/backups/live-first-write/0a1b2c3d4e5f-config.toml",
        &fixture("live/superba-krill-pat.config.toml"),
    );
    let a = run(
        &h,
        Some(&fixture("live/ccs-kimi.config.toml")),
        None,
        &changed,
    );
    assert_eq!(a.source, ChangeSource::CcSwitch { provider: None });
    assert!(evidence_keys(&a).contains(&"ownership.evidence.marker_backup"));
}

#[test]
fn a_provider_the_tool_does_not_mark_current_still_counts() {
    // cc-switch's "PackyCode" row is not current, but Codex now uses its address.
    let h = Home::new();
    with_stores(&h);
    let config = "model_provider = \"packycode\"\n\n[model_providers.packycode]\nname = \"packycode\"\nbase_url = \"https://codex-api.packycode.example/v1/\"\nwire_api = \"responses\"\n";
    let k = keys(&["model_provider"]);
    let a = run(
        &h,
        Some(config),
        None,
        &Changed {
            keys: &k,
            auth: false,
        },
    );
    assert_eq!(
        a.source,
        ChangeSource::CcSwitch {
            provider: Some("PackyCode".into())
        }
    );
    assert_eq!(evidence_keys(&a), ["ownership.evidence.store_other"]);
}

#[test]
fn without_a_tool_the_change_is_put_down_to_what_changed() {
    let h = Home::new();
    with_stores(&h);
    let superba = fixture("live/superba-krill-pat.config.toml");
    // The app's model picker.
    let model = superba.replace("gpt-5.5", "gpt-5.6-sol");
    let k = keys(&["model"]);
    let a = run(
        &h,
        Some(&model),
        None,
        &Changed {
            keys: &k,
            auth: false,
        },
    );
    assert_eq!(a.source, ChangeSource::CodexApp);
    // `codex login` in the app.
    let login = r#"{"auth_mode":"chatgpt","tokens":{"id_token":"x.e30.y","access_token":"a","refresh_token":"r","account_id":"acct"}}"#;
    let a = run(
        &h,
        Some(&superba),
        Some(login),
        &Changed {
            keys: &[],
            auth: true,
        },
    );
    assert_eq!(a.source, ChangeSource::CodexLogin);
    // A hand edit pointing Codex somewhere no tool knows.
    let hand = superba.replace(
        "https://api.krill-code.net/backend-api/codex",
        "https://other.example.org/v1",
    );
    let k = keys(&["openai_base_url"]);
    let a = run(
        &h,
        Some(&hand),
        None,
        &Changed {
            keys: &k,
            auth: false,
        },
    );
    assert_eq!(a.source, ChangeSource::Unknown);
    assert!(a.evidence.is_empty());
}

#[test]
fn local_gateways_and_placeholder_keys_are_markers() {
    let h = Home::new();
    let k = keys(&["model_provider"]);
    let changed = Changed {
        keys: &k,
        auth: false,
    };
    let ccs_proxy = "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"custom\"\nbase_url = \"http://127.0.0.1:15721/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"PROXY_MANAGED\"\n";
    let a = run(&h, Some(ccs_proxy), None, &changed);
    assert_eq!(a.source, ChangeSource::CcSwitch { provider: None });
    let aitb_gateway = "#:schema none\nmodel_provider = \"custom\"\n\n[model_providers.custom]\nname = \"OpenAI\"\nbase_url = \"http://127.0.0.1:37123/openai/v1\"\nwire_api = \"responses\"\n";
    let a = run(
        &h,
        Some(aitb_gateway),
        Some("{\n  \"OPENAI_API_KEY\": \"ai-toolbox-gateway\",\n  \"auth_mode\": \"apikey\"\n}"),
        &changed,
    );
    assert_eq!(a.source, ChangeSource::AiToolbox { provider: None });
    assert!(evidence_keys(&a).contains(&"ownership.evidence.marker_gateway"));
}

#[test]
fn an_applied_chatgpt_account_must_be_the_signed_in_one() {
    // AI Toolbox's applied official account is alice@example.com.
    let h = Home::new();
    with_stores(&h);
    let superba = fixture("live/superba-krill-pat.config.toml").replace(
        "model_provider = \"openai\"\nopenai_base_url = \"https://api.krill-code.net/backend-api/codex\"\nchatgpt_base_url = \"https://api.krill-code.net/backend-api/\"\n",
        "",
    );
    let login = |email_claims: &str| {
        format!(
            r#"{{"auth_mode":"chatgpt","tokens":{{"id_token":"e30.{email_claims}.sig","access_token":"a","refresh_token":"r","account_id":"acct"}}}}"#
        )
    };
    let changed = Changed {
        keys: &[],
        auth: true,
    };
    // eyJlbWFpbCI6ImFsaWNlQGV4YW1wbGUuY29tIn0 = {"email":"alice@example.com"}
    let a = run(
        &h,
        Some(&superba),
        Some(&login("eyJlbWFpbCI6ImFsaWNlQGV4YW1wbGUuY29tIn0")),
        &changed,
    );
    assert_eq!(
        a.source,
        ChangeSource::AiToolbox {
            provider: Some("alice@example.com".into())
        }
    );
    assert_eq!(evidence_keys(&a), ["ownership.evidence.store_official"]);
    // Someone else signed in with Codex itself.
    // eyJlbWFpbCI6ImJvYkBleGFtcGxlLmNvbSJ9 = {"email":"bob@example.com"}
    let a = run(
        &h,
        Some(&superba),
        Some(&login("eyJlbWFpbCI6ImJvYkBleGFtcGxlLmNvbSJ9")),
        &changed,
    );
    assert_eq!(a.source, ChangeSource::CodexLogin);
}
