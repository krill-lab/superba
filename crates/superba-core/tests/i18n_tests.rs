#![allow(missing_docs)]

use std::collections::BTreeSet;

use pretty_assertions::assert_eq;
use superba_core::codex::VersionCapability;
use superba_core::fdlimit;
use superba_core::i18n::{core_catalog_source, flatten_toml, interpolate};
use superba_core::{
    AuthMode, BlockReason, Catalog, CodexVersion, CostSource, Error, FullRestartReason, KeyAccess,
    Locale, LocalizedText, RouterLineHealth, UsageNotice,
};

fn placeholders(template: &str) -> BTreeSet<&str> {
    template
        .split('{')
        .skip(1)
        .map(|part| {
            part.split_once('}')
                .expect("catalog placeholder must have a closing brace")
                .0
        })
        .collect()
}

fn assert_catalog_contract(prefix: &str, messages: &[LocalizedText]) {
    let message_keys: BTreeSet<_> = messages.iter().map(|text| text.key.as_str()).collect();
    for locale in Locale::ALL {
        let entries = flatten_toml(core_catalog_source(locale)).unwrap();
        let catalog_keys: BTreeSet<_> = entries
            .keys()
            .filter(|key| key.starts_with(prefix))
            .map(String::as_str)
            .collect();
        assert_eq!(
            message_keys, catalog_keys,
            "message coverage for {prefix} in {locale}"
        );
        for text in messages {
            let template = entries
                .get(&text.key)
                .unwrap_or_else(|| panic!("{} missing in {locale}", text.key));
            let arg_names: BTreeSet<_> = text
                .args
                .iter()
                .map(|(name, _)| name.as_str())
                .chain(text.nested.iter().map(|(name, _)| name.as_str()))
                .collect();
            for name in placeholders(template).union(&placeholders(&text.fallback)) {
                assert!(
                    arg_names.contains(name),
                    "{} in {locale}: missing argument {{{name}}}",
                    text.key
                );
            }
            let rendered = Catalog::new(locale).text(text);
            assert!(
                placeholders(&rendered).is_empty(),
                "{} in {locale}: unexpanded template {rendered:?}",
                text.key
            );
        }
    }
}

#[test]
fn core_catalogs_have_identical_nonempty_key_sets() {
    let en = flatten_toml(core_catalog_source(Locale::En)).unwrap();
    let zh = flatten_toml(core_catalog_source(Locale::ZhCn)).unwrap();
    assert!(!en.is_empty());
    assert_eq!(
        en.keys().collect::<BTreeSet<_>>(),
        zh.keys().collect::<BTreeSet<_>>()
    );
    assert!(
        en.values()
            .chain(zh.values())
            .all(|value| !value.is_empty())
    );
}

#[test]
fn catalogs_render_named_arguments_in_both_languages() {
    let text = KeyAccess::Unknown {
        status: Some(503),
        message: "maintenance".into(),
    }
    .text();
    let english = Catalog::new(Locale::En);
    let chinese = Catalog::new(Locale::ZhCn);
    assert_eq!(english.locale(), Locale::En);
    assert_eq!(chinese.locale(), Locale::ZhCn);
    assert_eq!(english.text(&text), "Unexpected answer (503): maintenance");
    assert_eq!(chinese.text(&text), "意外的响应（503）：maintenance");
    let missing = KeyAccess::Unknown {
        status: None,
        message: "offline".into(),
    }
    .text();
    assert_eq!(english.text(&missing), "Unexpected answer (-): offline");
    assert_eq!(chinese.text(&missing), "意外的响应（-）：offline");
}

#[test]
fn catalog_falls_back_to_english_then_localized_fallback() {
    let mut catalog = Catalog::new(Locale::ZhCn);
    catalog
        .add_toml(Locale::En, "[test]\nenglish_only = 'English {name}'")
        .unwrap();
    let english_only =
        LocalizedText::new("test.english_only", "Fallback {name}").arg("name", "Ada");
    assert_eq!(catalog.text(&english_only), "English Ada");
    assert_eq!(catalog.get("test.english_only"), Some("English {name}"));
    let missing = LocalizedText::new("test.missing", "Fallback {name}").arg("name", "Ada");
    assert_eq!(catalog.text(&missing), "Fallback Ada");
    assert_eq!(catalog.get("test.missing"), None);
    assert_eq!(
        catalog.tr("test.missing", "literal fallback"),
        "literal fallback"
    );
}

