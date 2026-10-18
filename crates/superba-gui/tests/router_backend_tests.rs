#![allow(missing_docs)]

//! Non-drawing code behind the router page: candidates and members from the
//! sites and profiles, the mapping of the router's status, and member edits
//! through the switch service.

mod support;

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use superba_core::settings::{AuthChoice, CodexSettings};
use superba_core::{
    AuthMode, KeyRef, LineId, ModelDefaults, Profile, ProfileId, ProfileSet, RouterLineHealth,
    RouterLineView, RouterMemberView, RouterView, Site, SiteId, catalog,
};
use superba_gui::pages::router::backend::{candidates, live, members, with_member};
use superba_gui::pages::router::model::{LineHealth, MemberKind};
use superba_router::RouterStatus;
use url::Url;

fn compatible(id: &str, url: &str) -> Site {
    superba_switch::new_compatible_site(SiteId::new(id), id, &Url::parse(url).unwrap()).unwrap()
}

fn profile(id: &str, site: &str) -> Profile {
    Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        id: ProfileId::new(id),
        name: format!("{id} profile"),
        builtin: None,
        site_id: SiteId::new(site),
        line_id: None,
        backup_lines: Vec::new(),
        auth_mode: AuthMode::ApiKey,
        auth: AuthChoice::Auto,
        key_ref: Some(KeyRef::Keyring {
            service: "superba-test".into(),
            account: id.into(),
        }),
        base_url_override: None,
        model: ModelDefaults::default(),
        codex_settings: CodexSettings::new(),
    }
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[test]
fn candidates_are_key_based_krill_or_custom_profiles_not_yet_members() {
    let sites = vec![
        catalog::krill_site(),
        catalog::official_site(),
        compatible("acme", "https://api.acme.test/v1"),
        compatible("beta", "https://api.beta.test/v1"),
    ];
    let mut set = ProfileSet::default();
    set.profiles.push(profile("krill", catalog::KRILL_SITE_ID));
    set.profiles.push(profile("acme", "acme"));
    set.profiles.push(profile("acme-2", "acme"));
    let mut keyless = profile("beta", "beta");
    keyless.key_ref = None;
    set.profiles.push(keyless);
    set.profiles
        .push(profile("official-ish", catalog::OFFICIAL_SITE_ID));
    let all = candidates(&sites, &set, &[]);
    let ids: Vec<_> = all.iter().map(|c| (c.site_id.as_str(), c.kind)).collect();
    assert_eq!(
        ids,
        [
            (catalog::KRILL_SITE_ID, MemberKind::Krill),
            ("acme", MemberKind::Compatible)
        ]
    );
    // A member profile takes its site out of the candidates.
    let rest = candidates(&sites, &set, &[ProfileId::new("acme-2")]);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].site_id, catalog::KRILL_SITE_ID);
}

#[test]
fn members_follow_the_saved_order_with_live_lines_or_the_sites_lines() {
    let mut acme = compatible("acme", "https://api.acme.test/v1");
    acme.model_map = map(&[("gpt", "acme/gpt")]);
    let sites = vec![catalog::krill_site(), acme];
    let mut set = ProfileSet::default();
    let mut krill = profile("krill", catalog::KRILL_SITE_ID);
    krill.set_line_priority([LineId::new(catalog::KRILL_LINE_CDN)]);
    set.profiles.push(krill);
    set.profiles.push(profile("acme", "acme"));
    let mut view = RouterView::default();
    view.settings.members = vec![ProfileId::new("acme"), ProfileId::new("krill")];
    view.members = vec![RouterMemberView {
        profile_id: ProfileId::new("acme"),
        name: "Acme".into(),
        lines: vec![RouterLineView {
            id: "default".into(),
            name: "api.acme.test".into(),
            health: RouterLineHealth::Open,
            ttfb_ms: Some(180),
            requests: 12,
            failovers_away: 3,
            last_error: Some("502".into()),
        }],
    }];
    let m = members(&view, &sites, &set);
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].id, "acme");
    assert_eq!(m[0].kind, MemberKind::Compatible);
    assert_eq!(m[0].model_mappings, 1);
    assert!(m[0].has_key);
    assert_eq!(m[0].lines[0].health, LineHealth::Down);
    let metrics = m[0].lines[0].metrics.as_ref().unwrap();
    assert_eq!((metrics.requests, metrics.failovers_away), (12, 3));
    // Not running: the site's lines in the profile's priority order.
    assert_eq!(m[1].kind, MemberKind::Krill);
    assert!(m[1].websocket);
    assert_eq!(m[1].lines[0].id, catalog::KRILL_LINE_CDN);
    assert!(m[1].lines.iter().all(|l| l.health == LineHealth::Unknown));
    assert!(members(&RouterView::default(), &sites, &set).is_empty());
}

