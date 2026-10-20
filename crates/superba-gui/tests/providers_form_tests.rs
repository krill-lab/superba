#![allow(missing_docs)]

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use superba_core::{
    AuthChoice, AuthMode, Error, KeyRef, LineId, ModelDefaults, Profile, ProfileId, Site, SiteId,
    SiteKind,
};
use superba_gui::pages::providers::form::*;
use superba_switch::new_compatible_site;
use url::Url;

fn row(from: &str, to: &str) -> MapRow {
    MapRow {
        from: from.into(),
        to: to.into(),
    }
}

fn form() -> SiteForm {
    SiteForm {
        name: "  Example Relay  ".into(),
        url: "  https://api.example.com/v1  ".into(),
        key: "  test-api-key  ".into(),
        model: "  example-model  ".into(),
        ..SiteForm::add()
    }
}

fn site(url: &str) -> Site {
    new_compatible_site(
        SiteId::new("saved-site"),
        "Saved Site",
        &Url::parse(url).unwrap(),
    )
    .unwrap()
}

fn existing() -> (Site, Profile) {
    let site = site("https://old.example.com/v1");
    let mut profile = Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        anthropic: None,
        model_source: None,
        id: ProfileId::new("saved-profile"),
        name: "Saved Profile".into(),
        builtin: None,
        site_id: site.id.clone(),
        line_id: Some(LineId::new("default")),
        backup_lines: vec![LineId::new("backup")],
        disabled_lines: Default::default(),
        auth_mode: AuthMode::ApiKey,
        auth: AuthChoice::ApiKey,
        key_ref: Some(KeyRef::Keyring {
            service: "test-service".into(),
            account: "saved-profile".into(),
        }),
        base_url_override: None,
        model: ModelDefaults {
            auto_compact: None,
            context_window: None,
            speed: None,
            model: Some("old-model".into()),
            reasoning_effort: Some("high".into()),
        },
        codex_settings: Default::default(),
    };
    profile
        .codex_settings
        .set("features.multi_agent", true)
        .unwrap();
    (site, profile)
}

#[test]
fn slug_lowercases_ascii_collapses_separators_and_trims_dashes() {
    for (name, expected) in [
        ("  My__API / Relay...42  ", "my-api-relay-42"),
        ("---Already--Slugged---", "already-slugged"),
        ("中A文B站", "a-b"),
    ] {
        assert_eq!(slug_id(name, &[]), expected);
    }
}

#[test]
fn slug_uses_site_for_empty_or_non_ascii_names() {
    for name in ["", "  ---  ", "中文站点", "🚀"] {
        assert_eq!(slug_id(name, &[]), "site");
        assert_eq!(slug_id(name, &["site", "site-2"]), "site-3");
    }
}

#[test]
fn slug_skips_reserved_and_taken_ids() {
    for reserved in ["krill", "official", "original"] {
        assert_eq!(
            slug_id(&reserved.to_uppercase(), &[]),
            format!("{reserved}-2")
        );
        let second = format!("{reserved}-2");
        assert_eq!(slug_id(reserved, &[&second]), format!("{reserved}-3"));
    }
    assert_eq!(
        slug_id("Relay", &["relay", "relay-2", "relay-3"]),
        "relay-4"
    );
    assert_eq!(slug_id("Relay", &["relay", "relay-3"]), "relay-2");
}

#[test]
fn slug_keeps_long_ids_with_collision_suffixes_within_48_characters() {
    let name = "A".repeat(80);
    let base = "a".repeat(48);
    assert_eq!(slug_id(&name, &[]), base);
    let mut taken = vec![base];
    for suffix in 2..=9 {
        taken.push(format!("{}-{suffix}", "a".repeat(46)));
    }
    let taken: Vec<_> = taken.iter().map(String::as_str).collect();
    let id = slug_id(&name, &taken);
    assert_eq!(id, format!("{}-10", "a".repeat(45)));
    assert_eq!(id.len(), 48);
    assert!(!taken.contains(&id.as_str()));
}

