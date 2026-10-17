//! Mapping found providers to Superba: Krill detection (exact line, backup
//! lines, another Krill address), custom sites with lines and model maps,
//! refused addresses, notes, conflicts (name, address, Krill line) and what an
//! import of each conflict choice saves; the same provider in both tools.

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use superba_core::catalog::{self, builtin_sites};
use superba_core::{
    AuthChoice, CandidateStatus, ConflictAction, ForeignApp, ImportKey, ImportSelection,
    ImportSource, ImportStore, LineId, LineSource, LocalizedText, Profile, ProfileId, ProfileSet,
    Result, Site, SiteEndpoints, SiteId, SiteKind, StoreStatus,
};
use superba_import::{ForeignKey, ForeignProvider, PlannedAction, SourceRead, plan, preview};
use url::Url;

fn provider(source: ImportSource, id: &str, name: &str, url: &str) -> ForeignProvider {
    let mut p = ForeignProvider::new(source, format!("codex/{id}"), name, ForeignApp::Codex);
    p.base_url = Some(url.into());
    p.key = ForeignKey::from_text(Some(&format!("sk-test-{id}-0000")));
    p.model = Some("gpt-5.5".into());
    p.wire_api = Some("responses".into());
    p
}

fn read(source: ImportSource, providers: Vec<ForeignProvider>) -> SourceRead {
    SourceRead {
        source,
        stores: vec![ImportStore {
            path: "/x".into(),
            display_path: "~/x".into(),
            format: "test".into(),
            status: StoreStatus::Read,
            note: None,
        }],
        providers,
    }
}

/// `superba_switch::new_compatible_site` in miniature (no prober validation).
fn make_site(id: SiteId, name: &str, url: &Url) -> Result<Site> {
    let origin = superba_core::Origin::parse(&url.origin().ascii_serialization())?;
    Ok(Site {
        id,
        kind: SiteKind::Compatible,
        name: LocalizedText::new("", name),
        homepage: None,
        lines: vec![superba_core::Line {
            id: LineId::new("default"),
            name: LocalizedText::new("", origin.host()),
            origin,
            source: LineSource::User,
        }],
        capabilities: catalog::compatible_capabilities(),
        endpoints: SiteEndpoints {
            models_unauth: format!("{}/models", url.path().trim_end_matches('/')),
            ..SiteEndpoints::default()
        },
        default_line: Some(LineId::new("default")),
        model_map: BTreeMap::new(),
    })
}

fn one(p: ForeignProvider) -> superba_core::ImportCandidate {
    let scan = preview(
        &[read(p.source, vec![p])],
        &ProfileSet::default(),
        &builtin_sites(),
    );
    scan.candidates().next().unwrap().clone()
}

#[test]
fn krill_lines_are_recognised_by_origin_with_their_backup_lines() {
    let mut p = provider(
        ImportSource::Ccs,
        "k",
        "Krill CN",
        "https://api-slb.krill-code.net/codex/v1",
    );
    p.extra_urls = vec![
        "https://api.cdn-krill-ai.com/v1".into(),
        "https://elsewhere.example.com/v1".into(),
    ];
    p.model_map.insert("gpt-5.5".into(), "x".into());
    let c = one(p);
    assert!(c.importable() && c.preselected);
    let t = c.target.unwrap();
    assert_eq!(t.kind, SiteKind::Krill);
    assert_eq!(t.site_id.as_str(), catalog::KRILL_SITE_ID);
    assert_eq!(
        t.lines.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
        [catalog::KRILL_LINE_CN_FAST, catalog::KRILL_LINE_CDN]
    );
    assert!(t.base_url.is_none() && t.model_map.is_empty());
    assert!(
        c.notes
            .iter()
            .any(|n| n.key == "import.note.krill_model_map")
    );

    // Another Krill host: the Krill preset on its default line, with a note.
    let c = one(provider(
        ImportSource::Ccs,
        "k2",
        "Krill old",
        "https://relay.krill-ai.com/v1",
    ));
    let t = c.target.unwrap();
    assert_eq!(t.kind, SiteKind::Krill);
    assert_eq!(t.lines[0].id.as_str(), catalog::KRILL_DEFAULT_LINE);
    assert!(
        c.notes
            .iter()
            .any(|n| n.key == "import.note.krill_unknown_line")
    );
}

