#![allow(missing_docs)]

mod support;

use pretty_assertions::assert_eq;
use superba_core::{
    ActiveState, AuthMode, BuiltinProfile, CodexPath, Line, LineCaps, LineSource, Locale,
    LocalizedText, MigrationItem, MigrationPlan, MigrationRequest, Origin, PlanDestination, Site,
    SiteKind, SwitchService, Timestamp, catalog,
};
use superba_gui::design::{fonts::FontReport, tokens::DARK};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Cx, Data, dest_text, localize_text};
use superba_gui::shell::migration_sessions;
use support::fake::{FakeOptions, FakeService};

fn test_site() -> Site {
    Site {
        id: "krill-e2e".into(),
        kind: SiteKind::Krill,
        name: LocalizedText::new("", "Krill e2e"),
        lines: [
            ("a", "E2E line A"),
            ("b", "E2E line B"),
            ("c", "E2E line C"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (id, name))| Line {
            id: id.into(),
            name: LocalizedText::new("", name),
            origin: Origin::parse(&format!("https://127.0.0.1:{}", 8443 + i)).unwrap(),
            source: LineSource::Test,
        })
        .collect(),
        default_line: Some("a".into()),
        ..catalog::krill_site()
    }
}

async fn data() -> Data {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..FakeOptions::default()
    });
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data.sites.push(test_site());

    let mut compatible = superba_switch::new_compatible_site(
        "compatible".into(),
        "My API",
        &"https://api.example.com/v1".parse().unwrap(),
    )
    .unwrap();
    // Line ids are scoped to a site, even when they match a built-in Krill line.
    compatible.lines[0].id = "overseas".into();
    compatible.lines[0].name = LocalizedText::new("", "My API endpoint");
    compatible.default_line = Some("overseas".into());
    data.sites.push(compatible);

    let profiles = &mut data.profiles.value.as_mut().unwrap().profiles;
    let mut first = profiles.iter().find(|p| p.id == "krill").unwrap().clone();
    first.id = "e2e-first".into();
    first.name = "First e2e profile".into();
    first.site_id = "krill-e2e".into();
    first.line_id = Some("a".into());
    first.auth_mode = AuthMode::ApiKey;
    let mut applied = first.clone();
    applied.id = "e2e-active".into();
    applied.line_id = Some("c".into());
    applied.auth_mode = AuthMode::Pat;
    let mut builtin = first.clone();
    builtin.id = "e2e-builtin".into();
    builtin.builtin = Some(BuiltinProfile::Original);
    profiles.extend([builtin, first, applied]);
    data
}

fn active(data: &mut Data) -> &mut ActiveState {
    data.overview
        .value
        .as_mut()
        .unwrap()
        .active
        .as_mut()
        .unwrap()
}

fn apply_test_site(data: &mut Data) {
    let a = active(data);
    a.site_id = Some("krill-e2e".into());
    a.profile_id = Some("e2e-active".into());
    a.line_id = Some("c".into());
    a.auth_mode = Some(AuthMode::Pat);
}