#[test]
fn slug_trims_a_separator_at_the_suffix_truncation_boundary() {
    let name = format!("{}-bc", "a".repeat(45));
    let id = slug_id(&name, &[&name]);
    assert_eq!(id, format!("{}-2", "a".repeat(45)));
    assert!(id.len() <= 48);
}

#[test]
fn url_check_accepts_https() {
    for input in [
        "https://api.example.com/v1",
        "https://localhost:8443/v1",
        "https://[::1]/v1",
    ] {
        assert_eq!(check_url(&Url::parse(input).unwrap()), Ok(()), "{input}");
    }
}

#[test]
fn url_check_accepts_http_for_loopback_hosts() {
    for input in [
        "http://127.0.0.1:8123/v1",
        "http://[::1]:8123/v1",
        "http://localhost/v1",
    ] {
        assert_eq!(check_url(&Url::parse(input).unwrap()), Ok(()), "{input}");
    }
}

#[test]
fn url_check_rejects_http_for_non_loopback_hosts() {
    for input in [
        "http://api.example.com/v1",
        "http://203.0.113.2/v1",
        "http://0.0.0.0/v1",
        "http://[::]/v1",
        "http://localhost.example.com/v1",
        "http://127.0.0.1.example.com/v1",
    ] {
        assert_eq!(
            check_url(&Url::parse(input).unwrap()),
            Err(FormError::InsecureUrl),
            "{input}"
        );
    }
}

#[test]
fn url_check_rejects_schemes_other_than_http_and_https() {
    for input in [
        "ftp://localhost/v1",
        "file:///tmp/models",
        "mailto:test@example.com",
        "ws://127.0.0.1/v1",
    ] {
        assert_eq!(
            check_url(&Url::parse(input).unwrap()),
            Err(FormError::InvalidUrl),
            "{input}"
        );
    }
}

#[test]
fn prepare_add_requires_name_url_and_key() {
    for missing in ["name", "url", "key"] {
        let mut form = form();
        match missing {
            "name" => form.name = " \t".into(),
            "url" => form.url = " \n".into(),
            "key" => form.key = " \t\n".into(),
            _ => unreachable!(),
        }
        assert_eq!(
            prepare(&form, &[], None, new_compatible_site).unwrap_err(),
            FormError::Required,
            "{missing}"
        );
    }
}

#[test]
fn prepare_rejects_malformed_or_unsupported_urls() {
    for input in ["not a URL", "https://[", "ftp://api.example.com/v1"] {
        let form = SiteForm {
            url: input.into(),
            ..form()
        };
        assert_eq!(
            prepare(&form, &[], None, new_compatible_site).unwrap_err(),
            FormError::InvalidUrl
        );
    }
}

#[test]
fn prepare_rejects_insecure_remote_http() {
    let form = SiteForm {
        url: "http://api.example.com/v1".into(),
        ..form()
    };
    assert_eq!(
        prepare(&form, &[], None, new_compatible_site).unwrap_err(),
        FormError::InsecureUrl
    );
}

#[test]
fn prepare_add_builds_a_compatible_site_and_api_key_profile() {
    let mut form = form();
    form.mapping = vec![row(" gpt-5.5 ", " provider/gpt-5.5 "), MapRow::default()];
    let prepared = prepare(
        &form,
        &["example-relay", "example-relay-2"],
        None,
        new_compatible_site,
    )
    .unwrap();
    assert_eq!(prepared.site.kind, SiteKind::Compatible);
    assert_eq!(prepared.site.id.as_str(), "example-relay-3");
    assert_eq!(prepared.site.name.fallback, "Example Relay");
    assert_eq!(base_url(&prepared.site), "https://api.example.com/v1");
    assert_eq!(prepared.profile.id.as_str(), prepared.site.id.as_str());
    assert_eq!(prepared.profile.site_id, prepared.site.id);
    assert_eq!(prepared.profile.name, "Example Relay");
    assert_eq!(prepared.profile.auth_mode, AuthMode::ApiKey);
    assert_eq!(prepared.profile.auth, AuthChoice::ApiKey);
    assert_eq!(
        prepared.profile.model.model.as_deref(),
        Some("example-model")
    );
    assert_eq!(prepared.key.as_ref().unwrap().expose(), "test-api-key");
    assert_eq!(
        prepared.site.model_map,
        BTreeMap::from([("gpt-5.5".into(), "provider/gpt-5.5".into())])
    );
}

