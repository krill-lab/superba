#![allow(missing_docs)]

use std::collections::BTreeMap;

use superba_core::catalog::{
    builtin_profiles, builtin_sites, is_krill_host, krill_site, official_site,
};
use superba_core::origin::chatgpt_base_url;
use superba_core::{
    AuthMode, BuiltinProfile, CodexPath, LineId, Origin, ResolvedEndpoints, SiteKind,
};

#[test]
fn https_origins_are_normalized_without_trailing_slashes() {
    for (input, expected) in [
        ("https://api.example.com", "https://api.example.com"),
        ("https://api.example.com/", "https://api.example.com"),
        (
            "https://api.example.com:8443/",
            "https://api.example.com:8443",
        ),
        (
            "  https://API.EXAMPLE.COM:443/  ",
            "https://api.example.com",
        ),
    ] {
        let origin = Origin::parse(input).expect(input);
        assert_eq!(origin.as_str(), expected);
        assert_eq!(origin.to_string(), expected);
        assert!(!origin.as_str().ends_with('/'));
        assert_eq!(origin.host(), "api.example.com");
        assert!(!origin.is_loopback());
        assert_eq!(expected.parse::<Origin>().unwrap(), origin);
    }
}

#[test]
fn origins_reject_paths_queries_fragments_credentials_and_remote_http() {
    for input in [
        "https://api.example.com/v1",
        "https://api.example.com//",
        "https://api.example.com?key=value",
        "https://api.example.com/?",
        "https://api.example.com#fragment",
        "https://api.example.com/#",
        "https://user@api.example.com",
        "https://user:password@api.example.com",
        "https://:password@api.example.com",
        "ftp://api.example.com",
        "http://api.example.com",
        "http://203.0.113.10:8080",
        "http://localhost.example.com",
        "api.example.com",
        "",
    ] {
        assert!(
            Origin::parse(input).is_err(),
            "accepted invalid origin {input:?}"
        );
    }
}

#[test]
fn http_is_allowed_for_loopback_hosts() {
    for input in [
        "http://127.0.0.1:8123",
        "http://127.0.0.2:8123",
        "http://localhost",
        "http://localhost:8123/",
        "http://[::1]:8123",
    ] {
        let origin = Origin::parse(input).expect(input);
        assert!(origin.is_loopback(), "{input}");
        assert!(!origin.as_str().ends_with('/'));
    }
}

#[test]
fn http_rejects_dns_names_that_merely_begin_with_127() {
    assert!(Origin::parse("http://127.attacker.example:8080").is_err());
    assert!(
        !Origin::parse("https://127.attacker.example")
            .unwrap()
            .is_loopback()
    );
}

#[test]
fn origin_join_keeps_the_origin_and_accepts_paths_with_or_without_leading_slash() {
    let origin = Origin::parse("https://api.example.com:8443/").unwrap();
    for path in ["/backend-api/codex", "backend-api/codex"] {
        assert_eq!(
            origin.join(path).as_str(),
            "https://api.example.com:8443/backend-api/codex"
        );
    }
    assert_eq!(origin.join("/").as_str(), "https://api.example.com:8443/");
    assert_eq!(
        origin.join("/v1/models?limit=5").as_str(),
        "https://api.example.com:8443/v1/models?limit=5"
    );
}

