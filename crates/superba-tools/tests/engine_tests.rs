//! The engine end to end against a local server and fake tool programs
//! (shell scripts): install → version → update → removal for pi, opencode,
//! Claude Code and dsh's Node-free runtime; a checksum mismatch installs
//! nothing; settings and data stay; the updater switches; one operation per
//! tool at a time.

#![cfg(unix)]

mod support;

use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{
    Error, NetworkSettings, NoToolProgress, ToolId, ToolInstallKind, ToolOp, ToolRequest,
    ToolSettingsChange, ToolSupport, ToolUpdater,
};
use superba_tools::platform::{Arch, Os, Platform};
use superba_tools::state::ToolLock;
use superba_tools::{NoHost, ToolManager};
use support::{Mock, Route, ZipFile, context, sha256_hex, sha512_b64, sources, targz, zip_bytes};

fn manager(root: &Path, mock: &Mock, extra_path: &[&Path], env: &[(&str, &str)]) -> ToolManager {
    let mut ctx = context(root, extra_path);
    if !env.is_empty() {
        let mut map = superba_core::MapEnv::new();
        for k in ["PATH", "SHELL"] {
            if let Some(v) = ctx.env.var(k) {
                map = map.with(k, v);
            }
        }
        for (k, v) in env {
            map = map.with(k, *v);
        }
        ctx.env = Arc::new(map);
    }
    ToolManager::with(
        &ctx,
        NetworkSettings::default(),
        Arc::new(NoHost),
        Platform::new(Os::Linux, Arch::X64),
        sources(&mock.base),
    )
}

fn req(tool: ToolId, op: ToolOp, version: Option<&str>) -> ToolRequest {
    ToolRequest {
        version: version.map(str::to_owned),
        ..ToolRequest::new(tool, op)
    }
}

fn script(version_line: &str, extra: &str) -> Vec<u8> {
    format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"{version_line}\"; exit 0; fi\n{extra}\nexit 0\n")
        .into_bytes()
}

fn pi_routes(mock: &Mock, version: &str, bad_sum: bool) {
    let tgz = targz(
        &[
            ("pi/pi", &script(version, ""), 0o755),
            ("pi/themes/dark.json", b"{}", 0o644),
        ],
        &[],
    );
    let sum = if bad_sum {
        "0".repeat(64)
    } else {
        sha256_hex(&tgz)
    };
    let base = format!("/github/earendil-works/pi/releases/download/v{version}");
    mock.route(
        &format!("{base}/SHA256SUMS"),
        Route::ok(format!(
            "{sum}  pi-linux-x64.tar.gz\n{}  pi-darwin-arm64.tar.gz\n",
            "1".repeat(64)
        )),
    );
    mock.route(&format!("{base}/pi-linux-x64.tar.gz"), Route::ok(tgz));
}