#[test]
fn catalog_layers_override_within_the_selected_locale() {
    let mut catalog = Catalog::new(Locale::ZhCn);
    catalog
        .add_toml(Locale::En, "[test]\ngreeting = 'Hello {name}'")
        .unwrap();
    catalog
        .add_toml(Locale::ZhCn, "[test]\ngreeting = '你好，{name}'")
        .unwrap();
    catalog
        .add_toml(Locale::En, "[test]\ngreeting = 'Updated English {name}'")
        .unwrap();
    let text = LocalizedText::new("test.greeting", "missing").arg("name", "Ada");
    assert_eq!(catalog.text(&text), "你好，Ada");
    catalog
        .add_toml(Locale::ZhCn, "[test]\ngreeting = '欢迎，{name}'")
        .unwrap();
    assert_eq!(catalog.text(&text), "欢迎，Ada");
    let mut english = Catalog::new(Locale::En);
    english
        .add_toml(Locale::ZhCn, "[test]\ngreeting = '你好，{name}'")
        .unwrap();
    assert_eq!(english.get("test.greeting"), None);
}

#[test]
fn localized_text_replaces_duplicate_arguments_and_preserves_unknown_placeholders() {
    let text = LocalizedText::new("test.value", "{name}: {count}; {name} / {unknown}")
        .arg("name", "old")
        .arg("count", 2)
        .arg("name", "new");
    assert_eq!(text.args.len(), 2);
    assert_eq!(text.render_fallback(), "new: 2; new / {unknown}");
    assert_eq!(text.to_string(), text.render_fallback());
    assert_eq!(interpolate("{unknown}", &[]), "{unknown}");
}

#[test]
fn locale_parsing_accepts_common_environment_forms_and_rejects_unknown_languages() {
    for input in ["zh_CN.UTF-8", "zh-CN", "zh_Hans", " ZH_cn ", "zh"] {
        assert_eq!(Locale::parse(input), Some(Locale::ZhCn), "{input}");
    }
    for input in ["en_US", "en_US.UTF-8", "en", "EN-us", "C", "POSIX"] {
        assert_eq!(Locale::parse(input), Some(Locale::En), "{input}");
    }
    for input in ["fr", "de_DE.UTF-8", "", "english", "zho"] {
        assert_eq!(Locale::parse(input), None, "{input}");
    }
    assert_eq!(Locale::En.code(), "en");
    assert_eq!(Locale::ZhCn.to_string(), "zh-CN");
}

#[test]
fn flatten_toml_accepts_dotted_and_nested_keys_but_rejects_nontext_values() {
    let flat =
        flatten_toml("root = 'Root'\n[nested]\nleaf = 'Leaf'\n[nested.deep]\nmessage = 'Deep'")
            .unwrap();
    assert_eq!(flat.len(), 3);
    assert_eq!(flat["root"], "Root");
    assert_eq!(flat["nested.leaf"], "Leaf");
    assert_eq!(flat["nested.deep.message"], "Deep");
    for input in ["key = 42", "key = true", "key = ['text']", "[unterminated"] {
        assert_eq!(flatten_toml(input).unwrap_err().code(), "parse");
    }
}

#[test]
fn every_full_restart_reason_has_complete_translations() {
    let texts: Vec<_> = [
        FullRestartReason::NoCdp,
        FullRestartReason::NoBridge,
        FullRestartReason::AuthApiMissing,
        FullRestartReason::AuthApiUnhealthy,
        FullRestartReason::CrossAuthFamily,
        FullRestartReason::SessionMigration,
        FullRestartReason::PlatformNoHot,
        FullRestartReason::Requested,
    ]
    .into_iter()
    .map(FullRestartReason::text)
    .collect();
    assert_catalog_contract("restart.reason.", &texts);
}

