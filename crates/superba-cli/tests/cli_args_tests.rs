use std::ffi::OsString;

use clap::{CommandFactory, Parser};
use superba_cli::{Cli, Command, is_cli_invocation, subcommand_names};

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("spb").chain(args.iter().copied()))
}

#[test]
fn clap_definition_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn every_command_and_alias_parses() {
    for args in [
        vec!["status"],
        vec!["lines"],
        vec!["lines", "--probe", "--site", "krill"],
        vec!["key", "set", "--profile", "p", "--key-env", "TEST_KEY"],
        vec!["key", "set", "--profile", "p", "--stdin"],
        vec!["key", "set", "--stdin"],
        vec!["key", "set", "--key-stdin"],
        vec!["key", "check"],
        vec![
            "key",
            "check",
            "--profile",
            "p",
            "--site",
            "krill",
            "--line",
            "overseas",
            "--auth",
            "pat",
            "--stdin",
        ],
        vec!["profile", "list"],
        vec!["profile", "add", "p"],
        vec!["profile", "add", "p", "--stdin"],
        vec!["profile", "add", "p", "--key-stdin"],
        vec!["profile", "remove", "p"],
        vec!["profile", "use", "p"],
        vec!["switch"],
        vec!["restore"],
        vec!["restore", "original"],
        vec!["restore", "official"],
        vec!["restore", "generation", "--generation", "42"],
        vec!["restore", "--provider", "no-migrate"],
        vec!["launch"],
        vec!["open-app"],
        vec!["launch", "--restart", "-y"],
        vec!["sessions"],
        vec!["sessions", "scan"],
        vec!["sessions", "migrate", "--from", "legacy"],
        vec!["sessions", "rollback", "migration-1"],
        vec!["remote", "list"],
        vec!["remote", "apply", "ssh:a", "ssh:b"],
        vec!["remote", "rollback", "ssh:a"],
        vec!["doctor"],
        vec!["recover"],
        vec!["uninstall"],
        vec![
            "uninstall",
            "--delete-backups",
            "--provider",
            "keep-openai",
            "--yes",
        ],
    ] {
        assert!(parse(&args).is_ok(), "{args:?}");
    }
}

#[test]
fn gui_routes_all_commands_aliases_help_and_prefixed_global_options() {
    let names = subcommand_names();
    for command in [
        "status",
        "lines",
        "key",
        "profile",
        "switch",
        "restore",
        "launch",
        "open-app",
        "sessions",
        "remote",
        "doctor",
        "recover",
        "uninstall",
    ] {
        assert!(names.iter().any(|name| name == command));
        assert!(is_cli_invocation(&["superba".into(), command.into()]));
    }
    for args in [
        vec!["superba", "--help"],
        vec!["superba", "-h"],
        vec!["superba", "--version"],
        vec!["superba", "-V"],
        vec!["superba", "help"],
        vec!["superba", "--json", "--locale", "zh-CN", "status"],
        vec!["superba", "--locale=en", "open-app"],
        vec!["superba", "--verbose", "switch", "--dry-run"],
    ] {
        assert!(is_cli_invocation(
            &args.iter().map(OsString::from).collect::<Vec<_>>()
        ));
    }
    for args in [
        vec!["superba"],
        vec!["superba", "--gui"],
        vec!["superba", "--gui", "status"],
    ] {
        assert!(!is_cli_invocation(
            &args.iter().map(OsString::from).collect::<Vec<_>>()
        ));
    }
}

#[test]
fn global_options_work_after_nested_commands() {
    let cli = parse(&["profile", "list", "--json", "--locale", "zh-CN"]).unwrap();
    assert!(cli.json);
    assert_eq!(cli.locale.as_deref(), Some("zh-CN"));
    assert!(parse(&["--locale", "fr", "status"]).is_err());
}

#[test]
fn execution_flags_parse_and_countdown_is_bounded() {
    let cli = parse(&[
        "switch",
        "--line",
        "overseas",
        "--auth",
        "apikey",
        "--yes",
        "--allow-full-restart",
        "--remote",
        "ssh:a",
        "--remote",
        "ssh:b",
        "--no-local",
    ])
    .unwrap();
    let Command::Switch(sw) = cli.command else {
        panic!("expected switch")
    };
    assert_eq!(sw.answers.line.as_deref(), Some("overseas"));
    assert!(sw.exec.yes && sw.exec.allow_full_restart && sw.exec.no_local);
    assert_eq!(sw.exec.countdown, 0);
    assert_eq!(sw.exec.remotes, ["ssh:a", "ssh:b"]);
    for value in ["0", "1", "30"] {
        assert!(parse(&["switch", "--countdown", value]).is_ok());
    }
    for value in ["-1", "31", "999999999999", "one"] {
        assert!(parse(&["switch", "--countdown", value]).is_err());
    }
}

#[test]
fn every_execution_command_rejects_wait_and_force_together() {
    for mut args in [
        vec!["switch"],
        vec!["profile", "use", "p"],
        vec!["restore"],
        vec!["launch", "--restart"],
        vec!["sessions", "migrate", "--from", "legacy"],
        vec!["sessions", "rollback", "m"],
        vec!["remote", "apply", "ssh:a"],
        vec!["remote", "rollback", "ssh:a"],
        vec!["uninstall"],
    ] {
        args.extend(["--wait-turn", "--force"]);
        assert!(parse(&args).is_err(), "{args:?}");
    }
}

#[test]
fn key_sources_are_exclusive_and_set_requires_one() {
    assert!(parse(&["key", "set", "--profile", "p"]).is_err());
    assert!(
        parse(&[
            "key",
            "set",
            "--profile",
            "p",
            "--stdin",
            "--key-env",
            "TEST_KEY"
        ])
        .is_err()
    );
    assert!(parse(&["key", "check", "--stdin", "--key-env", "TEST_KEY"]).is_err());
    assert!(
        parse(&[
            "profile",
            "add",
            "p",
            "--key-stdin",
            "--key-env",
            "TEST_KEY"
        ])
        .is_err()
    );
    assert!(parse(&["key", "set", "--key-stdin", "--key-env", "TEST_KEY"]).is_err());
    assert!(parse(&["profile", "add", "p", "--stdin", "--key-env", "TEST_KEY"]).is_err());
}

#[test]
fn no_raw_key_argument_exists() {
    for flag in ["--key", "--api-key", "--token", "--secret"] {
        for mut args in [
            vec!["key", "set", "--profile", "p"],
            vec!["key", "check"],
            vec!["profile", "add", "p"],
            vec!["switch"],
        ] {
            args.extend([flag, "forbidden-value"]);
            assert!(parse(&args).is_err());
        }
    }
    assert!(parse(&["key", "set", "--profile", "p", "--stdin", "forbidden-value"]).is_err());
}

#[test]
fn contradictory_answers_and_incomplete_intents_are_rejected() {
    for args in [
        vec!["switch", "--take-over-env", "--keep-env"],
        vec![
            "switch",
            "--migrate-legacy-sessions",
            "--skip-legacy-sessions",
        ],
        vec!["restore", "generation"],
        vec!["sessions", "migrate"],
        vec!["remote", "apply"],
        vec!["remote", "rollback"],
        vec!["profile", "add"],
        vec!["profile", "use"],
    ] {
        assert!(parse(&args).is_err(), "{args:?}");
    }
}
