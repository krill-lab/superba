#![allow(missing_docs)]

use superba_core::ShellKind;
use superba_core::codex::ENV_AUTHAPI_BASE_URL;
use superba_core::env::{
    MARKER_BEGIN, MARKER_END, MARKER_NOTE, export_value, find_block, legacy_krill_export_value,
    render_block,
};

#[test]
fn posix_blocks_export_values_with_shell_metacharacters_escaped() {
    let value = r#"dollar$ backtick` quote" slash\"#;
    let assignment = r#"export NAME="dollar\$ backtick\` quote\" slash\\""#;
    for shell in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Sh] {
        let block = render_block(&shell, &[("NAME", value), ("EMPTY", "")]);
        let expected = format!(
            "{MARKER_BEGIN}\n{MARKER_NOTE}\n{assignment}\nexport EMPTY=\"\"\n{MARKER_END}\n"
        );
        assert_eq!(block, expected, "{shell:?}");
        assert_eq!(export_value(assignment, "NAME").as_deref(), Some(value));
    }
}

#[test]
fn certificate_paths_survive_shell_block_round_trips() {
    for shell in [ShellKind::Bash, ShellKind::Fish] {
        for value in [r"C:\Users\Jane Doe\codex-ca.pem", r"/home/a$b/ca\root.pem"] {
            let block = render_block(&shell, &[("CODEX_CA_CERTIFICATE", value)]);
            let parsed = block
                .lines()
                .find_map(|line| export_value(line, "CODEX_CA_CERTIFICATE"));
            assert_eq!(parsed.as_deref(), Some(value), "{shell:?}");
        }
    }
    assert_eq!(
        export_value(r"export NAME='C:\Users\Jane Doe\ca.pem'", "NAME").as_deref(),
        Some(r"C:\Users\Jane Doe\ca.pem")
    );
}

#[test]
fn fish_blocks_use_global_export_syntax() {
    let block = render_block(&ShellKind::Fish, &[("NAME", "hello world"), ("EMPTY", "")]);
    assert_eq!(
        block,
        format!(
            "{MARKER_BEGIN}\n{MARKER_NOTE}\nset -gx NAME \"hello world\"\nset -gx EMPTY \"\"\n{MARKER_END}\n"
        )
    );
    assert!(!block.contains("export "));
}

#[test]
fn empty_variable_list_still_produces_a_complete_block() {
    let block = render_block(&ShellKind::Bash, &[]);
    assert_eq!(
        block,
        format!("{MARKER_BEGIN}\n{MARKER_NOTE}\n{MARKER_END}\n")
    );
    assert_eq!(find_block(&block), Some(0..block.len()));
}

#[test]
fn find_block_returns_exact_byte_range_with_lf_crlf_and_surrounding_text() {
    let rendered = render_block(&ShellKind::Zsh, &[("NAME", "value")]);
    for newline in ["\n", "\r\n"] {
        let block = rendered.replace('\n', newline);
        for (prefix, suffix) in [
            (String::new(), String::new()),
            (
                format!("# 前置内容{newline}export OTHER=1{newline}"),
                format!("# after{newline}"),
            ),
        ] {
            let text = format!("{prefix}{block}{suffix}");
            let range = find_block(&text).unwrap();
            assert_eq!(range, prefix.len()..prefix.len() + block.len());
            assert_eq!(&text[range.clone()], block);
            let mut remaining = text.clone();
            remaining.replace_range(range, "");
            assert_eq!(remaining, format!("{prefix}{suffix}"));
        }
        let without_newline = block.trim_end_matches(['\r', '\n']);
        assert_eq!(find_block(without_newline), Some(0..without_newline.len()));
    }
}

#[test]
fn find_block_selects_only_the_first_complete_block() {
    let first = render_block(&ShellKind::Zsh, &[("FIRST", "1")]);
    let second = render_block(&ShellKind::Fish, &[("SECOND", "2")]);
    let text = format!("{first}# middle\n{second}");
    assert_eq!(find_block(&text), Some(0..first.len()));
}

#[test]
fn find_block_ignores_missing_unterminated_and_embedded_markers() {
    for text in [
        String::new(),
        "export NAME=value\n".into(),
        format!("{MARKER_BEGIN}\nexport NAME=value\n"),
        format!("{MARKER_END}\n{MARKER_BEGIN}\nexport NAME=value\n"),
        format!("echo '{MARKER_BEGIN}'\n{MARKER_END}\n"),
        format!("{MARKER_BEGIN} suffix\n{MARKER_END}\n"),
        format!("{MARKER_BEGIN}\n{MARKER_END} suffix\n"),
    ] {
        assert_eq!(find_block(&text), None, "{text:?}");
    }
}