fn status_json(state: &str) -> RouterStatus {
    serde_json::from_value(serde_json::json!({
        "service": "superba-router",
        "version": "0.1.0",
        "instance_id": "i",
        "pid": 42,
        "listen": "127.0.0.1:4000",
        "base_url": "http://127.0.0.1:4000/backend-api/codex",
        "started_at_ms": 0,
        "uptime_ms": 3_725_000,
        "config_generation": 1,
        "members": [{
            "id": "acme", "name": "Acme Inc", "kind": "compatible", "websocket": false,
            "model_mappings": 0,
            "lines": [{
                "id": "default", "name": "api.acme.test",
                "base_url": "https://api.acme.test/v1", "state": state,
                "consecutive_failures": 2, "reopens_in_ms": 4000,
                "last_error": "502", "last_ok_ms_ago": null, "ttfb_ms": 180,
                "requests": 12, "failovers_away": 3, "broken_streams": 0,
                "websockets": 0, "active_websockets": 0
            }]
        }],
        "totals": {"requests": 12, "active_requests": 1, "websockets": 0,
                   "active_websockets": 0, "failovers": 3, "unavailable": 0,
                   "rejected": 0, "ws_refused": 0},
        "sticky_sessions": 2,
        "recent": [{"at_ms": 1, "kind": "failover", "member": "acme",
                    "line": "default", "detail": "502"}]
    }))
    .expect("status JSON matches RouterStatus")
}

#[test]
fn live_status_names_members_and_maps_event_kinds() {
    let l = live(&status_json("healthy"), &[]);
    assert_eq!(l.listen, "127.0.0.1:4000");
    assert_eq!(l.totals.sticky_sessions, 2);
    assert_eq!(l.events.len(), 1);
    assert_eq!(l.events[0].kind_key, "router.event.failover");
    assert!(l.events[0].severe);
    assert_eq!(l.events[0].member.as_deref(), Some("Acme Inc"));
}

/// A status from a router without a local budget still maps (all zero); a
/// router that ran short of open files shows it, and the page warns while the
/// last shortage is recent (user bug 35).
#[test]
fn live_status_carries_the_local_capacity_and_shortages() {
    use superba_gui::pages::router::model::{LOCAL_SHORTAGE_WINDOW_MS, recent_shortage};

    let l = live(&status_json("healthy"), &[]);
    assert_eq!(l.local, superba_core::RouterLocalView::default());
    assert!(!recent_shortage(&l.local, 10_000));

    let mut status = status_json("healthy");
    status.local = serde_json::from_value(serde_json::json!({
        "open_files_limit": 256, "open_files_limit_now": 256,
        "max_concurrent": 64, "in_flight": 64, "max_connections": 128,
        "connections": 128, "busy": 9, "last_busy_ms": 5_000,
        "file_limit_errors": 2,
        "last_file_limit": {"at_ms": 7_000, "place": "connecting to acme/default",
                            "detail": "Too many open files (os error 24)"}
    }))
    .unwrap();
    status.recent.push(superba_router::RouteEvent {
        at_ms: 7_000,
        kind: superba_router::RouteEventKind::LocalLimit,
        member: None,
        line: None,
        detail: Some("too many open files".into()),
    });
    let l = live(&status, &[]);
    assert_eq!(l.local.open_files_limit, Some(256));
    assert_eq!((l.local.in_flight, l.local.max_concurrent), (64, 64));
    assert_eq!((l.local.busy, l.local.file_limit_errors), (9, 2));
    assert_eq!(l.local.last_problem_ms, Some(7_000));
    assert_eq!(
        l.local.last_file_limit.as_deref(),
        Some("connecting to acme/default: Too many open files (os error 24)")
    );
    assert!(recent_shortage(&l.local, 8_000));
    assert!(!recent_shortage(
        &l.local,
        7_000 + LOCAL_SHORTAGE_WINDOW_MS + 1
    ));
    let e = l.events.last().unwrap();
    assert_eq!(e.kind_key, "router.event.local_limit");
    assert!(e.severe);
}

