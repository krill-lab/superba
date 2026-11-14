//! A Krill catalog keeps only the models Codex offers as its own: Krill's
//! `/v1/models` (merged in with model source "auto") also names grok, gemini,
//! gpt-image and the like, which the app would list as bare entries.

use std::collections::BTreeMap;

use superba_core::{ModelListKind, ModelOverride, ModelSpec};
use superba_models::BundledCatalog;
use superba_switch::models::krill_keeps;

fn spec(slug: &str) -> ModelSpec {
    ModelSpec {
        slug: slug.into(),
        ..ModelSpec::default()
    }
}

#[test]
fn krill_keeps_codex_models_and_drops_the_rest() {
    let template = BundledCatalog {
        codex_version: None,
        entries: vec![serde_json::json!({"slug": "gpt-5.5"})],
        required_fields: Vec::new(),
    };
    let feed = vec![spec("gpt-6.1-terra")];
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "kimi-k3".to_owned(),
        ModelOverride {
            hidden: true,
            ..ModelOverride::default()
        },
    );
    let keeps = |m: &ModelSpec| krill_keeps(m, &template, &feed, &overrides);

    // From Krill's Codex list: its native entry, or named by that list.
    let native = ModelSpec {
        raw: Some(serde_json::json!({"slug": "gpt-6.1-sol"})),
        ..spec("gpt-6.1-sol")
    };
    assert!(keeps(&native));
    let codex_listed = ModelSpec {
        lists: vec![ModelListKind::Codex, ModelListKind::Standard],
        ..spec("gpt-6-astra")
    };
    assert!(keeps(&codex_listed));
    // Known to the app's bundled list or the signed model list.
    assert!(keeps(&spec("gpt-5.5")));
    assert!(keeps(&spec("gpt-6.1-terra")));
    // A per-model value that needs a catalog entry.
    assert!(keeps(&spec("kimi-k3")));
    // Only in /v1/models: dropped.
    let standard_only = ModelSpec {
        lists: vec![ModelListKind::Standard],
        ..spec("grok-4.5")
    };
    assert!(!keeps(&standard_only));
    assert!(!keeps(&spec("gpt-image-2")));
}

#[test]
fn mixed_models_leave_out_media_models_and_get_readable_names() {
    use superba_core::{display_name_of, is_chat_model};
    for chat in [
        "deepseek-v4.1-flash",
        "kimi-k3.1-pro",
        "GLM-5.3",
        "grok-4.7",
        "gemini-4-argon",
        "doubao-seed-2-1-pro",
    ] {
        assert!(is_chat_model(chat), "{chat}");
    }
    for media in [
        "gpt-image-2",
        "grok-imagine-video-1.5",
        "grok-imagine-image",
        "text-embedding-3-large",
        "tts-1",
        "whisper-1",
        "gpt-4o-realtime-preview",
        "sora-2",
    ] {
        assert!(!is_chat_model(media), "{media}");
    }
    assert_eq!(display_name_of("deepseek-v4.1-flash"), "DeepSeek V4.1 Flash");
    assert_eq!(display_name_of("kimi-k3.1-pro"), "Kimi K3.1 Pro");
    assert_eq!(display_name_of("gpt-6.1-sol"), "GPT 6.1 Sol");
    assert_eq!(display_name_of("GLM-5.3"), "GLM-5.3");
    assert_eq!(display_name_of("krill/deepseek-x"), "krill/deepseek-x");
}