#[test]
fn prepare_add_leaves_a_blank_default_model_unset() {
    let form = SiteForm {
        model: " \t\n".into(),
        ..form()
    };
    let prepared = prepare(&form, &[], None, new_compatible_site).unwrap();
    assert_eq!(prepared.profile.model.model, None);
}

#[test]
fn prepare_propagates_mapping_validation_errors() {
    for (mapping, error) in [
        (vec![row("model", " ")], FormError::IncompleteMapping),
        (
            vec![row("model", "a"), row("model", "b")],
            FormError::DuplicateMapping("model".into()),
        ),
    ] {
        let form = SiteForm { mapping, ..form() };
        assert_eq!(
            prepare(&form, &[], None, new_compatible_site).unwrap_err(),
            error
        );
    }
}

#[test]
fn prepare_edit_keeps_ids_and_all_profile_fields_except_name_model_and_url() {
    let (mut site, mut profile) = existing();
    site.homepage = Some(Url::parse("https://accounts.example.com/").unwrap());
    profile.base_url_override = Some(Url::parse("https://old.example.com/v1").unwrap());
    let mut form = SiteForm::edit(&site, &profile);
    form.name = "  Renamed Site  ".into();
    form.url = "https://new.example.com/v2".into();
    form.model = "  new-model  ".into();
    form.key = " \n".into();
    let prepared = prepare(
        &form,
        &["renamed-site", "saved-site", "saved-profile"],
        Some((&site, &profile)),
        new_compatible_site,
    )
    .unwrap();
    let mut expected = profile.clone();
    expected.name = "Renamed Site".into();
    expected.model.model = Some("new-model".into());
    // An existing override follows the edited URL.
    expected.base_url_override = Some(Url::parse("https://new.example.com/v2").unwrap());
    assert_eq!(prepared.profile, expected);
    assert_eq!(prepared.site.id, site.id);
    assert_eq!(prepared.site.name.fallback, "Renamed Site");
    assert_eq!(prepared.site.homepage, site.homepage);
    assert_eq!(base_url(&prepared.site), "https://new.example.com/v2");
    assert!(prepared.key.is_none());
}

#[test]
fn prepare_edit_can_replace_the_key() {
    let (site, profile) = existing();
    let mut form = SiteForm::edit(&site, &profile);
    form.key = " \t replacement-key \n".into();
    let prepared = prepare(&form, &[], Some((&site, &profile)), new_compatible_site).unwrap();
    assert_eq!(prepared.key.as_ref().unwrap().expose(), "replacement-key");
}

#[test]
fn prepare_edit_clears_a_blank_model_without_losing_reasoning_effort() {
    let (site, profile) = existing();
    let mut form = SiteForm::edit(&site, &profile);
    form.model = " \t".into();
    let prepared = prepare(&form, &[], Some((&site, &profile)), new_compatible_site).unwrap();
    assert_eq!(prepared.profile.model.model, None);
    assert_eq!(
        prepared.profile.model.reasoning_effort,
        profile.model.reasoning_effort
    );
}

#[test]
fn prepare_wraps_constructor_errors_as_rejected_messages() {
    let error = prepare(&form(), &[], None, |id, name, url| {
        assert_eq!(id.as_str(), "example-relay");
        assert_eq!(name, "Example Relay");
        assert_eq!(url.as_str(), "https://api.example.com/v1");
        Err(Error::Invalid {
            message: "constructor refused this site".into(),
        })
    })
    .unwrap_err();
    assert_eq!(
        error,
        FormError::Rejected("invalid input: constructor refused this site".into())
    );
}