#[tokio::test]
async fn pi_install_update_uninstall() {
    let mock = Mock::start().await;
    mock.route(
        "/pi/api/latest-version",
        Route::ok(r#"{"ok":true,"version":"1.3.0"}"#),
    );
    pi_routes(&mock, "1.2.0", false);
    pi_routes(&mock, "1.3.0", false);
    let root = tempfile::tempdir().unwrap();
    let tm = manager(root.path(), &mock, &[], &[]);
    let home = root.path().join("home");
    support::write(&home.join(".pi/agent/settings.json"), b"{\"keep\":true}");

    let row = tm.row(ToolId::Pi, false).await.unwrap();
    assert_eq!(row.support, ToolSupport::Full);
    assert!(row.installs.is_empty());
    assert_eq!(row.latest.as_deref(), Some("1.3.0"));
    assert!(row.actions.install && !row.actions.update && !row.actions.uninstall);

    let rep = tm
        .run(
            req(ToolId::Pi, ToolOp::Install, Some("1.2.0")),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version.as_deref(), Some("1.2.0"));
    let exe = root.path().join("data/tools/pi/app/pi");
    assert_eq!(rep.path.as_deref(), Some(exe.as_path()));
    assert!(
        root.path()
            .join("data/tools/pi/app/themes/dark.json")
            .is_file()
    );
    let launcher = home.join(".local/bin/pi");
    let text = std::fs::read_to_string(&launcher).unwrap();
    assert!(text.contains("Installed by Superba") && text.contains(&*exe.to_string_lossy()));
    let log = std::fs::read_to_string(&rep.log).unwrap();
    assert!(log.contains("verified: sha256"), "{log}");

    let row = tm.row(ToolId::Pi, false).await.unwrap();
    let i = row.primary().unwrap();
    assert_eq!(
        (i.kind, i.by_superba, i.on_path),
        (ToolInstallKind::Standalone, true, true)
    );
    assert_eq!(row.installed_version(), Some("1.2.0"));
    assert!(row.update_available && row.actions.update && row.actions.uninstall);
    assert!(!row.actions.install, "already installed");

    let rep = tm
        .run(
            req(ToolId::Pi, ToolOp::Update, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version.as_deref(), Some("1.3.0"));
    let row = tm.row(ToolId::Pi, false).await.unwrap();
    assert_eq!(row.installed_version(), Some("1.3.0"));
    assert!(!row.update_available);
    assert!(!root.path().join("data/tools/pi/app.old").exists());

    let rep = tm
        .run(
            req(ToolId::Pi, ToolOp::Uninstall, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version, None);
    assert!(!exe.exists() && !launcher.exists());
    assert_eq!(
        std::fs::read(home.join(".pi/agent/settings.json")).unwrap(),
        b"{\"keep\":true}",
        "pi's settings are never touched"
    );
    assert!(tm.row(ToolId::Pi, false).await.unwrap().installs.is_empty());
}

#[tokio::test]
async fn a_checksum_mismatch_installs_nothing() {
    let mock = Mock::start().await;
    mock.route(
        "/pi/api/latest-version",
        Route::ok(r#"{"version":"1.3.0"}"#),
    );
    pi_routes(&mock, "1.3.0", true);
    let root = tempfile::tempdir().unwrap();
    let tm = manager(root.path(), &mock, &[], &[]);
    let err = tm
        .run(
            req(ToolId::Pi, ToolOp::Install, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("does not match its published sha256"),
        "{err}"
    );
    assert!(!root.path().join("data/tools/pi/app").exists());
    assert!(!root.path().join("home/.local/bin/pi").exists());
    let log = std::fs::read_to_string(root.path().join("data/logs/tools/pi.log")).unwrap();
    assert!(log.contains("download failed"), "{log}");
    assert!(tm.row(ToolId::Pi, false).await.unwrap().installs.is_empty());
}

#[tokio::test]
async fn one_operation_per_tool_at_a_time() {
    let mock = Mock::start().await;
    let root = tempfile::tempdir().unwrap();
    let tm = manager(root.path(), &mock, &[], &[]);
    let data = superba_core::DataDir::new(root.path().join("data"));
    let _held = ToolLock::acquire(&data, ToolId::Pi).unwrap();
    let err = tm
        .run(
            req(ToolId::Pi, ToolOp::Install, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Locked { .. }), "{err:?}");
    // Another tool is not blocked by it.
    assert!(ToolLock::acquire(&data, ToolId::Dsh).is_ok());
}

fn opencode_routes(mock: &Mock) {
    // 2.0.32 = the newest: the standalone archive with the update API's sha256.
    let new = targz(&[("opencode", &script("opencode v2.0.32", ""), 0o755)], &[]);
    mock.route(
        "/opencode/update/api/latest/cli/npm",
        Route::ok(r#"{"version":"2.0.32"}"#),
    );
    let files = json!({"artifacts": [{"distribution": "opencode", "version": "2.0.32", "metadata": {"files": {
        "opencode-linux-x64.tar.gz": {"url": mock.url("/files/2.0.32/opencode-linux-x64.tar.gz"), "sha256": sha256_hex(&new), "size": new.len()}
    }}}]});
    mock.route(
        "/opencode/update/api/latest/cli",
        Route::ok(files.to_string()),
    );
    mock.route("/files/2.0.32/opencode-linux-x64.tar.gz", Route::ok(new));
    // 2.0.31: the npm platform package, checked by the registry's sha512.
    let old = targz(
        &[
            ("package/package.json", b"{}", 0o644),
            (
                "package/bin/opencode",
                &script("opencode v2.0.31", ""),
                0o755,
            ),
        ],
        &[],
    );
    let meta = json!({"dist": {"tarball": mock.url("/npmtgz/cli-linux-x64-2.0.31.tgz"), "integrity": format!("sha512-{}", sha512_b64(&old))}});
    mock.route(
        "/npm/@opencode%2fcli-linux-x64/2.0.31",
        Route::ok(meta.to_string()),
    );
    mock.route("/npmtgz/cli-linux-x64-2.0.31.tgz", Route::ok(old));
}

#[tokio::test]
async fn opencode_install_update_switch_uninstall() {
    let mock = Mock::start().await;
    opencode_routes(&mock);
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin = home.join(".opencode/bin");
    let tm = manager(root.path(), &mock, &[&bin], &[]);
    let config = home.join(".config/opencode/opencode.jsonc");
    support::write(&config, b"{\n  // mine\n  \"model\": \"krill/x\",\n}\n");

    let row = tm.row(ToolId::Opencode, false).await.unwrap();
    assert_eq!(
        row.channel.as_deref(),
        Some("latest"),
        "OpenCode 2 by default"
    );
    assert_eq!(row.latest.as_deref(), Some("2.0.32"));

    let rep = tm
        .run(
            req(ToolId::Opencode, ToolOp::Install, Some("2.0.31")),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version.as_deref(), Some("2.0.31"));
    let exe = bin.join("opencode");
    assert_eq!(rep.path.as_deref(), Some(exe.as_path()));
    let row = tm.row(ToolId::Opencode, false).await.unwrap();
    assert_eq!(row.installed_version(), Some("2.0.31"));
    assert!(row.update_available);
    assert_eq!(row.updater, ToolUpdater::Notifies);

    let rep = tm
        .run(
            req(ToolId::Opencode, ToolOp::Update, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version.as_deref(), Some("2.0.32"));

    let row = tm
        .settings(
            ToolId::Opencode,
            ToolSettingsChange {
                manage_updates: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.updater, ToolUpdater::Superba);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(
        text.contains("\"update\": \"disable\"") && text.contains("// mine"),
        "{text}"
    );
    let row = tm
        .settings(
            ToolId::Opencode,
            ToolSettingsChange {
                manage_updates: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.updater, ToolUpdater::Notifies);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "{\n  // mine\n  \"model\": \"krill/x\",\n}\n",
        "back to the user's file"
    );

    tm.run(
        req(ToolId::Opencode, ToolOp::Uninstall, None),
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    assert!(!exe.exists());
    assert!(config.is_file(), "settings are kept unless asked");
    assert!(
        !tm.ctx().tool_dir(ToolId::Opencode).exists(),
        "empty install staging folder is removed"
    );
    assert!(!tm.ctx().data.root().join("tools/opencode.lock").exists());
}

#[tokio::test]
async fn opencode_version_probes_leave_user_directories_untouched() {
    let mock = Mock::start().await;
    opencode_routes(&mock);
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin = home.join(".opencode/bin");
    let data = root.path().join("user-data");
    let cache = root.path().join("user-cache");
    let state = root.path().join("user-state");
    let tm = manager(
        root.path(),
        &mock,
        &[&bin],
        &[
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache.to_str().unwrap()),
            ("XDG_STATE_HOME", state.to_str().unwrap()),
        ],
    );
    let marker = data.join("opencode/session.txt");
    support::write(&marker, b"keep my session");
    let body = b"#!/bin/sh\n/bin/mkdir -p \"$XDG_DATA_HOME/opencode/log\" \"$XDG_DATA_HOME/opencode/repos\" \"$XDG_CACHE_HOME/opencode/bin\" \"$XDG_STATE_HOME/opencode\"\necho log > \"$XDG_DATA_HOME/opencode/log/opencode.log\"\necho 'opencode v2.0.32'\n";
    let archive = targz(&[("opencode", body, 0o755)], &[]);
    mock.route("/opencode/update/api/latest/cli", Route::ok(json!({"artifacts": [{"distribution": "opencode", "version": "2.0.32", "metadata": {"files": {
        "opencode-linux-x64.tar.gz": {"url": mock.url("/probe.tar.gz"), "sha256": sha256_hex(&archive), "size": archive.len()}
    }}}]}).to_string()));
    mock.route("/probe.tar.gz", Route::ok(archive));
    tm.run(
        req(ToolId::Opencode, ToolOp::Install, None),
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    assert!(
        !data.join("opencode/log").exists(),
        "install verification writes only to scratch"
    );
    superba_tools::forget_versions();
    assert_eq!(
        tm.row(ToolId::Opencode, false)
            .await
            .unwrap()
            .installed_version(),
        Some("2.0.32")
    );
    assert!(
        !data.join("opencode/log").exists(),
        "list probe writes only to scratch"
    );
    assert!(!data.join("opencode/repos").exists());
    assert!(!cache.exists() && !state.exists());
    assert!(!home.join(".local/share/opencode").exists());
    assert!(!home.join(".cache/opencode").exists());
    assert!(!home.join(".local/state/opencode").exists());
    assert_eq!(
        std::fs::read_dir(tm.ctx().data.root().join("tmp"))
            .unwrap()
            .count(),
        0
    );
    tm.run(
        req(ToolId::Opencode, ToolOp::Uninstall, None),
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&marker).unwrap(), b"keep my session");
    assert!(!tm.ctx().tool_dir(ToolId::Opencode).exists());
    assert!(!tm.ctx().data.root().join("tools/opencode.lock").exists());
}

fn claude_binary(v: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\nV={v}\ncase \"$1\" in\n  --version) echo \"$V (Claude Code)\" ;;\n  install) /bin/mkdir -p \"$HOME/.local/bin\" && /bin/ln -sfn \"$HOME/.local/share/claude/versions/$2\" \"$HOME/.local/bin/claude\" && echo \"Claude Code successfully installed! $2\" ;;\n  daemon) exit 0 ;;\n  *) exit 3 ;;\nesac\n"
    )
    .into_bytes()
}

fn claude_routes(mock: &Mock) {
    mock.route("/claude/stable", Route::ok("2.1.292\n"));
    mock.route("/claude/latest", Route::ok("2.1.302"));
    for v in ["2.1.292", "2.1.302"] {
        let bin = claude_binary(v);
        let manifest = json!({"version": v, "platforms": {"linux-x64": {"binary": "claude", "checksum": sha256_hex(&bin), "size": bin.len()}}});
        mock.route(
            &format!("/claude/{v}/manifest.json"),
            Route::ok(manifest.to_string()),
        );
        mock.route(&format!("/claude/{v}/linux-x64/claude"), Route::ok(bin));
    }
}

#[tokio::test]
async fn claude_code_native_install_update_switch_uninstall() {
    let mock = Mock::start().await;
    claude_routes(&mock);
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let tm = manager(root.path(), &mock, &[], &[]);
    let settings = home.join(".claude/settings.json");
    support::write(&settings, br#"{"model":"opus","env":{"FOO":"bar"}}"#);
    support::write(&home.join(".claude.json"), b"{\"userID\":\"u\"}");

    tm.settings(
        ToolId::ClaudeCode,
        ToolSettingsChange {
            channel: Some("stable".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let rep = tm
        .run(
            req(ToolId::ClaudeCode, ToolOp::Install, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version.as_deref(), Some("2.1.292"));
    let launcher = home.join(".local/bin/claude");
    let versions = home.join(".local/share/claude/versions");
    assert_eq!(
        std::fs::read_link(&launcher).unwrap(),
        versions.join("2.1.292")
    );
    let row = tm.row(ToolId::ClaudeCode, false).await.unwrap();
    let i = row.primary().unwrap();
    assert_eq!(
        (i.kind, i.version.as_deref()),
        (ToolInstallKind::Native, Some("2.1.292"))
    );
    assert!(i.on_path && i.by_superba);
    assert!(!row.update_available, "stable is 2.1.292");
    assert_eq!(row.updater, ToolUpdater::Automatic);

    tm.settings(
        ToolId::ClaudeCode,
        ToolSettingsChange {
            channel: Some("latest".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let row = tm.row(ToolId::ClaudeCode, false).await.unwrap();
    assert_eq!(row.latest.as_deref(), Some("2.1.302"));
    assert!(row.update_available && row.actions.update);
    let rep = tm
        .run(
            req(ToolId::ClaudeCode, ToolOp::Update, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version.as_deref(), Some("2.1.302"));
    assert_eq!(
        std::fs::read_link(&launcher).unwrap(),
        versions.join("2.1.302")
    );

    // "Superba manages updates": env.DISABLE_AUTOUPDATER only, other keys kept.
    let row = tm
        .settings(
            ToolId::ClaudeCode,
            ToolSettingsChange {
                manage_updates: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.updater, ToolUpdater::Superba);
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(
        v,
        json!({"model":"opus","env":{"FOO":"bar","DISABLE_AUTOUPDATER":"1"}})
    );
    assert!(
        !std::fs::read_to_string(&settings)
            .unwrap()
            .contains("DISABLE_UPDATES")
    );
    tm.settings(
        ToolId::ClaudeCode,
        ToolSettingsChange {
            manage_updates: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(v, json!({"model":"opus","env":{"FOO":"bar"}}));

    tm.run(
        req(ToolId::ClaudeCode, ToolOp::Uninstall, None),
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    assert!(std::fs::symlink_metadata(&launcher).is_err());
    assert!(!versions.exists());
    assert!(settings.is_file() && home.join(".claude.json").is_file());
}

#[tokio::test]
async fn the_updater_switch_takes_back_a_file_it_created() {
    let mock = Mock::start().await;
    claude_routes(&mock);
    opencode_routes(&mock);
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin = home.join(".opencode/bin");
    let tm = manager(root.path(), &mock, &[&bin], &[]);
    let on = ToolSettingsChange {
        manage_updates: Some(true),
        ..Default::default()
    };
    let off = ToolSettingsChange {
        manage_updates: Some(false),
        ..Default::default()
    };
    tm.run(
        req(ToolId::ClaudeCode, ToolOp::Install, None),
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    tm.run(
        req(ToolId::Opencode, ToolOp::Install, None),
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    let settings = home.join(".claude/settings.json");
    let config = home.join(".config/opencode/opencode.json");
    assert!(!settings.exists() && !config.exists());
    tm.settings(ToolId::ClaudeCode, on.clone()).await.unwrap();
    tm.settings(ToolId::Opencode, on).await.unwrap();
    assert!(settings.is_file() && config.is_file());
    tm.settings(ToolId::ClaudeCode, off.clone()).await.unwrap();
    tm.settings(ToolId::Opencode, off).await.unwrap();
    assert!(
        !settings.exists(),
        "Superba's own empty settings.json is removed again"
    );
    assert!(!config.exists(), "and so is opencode.json");
}

#[tokio::test]
async fn claude_code_asks_before_removing_an_npm_install_and_refuses_with_disable_updates() {
    let mock = Mock::start().await;
    claude_routes(&mock);
    let root = tempfile::tempdir().unwrap();
    let prefix = root.path().join("npm-global");
    support::write(
        &prefix.join("lib/node_modules/@anthropic-ai/claude-code/package.json"),
        br#"{"name":"@anthropic-ai/claude-code","version":"2.1.284"}"#,
    );
    let prefix_s = prefix.to_string_lossy().into_owned();
    let fake_bin = root.path().join("fake-bin");
    support::script(&fake_bin.join("npm"), "exit 99");
    let tm = manager(
        root.path(),
        &mock,
        &[&fake_bin],
        &[("NPM_CONFIG_PREFIX", &prefix_s)],
    );
    let row = tm.row(ToolId::ClaudeCode, false).await.unwrap();
    let npm = row
        .installs
        .iter()
        .find(|i| i.kind == ToolInstallKind::NpmGlobal)
        .unwrap();
    assert_eq!(npm.version.as_deref(), Some("2.1.284"));
    assert!(
        row.uninstall_cautions
            .iter()
            .any(|c| c.key == "tools.claude.caution_npm_uninstall")
    );
    let err = tm
        .run(
            req(ToolId::ClaudeCode, ToolOp::Uninstall, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("confirm first"), "{err}");
    assert!(
        row.actions.install,
        "the native build can be installed beside it"
    );
    assert!(
        row.install_cautions
            .iter()
            .any(|c| c.key == "tools.claude.caution_npm"),
        "{:?}",
        row.install_cautions
    );
    let err = tm
        .run(
            req(ToolId::ClaudeCode, ToolOp::Install, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("confirm first"), "{err}");
    assert!(!root.path().join("home/.local/share/claude").exists());
    let mut confirmed = req(ToolId::ClaudeCode, ToolOp::Install, None);
    confirmed.confirmed = true;
    tm.run(confirmed, Arc::new(NoToolProgress)).await.unwrap();

    // DISABLE_UPDATES makes `claude install` a silent no-op: refused.
    support::write(
        &root.path().join("home/.claude/settings.json"),
        br#"{"env":{"DISABLE_UPDATES":"1"}}"#,
    );
    let row = tm.row(ToolId::ClaudeCode, false).await.unwrap();
    assert!(
        matches!(row.updater, ToolUpdater::Blocked { .. }),
        "{:?}",
        row.updater
    );
    let mut again = req(ToolId::ClaudeCode, ToolOp::Update, Some("2.1.292"));
    again.confirmed = true;
    let err = tm.run(again, Arc::new(NoToolProgress)).await.unwrap_err();
    assert!(err.to_string().contains("DISABLE_UPDATES"), "{err}");
}

#[tokio::test]
async fn dsh_without_node_installs_the_runtime_build() {
    let mock = Mock::start().await;
    mock.route(
        "/pypi/deepseek-harness-runtime-bin/json",
        Route::ok(r#"{"info":{"version":"0.2.1rc2"}}"#),
    );
    let exe = script("0.2.1-rc.2", "");
    let wheel = zip_bytes(&[
        ZipFile::file("deepseek_harness_runtime/__init__.py", b"", 0o644),
        ZipFile::file(
            "deepseek_harness_runtime/runtime/deepseek-harness-sdk-runtime-linux-x64",
            &exe,
            0o755,
        ),
        ZipFile::file(
            "deepseek_harness_runtime/runtime/deepseek-harness-sdk-runtime-linux-x64-rg",
            b"rg",
            0o755,
        ),
    ]);
    let file = "deepseek_harness_runtime_bin-0.2.1rc2-py3-none-manylinux_2_28_x86_64.whl";
    let meta = json!({"urls": [{"filename": file, "url": mock.url("/files/w.whl"), "digests": {"sha256": sha256_hex(&wheel)}, "size": wheel.len()}]});
    mock.route(
        "/pypi/deepseek-harness-runtime-bin/0.2.1rc2/json",
        Route::ok(meta.to_string()),
    );
    mock.route("/files/w.whl", Route::ok(wheel));
    let root = tempfile::tempdir().unwrap();
    let tm = manager(root.path(), &mock, &[], &[]);
    let row = tm.row(ToolId::Dsh, false).await.unwrap();
    assert_eq!(row.channels, ["pypi"], "no Node on PATH");
    assert_eq!(row.latest.as_deref(), Some("0.2.1-rc.2"));
    assert!(row.notes.iter().any(|n| n.text.key == "tools.dsh.no_node"));
    let rep = tm
        .run(
            req(ToolId::Dsh, ToolOp::Install, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(rep.version.as_deref(), Some("0.2.1-rc.2"));
    let runtime = root.path().join("data/tools/dsh/runtime");
    assert!(
        runtime
            .join("deepseek-harness-sdk-runtime-linux-x64-rg")
            .is_file()
    );
    let row = tm.row(ToolId::Dsh, false).await.unwrap();
    let i = row.primary().unwrap();
    assert_eq!((i.kind, i.on_path), (ToolInstallKind::Runtime, true));
    tm.run(
        req(ToolId::Dsh, ToolOp::Uninstall, None),
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    assert!(!runtime.exists() && !root.path().join("home/.local/bin/dsh").exists());
}

#[tokio::test]
async fn apps_are_links_on_linux_and_a_view_lists_every_tool() {
    let mock = Mock::start().await;
    let root = tempfile::tempdir().unwrap();
    let tm = manager(root.path(), &mock, &[], &[]);
    let view = tm.view(false).await.unwrap();
    assert_eq!(view.rows.len(), ToolId::ALL.len());
    for id in [ToolId::Zcode, ToolId::Workbuddy, ToolId::WorkbuddyAi] {
        let row = view.row(id).unwrap();
        assert!(matches!(row.support, ToolSupport::Link { .. }), "{id}");
        assert!(!row.actions.install && !row.actions.update && !row.actions.uninstall);
        assert!(row.latest_error.is_some(), "the mock has no route: {id}");
    }
    assert_eq!(
        view.row(ToolId::Workbuddy).unwrap().updater,
        ToolUpdater::Forced
    );
    let err = tm
        .run(
            req(ToolId::Zcode, ToolOp::Install, None),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}