#[test]
fn every_block_reason_has_complete_translations_and_arguments() {
    let reasons = [
        BlockReason::VersionTooOld {
            consumer: "local CLI".into(),
            found: CodexVersion::new(0, 150, 0),
            required: CodexVersion::new(0, 162, 0),
            capability: VersionCapability::PersonalAccessToken,
        },
        BlockReason::LineUnavailable {
            line_id: "overseas".into(),
        },
        BlockReason::AuthModeUnavailable {
            line_id: "cn-fast".into(),
            auth_mode: AuthMode::Pat,
        },
        BlockReason::KeyRejected {
            line_id: "cdn".into(),
            access: KeyAccess::AccessPointDenied,
        },
        BlockReason::MissingKey,
        BlockReason::JournalPending,
        BlockReason::FileNotWritable {
            path: "/fixtures/config.toml".into(),
        },
        BlockReason::Unparseable {
            path: "/fixtures/auth.json".into(),
            message: "unexpected token".into(),
        },
        BlockReason::Elevated,
        BlockReason::OtherUser {
            account: "NT AUTHORITY\\SYSTEM".into(),
            desktop_user: Some("PC\\alice".into()),
        },
        BlockReason::OtherUser {
            account: "NT AUTHORITY\\LOCAL SERVICE".into(),
            desktop_user: None,
        },
        BlockReason::Locked {
            what: "state.lock".into(),
        },
        BlockReason::NoBaseline,
        BlockReason::UnsupportedTarget {
            detail: "unknown OS".into(),
        },
        BlockReason::RouterUnavailable {
            detail: "connection refused".into(),
        },
        BlockReason::RouterFromWslNat {
            distro: "Ubuntu".into(),
        },
        BlockReason::ModelCatalog {
            reason: superba_core::LocalizedText::new("x", "missing field `shell_type`"),
        },
    ];
    let texts: Vec<_> = reasons.iter().map(BlockReason::text).collect();
    assert_catalog_contract("block.", &texts);
}

#[test]
fn every_key_access_variant_has_complete_translations_and_arguments() {
    let variants = [
        KeyAccess::Ok,
        KeyAccess::InvalidKey,
        KeyAccess::AccessPointDenied,
        KeyAccess::QuotaExhausted,
        KeyAccess::ChannelDenied,
        KeyAccess::EdgeBlocked,
        KeyAccess::Unreachable,
        KeyAccess::Unknown {
            status: Some(502),
            message: "bad gateway".into(),
        },
        KeyAccess::Unknown {
            status: None,
            message: "no response".into(),
        },
    ];
    let texts: Vec<_> = variants.iter().map(KeyAccess::text).collect();
    assert_catalog_contract("key_access.", &texts);
}

#[test]
fn every_usage_text_has_complete_translations_and_arguments() {
    let notices = [
        UsageNotice::RateLimited {
            retry_after_secs: Some(600),
        },
        UsageNotice::RateLimited {
            retry_after_secs: None,
        },
        UsageNotice::RefreshFailed {
            reason: "timeout".into(),
        },
    ];
    let mut texts: Vec<_> = notices.iter().map(UsageNotice::text).collect();
    texts.extend([CostSource::Costs.text(), CostSource::UsageApi.text()]);
    assert_catalog_contract("usage.", &texts);
}

#[test]
fn claude_routing_reasons_have_complete_translations() {
    let texts = vec![
        superba_core::ClaudeIneligible::NoAnthropicEndpoint.text(),
        superba_core::ClaudeIneligible::NoProtocolRoute.text(),
        superba_core::RouterClaudeMember {
            profile_id: superba_core::ProfileId::new("translated"),
            name: "Translated".into(),
            endpoint: None,
            ineligible: None,
            translated: true,
            preferred: false,
            claude_only: false,
        }
        .connection_text(),
    ];
    assert_catalog_contract("claude_routing.", &texts);
}

