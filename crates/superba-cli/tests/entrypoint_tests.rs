use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    // Only parser/help paths are invoked: no production service is constructed.
    let directory = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_spb"))
        .env_clear()
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("SUPERBA_SECRET_BACKEND", "file")
        .env("SUPERBA_LOCALE", "en")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn help_and_version_succeed_without_production_services() {
    for args in [
        vec!["--help"],
        vec!["help", "profile", "add"],
        vec!["open-app", "--help"],
        vec!["--version"],
    ] {
        let output = invoke(&args);
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn chinese_help_includes_translated_headings_values_and_alias() {
    let output = invoke(&["--locale", "zh-CN", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for value in ["命令", "选项", "用法", "别名", "open-app"] {
        assert!(help.contains(value), "{value}");
    }
    assert!(!help.contains("Options:") && !help.contains("Commands:"));
    let output = invoke(&["--locale", "zh-CN", "help", "profile", "add"]);
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("创建配置") && help.contains("可选值") && help.contains("默认值"));
}

#[test]
fn json_parse_errors_help_and_version_are_single_values() {
    for (args, code, key) in [
        (
            vec!["--json", "key", "set", "--profile", "work"],
            2,
            "error",
        ),
        (
            vec!["switch", "--json", "--wait-turn", "--force"],
            2,
            "error",
        ),
        (vec!["--json", "--help"], 0, "help"),
        (vec!["--json", "--version"], 0, "version"),
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(code));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(value.get(key).is_some());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn parse_errors_never_echo_rejected_values() {
    let rejected = "private-input-not-a-key";
    let output = invoke(&["switch", "--key", rejected]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("spb help"));
    assert!(!stderr.contains(rejected));
    assert!(stderr.starts_with("error: "));
    assert_eq!(stderr.to_lowercase().matches("invalid input:").count(), 1);
}

#[test]
fn unknown_options_and_commands_are_named_but_values_never_echoed() {
    let secret = "private-input-not-a-key";
    for (args, expected) in [
        (
            vec!["site", "add", "ex", &format!("--base-urll={secret}")],
            vec![
                "--base-urll",
                "did you mean --base-url?",
                "spb help site add",
            ],
        ),
        (
            vec!["statuz"],
            vec!["statuz", "did you mean status?", "spb help"],
        ),
        (vec!["switch", secret], vec!["spb help switch"]),
        (vec![secret], vec!["Unknown command", "spb help"]),
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).unwrap();
        for e in expected {
            assert!(stderr.contains(e), "{e}: {stderr}");
        }
        assert!(!stderr.contains(secret), "{stderr}");
        assert!(!stderr.contains("<COMMAND>"), "{stderr}");
    }
}

#[test]
fn no_command_prints_help_on_stderr_and_keeps_usage_exit_code() {
    let output = invoke(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "Usage: spb",
        "Commands:",
        "status",
        "switch",
        "profile",
        "--help",
    ] {
        assert!(stderr.contains(expected), "{stderr}");
    }
    assert!(!stderr.contains("error:") && !stderr.contains("Invalid input"));
    let output = invoke(&["--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["help"].as_str().unwrap().contains("Commands:"));
}

#[test]
fn missing_required_arguments_name_the_schema_without_echoing_values() {
    for (args, expected) in [
        (
            vec!["key", "set", "--profile", "private-input-not-a-key"],
            "--stdin",
        ),
        (vec!["sessions", "migrate"], "--from"),
        (vec!["profile", "add"], "<ID>"),
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.starts_with("error: ") && stderr.contains(expected),
            "{stderr}"
        );
        assert!(!stderr.contains("private-input-not-a-key"));
    }
}

#[test]
fn key_set_help_documents_optional_profile_and_both_stdin_spellings() {
    for args in [
        vec!["key", "set", "--help"],
        vec!["profile", "add", "--help"],
    ] {
        let output = invoke(&args);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("--stdin") && stdout.contains("--key-stdin"),
            "{stdout}"
        );
        if args[0] == "key" {
            assert!(stdout.contains("otherwise the applied profile"));
            assert!(
                !stdout
                    .lines()
                    .find(|line| line.starts_with("Usage:"))
                    .unwrap()
                    .contains("--profile")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn superba_argv0_uses_superba_usage_and_points_to_gui_help() {
    use std::os::unix::process::CommandExt;

    let directory = tempfile::tempdir().unwrap();
    for (args, syntax) in [
        (vec!["--help"], "Usage: superba "),
        (vec!["switch", "--help"], "Usage: superba switch "),
        (
            vec!["help", "profile", "add"],
            "Usage: superba profile add ",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_spb"))
            .arg0("/isolated/bin/superba.exe")
            .env_clear()
            .env("HOME", directory.path())
            .env("SUPERBA_SECRET_BACKEND", "file")
            .env("SUPERBA_LOCALE", "en")
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(
            help.contains(syntax) && !help.contains("Usage: spb"),
            "{help}"
        );
        if args == ["--help"] {
            assert!(help.contains("GUI options: superba --gui-help"));
        }
    }
}
