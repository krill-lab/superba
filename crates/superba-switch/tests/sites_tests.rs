//! Custom sites in the service: probing (`probe_site`) and the model map in
//! direct mode.

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use superba_core::{
    AuthMode, Error, KeyInput, ModelDefaults, Profile, ProfileId, ResolvedMode, Secret, SiteId,
    SiteProbeRequest, SwitchRequest, SwitchService,
};
use superba_switch::Switcher;

async fn acme(w: &World, sw: &Switcher, map: &[(&str, &str)]) {
    let base = url::Url::parse("https://api.acme.example/v1").unwrap();
    let mut site = superba_switch::new_compatible_site(SiteId::new("acme"), "Acme", &base).unwrap();
    for (codex, mapped) in map {
        site.model_map.insert((*codex).into(), (*mapped).into());
    }
    sw.save_site(site).await.unwrap();
    let p = Profile {
        site_id: SiteId::new("acme"),
        line_id: None,
        model: ModelDefaults {
            auto_compact: None,
            context_window: None,
            speed: None,
            model: Some("gpt-5.5".into()),
            reasoning_effort: None,
        },
        ..krill_profile("acme", None, AuthMode::ApiKey)
    };
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:acme".into(), KEY.into());
    sw.save_profile(p, None).await.unwrap();
}

#[tokio::test]
async fn probe_without_a_key_only_probes_the_line() {
    let w = World::new();
    let sw = w.switcher();
    acme(&w, &sw, &[("gpt-5.5", "vendor/gpt-5.5")]).await;
    let report = sw
        .probe_site(SiteProbeRequest::new(SiteId::new("acme")))
        .await
        .unwrap();
    assert_eq!(report.line_id.as_str(), "default");
    assert!(report.caps.reachable);
    assert!(report.key_check.is_none() && report.models.is_none());
    assert_eq!(report.mapped.len(), 1);
    assert_eq!(report.mapped[0].available, None, "no catalog without a key");
    assert!(
        !w.calls()
            .iter()
            .any(|c| c.starts_with("prober.list_models")),
        "{:?}",
        w.calls()
    );
}

#[tokio::test]
async fn probe_with_a_key_checks_the_catalog_against_the_model_map() {
    let w = World::new();
    let sw = w.switcher();
    acme(
        &w,
        &sw,
        &[("gpt-5.5", "vendor/gpt-5.5"), ("gpt-x", "vendor/gone")],
    )
    .await;
    *w.prober.models.lock().unwrap() = Ok(vec!["vendor/gpt-5.5".into(), "other".into()]);
    let report = sw
        .probe_site(SiteProbeRequest {
            key: Some(KeyInput::Profile(ProfileId::new("acme"))),
            model: Some("gpt-5.5".into()),
            responses: true,
            ..SiteProbeRequest::new(SiteId::new("acme"))
        })
        .await
        .unwrap();
    assert!(report.key_check.unwrap().access.is_ok());
    assert_eq!(report.models.as_ref().map(Vec::len), Some(2));
    let available: Vec<_> = report
        .mapped
        .iter()
        .map(|m| (m.codex.as_str(), m.available))
        .collect();
    assert_eq!(available, [("gpt-5.5", Some(true)), ("gpt-x", Some(false))]);
    assert_eq!(
        report.model.as_deref(),
        Some("vendor/gpt-5.5"),
        "mapped name"
    );
    // The fake prober has no Responses probe: its error is reported, not raised.
    assert!(report.responses.is_none());
    assert!(matches!(
        report.responses_error,
        Some(Error::NotImplemented { .. })
    ));
    assert!(
        w.calls()
            .contains(&"prober.list_models https://api.acme.example/v1".to_owned()),
        "{:?}",
        w.calls()
    );
}

#[tokio::test]
async fn probe_refuses_unknown_lines_and_the_official_site() {
    let w = World::new();
    let sw = w.switcher();
    acme(&w, &sw, &[]).await;
    let e = sw
        .probe_site(SiteProbeRequest {
            line_id: Some("nope".into()),
            ..SiteProbeRequest::new(SiteId::new("acme"))
        })
        .await
        .unwrap_err();
    assert!(matches!(e, Error::NotFound { .. }), "{e}");
    let e = sw
        .probe_site(SiteProbeRequest::new(SiteId::new("official")))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "{e}");
    let e = sw
        .probe_site(SiteProbeRequest {
            key: Some(KeyInput::Secret(Secret::new(""))),
            ..SiteProbeRequest::new(SiteId::new("acme"))
        })
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Blocked { .. }), "{e}");
}

#[tokio::test]
async fn direct_mode_writes_the_sites_name_for_a_mapped_model() {
    let w = World::new();
    let sw = w.switcher();
    acme(&w, &sw, &[("gpt-5.5", "vendor/gpt-5.5")]).await;
    *w.prober.models.lock().unwrap() = Ok(vec!["vendor/gpt-5.5".into()]);
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("acme")))
        .await
        .unwrap();
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .expect("config plan");
    assert!(matches!(desired.mode, ResolvedMode::Token { .. }));
    assert_eq!(desired.model.model.as_deref(), Some("vendor/gpt-5.5"));
    assert!(
        plan.warnings
            .iter()
            .any(|n| n.text.key == "switch.warn.model_mapped"),
        "{:?}",
        plan.warnings
    );
    let check = plan.model_check.expect("model check");
    assert_eq!(check.current.as_deref(), Some("vendor/gpt-5.5"));
    assert!(check.available);
}

