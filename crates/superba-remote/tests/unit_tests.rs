//! Pure parts of superba-remote: quoting, byte encoding, ssh argv, error text,
//! record parsing, rc planning and turn parsing.

use std::path::PathBuf;
use std::process::Command;

use pretty_assertions::assert_eq;
use superba_core::codex::ENV_AUTHAPI_BASE_URL;
use superba_core::env::{LEGACY_COMMENT_PREFIX, MARKER_BEGIN, MARKER_END};
use superba_core::{
    ActivitySignal, EnvEditAction, EnvLocation, EnvOp, ShellKind, SshEndpoint, TargetId, Timestamp,
};
use superba_remote::envrc::{
    RC_FILES, RcBases, RcFile, neutralise_legacy, plan_env, rc_path, restore_legacy,
    shell_supported,
};
use superba_remote::records::{FileRead, Records};
use superba_remote::script::{backup_name, new_nonce, printf_bytes, sh_quote};
use superba_remote::ssh::{classify_ssh_failure, ssh_args};
use superba_remote::turns::{busy_threads, thread_id_from_path};

fn run_sh(shell: &str, code: &str) -> Vec<u8> {
    let out = Command::new(shell)
        .arg("-c")
        .arg(code)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run shell");
    assert!(
        out.status.success(),
        "{shell} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn shells() -> Vec<&'static str> {
    ["/bin/sh", "/bin/bash", "/bin/dash", "/usr/bin/zsh"]
        .into_iter()
        .filter(|s| std::path::Path::new(s).exists())
        .collect()
}

#[test]
fn sh_quote_round_trips_through_real_shells() {
    for s in [
        "",
        "plain",
        "it's",
        "a'b'c",
        "$HOME `x` \"q\" \\ \n tab\t",
        "中文 路径",
    ] {
        for sh in shells() {
            let out = run_sh(sh, &format!("printf '%s' {}", sh_quote(s)));
            assert_eq!(String::from_utf8(out).unwrap(), s, "{sh}");
        }
    }
}

#[test]
fn printf_bytes_reproduces_every_byte_in_real_shells() {
    let mut all: Vec<u8> = (0u8..=255).collect();
    all.extend_from_slice("中文 'quotes' \\back\\slash %s %b \n\n".as_bytes());
    // Longer than one printf chunk.
    let big: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    for bytes in [Vec::new(), b"x".to_vec(), all, big] {
        let cmd = printf_bytes(&bytes);
        assert!(cmd.is_ascii(), "script must stay ASCII");
        for sh in shells() {
            assert_eq!(run_sh(sh, &cmd), bytes, "{sh}");
        }
    }
}

#[test]
fn nonces_are_unique_hex() {
    let a = new_nonce();
    let b = new_nonce();
    assert_eq!(a.len(), 16);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[test]
fn backup_names_flatten_paths() {
    assert_eq!(
        backup_name("/home/u/.codex/auth.json"),
        "home%u%.codex%auth.json"
    );
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

#[test]
fn ssh_args_prefer_the_alias_alone() {
    let ep = SshEndpoint {
        alias: Some("devbox".into()),
        host: Some("203.0.113.1".into()),
        port: Some(2222),
        user: Some("root".into()),
        identity: Some(PathBuf::from("/k")),
    };
    assert_eq!(
        ssh_args(&ep, 8).unwrap(),
        s(&[
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
            "--",
            "devbox",
            "sh",
            "-s"
        ])
    );
}

#[test]
fn ssh_args_explicit_endpoint_uses_identity_port_and_user() {
    let ep = SshEndpoint {
        alias: None,
        host: Some("203.0.113.10".into()),
        port: Some(2222),
        user: Some("alice".into()),
        identity: Some(PathBuf::from("/home/me/.ssh/id_ed25519")),
    };
    let args = ssh_args(&ep, 8).unwrap();
    let tail: Vec<&str> = args.iter().skip(9).map(String::as_str).collect();
    assert_eq!(
        tail,
        [
            "-i",
            "/home/me/.ssh/id_ed25519",
            "-o",
            "IdentitiesOnly=yes",
            "-p",
            "2222",
            "-l",
            "alice",
            "--",
            "203.0.113.10",
            "sh",
            "-s"
        ]
    );
    assert!(args.contains(&"BatchMode=yes".to_owned()));
    assert!(
        !args.iter().any(|a| a.contains("StrictHostKeyChecking")),
        "host key policy is never touched"
    );
}

#[test]
fn ssh_args_reject_option_injection_and_missing_hosts() {
    let bad = [
        SshEndpoint {
            alias: Some("-oProxyCommand=evil".into()),
            ..SshEndpoint::default()
        },
        SshEndpoint {
            host: Some("-p1".into()),
            ..SshEndpoint::default()
        },
        SshEndpoint {
            host: Some("a b".into()),
            ..SshEndpoint::default()
        },
        SshEndpoint {
            host: Some("h".into()),
            user: Some("-x".into()),
            ..SshEndpoint::default()
        },
        SshEndpoint {
            host: Some("h".into()),
            identity: Some(PathBuf::from("-i")),
            ..SshEndpoint::default()
        },
        SshEndpoint::default(),
    ];
    for ep in bad {
        assert!(ssh_args(&ep, 8).is_err(), "{ep:?}");
    }
}

#[test]
fn ssh_failures_are_explained() {
    let cases = [
        (
            "alice@h: Permission denied (publickey,password).",
            "authentication failed",
        ),
        ("Host key verification failed.", "host key"),
        (
            "ssh: Could not resolve hostname nope: Name or service not known",
            "cannot be resolved",
        ),
        (
            "ssh: connect to host h port 22: Connection refused",
            "refused",
        ),
        (
            "ssh: connect to host h port 22: Connection timed out",
            "timed out",
        ),
        (
            "kex_exchange_identification: Connection closed by remote host",
            "closed",
        ),
        ("", "ssh failed"),
    ];
    for (stderr, want) in cases {
        let msg = classify_ssh_failure(stderr);
        assert!(msg.contains(want), "{stderr:?} -> {msg}");
    }
    // Debug noise and the "Permanently added" warning are not used as the detail.
    let msg = classify_ssh_failure(
        "debug1: x\nssh: connect to host h port 22: Connection refused\nWarning: Permanently added 'h' to known hosts.\n",
    );
    assert!(
        msg.ends_with("(ssh: connect to host h port 22: Connection refused)"),
        "{msg}"
    );
}

#[test]
fn records_ignore_noise_and_decode_kinds() {
    let n = "0123456789abcdef";
    let out = format!(
        "Welcome to host!\n\
         @@SB:{n}:K:os:4c696e7578\n\
         noise without newline@@SB:{n}:K:shell:2f62696e2f62617368\r\n\
         @@SB:ffffffffffffffff:K:os:00\n\
         @@SB:{n}:F:config:610a\n\
         @@SB:{n}:A:auth:\n\
         @@SB:{n}:U:rc.bashrc:\n\
         @@SB:{n}:L:rc.zshrc:\n\
         @@SB:{n}:K:rollout:61\n@@SB:{n}:K:rollout:62\n\
         @@SB:{n}:K:empty:\n"
    );
    let r = Records::parse(out.as_bytes(), n).unwrap();
    assert_eq!(r.get("os"), Some("Linux"));
    assert_eq!(r.get("shell"), Some("/bin/bash"));
    assert_eq!(
        r.file("config")
            .and_then(FileRead::bytes)
            .map(|b| b.expose().to_vec()),
        Some(b"a\n".to_vec())
    );
    assert_eq!(r.file("auth"), Some(&FileRead::Absent));
    assert_eq!(r.file("rc.bashrc"), Some(&FileRead::Unreadable));
    assert_eq!(r.file("rc.zshrc"), Some(&FileRead::TooLarge));
    assert_eq!(r.all("rollout"), vec!["a", "b"]);
    assert_eq!(r.get("empty"), Some(""));
    assert_eq!(r.get_nonempty("empty"), None);
    assert!(r.require_done("x").is_err());
}

#[test]
fn malformed_tagged_records_are_errors() {
    let n = "0123456789abcdef";
    for bad in [
        format!("@@SB:{n}:K:x\n"),
        format!("@@SB:{n}:K:x:zz\n"),
        format!("@@SB:{n}:Q:x:00\n"),
    ] {
        assert!(Records::parse(bad.as_bytes(), n).is_err(), "{bad}");
    }
}

// ---------------------------------------------------------------------------
// rc planning
// ---------------------------------------------------------------------------

const HOME: &str = "/home/u";

fn bases() -> RcBases {
    RcBases {
        home: HOME.into(),
        zdotdir: None,
        xdg_config_home: None,
    }
}
const ORIGIN: &str = "https://api.kbsfdxc.net";

fn rc(files: &[(&str, &str)]) -> Vec<RcFile> {
    RC_FILES
        .iter()
        .map(|spec| RcFile {
            spec: *spec,
            path: rc_path(&bases(), spec),
            content: files
                .iter()
                .find(|(n, _)| *n == spec.name)
                .map(|(_, c)| c.as_bytes().to_vec()),
            unreadable: false,
        })
        .collect()
}

fn set_op() -> EnvOp {
    EnvOp::Set {
        value: ORIGIN.into(),
        take_over_foreign: false,
    }
}

fn after(plan: &superba_remote::envrc::RcPlan, name: &str) -> Option<String> {
    plan.writes
        .iter()
        .find(|w| w.name == name)
        .and_then(|w| w.after.clone())
        .map(|b| String::from_utf8(b).unwrap())
}

/// Re-run planning on the result to check idempotence.
fn apply_writes(files: &[RcFile], plan: &superba_remote::envrc::RcPlan) -> Vec<RcFile> {
    files
        .iter()
        .map(|f| {
            let mut f = f.clone();
            if let Some(w) = plan.writes.iter().find(|w| w.name == f.spec.name) {
                f.content = w.after.clone();
            }
            f
        })
        .collect()
}

#[test]
fn zsh_gets_a_block_at_the_end_of_zshenv_and_legacy_lines_are_commented() {
    let legacy = format!("export {ENV_AUTHAPI_BASE_URL}=\"https://api-slb.kbsfdxc.net\"");
    let zshrc = format!("alias ll='ls -l'\n{legacy}\n");
    let files = rc(&[("zshenv", "export FOO=1"), ("zshrc", &zshrc)]);
    let plan = plan_env(&ShellKind::Zsh, &files, &set_op()).unwrap();
    let zshenv = after(&plan, "zshenv").unwrap();
    assert!(
        zshenv.starts_with("export FOO=1\n# >>> superba >>>\n"),
        "{zshenv}"
    );
    assert!(zshenv.contains(&format!("export {ENV_AUTHAPI_BASE_URL}=\"{ORIGIN}\"\n")));
    assert!(zshenv.ends_with(&format!("{MARKER_END}\n")));
    assert_eq!(
        after(&plan, "zshrc").unwrap(),
        format!("alias ll='ls -l'\n: {LEGACY_COMMENT_PREFIX}{legacy}\n")
    );
    assert!(plan.edits.iter().any(|e| matches!(
        &e.action,
        EnvEditAction::CommentLegacyLine { line_no: 2, original } if *original == legacy
    )));
    // Every edit names its rc file; the last edit of a file carries the final hash.
    for e in &plan.edits {
        assert!(matches!(&e.location, EnvLocation::ShellRc { .. }));
    }
    // Idempotent.
    let again = plan_env(&ShellKind::Zsh, &apply_writes(&files, &plan), &set_op()).unwrap();
    assert!(
        again.writes.is_empty() && again.edits.is_empty(),
        "{again:?}"
    );
}

#[test]
fn bash_writes_top_of_bashrc_and_the_existing_login_file() {
    let bashrc = "case $- in *i*) ;; *) return;; esac\nPS1='$ '\n";
    let files = rc(&[("bashrc", bashrc), ("bash_profile", "source ~/.bashrc")]);
    let plan = plan_env(&ShellKind::Bash, &files, &set_op()).unwrap();
    let new_rc = after(&plan, "bashrc").unwrap();
    assert!(new_rc.starts_with(MARKER_BEGIN), "{new_rc}");
    assert!(new_rc.ends_with(bashrc));
    let profile = after(&plan, "bash_profile").unwrap();
    assert!(
        profile.starts_with("source ~/.bashrc\n# >>> superba >>>"),
        "{profile}"
    );
    assert!(
        after(&plan, "profile").is_none(),
        ".profile is not read when .bash_profile exists"
    );

    // No login file at all: ~/.profile is created.
    let plan = plan_env(&ShellKind::Bash, &rc(&[]), &set_op()).unwrap();
    assert!(after(&plan, "profile").unwrap().starts_with(MARKER_BEGIN));
    assert!(after(&plan, "bashrc").unwrap().starts_with(MARKER_BEGIN));
}

#[test]
fn a_changed_value_replaces_the_block_in_place_once() {
    let files = rc(&[("profile", "X=1\n")]);
    let first = plan_env(&ShellKind::Sh, &files, &set_op()).unwrap();
    let applied = apply_writes(&files, &first);
    let other = EnvOp::Set {
        value: "https://api.kbsfdxc.com".into(),
        take_over_foreign: false,
    };
    let second = plan_env(&ShellKind::Sh, &applied, &other).unwrap();
    let text = after(&second, "profile").unwrap();
    assert_eq!(text.matches(MARKER_BEGIN).count(), 1);
    assert!(text.contains("https://api.kbsfdxc.com"));
    assert!(!text.contains(ORIGIN));
    // The old block's Krill export line is not mistaken for a legacy line.
    assert!(!text.contains(LEGACY_COMMENT_PREFIX));
}

#[test]
fn fish_uses_a_conf_d_file_and_foreign_lines_are_reported() {
    let cfg = format!("set -gx {ENV_AUTHAPI_BASE_URL} https://example.org\n");
    let files = rc(&[("fish_config", &cfg)]);
    let plan = plan_env(&ShellKind::Fish, &files, &set_op()).unwrap();
    let conf = after(&plan, "fish_superba").unwrap();
    assert!(conf.contains(&format!("set -gx {ENV_AUTHAPI_BASE_URL} \"{ORIGIN}\"")));
    assert!(
        after(&plan, "fish_config").is_none(),
        "foreign lines are left alone"
    );
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(plan.warnings[0].text.key, "remote.warn.foreign_env");
}

#[test]
fn restore_removes_blocks_and_uncomments_legacy_lines() {
    let legacy = format!("export {ENV_AUTHAPI_BASE_URL}=https://api.kbsfdxc.net");
    let bashrc = format!("A=1\n{legacy}\n");
    let files = rc(&[("bashrc", &bashrc), ("profile", "P=1\n")]);
    let set = plan_env(&ShellKind::Bash, &files, &set_op()).unwrap();
    let applied = apply_writes(&files, &set);
    for op in [
        EnvOp::Clear,
        EnvOp::RestoreBaseline {
            snapshots: vec![],
            legacy_lines: vec![],
        },
    ] {
        let restore = plan_env(&ShellKind::Bash, &applied, &op).unwrap();
        assert_eq!(after(&restore, "bashrc").unwrap(), bashrc);
        assert_eq!(after(&restore, "profile").unwrap(), "P=1\n");
        assert!(
            restore
                .edits
                .iter()
                .any(|e| matches!(e.action, EnvEditAction::UncommentLegacyLine { .. }))
        );
    }
    // fish: the Superba-only file is removed.
    let f = plan_env(&ShellKind::Fish, &rc(&[]), &set_op()).unwrap();
    let applied = apply_writes(&rc(&[]), &f);
    let restore = plan_env(&ShellKind::Fish, &applied, &EnvOp::Clear).unwrap();
    let w = restore
        .writes
        .iter()
        .find(|w| w.name == "fish_superba")
        .unwrap();
    assert!(w.after.is_none());
}

#[test]
fn unsupported_shells_and_unreadable_targets_are_refused() {
    for sh in ["csh", "tcsh", "nu", "xonsh"] {
        let k = ShellKind::Other(sh.into());
        assert!(!shell_supported(&k));
        assert!(plan_env(&k, &rc(&[]), &set_op()).is_err());
    }
    assert!(shell_supported(&ShellKind::Other("mksh".into())));
    let mut files = rc(&[]);
    for f in &mut files {
        if f.spec.name == "zshenv" {
            f.content = Some(vec![0xff, 0xfe]);
        }
    }
    assert!(plan_env(&ShellKind::Zsh, &files, &set_op()).is_err());
}

// ---------------------------------------------------------------------------
// turns
// ---------------------------------------------------------------------------

#[test]
fn thread_ids_come_from_rollout_names() {
    assert_eq!(
        thread_id_from_path(
            "/h/.codex/sessions/2026/10/16/rollout-2026-10-16T10-00-00-0199e0f2-7c1a-7b3e-9d2f-3a4b5c6d7e8f.jsonl"
        )
        .as_deref(),
        Some("0199e0f2-7c1a-7b3e-9d2f-3a4b5c6d7e8f")
    );
    assert_eq!(thread_id_from_path("/x/rollout-short.jsonl"), None);
}

#[test]
fn busy_threads_follow_tail_events_then_mtime() {
    let n = "0123456789abcdef";
    let rec = |v: &str| format!("@@SB:{n}:K:rollout:{}\n", hex::encode(v));
    let out = [
        rec("300|\"type\":\"task_started\"|/s/rollout-a.jsonl"),
        rec("2|\"type\":\"task_complete\"|/s/rollout-b.jsonl"),
        rec("5||/s/rollout-c.jsonl"),
        rec("500||/s/rollout-d.jsonl"),
        rec("1|\"type\": \"turn_started\"|/s/rollout-e.jsonl"),
        rec("1|\"type\":\"turn_aborted\"|/s/rollout-f.jsonl"),
        rec("garbage"),
        format!("@@SB:{n}:K:done:31\n"),
    ]
    .concat();
    let recs = Records::parse(out.as_bytes(), n).unwrap();
    let busy = busy_threads(
        &TargetId::remote("h"),
        &recs,
        Timestamp::from_millis(1_000_000),
    );
    let got: Vec<(&str, ActivitySignal)> = busy
        .iter()
        .map(|t| (t.rollout_path.as_deref().unwrap(), t.signal))
        .collect();
    assert_eq!(
        got,
        vec![
            ("/s/rollout-a.jsonl", ActivitySignal::RolloutTail),
            ("/s/rollout-c.jsonl", ActivitySignal::RolloutMtime),
            ("/s/rollout-e.jsonl", ActivitySignal::RolloutTail),
        ]
    );
    assert_eq!(busy[0].last_event_at, Some(Timestamp::from_millis(700_000)));
}

// ---------------------------------------------------------------------------
// i18n
// ---------------------------------------------------------------------------

fn placeholders(template: &str) -> std::collections::BTreeSet<String> {
    template
        .split('{')
        .skip(1)
        .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_owned()))
        .collect()
}

#[test]
fn every_emitted_text_has_both_core_translations_with_matching_placeholders() {
    use superba_core::i18n::{core_catalog_source, flatten_toml};
    use superba_core::{Catalog, Locale};
    for text in superba_remote::texts::all_samples() {
        let args: std::collections::BTreeSet<String> =
            text.args.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(placeholders(&text.fallback), args, "{} fallback", text.key);
        for locale in Locale::ALL {
            let entries = flatten_toml(core_catalog_source(locale)).unwrap();
            let template = entries
                .get(&text.key)
                .unwrap_or_else(|| panic!("{} missing in {locale}", text.key));
            assert_eq!(placeholders(template), args, "{} in {locale}", text.key);
            let rendered = Catalog::new(locale).text(&text);
            assert!(placeholders(&rendered).is_empty(), "{rendered}");
        }
    }
}

// ---------------------------------------------------------------------------
// review regressions
// ---------------------------------------------------------------------------

fn syntax_ok(shell: &str, text: &str) -> bool {
    Command::new(shell)
        .arg("-n")
        .arg("-c")
        .arg(text)
        .env_clear()
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn a_neutralised_legacy_line_keeps_blocks_syntactically_valid() {
    let legacy = format!("  export {ENV_AUTHAPI_BASE_URL}=https://api.kbsfdxc.net");
    let bashrc = format!("if true; then\n{legacy}\nfi\nf() {{\n{legacy}\n}}\n");
    let files = rc(&[("bashrc", &bashrc)]);
    let plan = plan_env(&ShellKind::Bash, &files, &set_op()).unwrap();
    let text = after(&plan, "bashrc").unwrap();
    assert!(
        text.contains(&format!("  : {LEGACY_COMMENT_PREFIX}export ")),
        "{text}"
    );
    for sh in shells() {
        assert!(syntax_ok(sh, &bashrc), "{sh}: original must parse");
        assert!(
            syntax_ok(sh, &text),
            "{sh}: neutralised file must parse:\n{text}"
        );
    }
    // Restore gives back the exact original lines (both comment forms).
    let restored = plan_env(
        &ShellKind::Bash,
        &apply_writes(&files, &plan),
        &EnvOp::Clear,
    )
    .unwrap();
    assert_eq!(after(&restored, "bashrc").unwrap(), bashrc);
    assert_eq!(
        restore_legacy(&format!("{LEGACY_COMMENT_PREFIX}{legacy}")).as_deref(),
        Some(legacy.as_str())
    );
    assert_eq!(
        restore_legacy(&neutralise_legacy(&legacy, true)).as_deref(),
        Some(legacy.as_str())
    );
    assert_eq!(restore_legacy("# [superba] unrelated comment"), None);
}

#[test]
fn fish_keeps_other_content_of_its_conf_d_file() {
    let files = rc(&[("fish_superba", "set -gx OTHER_SETTING 1\n")]);
    let plan = plan_env(&ShellKind::Fish, &files, &set_op()).unwrap();
    let text = after(&plan, "fish_superba").unwrap();
    assert!(
        text.starts_with("set -gx OTHER_SETTING 1\n# >>> superba >>>"),
        "{text}"
    );
    let restored = plan_env(
        &ShellKind::Fish,
        &apply_writes(&files, &plan),
        &EnvOp::Clear,
    )
    .unwrap();
    assert_eq!(
        after(&restored, "fish_superba").unwrap(),
        "set -gx OTHER_SETTING 1\n"
    );
}

#[test]
fn startup_file_bases_follow_inherited_zdotdir_and_xdg_config_home() {
    let b = RcBases {
        home: "/home/u".into(),
        zdotdir: Some("/srv/zsh".into()),
        xdg_config_home: Some("/srv/config".into()),
    };
    let path = |name: &str| rc_path(&b, RC_FILES.iter().find(|s| s.name == name).unwrap());
    assert_eq!(path("zshenv"), "/srv/zsh/.zshenv");
    assert_eq!(path("bashrc"), "/home/u/.bashrc");
    assert_eq!(path("fish_superba"), "/srv/config/fish/conf.d/superba.fish");
    assert_eq!(
        rc_path(
            &bases(),
            RC_FILES.iter().find(|s| s.name == "fish_config").unwrap()
        ),
        "/home/u/.config/fish/config.fish"
    );
}

#[test]
fn ssh_tokens_with_shell_metacharacters_are_refused() {
    for alias in [
        "dev$(id)", "dev`id`", "a;b", "a|b", "a&b", "a'b", "a\"b", "a*b", "a\\b", "a>b",
    ] {
        let ep = SshEndpoint {
            alias: Some(alias.into()),
            ..SshEndpoint::default()
        };
        assert!(ssh_args(&ep, 8).is_err(), "{alias}");
        let ep = SshEndpoint {
            host: Some(alias.into()),
            ..SshEndpoint::default()
        };
        assert!(ssh_args(&ep, 8).is_err(), "{alias}");
    }
    for ok in [
        "devbox",
        "gpu-01.example.org",
        "fe80::1%eth0",
        "u@h",
        "my_host",
    ] {
        let ep = SshEndpoint {
            alias: Some(ok.into()),
            ..SshEndpoint::default()
        };
        assert!(ssh_args(&ep, 8).is_ok(), "{ok}");
    }
}

#[test]
fn hostile_rollout_names_do_not_panic() {
    let name = format!("/s/rollout-é{}.jsonl", "a".repeat(35));
    assert_eq!(thread_id_from_path(&name), None);
    assert_eq!(thread_id_from_path("/s/é.jsonl"), None);
    assert_eq!(thread_id_from_path(""), None);
}

#[test]
fn incomplete_turn_scans_are_reported() {
    let n = "0123456789abcdef";
    let k = |name: &str| format!("@@SB:{n}:K:{name}:31\n");
    for flag in ["scan_failed", "truncated", "stat_failed"] {
        let out = format!("{}{}", k(flag), k("done"));
        let recs = Records::parse(out.as_bytes(), n).unwrap();
        assert!(!superba_remote::turns::scan_complete(&recs), "{flag}");
    }
    let recs = Records::parse(k("done").as_bytes(), n).unwrap();
    assert!(superba_remote::turns::scan_complete(&recs));
}
