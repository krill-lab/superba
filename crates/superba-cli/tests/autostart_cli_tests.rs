use clap::Parser;
use superba_cli::{AutostartCommand, Cli, Command};

#[test]
fn parses_all_login_startup_commands_and_rejects_implicit_changes() {
    for command in ["status", "on", "off"] {
        assert!(matches!(
            Cli::try_parse_from(["spb", "autostart", command])
                .unwrap()
                .command,
            Command::Autostart { .. }
        ));
    }
    assert!(matches!(
        Cli::try_parse_from(["spb", "autostart", "status"])
            .unwrap()
            .command,
        Command::Autostart {
            command: AutostartCommand::Status
        }
    ));
    assert!(Cli::try_parse_from(["spb", "autostart"]).is_err());
    assert!(Cli::try_parse_from(["spb", "autostart", "toggle"]).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn real_cli_lists_profiles_when_the_login_entry_is_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let entry = home.join(".config/autostart/superba.desktop");
    let run = || {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_spb"))
            .env_clear()
            .env("HOME", &home)
            .env("SUPERBA_DATA_DIR", dir.path().join("data"))
            .env("CODEX_HOME", home.join("codex"))
            .env("SUPERBA_SECRET_BACKEND", "file")
            .args(["--json", "profile", "list"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    std::fs::create_dir_all(&home).unwrap();
    let (before, _) = run();
    std::fs::create_dir_all(&entry).unwrap();
    let (after, warning) = run();

    assert_eq!(after, before);
    assert!(warning.contains("WARN"), "{warning}");
    assert!(
        warning.contains("login startup status cannot be read"),
        "{warning}"
    );
    assert!(warning.contains("superba.desktop"), "{warning}");
    assert!(entry.is_dir());
}

#[cfg(target_os = "linux")]
#[test]
fn real_cli_on_status_off_in_an_isolated_home() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let install = dir.path().join("custom install");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&install).unwrap();
    let spb = install.join("spb");
    std::fs::copy(env!("CARGO_BIN_EXE_spb"), &spb).unwrap();
    let gui = install.join("superba");
    std::fs::write(&gui, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&gui, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |mode: &str| {
        let out = std::process::Command::new(&spb)
            .env_clear()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("SUPERBA_DATA_DIR", dir.path().join("data"))
            .env("CODEX_HOME", home.join("codex"))
            .env("SUPERBA_SECRET_BACKEND", "file")
            .args(["--json", "autostart", mode])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let off = run("status");
    assert!(off.to_string().contains("\"enabled\":false"));
    let on = run("on");
    assert!(on.to_string().contains("\"enabled\":true"));
    let entry = home.join("config/autostart/superba.desktop");
    let text = std::fs::read_to_string(&entry).unwrap();
    assert!(text.contains(&format!("Exec=\"{}\" --autostart", gui.display())));
    assert_eq!(run("status"), on);
    assert!(run("off").to_string().contains("\"enabled\":false"));
    assert!(!entry.exists());
    run("off");
}
