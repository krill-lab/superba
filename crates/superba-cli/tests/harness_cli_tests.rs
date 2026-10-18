//! `spb harness` syntax (the commands run against real harness files in the
//! end-to-end runs; see the t2 report).

use clap::Parser;
use superba_cli::{Cli, Command, HarnessCommand};

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("spb").chain(args.iter().copied()))
}

#[test]
fn every_harness_command_parses() {
    for args in [
        vec!["harness", "list"],
        vec!["harness", "status", "dsh"],
        vec!["harness", "apply", "pi"],
        vec![
            "harness",
            "apply",
            "opencode",
            "--provider",
            "krill",
            "--protocol",
            "chat",
            "--model",
            "gpt-5.5",
            "--model",
            "a,b",
            "--default-model",
            "a",
            "--opencode-major",
            "1",
            "--yes",
            "--take-over",
        ],
        vec![
            "harness",
            "apply",
            "dsh",
            "--follow-codex",
            "--all-models",
            "--telemetry",
            "off",
        ],
        vec![
            "harness",
            "apply",
            "zcode",
            "--routed",
            "--router-url",
            "http://127.0.0.1:1",
        ],
        vec![
            "harness",
            "apply",
            "workbuddy",
            "--edition",
            "cn",
            "--edition",
            "intl",
            "--direct",
        ],
        vec!["harness", "restore", "pi", "--yes"],
        vec![
            "harness",
            "restore",
            "pi",
            "--backup",
            "20261018-000000-pi-abcdef",
        ],
        vec!["harness", "backups", "zcode"],
        vec!["harness", "endpoints", "compat", "--chat", "https://x/v1"],
        vec!["harness", "endpoints", "compat", "--clear"],
        vec!["harness", "token", "dsh", "--stdin"],
        vec!["harness", "token", "dsh", "--key-env", "ROUTER_TOKEN"],
        vec!["harness", "token", "dsh", "--clear"],
    ] {
        parse(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
    }
}

#[test]
fn conflicting_or_missing_flags_are_refused() {
    for args in [
        vec![
            "harness",
            "apply",
            "pi",
            "--provider",
            "a",
            "--follow-codex",
        ],
        vec!["harness", "apply", "pi", "--routed", "--direct"],
        vec!["harness", "apply", "pi", "--model", "a", "--all-models"],
        vec!["harness", "apply", "pi", "--protocol", "grpc"],
        vec!["harness", "apply", "pi", "--opencode-major", "3"],
        vec![
            "harness",
            "endpoints",
            "compat",
            "--chat",
            "https://x/v1",
            "--clear",
        ],
        vec!["harness", "token", "dsh"],
        vec!["harness", "status"],
    ] {
        assert!(parse(&args).is_err(), "{args:?} should be refused");
    }
}

#[test]
fn apply_flags_reach_the_command() {
    let cli = parse(&[
        "harness", "apply", "pi", "--model", "a,b", "--model", "c", "--yes",
    ])
    .unwrap();
    let Command::Harness {
        command: HarnessCommand::Apply(a),
    } = cli.command
    else {
        panic!("not harness apply");
    };
    assert_eq!(a.harness, "pi");
    assert_eq!(a.model, ["a,b", "c"]);
    assert!(a.yes && !a.take_over);
}