#[test]
fn openai_routing_reasons_have_complete_translations() {
    let texts = vec![
        superba_core::OpenAiIneligible::NoResponsesEndpoint.text(),
        superba_core::OpenAiIneligible::NoChatEndpoint.text(),
    ];
    assert_catalog_contract("openai_routing.", &texts);
}

#[test]
fn every_router_line_health_has_complete_translations() {
    let texts: Vec<_> = [
        RouterLineHealth::Healthy,
        RouterLineHealth::Open,
        RouterLineHealth::HalfOpen,
    ]
    .iter()
    .map(|h| h.text())
    .collect();
    assert_catalog_contract("router.", &texts);
}

#[test]
fn every_error_code_has_complete_translations_and_detail_argument() {
    let variants = [
        (
            Error::NotImplemented {
                what: "test component".into(),
            },
            "not_implemented",
        ),
        (
            Error::Unsupported {
                what: "platform".into(),
            },
            "unsupported",
        ),
        (
            Error::Io {
                path: "/fixtures/config.toml".into(),
                kind: "InvalidData".into(),
                message: "stream did not contain valid UTF-8".into(),
                open_files_limit: None,
            },
            "io",
        ),
        (
            Error::Parse {
                what: "config.toml".into(),
                message: "syntax error".into(),
            },
            "parse",
        ),
        (
            Error::Invalid {
                message: "bad origin".into(),
            },
            "invalid",
        ),
        (
            Error::NotFound {
                what: "profile".into(),
            },
            "not_found",
        ),
        (
            Error::Conflict {
                message: "changed hash".into(),
            },
            "conflict",
        ),
        (
            Error::Blocked {
                reasons: vec![BlockReason::MissingKey],
            },
            "blocked",
        ),
        (
            Error::Locked {
                what: "state.lock".into(),
            },
            "locked",
        ),
        (
            Error::Network {
                url: "https://example.com".into(),
                message: "connection refused".into(),
            },
            "network",
        ),
        (
            Error::Http {
                url: "https://example.com".into(),
                status: 503,
                message: "unavailable".into(),
            },
            "http",
        ),
        (
            Error::Cdp {
                message: "bridge missing".into(),
            },
            "cdp",
        ),
        (
            Error::Process {
                message: "process exited".into(),
            },
            "process",
        ),
        (
            Error::Ssh {
                host: "build-host".into(),
                message: "connection closed".into(),
            },
            "ssh",
        ),
        (
            Error::Wsl {
                distro: "Ubuntu".into(),
                message: "the distribution did not start".into(),
            },
            "wsl",
        ),
        (
            Error::Secret {
                message: "keyring unavailable".into(),
            },
            "secret",
        ),
        (Error::Cancelled, "cancelled"),
        (
            Error::Timeout {
                what: "probe".into(),
            },
            "timeout",
        ),
        (
            Error::Other {
                message: "unexpected state".into(),
            },
            "other",
        ),
    ];
    let texts: Vec<_> = variants
        .iter()
        .map(|(error, code)| {
            assert_eq!(error.code(), *code);
            let text = error.to_text();
            assert_eq!(text.key, format!("error.{code}"));
            assert_eq!(text.fallback, error.to_string());
            assert_eq!(text.args, vec![("detail".into(), error.detail())]);
            text
        })
        .collect();
    assert_catalog_contract("error.", &texts);
}