#[test]
fn other_addresses_become_custom_sites_with_lines_and_model_map() {
    let mut p = provider(
        ImportSource::AiToolbox,
        "c",
        "My Relay",
        "https://relay.example.com/openai/v1/",
    );
    p.extra_urls = vec![
        "https://relay.example.com/openai/v1".into(),
        "https://hk.relay.example.com/openai/v1".into(),
        "https://us.relay.example.com/v2".into(),
    ];
    p.model_map
        .insert("gpt-5.5".into(), "vendor/gpt-5.5".into());
    p.reasoning_effort = Some("high".into());
    let c = one(p);
    let t = c.target.clone().unwrap();
    assert_eq!(t.kind, SiteKind::Compatible);
    assert_eq!(t.site_id.as_str(), "my-relay");
    assert_eq!(t.profile_name, "My Relay");
    assert_eq!(
        t.base_url.as_ref().unwrap().as_str(),
        "https://relay.example.com/openai/v1/"
    );
    assert_eq!(
        t.lines
            .iter()
            .map(|l| l.origin.to_string())
            .collect::<Vec<_>>(),
        ["https://relay.example.com", "https://hk.relay.example.com"]
    );
    assert_eq!(t.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(t.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        t.model_map.get("gpt-5.5").map(String::as_str),
        Some("vendor/gpt-5.5")
    );
    assert!(
        c.notes
            .iter()
            .any(|n| n.key == "import.note.extra_urls_skipped")
    );
    assert_eq!(
        c.key,
        ImportKey::Present {
            masked: "••••0000".into()
        }
    );
}

#[test]
fn what_cannot_be_imported_says_why() {
    let reason = |p: ForeignProvider| match one(p).status {
        CandidateStatus::NotImportable { reason } => reason.key,
        other => panic!("{other:?}"),
    };
    let mut claude = provider(
        ImportSource::Ccs,
        "a",
        "Claude Relay",
        "https://x.example.com",
    );
    claude.app = ForeignApp::Claude;
    assert_eq!(reason(claude), "import.reason.not_codex");
    let mut official = provider(
        ImportSource::Ccs,
        "o",
        "OpenAI Official",
        "https://x.example.com",
    );
    official.official = true;
    assert_eq!(reason(official), "import.reason.official");
    let mut none = provider(ImportSource::Ccs, "n", "No URL", "");
    none.base_url = None;
    assert_eq!(reason(none), "import.reason.no_base_url");
    assert_eq!(
        reason(provider(
            ImportSource::Ccs,
            "h",
            "Plain",
            "http://relay.example.com/v1"
        )),
        "import.reason.insecure_url"
    );
    assert_eq!(
        reason(provider(
            ImportSource::Ccs,
            "q",
            "Query",
            "https://relay.example.com/v1?x=1"
        )),
        "import.reason.bad_url"
    );
    // Loopback http is fine (local tests).
    assert!(
        one(provider(
            ImportSource::Ccs,
            "l",
            "Local",
            "http://127.0.0.1:8080/v1"
        ))
        .importable()
    );
}

#[test]
fn keys_that_stay_behind_are_imported_without_a_key_and_say_so() {
    let mut p = provider(
        ImportSource::AiToolbox,
        "kc",
        "Keychain Relay",
        "https://kc.example.com/v1",
    );
    p.key = ForeignKey::Keychain;
    let c = one(p);
    assert!(c.importable() && c.preselected);
    assert_eq!(c.key, ImportKey::Keychain);
    let note = c
        .notes
        .iter()
        .find(|n| n.key == "import.note.keychain")
        .unwrap();
    assert!(
        note.render_fallback()
            .contains("AI Toolbox keeps this key in the system keychain")
    );
    let mut p = provider(
        ImportSource::AiToolbox,
        "env",
        "Env",
        "https://env.example.com/v1",
    );
    p.key = ForeignKey::Env("AZURE_OPENAI_API_KEY".into());
    let c = one(p);
    assert_eq!(
        c.key,
        ImportKey::EnvVar {
            name: "AZURE_OPENAI_API_KEY".into()
        }
    );
    assert!(c.notes.iter().any(|n| n.key == "import.note.env_key"));
    // Chat-format and disabled providers are offered but not pre-selected.
    let mut chat = provider(
        ImportSource::Ccs,
        "ch",
        "Chat",
        "https://chat.example.com/v1",
    );
    chat.wire_api = Some("chat".into());
    let c = one(chat);
    assert!(c.importable() && !c.preselected);
    assert!(c.notes.iter().any(|n| n.key == "import.note.chat_wire_api"));
    let mut off = provider(
        ImportSource::AiToolbox,
        "off",
        "Off",
        "https://off.example.com/v1",
    );
    off.deselect = true;
    assert!(!one(off).preselected);
}

fn existing_set() -> ProfileSet {
    let mut set = ProfileSet::default();
    let site = make_site(
        SiteId::new("my-relay"),
        "My Relay",
        &Url::parse("https://relay.example.com/v1").unwrap(),
    )
    .unwrap();
    set.custom_sites.push(site);
    let mut compat = krill("my-relay", "My Relay", None);
    compat.site_id = SiteId::new("my-relay");
    compat.auth_mode = superba_core::AuthMode::ApiKey;
    compat.set_auth(AuthChoice::ApiKey);
    set.profiles.push(compat);
    set.profiles
        .push(krill("work", "Krill", Some(catalog::KRILL_LINE_OVERSEAS)));
    set
}

fn krill(id: &str, name: &str, line: Option<&str>) -> Profile {
    let mut p = Profile {
        id: ProfileId::new(id),
        name: name.into(),
        builtin: None,
        site_id: SiteId::new(catalog::KRILL_SITE_ID),
        line_id: line.map(LineId::new),
        backup_lines: Vec::new(),
        auth_mode: superba_core::AuthMode::Pat,
        auth: AuthChoice::Auto,
        key_ref: None,
        base_url_override: None,
        model: Default::default(),
        codex_settings: Default::default(),
        model_overrides: Default::default(),
        model_picker: Default::default(),
    };
    p.set_auth(AuthChoice::Auto);
    p
}

fn sites(set: &ProfileSet) -> Vec<Site> {
    let mut s = builtin_sites();
    s.extend(set.custom_sites.clone());
    s
}

#[test]
fn conflicts_by_name_address_and_krill_line_default_to_skip() {
    let set = existing_set();
    let reads = [read(
        ImportSource::Ccs,
        vec![
            provider(
                ImportSource::Ccs,
                "a",
                "my relay",
                "https://relay.example.com/v1/",
            ),
            provider(
                ImportSource::Ccs,
                "b",
                "Other Name",
                "https://relay.example.com/v1",
            ),
            provider(
                ImportSource::Ccs,
                "c",
                "Krill Overseas",
                "https://api.krill-code.net/v1",
            ),
            provider(
                ImportSource::Ccs,
                "d",
                "Krill CDN",
                "https://api.cdn-krill-ai.com/v1",
            ),
            provider(
                ImportSource::Ccs,
                "e",
                "Krill",
                "https://new.example.com/v1",
            ),
        ],
    )];
    let scan = preview(&reads, &set, &sites(&set));
    let c: Vec<_> = scan.candidates().collect();
    let conflict = |i: usize| c[i].conflict.clone();
    let a = conflict(0).unwrap();
    assert!(a.same_name && a.same_base_url && a.profile.as_str() == "my-relay");
    let b = conflict(1).unwrap();
    assert!(!b.same_name && b.same_base_url);
    let k = conflict(2).unwrap();
    assert!(k.same_base_url && k.profile.as_str() == "work");
    assert!(conflict(3).is_none(), "another Krill line is no conflict");
    let e = conflict(4).unwrap();
    assert!(e.same_name && !e.same_base_url && e.profile.as_str() == "work");
    assert!(
        c.iter()
            .filter(|x| x.conflict.is_some())
            .all(|x| !x.preselected)
    );
    // The renamed name avoids every existing and proposed name.
    assert_eq!(
        c[0].target.as_ref().unwrap().profile_name,
        "my relay (CC Switch)"
    );
    assert_eq!(
        c[4].target.as_ref().unwrap().profile_name,
        "Krill (CC Switch)"
    );
    // Default selection (spb import without --select) takes them all; skip
    // leaves the conflicts alone.
    let sel = scan.default_selections(ConflictAction::Skip);
    assert_eq!(sel.len(), 5);
    let planned = plan(&reads, &scan, &set, &sites(&set), &sel, &make_site);
    let kinds: Vec<&str> = planned
        .iter()
        .map(|p| match &p.action {
            PlannedAction::Create { .. } => "create",
            PlannedAction::Update { .. } => "update",
            PlannedAction::Skip { .. } => "skip",
        })
        .collect();
    assert_eq!(kinds, ["skip", "skip", "skip", "create", "skip"]);
}

#[test]
fn rename_creates_next_to_it_and_update_changes_only_the_connection() {
    let set = existing_set();
    let mut a = provider(
        ImportSource::AiToolbox,
        "a",
        "My Relay",
        "https://relay2.example.com/v1",
    );
    a.extra_urls = vec!["https://hk.relay2.example.com/v1".into()];
    a.model_map
        .insert("gpt-5.5".into(), "vendor/gpt-5.5".into());
    let mut k = provider(
        ImportSource::AiToolbox,
        "k",
        "Krill",
        "https://api.cdn-krill-ai.com/codex/v1",
    );
    k.model = Some("gpt-6".into());
    let reads = [read(ImportSource::AiToolbox, vec![a, k])];
    let all = sites(&set);
    let scan = preview(&reads, &set, &all);
    let ids: Vec<String> = scan.candidates().map(|c| c.id.clone()).collect();

    let sel = |action| {
        ids.iter()
            .map(|i| ImportSelection::new(i.clone(), action))
            .collect::<Vec<_>>()
    };
    let renamed = plan(
        &reads,
        &scan,
        &set,
        &all,
        &sel(ConflictAction::Rename),
        &make_site,
    );
    match &renamed[0].action {
        PlannedAction::Create {
            site: Some(site),
            profile,
            key,
        } => {
            assert_eq!(profile.name, "My Relay (AI Toolbox)");
            assert_eq!(profile.id.as_str(), "my-relay-ai-toolbox");
            assert_eq!(site.id, profile.site_id);
            assert_eq!(site.lines.len(), 2);
            assert_eq!(site.model_map.len(), 1);
            assert_eq!(profile.auth_choice(), AuthChoice::ApiKey);
            assert_eq!(key.as_ref().unwrap().expose(), "sk-test-a-0000");
        }
        other => panic!("{other:?}"),
    }
    match &renamed[1].action {
        PlannedAction::Create {
            site: None,
            profile,
            ..
        } => {
            assert_eq!(profile.name, "Krill (AI Toolbox)");
            assert_eq!(profile.site_id.as_str(), catalog::KRILL_SITE_ID);
            assert_eq!(
                profile.line_id.as_ref().unwrap().as_str(),
                catalog::KRILL_LINE_CDN
            );
            assert_eq!(profile.auth_choice(), AuthChoice::Auto);
            assert!(
                !profile.codex_settings.is_empty(),
                "Krill's recommended settings"
            );
            assert_eq!(profile.model.model.as_deref(), Some("gpt-6"));
            assert!(
                !catalog::presets()
                    .iter()
                    .any(|p| p.id == profile.id.as_str())
            );
        }
        other => panic!("{other:?}"),
    }

    let updated = plan(
        &reads,
        &scan,
        &set,
        &all,
        &sel(ConflictAction::Update),
        &make_site,
    );
    match &updated[0].action {
        PlannedAction::Update {
            site: Some(site),
            profile,
            key,
        } => {
            assert_eq!(profile.id.as_str(), "my-relay");
            assert_eq!(profile.name, "My Relay", "the name stays");
            assert_eq!(site.id.as_str(), "my-relay");
            assert_eq!(
                site.lines[0].origin.to_string(),
                "https://relay2.example.com"
            );
            assert_eq!(site.lines.len(), 2);
            assert!(key.is_some());
        }
        other => panic!("{other:?}"),
    }
    match &updated[1].action {
        PlannedAction::Update {
            site: None,
            profile,
            ..
        } => {
            assert_eq!(profile.id.as_str(), "work");
            assert_eq!(
                profile
                    .line_priority()
                    .iter()
                    .map(LineId::as_str)
                    .collect::<Vec<_>>(),
                [catalog::KRILL_LINE_CDN, catalog::KRILL_LINE_OVERSEAS]
            );
            assert_eq!(profile.model.model.as_deref(), Some("gpt-6"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn updating_across_kinds_is_refused_and_unknown_selections_are_skipped() {
    let set = existing_set();
    // Named like the Krill profile, but a custom site.
    let reads = [read(
        ImportSource::Ccs,
        vec![provider(
            ImportSource::Ccs,
            "x",
            "Krill",
            "https://other.example.com/v1",
        )],
    )];
    let all = sites(&set);
    let scan = preview(&reads, &set, &all);
    let id = scan.candidates().next().unwrap().id.clone();
    let planned = plan(
        &reads,
        &scan,
        &set,
        &all,
        &[
            ImportSelection::new(id.clone(), ConflictAction::Update),
            ImportSelection::new(id, ConflictAction::Update),
            ImportSelection::new("ccs:codex/gone", ConflictAction::Skip),
        ],
        &make_site,
    );
    let reasons: Vec<String> = planned
        .iter()
        .map(|p| match &p.action {
            PlannedAction::Skip { reason } => reason.key.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        reasons,
        [
            "import.skip.update_kind",
            "import.skip.duplicate",
            "import.skip.gone"
        ]
    );
}

#[test]
fn the_same_provider_in_both_tools_is_selected_once() {
    let a = provider(
        ImportSource::Ccs,
        "aihub",
        "AiHub",
        "https://aihub.example.org/v1",
    );
    let mut b = provider(
        ImportSource::AiToolbox,
        "aihub",
        "AiHub (from CC Switch)",
        "https://aihub.example.org/v1",
    );
    b.key = a.key.clone();
    // Same address, another key: a second account, still pre-selected.
    let mut c = provider(
        ImportSource::AiToolbox,
        "aihub2",
        "AiHub team",
        "https://aihub.example.org/v1",
    );
    c.key = ForeignKey::from_text(Some("sk-test-other"));
    let reads = [
        read(ImportSource::Ccs, vec![a]),
        read(ImportSource::AiToolbox, vec![b, c]),
    ];
    let scan = preview(&reads, &ProfileSet::default(), &builtin_sites());
    let pre: Vec<(String, bool)> = scan
        .candidates()
        .map(|c| (c.name.clone(), c.preselected))
        .collect();
    assert_eq!(
        pre,
        [
            ("AiHub".to_owned(), true),
            ("AiHub (from CC Switch)".to_owned(), false),
            ("AiHub team".to_owned(), true)
        ]
    );
    let dup = scan.candidates().nth(1).unwrap();
    assert!(dup.notes.iter().any(|n| n.key == "import.note.duplicate"));
    // Proposed ids and names are unique across the whole scan.
    let names: Vec<String> = scan
        .candidates()
        .filter_map(|c| c.target.as_ref().map(|t| t.site_id.to_string()))
        .collect();
    assert_eq!(names, ["aihub", "aihub-from-cc-switch", "aihub-team"]);
}
