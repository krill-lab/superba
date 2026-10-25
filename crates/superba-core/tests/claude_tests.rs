//! Claude Code types (user bug 79): capability lists, slot variables, endpoint
//! URLs, presets suggested per provider, the owned keys and the values a
//! configuration wants (the key redacted in Debug).

use std::collections::BTreeSet;

use superba_core::claude::{
    self, ENV_API_KEY, ENV_AUTH_TOKEN, ModelFamily, anthropic_presets, capabilities_text, desired,
    env_path, managed_paths, parse_capabilities, suggested_capabilities,
};
use superba_core::{
    AnthropicAuth, AnthropicEndpoint, ClaudeCapability, ClaudeModels, ClaudeSlot, Secret, SlotModel,
};

#[test]
fn capability_lists_round_trip_in_a_fixed_order() {
    let (caps, unknown) = parse_capabilities(" Thinking, effort,max-effort, nope ,");
    assert_eq!(unknown, ["nope"]);
    assert_eq!(capabilities_text(&caps), "effort,max_effort,thinking");
    assert_eq!(ClaudeCapability::ALL.len(), 8);
    for c in ClaudeCapability::ALL {
        assert_eq!(ClaudeCapability::parse(c.as_str()), Some(c));
    }
    assert_eq!(capabilities_text(&BTreeSet::new()), "");
}

#[test]
fn slot_variables_are_named_like_claude_code_reads_them() {
    assert_eq!(
        ClaudeSlot::Opus.env_vars(),
        [
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES",
        ]
        .map(String::from)
    );
    assert_eq!(
        ClaudeSlot::Custom.env_capabilities(),
        "ANTHROPIC_CUSTOM_MODEL_OPTION_SUPPORTED_CAPABILITIES"
    );
    // Every slot variable is owned, plus both key variables, model and the
    // picker.
    let owned = managed_paths();
    for slot in ClaudeSlot::ALL {
        for v in slot.env_vars() {
            assert!(owned.contains(&env_path(&v)), "{v}");
        }
    }
    assert!(owned.contains(&env_path(ENV_AUTH_TOKEN)));
    assert!(owned.contains(&env_path(ENV_API_KEY)));
    assert!(owned.contains(&"model".to_owned()));
    assert!(owned.contains(&"modelPicker".to_owned()));
}

#[test]
fn endpoint_urls_are_checked() {
    assert!(AnthropicEndpoint::parse_base("https://api.deepseek.com/anthropic").is_ok());
    assert!(AnthropicEndpoint::parse_base("http://127.0.0.1:8080/claude").is_ok());
    assert_eq!(
        AnthropicEndpoint::parse_base("http://example.com"),
        Err("claude.endpoint.invalid.http")
    );
    assert_eq!(
        AnthropicEndpoint::parse_base("https://x.example/v1/messages"),
        Err("claude.endpoint.invalid.messages")
    );
    assert_eq!(
        AnthropicEndpoint::parse_base("https://x.example/?a=1"),
        Err("claude.endpoint.invalid.query")
    );
    assert_eq!(
        AnthropicEndpoint::parse_base(" "),
        Err("claude.endpoint.invalid.empty")
    );
    let e = AnthropicEndpoint::new(url::Url::parse("https://x.example/anthropic/").unwrap());
    assert_eq!(e.base_text(), "https://x.example/anthropic");
    assert_eq!(
        e.join("v1/models").as_str(),
        "https://x.example/anthropic/v1/models"
    );
    assert_eq!(
        AnthropicAuth::parse("x-api-key"),
        Some(AnthropicAuth::ApiKey)
    );
    assert_eq!(AnthropicAuth::default().env_var(), ENV_AUTH_TOKEN);
}

#[test]
fn presets_are_https_and_unique() {
    let presets = anthropic_presets();
    let mut ids = BTreeSet::new();
    for p in &presets {
        assert!(ids.insert(p.id), "duplicate {}", p.id);
        assert!(p.doc_url.starts_with("https://"), "{}", p.id);
        if let Some(b) = p.base_url {
            assert!(AnthropicEndpoint::parse_base(b).is_ok(), "{b}");
        }
    }
    // A compatible DeepSeek provider gets the DeepSeek preset.
    let site = superba_core::Site {
        id: "ds".into(),
        lines: vec![superba_core::Line {
            id: "default".into(),
            name: superba_core::LocalizedText::new("", "DeepSeek"),
            origin: superba_core::Origin::parse("https://api.deepseek.com").unwrap(),
            source: superba_core::LineSource::User,
        }],
        ..superba_core::catalog::compatible_template_site()
    };
    let profile = superba_core::catalog::builtin_profiles().remove(0);
    let profile = superba_core::Profile {
        builtin: None,
        site_id: "ds".into(),
        ..profile
    };
    let e = claude::effective_endpoint(&site, &profile).unwrap();
    assert_eq!(e.base_text(), "https://api.deepseek.com/anthropic");
    assert_eq!(e.preset.as_deref(), Some("deepseek"));
    // Krill: its line, all models.
    let krill = superba_core::catalog::krill_site();
    let p = superba_core::Profile {
        site_id: krill.id.clone(),
        ..profile
    };
    let e = claude::effective_endpoint(&krill, &p).unwrap();
    assert_eq!(e.base_text(), "https://api.kbsfdxc.net");
    assert_eq!(e.preset.as_deref(), Some(claude::PRESET_KRILL));
}

#[test]
fn suggestions_by_family_are_hints() {
    assert_eq!(ModelFamily::of("vendor/gpt-6-sol"), ModelFamily::Gpt);
    assert_eq!(ModelFamily::of("claude-opus-5-5"), ModelFamily::Claude);
    assert!(suggested_capabilities("claude-opus-5-5", None).is_empty());
    assert!(!suggested_capabilities("gpt-5-chat-latest", None).contains(&ClaudeCapability::Effort));
    assert!(suggested_capabilities("glm-5.3", None).contains(&ClaudeCapability::Thinking));
}

#[test]
fn desired_values_redact_the_key() {
    let mut models = ClaudeModels::default();
    models
        .slots
        .insert(ClaudeSlot::Sonnet, SlotModel::new("deepseek-v4"));
    let e = AnthropicEndpoint::new(url::Url::parse("https://api.deepseek.com/anthropic").unwrap());
    let d = desired(
        &e,
        &Secret::new("sk-very-secret-value-1234"),
        &models,
        &|_| None,
    );
    assert_eq!(
        d.get(&env_path("ANTHROPIC_DEFAULT_SONNET_MODEL")),
        Some(&serde_json::json!("deepseek-v4"))
    );
    assert_eq!(
        d.get(&env_path(
            "ANTHROPIC_DEFAULT_SONNET_MODEL_SUPPORTED_CAPABILITIES"
        )),
        Some(&serde_json::json!("effort,max_effort,thinking"))
    );
    assert!(d.owns(&env_path(ENV_API_KEY)));
    assert_eq!(d.get(&env_path(ENV_API_KEY)), None);
    let debug = format!("{d:?}");
    assert!(!debug.contains("sk-very-secret"), "{debug}");
}
