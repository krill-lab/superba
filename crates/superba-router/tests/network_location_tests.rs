//! User bug 39: Superba run from a Parallels shared folder
//! (`\\Mac\Home\Downloads\windows\`) installs the Run-key service with the
//! router program on the network share; at the next sign-in the share is often
//! not connected yet and the router does not start. The doctor says so (a
//! warning: it works while the share is there) and suggests the MSI.

use std::path::{Path, PathBuf};

use superba_router::doctor::{Finding, RouterFacts, findings};
use superba_router::manager::{is_network_location, network_path_text};

#[test]
fn unc_paths_are_network_locations_and_local_ones_are_not() {
    for network in [
        r"\\Mac\Home\Downloads\windows\superba-router.exe",
        r"\\mac\home\superba-router.exe",
        r"\\?\UNC\Mac\Home\Downloads\windows\superba-router.exe",
        r"\\?\unc\fileserver\tools\superba-router.exe",
        r"\\.\UNC\Mac\Home\superba-router.exe",
    ] {
        assert!(network_path_text(network), "{network}");
        assert!(is_network_location(Path::new(network)), "{network}");
    }
    for local in [
        r"C:\Program Files\Superba\superba-router.exe",
        r"\\?\C:\Program Files\Superba\superba-router.exe",
        r"\\.\pipe\superba",
        r"\\Mac",
        r"\\",
        "/Applications/Superba.app/Contents/MacOS/superba-router",
        "/usr/local/bin/superba-router",
        "superba-router",
    ] {
        assert!(!network_path_text(local), "{local}");
    }
    // A real local file is not on a network share (no false alarm).
    let exe = std::env::current_exe().unwrap();
    assert!(!is_network_location(&exe), "{}", exe.display());
}

/// The router's facts on the user's Windows machine: the program on the
/// share, the Run key installed with it, the router answering.
fn windows_facts(exe: &str, program: &str) -> RouterFacts {
    let facts = serde_json::json!({
        "superba_version": "0.1.1-alpha.1",
        "platform": "windows x86_64",
        "os_version": null,
        "data_dir": r"C:\Users\jane\AppData\Roaming\Superba",
        "exe": {
            "path": exe,
            "exists": true,
            "transient": false,
            "network": network_path_text(exe),
            "protected_folder": false,
            "candidates": [],
        },
        "service": {
            "kind": "windows_run_key",
            "status": {"Ok": {
                "kind": "windows_run_key",
                "installed": true,
                "running": true,
                "pid": 4242,
            }},
            "diagnosis": {
                "program": program,
                "fields": [],
                "crash_looping": false,
            },
        },
        "config": {
            "config_exists": true,
            "port": 50947,
            "members": [{"id": "krill", "name": "Krill", "lines": 3, "has_key": true}],
            "secrets_exists": true,
        },
        "runtime": null,
        "port": null,
        "live": {"up": true, "version": "0.1.1-alpha.1", "members": 1, "uptime_ms": 34000},
        "log": {"path": r"C:\Users\jane\AppData\Roaming\Superba\router\router.log", "exists": false, "tail": []},
        "foreground": [exe, "run"],
        "findings": [],
    });
    serde_json::from_value(facts).unwrap()
}

#[test]
fn the_doctor_warns_about_a_router_program_on_a_network_share() {
    let exe = r"\\Mac\Home\Downloads\windows\superba-router.exe";
    let facts = windows_facts(exe, exe);
    assert!(facts.exe.network);
    let found = findings(&facts, false, None);
    let warning = Finding::ExeOnNetwork {
        path: PathBuf::from(exe),
    };
    assert_eq!(found, vec![warning.clone()], "{found:?}");
    assert!(!warning.is_error(), "it runs while the share is connected");
    let english = warning.english();
    assert!(english.contains("network share"), "{english}");
    assert!(english.contains("MSI"), "{english}");

    // Installed with the MSI: nothing to warn about.
    let local = r"C:\Program Files\Superba\superba-router.exe";
    assert!(findings(&windows_facts(local, local), false, None).is_empty());

    // The MSI copy runs now, but the Run key still starts the share's copy.
    let found = findings(&windows_facts(local, exe), false, None);
    assert!(
        found.contains(&Finding::ExeOnNetwork {
            path: PathBuf::from(exe)
        }),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|f| matches!(f, Finding::StaleDefinition { .. })),
        "{found:?}"
    );
}
