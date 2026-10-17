#![allow(missing_docs)]

use superba_core::CodexVersion;
use superba_core::codex::{hot_restart_js, is_main_renderer_url, js_string};

#[test]
fn parses_cli_release_and_bundled_prerelease_versions() {
    let release = CodexVersion::parse("codex-cli 0.162.0").unwrap();
    assert_eq!(release, CodexVersion::new(0, 162, 0));
    assert_eq!(release.triple(), (0, 162, 0));
    let prerelease = CodexVersion::parse("0.162.0-alpha.17.2").unwrap();
    assert_eq!(prerelease.triple(), (0, 162, 0));
    assert_eq!(prerelease.pre.as_deref(), Some("alpha.17.2"));
    assert_eq!(
        CodexVersion::parse("版本: 1.2.3 (build)").unwrap(),
        CodexVersion::new(1, 2, 3)
    );
}

#[test]
fn parses_versions_with_a_v_prefix() {
    assert_eq!(
        CodexVersion::parse("v1.2.3").unwrap(),
        CodexVersion::new(1, 2, 3)
    );
}

#[test]
fn garbage_and_incomplete_versions_are_parse_errors() {
    for input in ["", "garbage", "codex-cli", "1", "1.2", "1.2.x"] {
        let error = CodexVersion::parse(input).expect_err(input);
        assert_eq!(error.code(), "parse");
    }
}

#[test]
fn malformed_four_component_version_is_not_reinterpreted_as_a_suffix() {
    assert!(CodexVersion::parse("1.2.3.4").is_err());
}

#[test]
fn semver_order_uses_numeric_prerelease_components_and_release_precedence() {
    let versions: Vec<_> = [
        "0.161.99",
        "0.162.0-1",
        "0.162.0-alpha",
        "0.162.0-alpha.2",
        "0.162.0-alpha.10",
        "0.162.0-alpha.10.1",
        "0.162.0-beta",
        "0.162.0",
        "0.162.1",
        "0.163.0",
        "1.0.0",
    ]
    .into_iter()
    .map(|s| CodexVersion::parse(s).unwrap())
    .collect();
    for (i, left) in versions.iter().enumerate() {
        assert_eq!(left.cmp(left), std::cmp::Ordering::Equal);
        for right in &versions[i + 1..] {
            assert!(left < right, "expected {left} < {right}");
            assert!(right > left, "expected {right} > {left}");
        }
    }
}

#[test]
fn version_gates_ignore_prerelease_but_compare_all_release_components() {
    let alpha = CodexVersion::parse("0.162.0-alpha.2").unwrap();
    let beta = CodexVersion::parse("0.162.0-beta").unwrap();
    let release = CodexVersion::new(0, 162, 0);
    assert!(alpha.meets(&release));
    assert!(alpha.meets(&beta));
    assert!(release.meets(&alpha));
    assert!(alpha.meets(&CodexVersion::new(0, 161, 99)));
    assert!(!alpha.meets(&CodexVersion::new(0, 162, 1)));
    assert!(!alpha.meets(&CodexVersion::new(0, 163, 0)));
    assert!(!alpha.meets(&CodexVersion::new(1, 0, 0)));
}

#[test]
fn version_display_fromstr_and_json_round_trip_as_strings() {
    for input in ["0.0.0", "0.162.0", "0.162.0-alpha.17.2", "1.2.3"] {
        let version = CodexVersion::parse(input).unwrap();
        assert_eq!(version.to_string(), input);
        assert_eq!(
            version.to_string().parse::<CodexVersion>().unwrap(),
            version
        );
        let json = serde_json::to_value(&version).unwrap();
        assert_eq!(json, serde_json::json!(input));
        assert_eq!(
            serde_json::from_value::<CodexVersion>(json).unwrap(),
            version
        );
    }
    assert!(serde_json::from_str::<CodexVersion>("123").is_err());
    assert!(serde_json::from_str::<CodexVersion>(r#""garbage""#).is_err());
}

#[test]
fn local_and_remote_restart_scripts_use_the_expected_ipc_contract() {
    for (host, remote) in [("local", false), ("ssh-production", true)] {
        let script = hot_restart_js(host, remote);
        assert!(script.contains(r#"type:"codex-app-server-restart""#));
        assert!(script.contains(&format!("hostId:{}", js_string(host))));
        assert!(script.contains("intent:'restart'"));
        assert!(script.contains("window.electronBridge"));
        assert!(script.contains("b.sendMessageFromView("));
        assert!(script.contains("return false"));
        assert!(script.contains("return true"));
        assert_eq!(script.contains("killCodexProcess"), remote);
        if remote {
            assert!(script.contains("killCodexProcess:true"));
        }
    }
}

#[test]
fn restart_scripts_escape_quotes_and_backslashes_in_host_ids() {
    let host = "host\"quoted'\\path\nnext";
    for remote in [false, true] {
        let script = hot_restart_js(host, remote);
        assert!(script.contains(r#"hostId:"host\"quoted'\\path\nnext",intent:'restart'"#));
        assert!(!script.contains(host));
    }
}

#[test]
fn js_strings_escape_controls_quotes_backslashes_and_line_separators() {
    for (input, expected) in [
        ("", r#""""#),
        ("plain ' 中文", r#""plain ' 中文""#),
        ("\"\\\n\r\t", r#""\"\\\n\r\t""#),
        ("\u{0}\u{8}\u{c}\u{1f}", r#""\u0000\u0008\u000c\u001f""#),
        ("\u{2028}\u{2029}", r#""\u2028\u2029""#),
    ] {
        let quoted = js_string(input);
        assert_eq!(quoted, expected);
        assert_eq!(serde_json::from_str::<String>(&quoted).unwrap(), input);
    }
    let controls: String = (0..32).map(char::from).collect();
    let quoted = js_string(&controls);
    assert_eq!(serde_json::from_str::<String>(&quoted).unwrap(), controls);
    assert!(!quoted.chars().any(|c| c < ' '));
}

#[test]
fn renderer_detection_excludes_overlays_and_prewarmed_quick_chat() {
    for input in [
        "app://-/index.html",
        "app://-/index.html?initialRoute=/",
        "app://-/index.html#/thread/123",
    ] {
        assert!(is_main_renderer_url(input), "{input}");
    }
    for input in [
        "app://-/index.html?initialRoute=/avatar-overlay",
        "app://-/index.html?initialRoute=/avatar-overlay&theme=dark",
        "app://-/index.html?quick-chat-prewarm=true",
        "app://-/index.html#quick-chat-prewarm",
        "https://example.com/index.html",
        "app://-/other.html",
        "",
    ] {
        assert!(!is_main_renderer_url(input), "{input}");
    }
}