#[test]
fn actionable_io_errors_have_their_own_complete_translations() {
    let io = |kind: &str, limit: Option<u64>| Error::Io {
        path: "/fixtures/.codex/thread-writer-locks/x.lock".into(),
        kind: kind.into(),
        message: "OS text (os error 24)".into(),
        open_files_limit: limit,
    };
    let cases = [
        (
            io(fdlimit::IO_KIND_TOO_MANY_OPEN_FILES, Some(256)),
            "error_io.too_many_open_files",
        ),
        (
            io(fdlimit::IO_KIND_TOO_MANY_OPEN_FILES, None),
            "error_io.too_many_open_files_no_limit",
        ),
        (io("PermissionDenied", None), "error_io.permission_denied"),
        (io("NotFound", None), "error_io.not_found"),
    ];
    let texts: Vec<_> = cases
        .iter()
        .map(|(error, key)| {
            assert_eq!(error.code(), "io");
            let text = error.to_text();
            assert_eq!(text.key, *key);
            text
        })
        .collect();
    assert_catalog_contract("error_io.", &texts);
    let zh = Catalog::new(Locale::ZhCn).text(&cases[0].0.to_text());
    assert!(zh.starts_with("打开的文件太多（系统上限 256）"), "{zh}");
    assert!(zh.contains("x.lock") && zh.contains("os error 24"), "{zh}");
    let en = Catalog::new(Locale::En).text(&cases[0].0.to_text());
    assert!(
        en.starts_with("Too many open files (system limit 256)"),
        "{en}"
    );
}

#[test]
fn io_errors_keep_the_os_code_and_name_the_open_file_limit() {
    let other = Error::io(
        "/fixtures/a",
        &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    );
    match &other {
        Error::Io {
            kind,
            open_files_limit,
            ..
        } => {
            assert_eq!(kind, "PermissionDenied");
            assert_eq!(*open_files_limit, None);
        }
        e => panic!("{e:?}"),
    }
    #[cfg(unix)]
    {
        let emfile = std::io::Error::from_raw_os_error(rustix::io::Errno::MFILE.raw_os_error());
        let error = Error::io("/fixtures/b", &emfile);
        let Error::Io {
            kind,
            message,
            open_files_limit,
            ..
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!(kind, fdlimit::IO_KIND_TOO_MANY_OPEN_FILES);
        assert!(message.contains(&format!(
            "os error {}",
            rustix::io::Errno::MFILE.raw_os_error()
        )));
        assert_eq!(*open_files_limit, fdlimit::open_files_limit());
        // The system-wide table being full is not this process's limit.
        let enfile = std::io::Error::from_raw_os_error(rustix::io::Errno::NFILE.raw_os_error());
        let error = Error::io("/fixtures/c", &enfile);
        assert!(matches!(
            &error,
            Error::Io { kind, open_files_limit: None, .. } if kind == fdlimit::IO_KIND_TOO_MANY_OPEN_FILES
        ));
    }
    // Old serialized errors (without the field) still parse.
    let json = r#"{"code":"io","path":"/x","kind":"NotFound","message":"gone"}"#;
    let parsed: Error = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.to_text().key, "error_io.not_found");
}

#[test]
fn every_auth_mode_has_complete_translations() {
    let texts: Vec<_> = [AuthMode::Pat, AuthMode::ApiKey, AuthMode::OfficialChatGpt]
        .into_iter()
        .map(AuthMode::text)
        .collect();
    assert_catalog_contract("auth_mode.", &texts);
}

#[test]
fn nested_arguments_are_localized_with_the_same_catalog() {
    use superba_core::{Catalog, Locale, LocalizedText};
    let line = LocalizedText::new("line.krill.overseas", "Overseas");
    let t = LocalizedText::new("test.nested", "Line {line} via {host}")
        .arg("host", "example")
        .arg_text("line", line.clone());
    assert_eq!(t.render_fallback(), "Line Overseas via example");
    let mut zh = Catalog::new(Locale::ZhCn);
    zh.add_toml(Locale::ZhCn, "[test]\nnested = '线路 {line}（{host}）'")
        .unwrap();
    assert_eq!(zh.text(&t), "线路 海外线路（example）");
    // A plain argument replaces a nested one of the same name and vice versa.
    let replaced = t.clone().arg("line", "x");
    assert!(replaced.nested.is_empty());
    assert_eq!(replaced.render_fallback(), "Line x via example");
    let back = replaced.arg_text("line", line);
    assert_eq!(back.args.len(), 1);
    assert_eq!(back.render_fallback(), "Line Overseas via example");
    // Serialization omits empty nested lists and round-trips nested ones.
    let json = serde_json::to_string(&t).unwrap();
    let parsed: LocalizedText = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, t);
    let plain = serde_json::to_value(LocalizedText::new("k", "v")).unwrap();
    assert!(plain.get("nested").is_none());
}

