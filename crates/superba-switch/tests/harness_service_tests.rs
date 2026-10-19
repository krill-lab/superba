//! The harnesses through the service: which provider, protocol, endpoint and
//! key an apply writes (Krill's line, declared endpoints, routed mode with
//! the router token from the key store), the model list, and apply /
//! restore of real files in the test home.

mod fakes;

use fakes::*;
use serde_json::{Value, json};
use superba_core::harness::{
    EndpointSource, HarnessId, HarnessPlanKind, HarnessProtocol, HarnessRoute, HarnessSettings,
    ProviderProtocols,
};
use superba_core::{AuthMode, ProfileId, Secret, SiteId, SwitchService};
use url::Url;

fn model_list() -> superba_core::ModelList {
    serde_json::from_value(json!({
        "source": { "kind": "upstream", "path": "/backend-api/codex/models", "fetched_at": 0 },
        "models": [
            { "slug": "gpt-5.6-luna", "display_name": "GPT-5.6 Luna", "context_window": 272000,
              "reasoning_levels": [{ "effort": "low" }, { "effort": "medium" }, { "effort": "high" }],
              "input_modalities": ["text", "image"] },
            { "slug": "hidden-one", "visibility": "none" },
            { "slug": "gpt-5.4-mini", "context_window": 400000 }
        ]
    }))
    .unwrap()
}

fn world() -> World {
    let w = World::new();
    *w.prober.model_list.lock().unwrap() = Some(Ok(model_list()));
    w
}

fn settings(provider: &str) -> HarnessSettings {
    HarnessSettings {
        provider: Some(ProfileId::new(provider)),
        ..HarnessSettings::default()
    }
}

#[tokio::test]
async fn harnesses_use_the_shared_model_source_selection_and_anthropic_base() {
    use superba_core::ModelSource;
    use superba_core::claude::AnthropicEndpoint;

    for source in [
        None,
        Some(ModelSource::Auto),
        Some(ModelSource::Codex),
        Some(ModelSource::Standard),
    ] {
        let w = world();
        let base = Url::parse("https://messages.example.com/anthropic").unwrap();
        {
            let mut set = w.profiles.set.lock().unwrap();
            let p = set
                .profiles
                .iter_mut()
                .find(|p| p.id == ProfileId::new("work"))
                .unwrap();
            p.model_source = source;
            p.anthropic = Some(AnthropicEndpoint::new(base.clone()));
        }
        let s = w.switcher();
        let mut st = settings("work");
        st.protocol = Some(HarnessProtocol::Anthropic);
        s.save_harness_settings(HarnessId::Pi, st).await.unwrap();
        w.prober.model_list_sources.lock().unwrap().clear();

        let plan = s
            .harness_plan(HarnessId::Pi, HarnessPlanKind::Apply)
            .await
            .unwrap();
        assert_eq!(
            *w.prober.model_list_sources.lock().unwrap(),
            vec![(
                Some(source.unwrap_or(ModelSource::Auto)),
                Some(base.clone())
            )],
        );
        s.harness_apply(plan, false).await.unwrap();
        let file: Value = serde_json::from_slice(
            &std::fs::read(w.ctx.home_dir.join(".pi/agent/models.json")).unwrap(),
        )
        .unwrap();
        let provider = &file["providers"]["superba"];
        assert_eq!(provider["baseUrl"], base.as_str());
        assert_eq!(provider["api"], "anthropic-messages");
        assert_eq!(provider["models"][0]["id"], "gpt-5.6-luna");
    }
}

