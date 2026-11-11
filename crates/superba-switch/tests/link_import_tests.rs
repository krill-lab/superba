//! `superba://import` links from the Krill website (feature 140) through the
//! service: the link's key goes into the Krill provider through the provider
//! import's mapping and saves, with API-key auth. An existing Krill provider
//! is updated (its lines stay) unless a new one is asked for; a new provider
//! takes the requested line or the default line. Dry runs write nothing, and
//! the switch planner then applies the provider with its API key.

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use superba_core::{
    AuthChoice, AuthMode, ConflictAction, ImportLink, ImportOutcome, ImportSource, LineId,
    LinkImportRequest, ProfileId, ResolvedMode, SwitchRequest, SwitchService,
};

const LINK_KEY: &str = "sk-krill-link-0123456789abcdef";

fn link(name: Option<&str>) -> ImportLink {
    let mut raw = format!("superba://import/v1?key={LINK_KEY}");
    if let Some(n) = name {
        raw.push_str(&format!("&name={n}"));
    }
    ImportLink::parse(&raw).unwrap()
}

fn request(on_conflict: ConflictAction, line: Option<&str>, dry_run: bool) -> LinkImportRequest {
    LinkImportRequest {
        link: link(None),
        line: line.map(LineId::new),
        on_conflict,
        dry_run,
    }
}

fn stored(w: &World, profile: &str) -> Option<String> {
    w.secrets
        .map
        .lock()
        .unwrap()
        .get(&format!("profile:{profile}"))
        .cloned()
}

#[tokio::test]
async fn a_dry_run_offers_to_update_the_krill_provider_and_writes_nothing() {
    let w = World::new();
    let sw = w.switcher();
    let before = sw.profile_set().await.unwrap();
    let saves = *w.profiles.saves.lock().unwrap();
    let report = sw
        .import_link(LinkImportRequest::preview(link(None)))
        .await
        .unwrap();
    let existing = report.existing.as_ref().expect("the Krill provider");
    assert_eq!(existing.profile, ProfileId::new("work"));
    assert_eq!(report.masked_key, "••••cdef");
    assert_eq!(report.item.source, ImportSource::KrillLink);
    assert!(matches!(
        &report.item.outcome,
        ImportOutcome::Updated { profile, key_stored: true, .. } if profile == "work"
    ));
    // The provider's line stays.
    assert_eq!(report.line, Some(LineId::new("overseas")));
    assert_eq!(sw.profile_set().await.unwrap(), before);
    assert_eq!(*w.profiles.saves.lock().unwrap(), saves);
    assert_ne!(stored(&w, "work").as_deref(), Some(LINK_KEY));
    assert_eq!(*w.config.applied.lock().unwrap(), 0);
}

#[tokio::test]
async fn an_update_replaces_the_key_keeps_the_lines_and_uses_api_key_auth() {
    let w = World::new();
    let sw = w.switcher();
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set.profiles.iter_mut().find(|p| p.id == "work").unwrap();
        p.set_line_priority([LineId::new("cdn"), LineId::new("overseas")]);
    }
    let before = sw.profile_set().await.unwrap();
    let report = sw
        .import_link(request(ConflictAction::Update, Some("cn-fast"), false))
        .await
        .unwrap();
    assert!(
        matches!(&report.item.outcome, ImportOutcome::Updated { profile, .. } if profile == "work"),
        "{report:?}"
    );
    assert_eq!(report.line, Some(LineId::new("cdn")));
    let set = sw.profile_set().await.unwrap();
    let work = set.profile(&ProfileId::new("work")).unwrap();
    let old = before.profile(&ProfileId::new("work")).unwrap();
    assert_eq!(work.line_priority(), old.line_priority());
    assert_eq!(work.name, old.name);
    assert_eq!(work.auth_mode, AuthMode::ApiKey);
    assert_eq!(work.auth_choice(), AuthChoice::ApiKey);
    assert_eq!(stored(&w, "work").as_deref(), Some(LINK_KEY));
    assert_eq!(set.profiles.len(), before.profiles.len());
    // Nothing went to Codex.
    assert_eq!(*w.config.applied.lock().unwrap(), 0);
}