#[test]
fn base_url_round_trips_paths_ports_and_bare_origins() {
    for url in [
        "https://api.example.com/v1",
        "https://api.example.com:8443/v1",
        "https://api.example.com:8443",
        "https://api.example.com",
        "https://api.example.com/custom/models",
    ] {
        assert_eq!(base_url(&site(url)), url);
    }
}

#[test]
fn form_debug_redacts_the_typed_key_even_when_revealed() {
    for reveal in [false, true] {
        let form = SiteForm {
            key: "  unique-test-secret-89  ".into(),
            reveal,
            ..form()
        };
        for debug in [format!("{form:?}"), format!("{form:#?}")] {
            assert!(!debug.contains("unique-test-secret-89"));
            assert!(debug.contains("key_entered: true"));
        }
    }
}

#[test]
fn typed_key_trims_input_and_returns_none_for_blanks() {
    assert_eq!(form().typed_key().unwrap().expose(), "test-api-key");
    for key in ["", " \t\n"] {
        assert!(
            SiteForm {
                key: key.into(),
                ..form()
            }
            .typed_key()
            .is_none()
        );
    }
}

#[test]
fn edit_form_prefills_saved_fields_and_sorted_mapping_without_a_key() {
    let (mut site, profile) = existing();
    site.model_map = BTreeMap::from([
        ("z-model".into(), "provider/z".into()),
        ("a-model".into(), "provider/a".into()),
    ]);
    let form = SiteForm::edit(&site, &profile);
    assert_eq!(
        form.mode,
        Some(FormMode::Edit {
            site: site.id.clone(),
            profile: profile.id.clone()
        })
    );
    assert!(form.is_edit());
    assert_eq!(form.name, "Saved Site");
    assert_eq!(form.url, "https://old.example.com/v1");
    assert_eq!(form.model, "old-model");
    assert_eq!(
        form.mapping,
        vec![row("a-model", "provider/a"), row("z-model", "provider/z")]
    );
    assert_eq!(form.key, "");
    assert!(!form.reveal);
}

#[test]
fn edit_form_prefers_the_profile_url_override_and_handles_no_model() {
    let (site, mut profile) = existing();
    profile.base_url_override = Some(Url::parse("https://override.example.com:8443/v2/").unwrap());
    profile.model.model = None;
    let form = SiteForm::edit(&site, &profile);
    assert_eq!(form.url, "https://override.example.com:8443/v2");
    assert_eq!(form.model, "");
}

#[test]
fn edited_url_replaces_an_existing_profile_override() {
    let (site, mut profile) = existing();
    profile.base_url_override = Some(Url::parse("https://old.example.com/v1").unwrap());
    let mut form = SiteForm::edit(&site, &profile);
    form.url = "https://new.example.com/v2".into();
    let prepared = prepare(&form, &[], Some((&site, &profile)), new_compatible_site).unwrap();
    let reopened = SiteForm::edit(&prepared.site, &prepared.profile);
    assert_eq!(base_url(&prepared.site), "https://new.example.com/v2");
    assert_eq!(
        prepared.profile.base_url_override.as_ref().map(Url::as_str),
        Some("https://new.example.com/v2")
    );
    assert_eq!(reopened.url, "https://new.example.com/v2");
}

#[test]
fn edit_without_an_override_keeps_none() {
    let (site, profile) = existing();
    assert!(profile.base_url_override.is_none());
    let mut form = SiteForm::edit(&site, &profile);
    form.url = "https://new.example.com/v2".into();
    let prepared = prepare(&form, &[], Some((&site, &profile)), new_compatible_site).unwrap();
    assert!(prepared.profile.base_url_override.is_none());
}