#[test]
fn plural_selector_picks_the_singular_only_for_one() {
    let t = |n: u32| {
        LocalizedText::new(
            "test.plural",
            "{n} {n|conversation is|conversations are} running",
        )
        .arg("n", n)
        .render_fallback()
    };
    assert_eq!(t(1), "1 conversation is running");
    assert_eq!(t(0), "0 conversations are running");
    assert_eq!(t(2), "2 conversations are running");
    // Unknown argument names and malformed selectors are left as they are.
    assert_eq!(
        interpolate("{x|a|b} {n|a}", &[("n".into(), "1".into())]),
        "{x|a|b} {n|a}"
    );
    // A Chinese template without a selector is unaffected.
    assert_eq!(
        interpolate("有 {n} 个对话", &[("n".into(), "1".into())]),
        "有 1 个对话"
    );
}

#[test]
fn error_text_detail_does_not_repeat_the_category() {
    let e = superba_core::Error::invalid("origin scheme http not allowed");
    assert_eq!(e.detail(), "origin scheme http not allowed");
    let rendered = Catalog::new(Locale::En).text(&e.to_text());
    assert_eq!(rendered, "Invalid input: origin scheme http not allowed");
    let nf = superba_core::Error::NotFound {
        what: "profile nope".into(),
    };
    assert_eq!(
        Catalog::new(Locale::ZhCn).text(&nf.to_text()),
        "未找到：profile nope"
    );
    assert_eq!(superba_core::Error::Cancelled.detail(), "");
}

// ---------------------------------------------------------------------------
// v0.3 per-area core catalog files
// ---------------------------------------------------------------------------

#[test]
fn core_area_files_have_locale_parity_and_concatenate_cleanly() {
    use std::collections::BTreeMap;
    use superba_core::i18n::core_catalog_files;

    let en = core_catalog_files(Locale::En);
    let zh = core_catalog_files(Locale::ZhCn);
    assert_eq!(en.len(), zh.len());
    assert!(en.len() >= 5, "main file plus four area files");
    for ((en_name, en_src), (zh_name, zh_src)) in en.iter().zip(zh) {
        assert_eq!(
            en_name.replace(".en.", ".").replace("en.toml", "toml"),
            zh_name
                .replace(".zh-CN.", ".")
                .replace("zh-CN.toml", "toml"),
            "file order"
        );
        let en_keys: BTreeSet<_> = flatten_toml(en_src).unwrap().into_keys().collect();
        let zh_keys: BTreeSet<_> = flatten_toml(zh_src).unwrap().into_keys().collect();
        assert_eq!(en_keys, zh_keys, "{en_name} vs {zh_name}");
    }
    for locale in Locale::ALL {
        let mut union: BTreeMap<String, String> = BTreeMap::new();
        for (name, src) in core_catalog_files(locale) {
            // An area file must open with a table header, or its first keys would
            // attach to the previous file's last table after concatenation.
            if name.starts_with("core.") {
                let first = src
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .unwrap_or_default();
                assert!(
                    first.starts_with('['),
                    "{name} must start with a table header"
                );
            }
            for (key, value) in flatten_toml(src).unwrap() {
                assert!(!value.is_empty(), "{name}: {key} is empty");
                assert!(
                    union.insert(key.clone(), value).is_none(),
                    "{name}: {key} is defined in another core file"
                );
            }
        }
        let merged: BTreeMap<String, String> = flatten_toml(core_catalog_source(locale))
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(merged, union, "concatenated core catalog for {locale}");
    }
}