#[test]
fn find_block_includes_whitespace_and_newline_of_indented_end_marker() {
    let block = format!("  {MARKER_BEGIN}\r\nexport NAME=value\r\n  {MARKER_END}  \r\n");
    let text = format!("# before\r\n{block}# after\r\n");
    let range = find_block(&text).unwrap();
    assert_eq!(&text[range], block);
}

#[test]
fn legacy_krill_exports_allow_quotes_semicolons_and_trailing_comments() {
    let origin = "https://api.krill-code.net";
    for line in [
        format!("export {ENV_AUTHAPI_BASE_URL}={origin}"),
        format!("export {ENV_AUTHAPI_BASE_URL}=\"{origin}\""),
        format!("export {ENV_AUTHAPI_BASE_URL}='{origin}'"),
        format!("export {ENV_AUTHAPI_BASE_URL}={origin};"),
        format!("export {ENV_AUTHAPI_BASE_URL}=\"{origin}\";"),
        format!("export {ENV_AUTHAPI_BASE_URL}={origin} # tutorial"),
        format!("  export {ENV_AUTHAPI_BASE_URL}='{origin}'; # tutorial\r\n"),
    ] {
        assert_eq!(
            legacy_krill_export_value(&line).as_deref(),
            Some(origin),
            "{line:?}"
        );
    }
    for origin in [
        "https://api-slb.krill-code.net",
        "https://api.cdn-krill-ai.com",
        "https://api.krill-code.net:443/",
    ] {
        let line = format!("export {ENV_AUTHAPI_BASE_URL}={origin}");
        assert_eq!(legacy_krill_export_value(&line).as_deref(), Some(origin));
    }
}

#[test]
fn legacy_krill_exports_reject_comments_other_variables_and_foreign_hosts() {
    for line in [
        "# export CODEX_AUTHAPI_BASE_URL=https://api.krill-code.net",
        "   # export CODEX_AUTHAPI_BASE_URL=https://api.krill-code.net",
        "# [superba] export CODEX_AUTHAPI_BASE_URL=https://api.krill-code.net",
        "export CODEX_AUTHAPI_BASE_URL=https://example.com",
        "export CODEX_AUTHAPI_BASE_URL=https://evilkrill-code.net",
        "export CODEX_AUTHAPI_BASE_URL=https://api.krill-code.net.evil.example",
        "export CODEX_AUTHAPI_BASE_URL=ftp://api.krill-code.net",
        "export OTHER=https://api.krill-code.net",
        "export CODEX_AUTHAPI_BASE_URL_EXTRA=https://api.krill-code.net",
        "CODEX_AUTHAPI_BASE_URL=https://api.krill-code.net",
        "export CODEX_AUTHAPI_BASE_URL=",
        "export CODEX_AUTHAPI_BASE_URL=\"\"",
    ] {
        assert_eq!(legacy_krill_export_value(line), None, "{line:?}");
    }
}

#[test]
fn legacy_krill_export_checks_the_actual_host_not_credential_text() {
    let line = "export CODEX_AUTHAPI_BASE_URL=https://api.krill-code.net:443@evil.example";
    assert_eq!(legacy_krill_export_value(line), None);
}

#[test]
fn export_value_supports_fish_global_and_local_exports() {
    for (line, expected) in [
        ("set -gx NAME value", Some("value")),
        ("set -x NAME 'hello world'", Some("hello world")),
        (
            "  set -gx NAME \"hello world\"; # note",
            Some("hello world"),
        ),
        ("set -gx NAME    value", Some("value")),
        ("set -gx NAME \"\"", None),
        ("set -gx OTHER value", None),
        ("set -gx NAME_EXTRA value", None),
        ("# set -gx NAME value", None),
        ("set NAME value", None),
    ] {
        assert_eq!(export_value(line, "NAME").as_deref(), expected, "{line:?}");
    }
}

#[test]
fn shell_paths_recognize_posix_and_windows_basenames() {
    for (path, expected) in [
        ("/bin/zsh", ShellKind::Zsh),
        ("/usr/bin/bash", ShellKind::Bash),
        ("fish", ShellKind::Fish),
        ("/usr/local/bin/fish ", ShellKind::Fish),
        ("/bin/sh", ShellKind::Sh),
        ("/bin/dash", ShellKind::Sh),
        ("ash", ShellKind::Sh),
        ("ksh", ShellKind::Sh),
        (r"C:\Program Files\Git\bin\bash.exe", ShellKind::Bash),
        ("/bin/nu", ShellKind::Other("nu".into())),
        ("pwsh.exe", ShellKind::Other("pwsh".into())),
        ("", ShellKind::Other(String::new())),
    ] {
        let actual = ShellKind::from_shell_path(path);
        assert_eq!(actual.is_fish(), matches!(expected, ShellKind::Fish));
        assert_eq!(actual, expected, "{path:?}");
    }
}