#[tokio::test]
async fn krill_provider_gives_its_line_for_every_protocol_and_its_models() {
    let w = world();
    let s = w.switcher();
    s.save_harness_settings(HarnessId::Pi, settings("work"))
        .await
        .unwrap();
    let view = s.harness_view(HarnessId::Pi).await.unwrap();
    assert_eq!(
        view.protocols,
        vec![
            HarnessProtocol::Responses,
            HarnessProtocol::Chat,
            HarnessProtocol::Anthropic
        ]
    );
    let e = view.endpoint.unwrap();
    assert_eq!(e.protocol, HarnessProtocol::Responses);
    assert_eq!(e.source, EndpointSource::Krill);
    assert_eq!(e.base_text(), format!("{OVERSEAS}/v1"));
    // Apply: the models of the list (hidden ones left out), the provider's key.
    let plan = s
        .harness_plan(HarnessId::Pi, HarnessPlanKind::Apply)
        .await
        .unwrap();
    let report = s.harness_apply(plan, false).await.unwrap();
    assert_eq!(report.files.len(), 2);
    let v: Value = serde_json::from_slice(
        &std::fs::read(w.ctx.home_dir.join(".pi/agent/models.json")).unwrap(),
    )
    .unwrap();
    let p = &v["providers"]["superba"];
    assert_eq!(p["apiKey"], KEY);
    let ids: Vec<&str> = p["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gpt-5.6-luna", "gpt-5.4-mini"]);
    assert_eq!(p["models"][0]["contextWindow"], 272000);
    // Listed as applied, then restored.
    let list = s.harness_list().await.unwrap();
    assert_eq!(list.len(), 5);
    let pi = list.iter().find(|x| x.harness == HarnessId::Pi).unwrap();
    assert!(pi.applied);
    assert_eq!(pi.applied_provider, Some(ProfileId::new("work")));
    let plan = s
        .harness_plan(HarnessId::Pi, HarnessPlanKind::Restore)
        .await
        .unwrap();
    s.harness_apply(plan, false).await.unwrap();
    assert!(!w.ctx.home_dir.join(".pi").exists());
}