#[test]
fn member_edits_and_mode_go_through_the_switch_service() {
    assert_eq!(
        with_member(
            &[ProfileId::new("a"), ProfileId::new("b")],
            &ProfileId::new("a"),
            true
        ),
        vec![ProfileId::new("b"), ProfileId::new("a")]
    );
    assert_eq!(
        with_member(&[ProfileId::new("a")], &ProfileId::new("a"), false),
        Vec::<ProfileId>::new()
    );
}

#[tokio::test]
async fn the_router_backend_saves_members_and_installs_only_on_request() {
    use superba_core::SwitchService;
    use superba_gui::pages::router::backend::{RouterBackend, SystemRouter};
    use superba_gui::pages::router::model::Candidate;

    let service = std::sync::Arc::new(support::fake::FakeService::new(
        support::fake::FakeOptions {
            fast: true,
            ..Default::default()
        },
    ));
    let profiles = service.profile_set().await.unwrap();
    let krill = profiles
        .profiles
        .iter()
        .find(|p| p.builtin.is_none() && p.key_ref.is_some())
        .expect("the fake has a Krill profile with a key")
        .clone();
    let backend = SystemRouter::for_service(service.clone());
    let snap = backend.snapshot().await.unwrap();
    assert!(snap.members.is_empty());
    assert!(
        snap.candidates
            .iter()
            .any(|c| c.profile_id == krill.id.as_str())
    );
    assert!(backend.install().await.is_err(), "no member yet");
    backend
        .add_member(Candidate {
            site_id: krill.site_id.to_string(),
            profile_id: krill.id.to_string(),
            name: krill.name.clone(),
            kind: MemberKind::Krill,
        })
        .await
        .unwrap();
    let view = service.router_status().await.unwrap();
    assert_eq!(view.settings.members, vec![krill.id.clone()]);
    assert!(
        !view.installed,
        "adding a member only saves the list; installing is explicit"
    );
    let snap = backend.snapshot().await.unwrap();
    assert_eq!(snap.members.len(), 1);
    assert!(snap.live.is_err(), "no admin API in tests");
    backend.install().await.unwrap();
    assert!(service.router_status().await.unwrap().installed);
    backend.remove_member(krill.id.to_string()).await.unwrap();
    let view = service.router_status().await.unwrap();
    assert!(view.settings.members.is_empty());
    assert!(
        !view.installed,
        "removing the last member uninstalls the router"
    );
    // Without an installed router, removing the last member only saves the list.
    backend
        .add_member(Candidate {
            site_id: krill.site_id.to_string(),
            profile_id: krill.id.to_string(),
            name: krill.name.clone(),
            kind: MemberKind::Krill,
        })
        .await
        .unwrap();
    backend.remove_member(krill.id.to_string()).await.unwrap();
    let view = service.router_status().await.unwrap();
    assert!(view.settings.members.is_empty() && !view.installed);
}
