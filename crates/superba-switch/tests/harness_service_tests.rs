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
    assert!(
        err.contains("direct connections need a native endpoint"),
        "{err}"
    );
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
async fn workbuddy_uses_a_responses_member_through_an_enabled_chat_conversion() {
    use superba_core::RouterInstallRequest;
    use superba_router::translate::{ir::Protocol, routing::Conversion};

    let w = world();
    add_compatible(&w);
    let s = w.switcher();
    s.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("compat")],
    })
    .await
    .unwrap();
    let mut st = settings("compat");
    st.route = HarnessRoute::Routed;
    st.router_url = Some(Url::parse("http://127.0.0.1:18431").unwrap());
    s.save_harness_settings(HarnessId::WorkBuddy, st)
        .await
        .unwrap();
    s.set_harness_router_token(
        HarnessId::WorkBuddy,
        Some(Secret::new("rt-workbuddy-translation")),
    )
    .await
    .unwrap();
    let view = s.harness_view(HarnessId::WorkBuddy).await.unwrap();
    assert_eq!(view.protocols, [HarnessProtocol::Chat]);
    assert_eq!(
        view.endpoint.unwrap().base_text(),
        "http://127.0.0.1:18431/openai/v1"
    );
    let plan = s
        .harness_plan(HarnessId::WorkBuddy, HarnessPlanKind::Apply)
        .await
        .unwrap();
    assert_eq!(plan.protocol, Some(HarnessProtocol::Chat));
    s.harness_apply(plan, false).await.unwrap();
    let text = std::fs::read_to_string(w.ctx.home_dir.join(".workbuddy/models.json")).unwrap();
    let written: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        written["models"][0]["url"],
        "http://127.0.0.1:18431/openai/v1"
    );
    assert!(!text.contains("sk-compat-key-1234"));

    // Turning this direction off removes it from both the view and apply.
    w.router
        .written
        .lock()
        .unwrap()
        .last_mut()
        .unwrap()
        .translation
        .disabled
        .insert(Conversion {
            from: Protocol::Chat,
            to: Protocol::Responses,
        });
    let view = s.harness_view(HarnessId::WorkBuddy).await.unwrap();
    assert!(view.protocols.is_empty());
    assert!(view.endpoint.is_none());
    assert!(
        s.harness_plan(HarnessId::WorkBuddy, HarnessPlanKind::Apply)
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
    assert!(err.contains("Set up the local router"), "{err}");
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

#[tokio::test]
async fn failed_model_lists_are_visible_and_prevent_apply_without_touching_files() {
    let w = world();
    let svc = w.switcher();
    svc.save_harness_settings(HarnessId::Pi, settings("work"))
        .await
        .unwrap();
    let plan = svc
        .harness_plan(HarnessId::Pi, HarnessPlanKind::Apply)
        .await
        .unwrap();
    svc.harness_apply(plan, false).await.unwrap();
    let path = w.ctx.home_dir.join(".pi/agent/models.json");
    let before = std::fs::read(&path).unwrap();
    *w.prober.model_list.lock().unwrap() = Some(Err(superba_core::Error::other(
        "model endpoint unavailable",
    )));
    let view = svc.harness_view(HarnessId::Pi).await.unwrap();
    assert!(view.notes.iter().any(|n| {
        n.text
            .render_fallback()
            .contains("model endpoint unavailable")
    }));
    let error = svc
        .harness_plan(HarnessId::Pi, HarnessPlanKind::Apply)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("model endpoint unavailable"));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[tokio::test]
async fn preview_never_saves_a_draft_or_changes_configuration_files() {
    let w = world();
    let svc = w.switcher();
    let view = svc
        .harness_preview(HarnessId::OpenCode, settings("work"))
        .await
        .unwrap();
    assert!(view.endpoint.is_some());
    let plan = svc
        .harness_plan_preview(HarnessId::OpenCode, settings("work"))
        .await
        .unwrap();
    assert!(!plan.changes.is_empty());
    assert!(!w.ctx.data_dir.harness_config_json().exists());
    assert!(view.files.iter().all(|f| !f.path.exists()));
    assert_eq!(
        svc.harness_view(HarnessId::OpenCode)
            .await
            .unwrap()
            .settings
            .provider,
        None
    );
}

#[tokio::test]
async fn harness_editor_backs_up_valid_edits_refuses_stale_and_invalid_saves() {
    use superba_core::SecretBytes;
    use superba_core::harness::HarnessFileSave;
    let w = world();
    let svc = w.switcher();
    svc.save_harness_settings(HarnessId::Pi, settings("work"))
        .await
        .unwrap();
    let plan = svc
        .harness_plan(HarnessId::Pi, HarnessPlanKind::Apply)
        .await
        .unwrap();
    svc.harness_apply(plan, false).await.unwrap();
    let doc = svc
        .harness_file(HarnessId::Pi, "settings.json".into())
        .await
        .unwrap();
    let save = HarnessFileSave {
        harness: HarnessId::Pi,
        file: doc.file.key.clone(),
        base_sha256: doc.sha256.clone(),
        contents: SecretBytes::new(b"{broken".to_vec()),
    };
    assert!(svc.save_harness_file(save).await.is_err());
    let mut value: Value = serde_json::from_slice(doc.contents.as_ref().unwrap().expose()).unwrap();
    value["theme"] = json!("light");
    let save = HarnessFileSave {
        harness: HarnessId::Pi,
        file: doc.file.key.clone(),
        base_sha256: doc.sha256,
        contents: SecretBytes::new(serde_json::to_vec(&value).unwrap()),
    };
    let saved = svc.save_harness_file(save.clone()).await.unwrap();
    assert!(matches!(
        svc.save_harness_file(save).await,
        Err(superba_core::Error::Conflict { .. })
    ));
    assert_ne!(
        saved.contents.unwrap().expose(),
        doc.contents.unwrap().expose()
    );
    let backups = svc.harness_backups(HarnessId::Pi).await.unwrap();
    assert!(
        backups
            .iter()
            .any(|b| b.source == superba_core::BackupSource::BeforeEdit)
    );
    assert!(
        svc.harness_file(HarnessId::Pi, "../../elsewhere".into())
            .await
            .is_err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn opencode_version_probe_has_disposable_home_and_data_directories() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    let mut w = world();
    let bin = w.ctx.home_dir.join("test-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("opencode");
    std::fs::write(&script,"#!/bin/sh\n/bin/mkdir -p \"$HOME/.local/share/opencode/log\"\nprintf log > \"$HOME/.local/share/opencode/log/opencode.log\"\nprintf data > \"$XDG_DATA_HOME/opencode.log\"\nprintf cwd > opencode.log\nprintf '2.0.32\\n'\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    w.ctx.env = Arc::new(
        superba_core::MapEnv::new()
            .with("PATH", bin.as_os_str())
            .with("SUPERBA_SECRET_BACKEND", "file"),
    );
    let view = w
        .switcher()
        .harness_view(HarnessId::OpenCode)
        .await
        .unwrap();
    assert_eq!(view.install.unwrap().version.as_deref(), Some("2.0.32"));
    assert!(
        !w.ctx
            .home_dir
            .join(".local/share/opencode/log/opencode.log")
            .exists()
    );
    assert!(!bin.join("opencode.log").exists());
    assert_eq!(
        std::fs::read_dir(w.ctx.data_dir.root().join("probes"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn reviewed_backup_restore_refuses_an_edit_after_preview() {
    let w = world();
    let sw = w.switcher();
    let view = sw
        .save_harness_settings(HarnessId::OpenCode, settings("work"))
        .await
        .unwrap();
    let path = &view.files[0].path;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "{\"theme\":\"dark\"}").unwrap();
    let plan = sw
        .harness_plan(HarnessId::OpenCode, HarnessPlanKind::Apply)
        .await
        .unwrap();
    sw.harness_apply(plan, false).await.unwrap();
    let doc = sw
        .harness_file(HarnessId::OpenCode, view.files[0].key.clone())
        .await
        .unwrap();
    let backups = sw.harness_backups(HarnessId::OpenCode).await.unwrap();
    let backup = backups
        .iter()
        .find(|b| b.source == superba_core::BackupSource::Original)
        .unwrap();
    std::fs::write(path, "{\"theme\":\"light\"}").unwrap();
    assert!(
        sw.harness_restore_reviewed_backup(HarnessId::OpenCode, backup.id.clone(), doc.sha256)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "{\"theme\":\"light\"}"
    );
}

#[tokio::test]
async fn local_router_apply_binds_the_tool_provider_without_adding_codex_failover_members() {
    use superba_router::token::{presenter_of, verify_source_token};
    let w = world();
    *w.router.port.lock().unwrap() = Some(18431);
    let sw = w.switcher();
    let mut st = settings("work");
    st.route = HarnessRoute::Routed;
    let plan = sw
        .harness_plan_preview(HarnessId::OpenCode, st.clone())
        .await
        .unwrap();
    let target = plan.desired.as_ref().unwrap();
    let source =
        verify_source_token(ROUTER_TOKEN.as_bytes(), target.key.expose().as_bytes()).unwrap();
    let who = presenter_of(&source);
    assert_eq!(who.harness, Some(superba_core::RequestHarness::OpenCode));
    assert_eq!(who.member.as_deref(), Some("work"));
    assert!(who.host.is_none());
    assert!(w.router.written.lock().unwrap().is_empty());
    assert!(!w.ctx.data_dir.harness_config_json().exists());
    sw.save_harness_settings(HarnessId::OpenCode, st)
        .await
        .unwrap();
    sw.harness_apply(plan, false).await.unwrap();
    let configurations = w.router.written.lock().unwrap();
    let config = configurations.last().unwrap();
    assert!(config.member("work").is_some());
    assert!(!config.serves_codex("work"));
    let text =
        std::fs::read_to_string(w.ctx.home_dir.join(".config/opencode/opencode.json")).unwrap();
    assert!(text.contains("http://127.0.0.1:18431/openai/v1"));
    assert!(!text.contains(KEY));
    let state = superba_config::harness::HarnessWriter::new(w.ctx.clone(), HarnessId::OpenCode)
        .state()
        .unwrap()
        .unwrap();
    assert_eq!(state.route, HarnessRoute::Routed);
    assert_eq!(state.provider, Some(ProfileId::new("work")));
}

#[tokio::test]
async fn following_codex_only_marks_the_tool_pending_and_keeps_its_applied_router_provider() {
    let w = world();
    add_compatible(&w);
    *w.router.port.lock().unwrap() = Some(18431);
    w.config.state.lock().unwrap().active = Some(superba_core::ActiveState {
        profile_id: Some(ProfileId::new("work")),
        site_id: None,
        line_id: None,
        auth_mode: Some(AuthMode::Pat),
        generation: 1,
        applied_at: superba_core::Timestamp(T0),
    });
    let sw = w.switcher();
    let st = HarnessSettings {
        route: HarnessRoute::Routed,
        ..Default::default()
    };
    sw.save_harness_settings(HarnessId::OpenCode, st)
        .await
        .unwrap();
    let plan = sw
        .harness_plan(HarnessId::OpenCode, HarnessPlanKind::Apply)
        .await
        .unwrap();
    sw.harness_apply(plan, false).await.unwrap();
    let path = w.ctx.home_dir.join(".config/opencode/opencode.json");
    let before = std::fs::read(&path).unwrap();
    w.config
        .state
        .lock()
        .unwrap()
        .active
        .as_mut()
        .unwrap()
        .profile_id = Some(ProfileId::new("compat"));
    let view = sw.harness_view(HarnessId::OpenCode).await.unwrap();
    assert!(!view.pending.is_empty());
    assert_eq!(view.applied.unwrap().provider, Some(ProfileId::new("work")));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // A later router rewrite (another tool's apply) retains the old applied provider.
    let st = HarnessSettings {
        route: HarnessRoute::Routed,
        ..settings("compat")
    };
    sw.save_harness_settings(HarnessId::Pi, st).await.unwrap();
    let plan = sw
        .harness_plan(HarnessId::Pi, HarnessPlanKind::Apply)
        .await
        .unwrap();
    sw.harness_apply(plan, false).await.unwrap();
    let configs = w.router.written.lock().unwrap();
    let last = configs.last().unwrap();
    assert!(last.member("work").is_some());
    assert!(last.member("compat").is_some());
    assert!(!last.serves_codex("work"));
    assert!(!last.serves_codex("compat"));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[tokio::test]
async fn failing_to_prepare_the_router_does_not_write_tool_files() {
    let w = world();
    *w.router.port.lock().unwrap() = Some(18431);
    *w.router.fail_install.lock().unwrap() =
        Some(superba_core::Error::other("cannot start router"));
    let sw = w.switcher();
    sw.save_harness_settings(
        HarnessId::Pi,
        HarnessSettings {
            route: HarnessRoute::Routed,
            ..settings("work")
        },
    )
    .await
    .unwrap();
    let plan = sw
        .harness_plan(HarnessId::Pi, HarnessPlanKind::Apply)
        .await
        .unwrap();
    assert!(
        sw.harness_apply(plan, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot start router")
    );
    assert!(!w.ctx.home_dir.join(".pi/agent/models.json").exists());
    assert!(
        superba_config::harness::HarnessWriter::new(w.ctx.clone(), HarnessId::Pi)
            .state()
            .unwrap()
            .is_none()
    );
}