#[test]
fn origin_serde_uses_a_validated_string() {
    let origin = Origin::parse("https://api.example.com:8443/").unwrap();
    let json = serde_json::to_string(&origin).unwrap();
    assert_eq!(json, r#""https://api.example.com:8443""#);
    assert_eq!(serde_json::from_str::<Origin>(&json).unwrap(), origin);
    assert!(serde_json::from_str::<Origin>(r#""https://api.example.com/v1""#).is_err());
    assert!(serde_json::from_str::<Origin>(r#"{"origin":"https://api.example.com"}"#).is_err());
}

#[test]
fn codex_paths_and_chatgpt_base_url_preserve_required_slashes() {
    let origin = Origin::parse("https://api.example.com:8443").unwrap();
    for (path, suffix, expected) in [
        (
            CodexPath::BackendApiCodex,
            "/backend-api/codex",
            "https://api.example.com:8443/backend-api/codex",
        ),
        (
            CodexPath::CodexV1,
            "/codex/v1",
            "https://api.example.com:8443/codex/v1",
        ),
        (CodexPath::AsIs, "", "https://api.example.com:8443/"),
    ] {
        assert_eq!(path.suffix(), suffix);
        assert_eq!(path.base_url(&origin).as_str(), expected);
    }
    let chatgpt = chatgpt_base_url(&origin);
    assert_eq!(
        chatgpt.as_str(),
        "https://api.example.com:8443/backend-api/"
    );
    assert_eq!(
        chatgpt.join("wham/accounts/check").unwrap().path(),
        "/backend-api/wham/accounts/check"
    );
}

#[test]
fn resolved_line_endpoints_only_add_auth_endpoints_for_pat() {
    let origin = Origin::parse("https://api.example.com").unwrap();
    let pat =
        ResolvedEndpoints::for_line(&origin, CodexPath::BackendApiCodex, AuthMode::Pat).unwrap();
    assert_eq!(pat.origin, origin);
    assert_eq!(pat.codex_path, CodexPath::BackendApiCodex);
    assert_eq!(
        pat.openai_base_url.as_str(),
        "https://api.example.com/backend-api/codex"
    );
    assert_eq!(
        pat.chatgpt_base_url.unwrap().as_str(),
        "https://api.example.com/backend-api/"
    );
    assert_eq!(pat.authapi_origin, Some(origin.clone()));
    // PAT is only valid on the backend-api path; `/codex/v1` is an API-key fallback.
    for path in [CodexPath::CodexV1, CodexPath::AsIs] {
        let err = ResolvedEndpoints::for_line(&origin, path, AuthMode::Pat).unwrap_err();
        assert_eq!(err.code(), "invalid");
    }
    for path in [
        CodexPath::BackendApiCodex,
        CodexPath::CodexV1,
        CodexPath::AsIs,
    ] {
        let apikey = ResolvedEndpoints::for_line(&origin, path, AuthMode::ApiKey).unwrap();
        assert_eq!(apikey.origin, origin);
        assert_eq!(apikey.codex_path, path);
        assert_eq!(apikey.openai_base_url, path.base_url(&origin));
        assert_eq!(apikey.chatgpt_base_url, None);
        assert_eq!(apikey.authapi_origin, None);
    }
}

#[test]
fn compatible_endpoints_preserve_the_full_base_url() {
    for input in [
        "https://compatible.example:8443/v1/",
        "http://127.0.0.1:8123/custom/v1",
    ] {
        let base = input.parse().unwrap();
        let endpoints = ResolvedEndpoints::for_base_url(&base).unwrap();
        assert_eq!(endpoints.openai_base_url, base);
        assert_eq!(
            endpoints.origin.as_str(),
            base.origin().ascii_serialization()
        );
        assert_eq!(endpoints.codex_path, CodexPath::AsIs);
        assert_eq!(endpoints.chatgpt_base_url, None);
        assert_eq!(endpoints.authapi_origin, None);
    }
    for input in [
        "http://compatible.example/v1",
        "ftp://compatible.example/v1",
    ] {
        assert!(ResolvedEndpoints::for_base_url(&input.parse().unwrap()).is_err());
    }
}

#[test]
fn krill_catalog_has_exactly_the_three_expected_lines_and_overseas_default() {
    let site = krill_site();
    assert_eq!(site.id, "krill");
    assert_eq!(site.kind, SiteKind::Krill);
    assert_eq!(site.lines.len(), 3);
    let actual: BTreeMap<_, _> = site
        .lines
        .iter()
        .map(|line| (line.id.as_str(), line.origin.as_str()))
        .collect();
    let expected = BTreeMap::from([
        ("cn-fast", "https://api-slb.krill-code.net".to_owned()),
        ("overseas", "https://api.krill-code.net".to_owned()),
        ("cdn", "https://api.cdn-krill-ai.com".to_owned()),
    ]);
    assert_eq!(actual, expected);
    assert_eq!(site.default_line, Some(LineId::from("overseas")));
    assert_eq!(site.default_line().unwrap().id, "overseas");
    assert_eq!(
        site.line(&LineId::from("cdn")).unwrap().origin.as_str(),
        "https://api.cdn-krill-ai.com"
    );
    assert!(site.line(&LineId::from("missing")).is_none());
    assert!(site.supports(AuthMode::Pat));
    assert!(site.supports(AuthMode::ApiKey));
    assert!(!site.supports(AuthMode::OfficialChatGpt));
}

#[test]
fn official_site_and_builtin_profile_identities_are_stable() {
    let site = official_site();
    assert_eq!(site.id, "official");
    assert_eq!(site.kind, SiteKind::Official);
    assert!(site.lines.is_empty());
    assert!(site.default_line.is_none());
    assert!(site.default_line().is_none());
    assert!(site.supports(AuthMode::OfficialChatGpt));
    assert!(!site.supports(AuthMode::ApiKey));
    assert!(!site.supports(AuthMode::Pat));
    let profiles = builtin_profiles();
    assert_eq!(
        profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["original", "official"]
    );
    assert_eq!(profiles[0].builtin, Some(BuiltinProfile::Original));
    assert_eq!(profiles[1].builtin, Some(BuiltinProfile::Official));
    assert!(
        profiles
            .iter()
            .all(|p| p.is_builtin() && p.key_ref.is_none())
    );
    assert_eq!(builtin_sites(), vec![krill_site(), site]);
}

#[test]
fn krill_host_matching_respects_domain_boundaries_and_normalization() {
    for host in [
        "krill-code.net",
        "api.krill-code.net",
        "nested.api.krill-code.net",
        "krill-code.com",
        "www.krill-code.com",
        "cdn-krill-ai.com",
        "api.cdn-krill-ai.com",
        "krill-ai.com",
        "api.krill-ai.com",
        "API.KRILL-CODE.NET.",
    ] {
        assert!(is_krill_host(host), "rejected {host}");
    }
    for host in [
        "evilkrill-code.net",
        "krill-code.net.evil.example",
        "api.krill-code.net.evil.example",
        "evilcdn-krill-ai.com",
        "notkrill-ai.com",
        "krill-code.org",
        "example.com",
        "",
    ] {
        assert!(!is_krill_host(host), "accepted {host}");
    }
}

#[test]
fn pat_endpoints_need_an_https_origin() {
    // Codex refuses a plain-http chatgpt_base_url, even on loopback.
    let http = Origin::parse("http://127.0.0.1:18001").unwrap();
    assert!(!http.is_https());
    let err =
        ResolvedEndpoints::for_line(&http, CodexPath::BackendApiCodex, AuthMode::Pat).unwrap_err();
    assert!(err.to_string().contains("https"), "{err}");
    assert!(
        ResolvedEndpoints::for_line(&http, CodexPath::BackendApiCodex, AuthMode::ApiKey).is_ok()
    );
    let https = Origin::parse("https://127.0.0.1:18001").unwrap();
    assert!(https.is_https());
    assert!(ResolvedEndpoints::for_line(&https, CodexPath::BackendApiCodex, AuthMode::Pat).is_ok());
}