#[tokio::test]
async fn workbuddy_takes_krills_chat_endpoint_and_chosen_models() {
    let w = world();
    let s = w.switcher();
    let mut st = settings("work");
    st.models = vec!["gpt-5.4-mini".into(), "by-hand".into()];
    st.default_model = Some("by-hand".into());
    s.save_harness_settings(HarnessId::WorkBuddy, st)
        .await
        .unwrap();
    let plan = s
        .harness_plan(HarnessId::WorkBuddy, HarnessPlanKind::Apply)
        .await
        .unwrap();
    assert_eq!(plan.protocol, Some(HarnessProtocol::Chat));
    s.harness_apply(plan, false).await.unwrap();
    let v: Value = serde_json::from_slice(
        &std::fs::read(w.ctx.home_dir.join(".workbuddy/models.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["availableModels"], json!(["gpt-5.4-mini", "by-hand"]));
    assert_eq!(v["models"][0]["url"], format!("{OVERSEAS}/v1"));
    assert_eq!(v["models"][0]["maxInputTokens"], 400000);
    assert!(v["models"][1].get("maxInputTokens").is_none());
}

fn add_compatible(w: &World) {
    let base = Url::parse("https://api.example.com/v1").unwrap();
    let site =
        superba_switch::new_compatible_site(SiteId::new("example"), "Example", &base).unwrap();
    w.profiles.set.lock().unwrap().custom_sites.push(site);
    let mut p = krill_profile("compat", None, AuthMode::ApiKey);
    p.site_id = SiteId::new("example");
    p.base_url_override = Some(base);
    w.profiles.set.lock().unwrap().profiles.push(p);
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:compat".into(), "sk-compat-key-1234".into());
}

#[tokio::test]
async fn compatible_sites_offer_responses_until_more_endpoints_are_declared() {
    let w = world();
    add_compatible(&w);
    let s = w.switcher();
    s.save_harness_settings(HarnessId::WorkBuddy, settings("compat"))
        .await
        .unwrap();
    let err = s
        .harness_plan(HarnessId::WorkBuddy, HarnessPlanKind::Apply)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no translation until 0.1.2"), "{err}");
    s.save_provider_protocols(
        ProfileId::new("compat"),
        ProviderProtocols {
            chat: Some(Url::parse("https://chat.example.com/v1").unwrap()),
            anthropic: None,
        },
    )
    .await
    .unwrap();
    let plan = s
        .harness_plan(HarnessId::WorkBuddy, HarnessPlanKind::Apply)
        .await
        .unwrap();
    assert_eq!(plan.protocol, Some(HarnessProtocol::Chat));
    // A protocol the provider does not offer is refused for the others too.
    let mut st = settings("compat");
    st.protocol = Some(HarnessProtocol::Anthropic);
    s.save_harness_settings(HarnessId::ZCode, st).await.unwrap();
    assert!(
        s.harness_plan(HarnessId::ZCode, HarnessPlanKind::Apply)
            .await
            .is_err()
    );
    // A protocol the harness does not speak cannot even be saved.
    let mut st = settings("compat");
    st.protocol = Some(HarnessProtocol::Responses);
    assert!(
        s.save_harness_settings(HarnessId::WorkBuddy, st)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn routed_mode_needs_the_router_url_and_token_then_writes_them() {
    let w = world();
    let s = w.switcher();
    let mut st = settings("work");
    st.route = HarnessRoute::Routed;
    s.save_harness_settings(HarnessId::OpenCode, st.clone())
        .await
        .unwrap();
    let err = s
        .harness_plan(HarnessId::OpenCode, HarnessPlanKind::Apply)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("router's URL"), "{err}");
    st.router_url = Some(Url::parse("http://127.0.0.1:18731").unwrap());
    s.save_harness_settings(HarnessId::OpenCode, st)
        .await
        .unwrap();
    let err = s
        .harness_plan(HarnessId::OpenCode, HarnessPlanKind::Apply)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("router token"), "{err}");
    let view = s
        .set_harness_router_token(
            HarnessId::OpenCode,
            Some(Secret::new("rt-harness-token-0001")),
        )
        .await
        .unwrap();
    assert!(view.settings.router_token.is_some());
    assert_eq!(
        view.endpoint.unwrap().base_text(),
        "http://127.0.0.1:18731/openai/v1"
    );
    let plan = s
        .harness_plan(HarnessId::OpenCode, HarnessPlanKind::Apply)
        .await
        .unwrap();
    assert!(
        plan.notes
            .iter()
            .any(|n| n.text.key == "switch.harness.routed")
    );
    s.harness_apply(plan, false).await.unwrap();
    let text =
        std::fs::read_to_string(w.ctx.home_dir.join(".config/opencode/opencode.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let p = &v["providers"]["superba"];
    assert_eq!(p["settings"]["apiKey"], "rt-harness-token-0001");
    assert_eq!(p["settings"]["baseURL"], "http://127.0.0.1:18731/openai/v1");
    assert_eq!(p["name"], "Superba · work (router)");
    assert!(!text.contains(KEY));
    // Clearing the token deletes it from the key store.
    let view = s
        .set_harness_router_token(HarnessId::OpenCode, None)
        .await
        .unwrap();
    assert!(view.settings.router_token.is_none());
}

#[tokio::test]
async fn a_provider_without_a_key_or_an_unknown_one_is_refused() {
    let w = world();
    let s = w.switcher();
    assert!(
        s.save_harness_settings(HarnessId::Dsh, settings("nope"))
            .await
            .is_err()
    );
    w.profiles
        .set
        .lock()
        .unwrap()
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap()
        .key_ref = None;
    s.save_harness_settings(HarnessId::Dsh, settings("work"))
        .await
        .unwrap();
    assert!(
        s.harness_plan(HarnessId::Dsh, HarnessPlanKind::Apply)
            .await
            .is_err()
    );
    let view = s.harness_view(HarnessId::Dsh).await.unwrap();
    assert!(view.endpoint.is_none());
    assert!(
        view.notes
            .iter()
            .any(|n| n.text.key == "switch.harness.not_ready")
    );
}