fn with_cx<T>(data: &Data, locale: Locale, f: impl FnOnce(&Cx<'_>) -> T) -> T {
    let tr = Tr::new(locale);
    let fonts = FontReport::default();
    f(&Cx {
        tr: &tr,
        pal: &DARK,
        data,
        busy: false,
        now: Timestamp::EPOCH,
        fonts: &fonts,
        service: &support::idle_service(),
        log: &[],
    })
}

#[test]
fn no_overview_has_no_active_provider_and_probes_the_default_preset() {
    let data = Data::default();
    assert_eq!(data.active_site(), None);
    assert!(data.active_profile().is_none());
    assert!(
        data.site_caps(&"krill".into(), &"overseas".into())
            .is_none()
    );
    assert!(!data.probing_site(&"krill".into()));
    // The line tiles and the shell's first probe use the default preset's site.
    assert_eq!(
        data.lines_site_id().map(|s| s.to_string()).as_deref(),
        Some("krill")
    );
    with_cx(&data, Locale::En, |cx| assert_eq!(cx.active_label(), None));
}

#[tokio::test]
async fn the_active_site_is_the_applied_site_of_any_kind() {
    let mut data = data().await;
    for (applied, expected) in [
        ("krill", Some("krill")),
        ("krill-e2e", Some("krill-e2e")),
        ("compatible", Some("compatible")),
        ("official", Some("official")),
        ("missing", None),
    ] {
        active(&mut data).site_id = Some(applied.into());
        assert_eq!(
            data.active_site().map(|s| s.id.to_string()).as_deref(),
            expected,
            "{applied}"
        );
        assert_eq!(data.official_active(), applied == "official", "{applied}");
    }
    active(&mut data).site_id = None;
    assert_eq!(data.active_site(), None);
    data.overview.value.as_mut().unwrap().active = None;
    assert_eq!(data.active_site(), None);
    assert!(data.active_profile().is_none());
}

#[tokio::test]
async fn a_missing_applied_site_has_no_site_but_keeps_its_profile() {
    let mut data = data().await;
    apply_test_site(&mut data);
    data.sites.retain(|s| s.id != "krill-e2e");
    assert_eq!(data.active_site(), None);
    assert_eq!(data.active_profile().unwrap().id.as_str(), "e2e-active");
    // The shell still has a site to probe: the first provider's.
    assert!(data.lines_site_id().is_some());
}

#[tokio::test]
async fn site_and_profile_follow_the_applied_test_site() {
    let mut data = data().await;
    apply_test_site(&mut data);
    assert_eq!(data.active_site(), Some(test_site()));
    let profile = data.active_profile().unwrap();
    assert_eq!(profile.id.as_str(), "e2e-active");
    assert_eq!(profile.site_id.as_str(), "krill-e2e");
    assert_eq!(profile.line_id.as_ref().unwrap().as_str(), "c");
    assert_eq!(profile.auth_mode, AuthMode::Pat);
}

#[tokio::test]
async fn the_active_profile_is_never_a_builtin_and_falls_back_on_the_applied_site() {
    let mut data = data().await;
    apply_test_site(&mut data);
    active(&mut data).profile_id = Some("e2e-builtin".into());
    assert!(
        data.active_profile().is_none(),
        "a built-in is not a provider"
    );
    active(&mut data).profile_id = Some("missing".into());
    assert!(data.active_profile().is_none());
    // An older state without a profile id: the first provider on the applied site.
    active(&mut data).profile_id = None;
    assert_eq!(data.active_profile().unwrap().id.as_str(), "e2e-first");
    data.profiles
        .value
        .as_mut()
        .unwrap()
        .profiles
        .retain(|p| p.site_id != "krill-e2e" || p.builtin.is_some());
    assert!(data.active_profile().is_none());
}

#[tokio::test]
async fn a_second_provider_on_the_same_site_can_be_the_applied_one() {
    let mut data = data().await;
    let mut second = data.profile(&"krill".into()).unwrap().clone();
    second.id = "krill-second".into();
    data.profiles.value.as_mut().unwrap().profiles.push(second);
    active(&mut data).profile_id = Some("krill-second".into());
    assert_eq!(data.active_profile().unwrap().id.as_str(), "krill-second");
    assert_eq!(
        data.providers().len(),
        4,
        "krill, e2e-first, e2e-active, krill-second"
    );
}

fn caps(site: &str, line: &str, latency_ms: u32) -> LineCaps {
    LineCaps {
        site_id: site.into(),
        line_id: line.into(),
        checked_at: Timestamp::EPOCH,
        reachable: true,
        latency_ms: Some(latency_ms),
        pat_ok: true,
        apikey_ok: true,
        codex_path: Some(CodexPath::BackendApiCodex),
        blocked_by_edge: false,
        key_access: None,
        checks: vec![],
        detail: Some(LocalizedText::new("probe.ok", "All checks passed.")),
    }
}

#[tokio::test]
async fn caps_and_probing_are_per_site() {
    let mut data = data().await;
    let builtin = data.caps.entry("krill".into()).or_default();
    builtin.finish(Ok(vec![caps("krill", "overseas", 100)]));
    builtin.start();
    let custom = caps("krill-e2e", "c", 12);
    data.caps
        .entry("krill-e2e".into())
        .or_default()
        .finish(Ok(vec![custom.clone()]));
    assert!(data.probing_site(&"krill".into()));
    assert!(!data.probing_site(&"krill-e2e".into()));
    assert_eq!(
        data.site_caps(&"krill".into(), &"overseas".into())
            .unwrap()
            .latency_ms,
        Some(100)
    );
    // Line ids are scoped to their site.
    assert!(data.site_caps(&"krill".into(), &"c".into()).is_none());
    assert_eq!(
        data.site_caps(&"krill-e2e".into(), &"c".into()),
        Some(&custom)
    );
    assert!(
        data.site_caps(&"krill-e2e".into(), &"overseas".into())
            .is_none()
    );
}

#[tokio::test]
async fn builtin_lines_and_the_translated_active_label_are_kept() {
    let data = data().await;
    let mut expected_site = catalog::krill_site();
    expected_site.capabilities.account_summary = true;
    assert_eq!(data.active_site(), Some(expected_site));
    assert_eq!(data.active_profile().unwrap().id.as_str(), "krill");
    for locale in Locale::ALL {
        with_cx(&data, locale, |cx| {
            assert_eq!(
                cx.active_label(),
                Some(format!(
                    "{} · {} · {}",
                    cx.tr.t("site.krill"),
                    cx.tr.t("line.krill.overseas"),
                    cx.tr.t("auth.short.pat"),
                )),
            );
        });
    }
}

#[tokio::test]
async fn active_label_uses_test_site_and_line_names() {
    let mut data = data().await;
    apply_test_site(&mut data);
    for locale in Locale::ALL {
        with_cx(&data, locale, |cx| {
            assert_eq!(
                cx.active_label(),
                Some(format!(
                    "Krill e2e · E2E line C · {}",
                    cx.tr.t("auth.short.pat")
                )),
            );
            assert_eq!(cx.line_name(&"c".into()), "E2E line C");
            assert_eq!(cx.line_name(&"overseas".into()), "overseas");
        });
    }
}

#[tokio::test]
async fn compatible_active_label_resolves_lines_in_its_own_site() {
    let mut data = data().await;
    let a = active(&mut data);
    a.site_id = Some("compatible".into());
    a.line_id = Some("overseas".into());
    a.auth_mode = Some(AuthMode::ApiKey);
    for locale in Locale::ALL {
        with_cx(&data, locale, |cx| {
            assert_eq!(
                cx.active_label(),
                Some(format!(
                    "My API · My API endpoint · {}",
                    cx.tr.t("auth.short.apikey")
                )),
            );
        });
    }
}

#[tokio::test]
async fn active_label_falls_back_only_for_unknown_sites_or_lines() {
    let mut data = data().await;
    for (site, line, expected) in [
        ("unknown", "overseas", "unknown · overseas"),
        ("krill-e2e", "overseas", "Krill e2e · overseas"),
    ] {
        let a = active(&mut data);
        a.site_id = Some(site.into());
        a.line_id = Some(line.into());
        with_cx(&data, Locale::En, |cx| {
            assert_eq!(
                cx.active_label(),
                Some(format!("{expected} · {}", cx.tr.t("auth.short.pat"))),
            );
        });
    }
}

#[tokio::test]
async fn official_active_label_keeps_its_translated_name() {
    let mut data = data().await;
    let a = active(&mut data);
    a.site_id = Some("official".into());
    a.line_id = None;
    a.auth_mode = Some(AuthMode::OfficialChatGpt);
    data.sites
        .iter_mut()
        .find(|s| s.id == "official")
        .unwrap()
        .name = LocalizedText::new("", "Untranslated name");
    for locale in Locale::ALL {
        with_cx(&data, locale, |cx| {
            assert_eq!(
                cx.active_label(),
                Some(format!(
                    "{} · {}",
                    cx.tr.t("site.official"),
                    cx.tr.t("auth.short.chatgpt")
                )),
            );
        });
    }
}

#[tokio::test]
async fn line_ids_and_english_names_localize_on_the_effective_site() {
    let mut data = data().await;
    apply_test_site(&mut data);
    // A catalog-backed custom name verifies that display uses the translator.
    data.sites
        .iter_mut()
        .find(|s| s.id == "krill-e2e")
        .unwrap()
        .lines[2]
        .name = LocalizedText::new("line.krill.cdn", "E2E line C");
    with_cx(&data, Locale::ZhCn, |cx| {
        let name = cx.tr.t("line.krill.cdn");
        assert_eq!(cx.line_name(&"c".into()), name);
        assert_eq!(
            cx.active_label(),
            Some(format!(
                "Krill e2e · {name} · {}",
                cx.tr.t("auth.short.pat")
            )),
        );
        for line in ["c", "E2E line C"] {
            let text = LocalizedText::new("", "{site} · {line}")
                .arg("site", "krill-e2e")
                .arg("line", line);
            assert_eq!(
                localize_text(cx.tr, &data, &text),
                format!("Krill e2e · {name}")
            );
        }
    });
}

fn destination(site: &str, line: &str) -> PlanDestination {
    PlanDestination {
        profile_id: None,
        site_id: Some(site.into()),
        line_id: Some(line.into()),
        auth_mode: Some(AuthMode::Pat),
        label: LocalizedText::new("", "Backend destination label"),
    }
}

#[tokio::test]
async fn destination_resolves_the_target_krill_kind_site_even_when_inactive() {
    let mut data = data().await;
    for applied in ["krill", "krill-e2e"] {
        active(&mut data).site_id = Some(applied.into());
        for locale in Locale::ALL {
            let tr = Tr::new(locale);
            assert_eq!(
                dest_text(&tr, &data, &destination("krill-e2e", "c")),
                format!("Krill e2e · E2E line C · {}", tr.t("auth.short.pat")),
            );
            assert_eq!(
                dest_text(&tr, &data, &destination("krill", "overseas")),
                format!(
                    "Krill · {} · {}",
                    tr.t("line.krill.overseas"),
                    tr.t("auth.short.pat")
                ),
            );
        }
    }
}

#[tokio::test]
async fn destination_uses_label_for_other_kinds_and_id_for_unknown_krill_line() {
    let data = data().await;
    let tr = Tr::new(Locale::En);
    for site in ["compatible", "official", "unknown"] {
        assert_eq!(
            dest_text(&tr, &data, &destination(site, "overseas")),
            "Backend destination label",
        );
    }
    assert_eq!(
        dest_text(&tr, &data, &destination("krill-e2e", "unknown")),
        format!("Krill e2e · unknown · {}", tr.t("auth.short.pat")),
    );
}

fn migration(thread_count: u32, rollout_count: u32, sessions: u32) -> MigrationPlan {
    MigrationPlan {
        id: "test-migration".into(),
        codex_home: "/tmp/test-codex".into(),
        request: MigrationRequest {
            to_provider: "openai".into(),
            selectors: vec![],
            include_archived: true,
            exclude: vec![],
        },
        thread_count,
        rollout_count,
        by_source_provider: Default::default(),
        backup_dir: "/tmp/test-backup".into(),
        backup_bytes: 0,
        items: (0..sessions)
            .map(|n| MigrationItem {
                thread_id: format!("session-{n}"),
                title: None,
                source_provider: "krill".into(),
                created_at: None,
                archived: false,
            })
            .collect(),
    }
}

#[test]
fn migration_sessions_uses_larger_count_without_items() {
    for (threads, rollouts, expected) in [(0, 0, 0), (10, 6, 10), (2, 6, 6)] {
        assert_eq!(
            migration_sessions(&migration(threads, rollouts, 0)),
            expected
        );
    }
}

#[test]
fn migration_sessions_counts_items_instead_of_database_rows() {
    for (threads, rollouts) in [(10, 6), (6, 10), (0, 0)] {
        assert_eq!(migration_sessions(&migration(threads, rollouts, 6)), 6);
    }
}