#[tokio::test]
async fn add_as_new_creates_a_krill_provider_on_the_requested_line() {
    let w = World::new();
    let sw = w.switcher();
    let mut req = request(ConflictAction::Rename, Some("cdn"), false);
    req.link = link(Some("My%20laptop"));
    let report = sw.import_link(req).await.unwrap();
    let ImportOutcome::Created {
        profile,
        profile_name,
        site,
        key_stored,
    } = &report.item.outcome
    else {
        panic!("{report:?}");
    };
    assert_eq!(profile_name, "My laptop");
    assert_eq!(site, &None);
    assert!(key_stored);
    assert_eq!(report.line, Some(LineId::new("cdn")));
    let set = sw.profile_set().await.unwrap();
    let new = set.profile(profile).unwrap();
    assert_eq!(new.site_id.as_str(), "krill");
    assert_eq!(new.line_priority(), vec![LineId::new("cdn")]);
    assert_eq!(new.auth_mode, AuthMode::ApiKey);
    assert_eq!(stored(&w, profile.as_str()).as_deref(), Some(LINK_KEY));
    // The existing provider is untouched.
    let work = set.profile(&ProfileId::new("work")).unwrap();
    assert_eq!(work.auth_mode, AuthMode::Pat);
    assert_ne!(stored(&w, "work").as_deref(), Some(LINK_KEY));
}

#[tokio::test]
async fn skip_imports_nothing() {
    let w = World::new();
    let sw = w.switcher();
    let before = sw.profile_set().await.unwrap();
    let report = sw
        .import_link(request(ConflictAction::Skip, None, false))
        .await
        .unwrap();
    assert!(matches!(report.item.outcome, ImportOutcome::Skipped { .. }));
    assert_eq!(sw.profile_set().await.unwrap(), before);
}

#[tokio::test]
async fn without_a_krill_provider_a_new_one_takes_the_default_or_requested_line() {
    let w = World::new();
    w.profiles.set.lock().unwrap().profiles.clear();
    w.secrets.map.lock().unwrap().clear();
    let sw = w.switcher();
    let preview = sw
        .import_link(LinkImportRequest::preview(link(None)))
        .await
        .unwrap();
    assert_eq!(preview.existing, None);
    assert!(matches!(
        &preview.item.outcome,
        ImportOutcome::Created { profile_name, .. } if profile_name == "Krill"
    ));
    assert_eq!(preview.line, Some(LineId::new("overseas")));
    assert!(sw.profile_set().await.unwrap().profiles.is_empty());
    // An unknown requested line falls back to the default line.
    let report = sw
        .import_link(request(ConflictAction::Update, Some("mars"), false))
        .await
        .unwrap();
    assert_eq!(report.line, Some(LineId::new("overseas")));
    let id = report.item.outcome.profile().unwrap().clone();
    // The planner applies it with its API key.
    let plan = sw.plan(SwitchRequest::profile(id)).await.unwrap();
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .expect("config plan");
    match &desired.mode {
        ResolvedMode::Token {
            auth,
            key,
            endpoints,
            ..
        } => {
            assert_eq!(*auth, AuthMode::ApiKey);
            assert_eq!(key.expose(), LINK_KEY);
            assert!(
                endpoints
                    .openai_base_url
                    .as_str()
                    .starts_with("https://api.kbsfdxc.net/"),
                "{}",
                endpoints.openai_base_url
            );
        }
        other => panic!("unexpected mode {other:?}"),
    }
}

#[tokio::test]
async fn an_updated_pat_provider_is_planned_with_its_new_api_key() {
    let w = World::new();
    let sw = w.switcher();
    sw.import_link(request(ConflictAction::Update, None, false))
        .await
        .unwrap();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .expect("config plan");
    assert_eq!(desired.auth_mode(), Some(AuthMode::ApiKey));
    match &desired.mode {
        ResolvedMode::Token { key, .. } => assert_eq!(key.expose(), LINK_KEY),
        other => panic!("unexpected mode {other:?}"),
    }
}

#[tokio::test]
async fn the_link_never_updates_a_custom_provider() {
    let w = World::new();
    {
        let mut set = w.profiles.set.lock().unwrap();
        set.profiles.clear();
        let mut custom = krill_profile("relay", None, AuthMode::ApiKey);
        custom.site_id = superba_core::SiteId::new("relay");
        custom.name = "Krill".into();
        set.profiles.push(custom);
    }
    let sw = w.switcher();
    let preview = sw
        .import_link(LinkImportRequest::preview(link(None)))
        .await
        .unwrap();
    // Not a Krill-site provider, even though it is named "Krill".
    assert_eq!(preview.existing, None);
    assert!(matches!(
        &preview.item.outcome,
        ImportOutcome::Created { profile, .. } if profile != "relay"
    ));
}