#[test]
fn every_models_area_text_has_complete_translations() {
    use superba_core::{CatalogFileStatus, SpeedEffect, SpeedTier};
    let tiers: Vec<_> = SpeedTier::ALL.iter().map(|t| t.text()).collect();
    assert_catalog_contract("speed_tier.", &tiers);
    let effects: Vec<_> = [
        SpeedEffect::Applies,
        SpeedEffect::ViaModelName {
            model: "gpt-6-astra-ultrafast".into(),
        },
        SpeedEffect::AppForcesStandard,
        SpeedEffect::NotOffered,
        SpeedEffect::AppMenu,
    ]
    .iter()
    .map(SpeedEffect::text)
    .collect();
    assert_catalog_contract("speed_effect.", &effects);
    let files: Vec<_> = [
        CatalogFileStatus::NotNeeded,
        CatalogFileStatus::Written {
            path: "/h/.codex/superba/models-0a1b2c3d.json".into(),
            sha8: "0a1b2c3d".into(),
            consumer: CodexVersion::new(0, 162, 0),
        },
        CatalogFileStatus::Failed {
            reason: Error::NotImplemented { what: "x".into() }.to_text(),
        },
    ]
    .iter()
    .map(CatalogFileStatus::text)
    .collect();
    assert_catalog_contract("catalog_file.", &files);
}

#[test]
fn every_requests_area_text_has_complete_translations() {
    use superba_core::{RequestClient, RequestStatus};
    let clients: Vec<_> = [
        RequestClient::Desktop,
        RequestClient::Tui,
        RequestClient::Exec,
        RequestClient::Other,
    ]
    .iter()
    .map(|c| c.text())
    .collect();
    assert_catalog_contract("request_client.", &clients);
    let harnesses: Vec<_> = superba_core::RequestHarness::ALL
        .iter()
        .map(|h| h.text())
        .collect();
    assert_catalog_contract("request_harness.", &harnesses);
    let protocols: Vec<_> = [
        superba_core::RequestProtocol::Responses,
        superba_core::RequestProtocol::ChatCompletions,
        superba_core::RequestProtocol::Messages,
        superba_core::RequestProtocol::Gemini,
    ]
    .iter()
    .map(|p| p.text())
    .collect();
    assert_catalog_contract("request_protocol.", &protocols);
    let agents: Vec<_> = [
        superba_core::AgentKind::Main,
        superba_core::AgentKind::Subagent,
        superba_core::AgentKind::Background,
    ]
    .iter()
    .map(|a| a.text())
    .collect();
    assert_catalog_contract("agent_kind.", &agents);
    let statuses: Vec<_> = [
        RequestStatus::Ok,
        RequestStatus::Http {
            status: 502,
            code: Some("router_unavailable".into()),
        },
        RequestStatus::Failed { code: None },
        RequestStatus::Incomplete {
            reason: Some("max_output_tokens".into()),
        },
        RequestStatus::Broken,
        RequestStatus::Cancelled,
    ]
    .iter()
    .map(RequestStatus::text)
    .collect();
    assert_catalog_contract("request_status.", &statuses);
}

#[test]
fn every_versions_area_text_has_complete_translations() {
    use superba_core::{CodexInstallKind, CompatTier};
    let kinds: Vec<_> = [
        CodexInstallKind::Standalone,
        CodexInstallKind::Npm,
        CodexInstallKind::Homebrew,
        CodexInstallKind::AppBundled,
        CodexInstallKind::SuperbaStore,
        CodexInstallKind::Other,
    ]
    .iter()
    .map(|k| k.text())
    .collect();
    assert_catalog_contract("install_kind.", &kinds);
    let tiers: Vec<_> = [
        CompatTier::Same,
        CompatTier::Compatible,
        CompatTier::Older,
        CompatTier::BelowFloor,
        CompatTier::Newer,
    ]
    .iter()
    .map(|t| t.text())
    .collect();
    assert_catalog_contract("compat_tier.", &tiers);
}

#[test]
fn every_hosts_area_text_has_complete_translations() {
    use superba_core::{HostSyncAction, SkipReason};
    let actions: Vec<_> = [
        HostSyncAction::Write,
        HostSyncAction::Skip {
            reason: SkipReason::Removed,
        },
        HostSyncAction::Remove,
        HostSyncAction::Conflict,
    ]
    .iter()
    .map(HostSyncAction::text)
    .collect();
    assert_catalog_contract("host_sync.", &actions);
}