#[tokio::test]
async fn every_switch_to_a_mapped_site_writes_the_sites_name() {
    // The profile sets no model: Codex's model comes from config.toml. The first
    // switch writes the site's name for it; a later switch to the same profile
    // (e.g. after a speed or model edit) must keep writing that name instead of
    // releasing the key back to the user's own value (seen in e2e: the model
    // flipped between gpt-5.6-luna and gpt-6-luna on every switch).
    let w = World::new();
    let sw = w.switcher();
    acme(&w, &sw, &[("gpt-6-luna", "vendor/luna")]).await;
    let mut p = sw
        .profile_set()
        .await
        .unwrap()
        .profile(&ProfileId::new("acme"))
        .cloned()
        .unwrap();
    p.model.model = None;
    sw.save_profile(p, None).await.unwrap();
    *w.prober.models.lock().unwrap() = Ok(vec!["vendor/luna".into()]);
    for current in ["gpt-6-luna", "vendor/luna"] {
        w.config.state.lock().unwrap().model = Some(current.into());
        let plan = sw
            .plan(SwitchRequest::profile(ProfileId::new("acme")))
            .await
            .unwrap();
        let desired = plan.config.unwrap().desired.unwrap();
        assert_eq!(
            desired.model.model.as_deref(),
            Some("vendor/luna"),
            "config.toml model {current}"
        );
    }
}

#[tokio::test]
async fn after_another_profiles_model_the_users_own_model_is_mapped() {
    // The applied profile wrote its own model (gpt-6-astra); the mapped site's
    // profile sets none, so the switch restores the user's model (gpt-6-luna,
    // from the baseline) and that is the name the site map must translate.
    let w = World::new();
    let sw = w.switcher();
    acme(&w, &sw, &[("gpt-6-luna", "vendor/luna")]).await;
    let set = sw.profile_set().await.unwrap();
    let mut work = set.profile(&ProfileId::new("work")).cloned().unwrap();
    work.model.model = Some("gpt-6-astra".into());
    sw.save_profile(work, None).await.unwrap();
    let mut acme_p = set.profile(&ProfileId::new("acme")).cloned().unwrap();
    acme_p.model.model = None;
    sw.save_profile(acme_p, None).await.unwrap();
    {
        let mut st = w.config.state.lock().unwrap();
        st.model = Some("gpt-6-astra".into());
        st.active = Some(superba_core::ActiveState {
            profile_id: Some(ProfileId::new("work")),
            site_id: Some(SiteId::new("krill")),
            line_id: None,
            auth_mode: Some(AuthMode::Pat),
            generation: 1,
            applied_at: superba_core::Timestamp(0),
        });
    }
    w.config
        .baseline
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .keys
        .push(superba_core::KeySnapshot {
            path: "model".into(),
            value: Some("\"gpt-6-luna\"".into()),
        });
    *w.prober.models.lock().unwrap() = Ok(vec!["vendor/luna".into()]);
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("acme")))
        .await
        .unwrap();
    let desired = plan.config.unwrap().desired.unwrap();
    assert_eq!(desired.model.model.as_deref(), Some("vendor/luna"));
}

#[tokio::test]
async fn unmapped_models_are_left_alone() {
    let w = World::new();
    let sw = w.switcher();
    acme(&w, &sw, &[("other", "vendor/other")]).await;
    *w.prober.models.lock().unwrap() = Ok(vec!["gpt-5.5".into()]);
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("acme")))
        .await
        .unwrap();
    let desired = plan.config.unwrap().desired.unwrap();
    assert_eq!(desired.model.model.as_deref(), Some("gpt-5.5"));
    assert!(
        !plan
            .warnings
            .iter()
            .any(|n| n.text.key == "switch.warn.model_mapped")
    );
}

#[tokio::test]
async fn an_unsaved_site_is_probed_with_a_typed_key_and_never_stored() {
    let w = World::new();
    let sw = w.switcher();
    let base = url::Url::parse("https://api.acme.example/v1").unwrap();
    let site = superba_switch::new_compatible_site(SiteId::new("acme"), "Acme", &base).unwrap();
    *w.prober.models.lock().unwrap() = Ok(vec!["vendor/gpt-5.5".into()]);
    let report = sw
        .probe_site(SiteProbeRequest {
            key: Some(KeyInput::Secret(Secret::new(KEY))),
            ..SiteProbeRequest::unsaved(site.clone())
        })
        .await
        .unwrap();
    assert_eq!(report.site_id.as_str(), "acme");
    assert!(report.key_check.unwrap().access.is_ok());
    assert_eq!(report.models.as_ref().map(Vec::len), Some(1));
    assert!(sw.sites().await.unwrap().iter().all(|s| s.id != site.id));
    assert!(w.profiles.set.lock().unwrap().custom_sites.is_empty());

    // The same rules as saving: a site pointing at a Krill host is refused.
    let krill = url::Url::parse("https://api.kbsfdxc.net/backend-api/codex").unwrap();
    let mut bad = superba_switch::new_compatible_site(SiteId::new("k"), "K", &krill).unwrap();
    bad.kind = superba_core::SiteKind::Compatible;
    let e = sw
        .probe_site(SiteProbeRequest::unsaved(bad))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "{e}");
}
