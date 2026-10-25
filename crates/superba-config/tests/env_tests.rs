//! Public persistence contracts, with isolated homes and no platform services.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pretty_assertions::assert_eq;
use superba_config::{EnvPlatform, MemoryNativeEnv, NativeEnv, ShellEnvPersister, snapshots_from};
use superba_core::{
    DataDir, EnvEditAction, EnvLocation, EnvObservation, EnvOp, EnvPersister, EnvPlan, Error,
    LocalContext, MapEnv, RcLine, ShellKind,
};
use tempfile::TempDir;

const VAR: &str = "CODEX_AUTHAPI_BASE_URL";
const OLD: &str = "https://api-slb.kbsfdxc.net";
const VALUE: &str = "https://api.kbsfdxc.net";
const NEXT: &str = "https://api.kbsfdxc.com";
const BASH: &str = "# bash settings\n[ -z \"$PS1\" ] && return\nexport CODEX_AUTHAPI_BASE_URL=\"https://api-slb.kbsfdxc.net\"\n";
const ZSH: &str = "# zsh settings\nexport CODEX_AUTHAPI_BASE_URL=https://api-slb.kbsfdxc.net\n";

struct TestEnv {
    tmp: TempDir,
    ctx: LocalContext,
    native: Arc<MemoryNativeEnv>,
    persister: ShellEnvPersister,
}

impl TestEnv {
    fn new(platform: EnvPlatform, shell: &str) -> Self {
        Self::with(
            tempfile::tempdir().unwrap(),
            platform,
            MapEnv::new().with("SHELL", shell),
            MemoryNativeEnv::new(),
        )
    }

    fn with(tmp: TempDir, platform: EnvPlatform, env: MapEnv, native: MemoryNativeEnv) -> Self {
        let ctx = LocalContext::new(
            tmp.path().join("home"),
            DataDir::new(tmp.path().join("data")),
            Arc::new(env),
        );
        fs::create_dir_all(&ctx.home_dir).unwrap();
        let native = Arc::new(native);
        let persister = ShellEnvPersister::with_platform(ctx.clone(), platform, native.clone());
        Self {
            tmp,
            ctx,
            native,
            persister,
        }
    }

    fn home(&self) -> &Path {
        &self.ctx.home_dir
    }

    fn seed_legacy(&self) {
        write(&self.home().join(".bashrc"), BASH);
        write(&self.home().join(".zshrc"), ZSH);
    }

    async fn plan(&self, op: EnvOp) -> EnvPlan {
        self.persister.plan(VAR, op).await.unwrap()
    }

    async fn apply(&self, op: EnvOp) -> superba_core::EnvApplied {
        self.persister.apply(&self.plan(op).await).await.unwrap()
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn set(value: &str) -> EnvOp {
    EnvOp::Set {
        value: value.into(),
        take_over_foreign: false,
    }
}

// Independent expectations: do not render expected bytes with the production renderer.
fn block(assignment: &str) -> String {
    format!(
        "# >>> superba >>>\n# Managed by Superba. Edits inside this block are overwritten.\n{assignment}\n# <<< superba <<<\n"
    )
}

fn shell_block(value: &str) -> String {
    block(&format!("export {VAR}=\"{value}\""))
}

fn commented(text: &str) -> String {
    text.replace(
        "export CODEX_AUTHAPI_BASE_URL=",
        "# [superba] export CODEX_AUTHAPI_BASE_URL=",
    )
}

fn expected_linux(value: &str) -> BTreeMap<PathBuf, Vec<u8>> {
    [
        (
            ".bashrc",
            format!("{}\n{}", shell_block(value), commented(BASH)),
        ),
        (".zshrc", commented(ZSH)),
        (".profile", shell_block(value)),
        (".zshenv", shell_block(value)),
        (
            ".config/environment.d/60-superba.conf",
            block(&format!("{VAR}={value}")),
        ),
    ]
    .into_iter()
    .map(|(p, text)| (PathBuf::from(p), text.into_bytes()))
    .collect()
}

#[tokio::test]
async fn linux_takeover_plan_is_read_only_and_apply_is_exact_and_idempotent() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    e.seed_legacy();
    let before = files(e.tmp.path());
    let report = e.persister.inspect(VAR).await.unwrap();
    assert_eq!(report.var, VAR);
    assert_eq!(report.process_value, EnvObservation::Absent);
    assert_eq!(
        report.legacy_lines,
        vec![
            RcLine {
                path: e.home().join(".bashrc"),
                line_no: 3,
                text: format!("export {VAR}=\"{OLD}\""),
                value: OLD.into(),
            },
            RcLine {
                path: e.home().join(".zshrc"),
                line_no: 2,
                text: format!("export {VAR}={OLD}"),
                value: OLD.into(),
            },
        ]
    );
    assert!(report.foreign_lines.is_empty());
    assert!(
        report
            .persisted
            .iter()
            .all(|p| !p.managed && p.value.is_none())
    );

    let plan = e.plan(set(VALUE)).await;
    assert_eq!(files(e.tmp.path()), before);
    assert!(!e.ctx.data_dir.root().exists());
    assert_eq!(plan.edits.len(), 6);
    let comments: Vec<_> = plan
        .edits
        .iter()
        .filter_map(|edit| match (&edit.location, &edit.action) {
            (
                EnvLocation::ShellRc { path, .. },
                EnvEditAction::CommentLegacyLine { line_no, original },
            ) => Some((path.clone(), *line_no, original.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        comments,
        report
            .legacy_lines
            .iter()
            .map(|l| (l.path.clone(), l.line_no, l.text.clone()))
            .collect::<Vec<_>>()
    );
    let writes: Vec<_> = plan
        .edits
        .iter()
        .filter(|e| e.action == EnvEditAction::WriteBlock)
        .map(|e| e.location.clone())
        .collect();
    assert_eq!(
        writes,
        vec![
            EnvLocation::ShellRc {
                path: e.home().join(".profile"),
                shell: ShellKind::Sh
            },
            EnvLocation::ShellRc {
                path: e.home().join(".bashrc"),
                shell: ShellKind::Bash
            },
            EnvLocation::ShellRc {
                path: e.home().join(".zshenv"),
                shell: ShellKind::Zsh
            },
            EnvLocation::EnvironmentD {
                path: e.home().join(".config/environment.d/60-superba.conf")
            },
        ]
    );
    let applied = e.persister.apply(&plan).await.unwrap();
    assert_eq!(applied.edits, plan.edits);
    assert_eq!(files(e.home()), expected_linux(VALUE));
    let managed = e.persister.inspect(VAR).await.unwrap();
    assert!(managed.legacy_lines.is_empty());
    assert!(managed.foreign_lines.is_empty());
    assert_eq!(managed.persisted.len(), 4);
    assert!(
        managed
            .persisted
            .iter()
            .all(|p| p.managed && p.value.as_deref() == Some(VALUE))
    );
    assert!(e.plan(set(VALUE)).await.is_noop());

    let second = e.plan(set(NEXT)).await;
    assert_eq!(second.edits.len(), 4);
    assert!(
        second
            .edits
            .iter()
            .all(|e| e.action == EnvEditAction::WriteBlock)
    );
    e.persister.apply(&second).await.unwrap();
    assert_eq!(files(e.home()), expected_linux(NEXT));
}

#[tokio::test]
async fn revert_restores_original_bytes_and_removes_new_files() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    e.seed_legacy();
    // Include an existing login file with deliberately different whitespace.
    write(
        &e.home().join(".bash_profile"),
        "# login\n\nexport USER_SETTING=1",
    );
    let before = files(e.home());
    let applied = e.apply(set(VALUE)).await;
    assert_eq!(applied.backups.len(), 3);
    assert!(
        applied
            .backups
            .iter()
            .all(|p| p.starts_with(e.ctx.data_dir.rc_backups_dir()))
    );
    e.persister.revert(&applied).await.unwrap();
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn revert_preserves_a_user_append_while_removing_block_and_restoring_legacy() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    e.seed_legacy();
    let mut before = files(e.home());
    let applied = e.apply(set(VALUE)).await;
    let path = e.home().join(".bashrc");
    let appended = "alias mine='echo 用户'\n";
    let after = fs::read_to_string(&path).unwrap() + appended;
    write(&path, &after);
    before
        .get_mut(Path::new(".bashrc"))
        .unwrap()
        .extend_from_slice(appended.as_bytes());
    e.persister.revert(&applied).await.unwrap();
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn clear_and_restore_baseline_both_recover_original_bytes() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    e.seed_legacy();
    let before = files(e.home());
    let baseline = e.persister.inspect(VAR).await.unwrap();
    let snapshots = snapshots_from(&baseline);
    assert_eq!(snapshots.len(), baseline.persisted.len());
    for (snapshot, persisted) in snapshots.iter().zip(&baseline.persisted) {
        assert_eq!(snapshot.location, persisted.location);
        assert_eq!(snapshot.value, persisted.value);
    }
    e.apply(set(VALUE)).await;
    e.apply(EnvOp::Clear).await;
    assert_eq!(files(e.home()), before);
    assert!(e.plan(EnvOp::Clear).await.is_noop());
    e.apply(EnvOp::RestoreBaseline {
        snapshots,
        legacy_lines: baseline.legacy_lines,
    })
    .await;
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn restore_baseline_can_remove_live_blocks_without_an_intermediate_clear() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    e.seed_legacy();
    let before = files(e.home());
    let baseline = e.persister.inspect(VAR).await.unwrap();
    e.apply(set(VALUE)).await;
    e.apply(EnvOp::RestoreBaseline {
        snapshots: snapshots_from(&baseline),
        legacy_lines: baseline.legacy_lines,
    })
    .await;
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn foreign_lines_require_takeover_and_clear_restores_them() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let original = "# my endpoint\nexport CODEX_AUTHAPI_BASE_URL=https://example.com\n";
    write(&e.home().join(".bashrc"), original);
    let report = e.persister.inspect(VAR).await.unwrap();
    assert!(report.legacy_lines.is_empty());
    assert_eq!(
        report.foreign_lines,
        vec![RcLine {
            path: e.home().join(".bashrc"),
            line_no: 2,
            text: format!("export {VAR}=https://example.com"),
            value: "https://example.com".into(),
        }]
    );
    let keep = e.plan(set(VALUE)).await;
    assert!(
        keep.edits
            .iter()
            .all(|e| e.action == EnvEditAction::WriteBlock)
    );
    e.persister.apply(&keep).await.unwrap();
    assert_eq!(
        fs::read_to_string(e.home().join(".bashrc")).unwrap(),
        format!("{}\n{original}", shell_block(VALUE))
    );
    assert_eq!(
        e.persister.inspect(VAR).await.unwrap().foreign_lines.len(),
        1
    );
    let takeover = e
        .plan(EnvOp::Set {
            value: VALUE.into(),
            take_over_foreign: true,
        })
        .await;
    assert_eq!(takeover.edits.len(), 1);
    assert!(matches!(
        takeover.edits[0].action,
        EnvEditAction::CommentLegacyLine { .. }
    ));
    e.persister.apply(&takeover).await.unwrap();
    assert_eq!(
        fs::read_to_string(e.home().join(".bashrc")).unwrap(),
        format!("{}\n{}", shell_block(VALUE), commented(original))
    );
    assert!(
        e.persister
            .inspect(VAR)
            .await
            .unwrap()
            .foreign_lines
            .is_empty()
    );
    // Reconstruct the persister to verify that takeover bookkeeping is durable.
    let restarted =
        ShellEnvPersister::with_platform(e.ctx.clone(), EnvPlatform::Linux, e.native.clone());
    let clear = restarted.plan(VAR, EnvOp::Clear).await.unwrap();
    restarted.apply(&clear).await.unwrap();
    assert_eq!(
        files(e.home()),
        BTreeMap::from([(PathBuf::from(".bashrc"), original.as_bytes().to_vec())])
    );
}

#[tokio::test]
async fn fish_directory_enables_conf_d_even_with_bash_as_the_login_shell() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    fs::create_dir_all(e.home().join(".config/fish")).unwrap();
    write(
        &e.home().join(".config/fish/config.fish"),
        "# my fish config\n",
    );
    let before = files(e.home());
    e.apply(set(VALUE)).await;
    let path = e.home().join(".config/fish/conf.d/superba.fish");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        block(&format!("set -gx {VAR} \"{VALUE}\""))
    );
    let report = e.persister.inspect(VAR).await.unwrap();
    assert!(report.persisted.iter().any(|p| p.location
        == EnvLocation::ShellRc {
            path: path.clone(),
            shell: ShellKind::Fish
        }
        && p.value.as_deref() == Some(VALUE)
        && p.managed));
    assert!(e.plan(set(VALUE)).await.is_noop());
    e.apply(EnvOp::Clear).await;
    assert!(!path.exists());
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn absolute_xdg_config_home_and_zdotdir_are_used_for_writes_and_scans() {
    let tmp = tempfile::tempdir().unwrap();
    let xdg = tmp.path().join("xdg");
    let zdot = tmp.path().join("zdot");
    let env = MapEnv::new()
        .with("SHELL", "/usr/bin/fish")
        .with("XDG_CONFIG_HOME", &xdg)
        .with("ZDOTDIR", &zdot);
    let e = TestEnv::with(tmp, EnvPlatform::Linux, env, MemoryNativeEnv::new());
    write(&zdot.join(".zshrc"), ZSH);
    e.apply(set(VALUE)).await;
    assert_eq!(
        fs::read_to_string(zdot.join(".zshenv")).unwrap(),
        shell_block(VALUE)
    );
    assert_eq!(
        fs::read_to_string(zdot.join(".zshrc")).unwrap(),
        commented(ZSH)
    );
    assert_eq!(
        fs::read_to_string(xdg.join("fish/conf.d/superba.fish")).unwrap(),
        block(&format!("set -gx {VAR} \"{VALUE}\""))
    );
    assert_eq!(
        fs::read_to_string(xdg.join("environment.d/60-superba.conf")).unwrap(),
        block(&format!("{VAR}={VALUE}"))
    );
    assert!(!e.home().join(".config").exists());
    assert!(!e.home().join(".zshenv").exists());
    e.apply(EnvOp::Clear).await;
    assert!(!zdot.join(".zshenv").exists());
    assert!(!xdg.join("fish/conf.d/superba.fish").exists());
    assert!(!xdg.join("environment.d/60-superba.conf").exists());
}

#[tokio::test]
async fn macos_writes_zshenv_launchagent_and_memory_launchctl_then_clears_them() {
    let e = TestEnv::new(EnvPlatform::MacOs, "/bin/zsh");
    let plan = e.plan(set(VALUE)).await;
    assert_eq!(plan.edits.len(), 3);
    let native: Vec<_> = plan
        .edits
        .iter()
        .filter(|e| e.location == EnvLocation::Launchctl)
        .collect();
    assert_eq!(native.len(), 1);
    assert_eq!(
        native[0].action,
        EnvEditAction::SetValue {
            value: VALUE.into()
        }
    );
    assert_eq!(native[0].before, None);
    assert_eq!(native[0].after.as_deref(), Some(VALUE));
    assert_eq!(e.native.get(&EnvLocation::Launchctl, VAR).unwrap(), None);
    assert!(files(e.home()).is_empty());
    e.persister.apply(&plan).await.unwrap();
    let plist = e
        .home()
        .join("Library/LaunchAgents/com.krill-code.superba.env.codex-authapi-base-url.plist");
    let text = fs::read_to_string(&plist).unwrap();
    let strings: Vec<_> = text
        .split("<string>")
        .skip(1)
        .map(|s| s.split_once("</string>").unwrap().0)
        .collect();
    assert_eq!(
        strings,
        vec![
            "com.krill-code.superba.env.codex-authapi-base-url",
            "/bin/launchctl",
            "setenv",
            VAR,
            VALUE
        ]
    );
    assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
    assert_eq!(
        fs::read_to_string(e.home().join(".zshenv")).unwrap(),
        shell_block(VALUE)
    );
    assert_eq!(files(e.home()).len(), 2);
    assert_eq!(
        e.native
            .get(&EnvLocation::Launchctl, VAR)
            .unwrap()
            .as_deref(),
        Some(VALUE)
    );
    let report = e.persister.inspect(VAR).await.unwrap();
    assert_eq!(report.persisted.len(), 3);
    assert!(
        report
            .persisted
            .iter()
            .all(|p| p.managed && p.value.as_deref() == Some(VALUE))
    );
    assert!(e.plan(set(VALUE)).await.is_noop());
    e.apply(EnvOp::Clear).await;
    assert!(!plist.exists());
    assert!(files(e.home()).is_empty());
    assert_eq!(e.native.get(&EnvLocation::Launchctl, VAR).unwrap(), None);
}

#[tokio::test]
async fn windows_clear_restores_the_original_setx_value_after_multiple_sets() {
    let e = TestEnv::with(
        tempfile::tempdir().unwrap(),
        EnvPlatform::Windows,
        MapEnv::new().with("SHELL", "powershell.exe"),
        MemoryNativeEnv::new().with(&EnvLocation::WindowsUserEnv, VAR, OLD),
    );
    // Even existing shell files must be ignored on Windows.
    e.seed_legacy();
    let before = files(e.home());
    let report = e.persister.inspect(VAR).await.unwrap();
    assert!(report.legacy_lines.is_empty());
    assert!(!report.persisted[0].managed);
    let plan = e.plan(set(VALUE)).await;
    assert_eq!(plan.edits.len(), 1);
    let edit = &plan.edits[0];
    assert_eq!(edit.location, EnvLocation::WindowsUserEnv);
    assert_eq!(
        edit.action,
        EnvEditAction::SetValue {
            value: VALUE.into()
        }
    );
    assert_eq!(edit.before.as_deref(), Some(OLD));
    e.persister.apply(&plan).await.unwrap();
    assert_eq!(
        e.native
            .get(&EnvLocation::WindowsUserEnv, VAR)
            .unwrap()
            .as_deref(),
        Some(VALUE)
    );
    e.apply(set(NEXT)).await;
    let clear = e.plan(EnvOp::Clear).await;
    assert_eq!(clear.edits.len(), 1);
    assert_eq!(
        clear.edits[0].action,
        EnvEditAction::SetValue { value: OLD.into() }
    );
    assert_eq!(clear.edits[0].before.as_deref(), Some(NEXT));
    e.persister.apply(&clear).await.unwrap();
    assert_eq!(
        e.native
            .get(&EnvLocation::WindowsUserEnv, VAR)
            .unwrap()
            .as_deref(),
        Some(OLD)
    );
    assert_eq!(files(e.home()), before);
    assert!(e.plan(EnvOp::Clear).await.is_noop());
}

#[tokio::test]
async fn windows_clear_deletes_a_value_that_was_originally_absent() {
    let e = TestEnv::new(EnvPlatform::Windows, "powershell.exe");
    let applied = e.apply(set(VALUE)).await;
    assert_eq!(applied.edits.len(), 1);
    assert_eq!(applied.edits[0].location, EnvLocation::WindowsUserEnv);
    assert_eq!(applied.edits[0].before, None);
    assert!(applied.backups.is_empty());
    let clear = e.plan(EnvOp::Clear).await;
    assert_eq!(clear.edits.len(), 1);
    assert_eq!(clear.edits[0].location, EnvLocation::WindowsUserEnv);
    assert_eq!(clear.edits[0].action, EnvEditAction::DeleteValue);
    e.persister.apply(&clear).await.unwrap();
    assert_eq!(
        e.native.get(&EnvLocation::WindowsUserEnv, VAR).unwrap(),
        None
    );
    assert!(files(e.home()).is_empty());
}

#[tokio::test]
async fn stale_set_plan_rebases_on_the_current_file_and_revert_keeps_that_preimage() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    e.seed_legacy();
    let plan = e.plan(set(VALUE)).await;
    let changed = format!("# inserted before planning finished\n{BASH}alias fresh='echo fresh'\n");
    write(&e.home().join(".bashrc"), &changed);
    let before_apply = files(e.home());
    let applied = e.persister.apply(&plan).await.unwrap();
    assert_eq!(
        fs::read_to_string(e.home().join(".bashrc")).unwrap(),
        format!("{}\n{}", shell_block(VALUE), commented(&changed))
    );
    assert!(applied.edits.iter().any(|e| matches!(
        &e.action,
        EnvEditAction::CommentLegacyLine { line_no: 4, .. }
    )));
    e.persister.revert(&applied).await.unwrap();
    assert_eq!(files(e.home()), before_apply);
}

#[tokio::test]
async fn invalid_names_and_control_characters_are_rejected_without_writes() {
    for platform in [EnvPlatform::Linux, EnvPlatform::MacOs, EnvPlatform::Windows] {
        let e = TestEnv::new(platform, "/bin/bash");
        for var in [
            "", "1BAD", "BAD-NAME", "BAD=NAME", "BAD NAME", "A\nB", "变量",
        ] {
            for op in [set(VALUE), EnvOp::Clear] {
                assert!(
                    matches!(e.persister.plan(var, op).await, Err(Error::Invalid { .. })),
                    "{platform:?}: {var:?}"
                );
            }
            assert!(matches!(
                e.persister.plan_comment_out(var).await,
                Err(Error::Invalid { .. })
            ));
        }
        for value in [
            "",
            "   ",
            "first\nsecond",
            "first\rsecond",
            "nul\0value",
            "tab\tvalue",
        ] {
            assert!(
                matches!(
                    e.persister.plan(VAR, set(value)).await,
                    Err(Error::Invalid { .. })
                ),
                "{platform:?}: {value:?}"
            );
        }
        assert!(files(e.tmp.path()).is_empty());
        assert_eq!(e.native.get(&EnvLocation::Launchctl, VAR).unwrap(), None);
        assert_eq!(
            e.native.get(&EnvLocation::WindowsUserEnv, VAR).unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn comment_out_covers_all_scanned_shell_files_and_round_trips() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let posix = "# keep this comment\nexport OPENAI_BASE_URL=https://one.example\nexport OPENAI_BASE_URL='https://two.example'\n# export OPENAI_BASE_URL=https://already-commented.example\nexport OPENAI_BASE_URL_SUFFIX=unrelated\n";
    for path in [
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".profile",
        ".zshenv",
        ".zprofile",
        ".zshrc",
        ".zlogin",
    ] {
        write(&e.home().join(path), posix);
    }
    for path in [".config/fish/config.fish", ".config/fish/conf.d/user.fish"] {
        write(
            &e.home().join(path),
            "set -gx OPENAI_BASE_URL https://one.example\nset -x OPENAI_BASE_URL https://two.example\n",
        );
    }
    let before = files(e.home());
    let plan = e
        .persister
        .plan_comment_out("OPENAI_BASE_URL")
        .await
        .unwrap();
    assert_eq!(files(e.home()), before);
    assert_eq!(plan.edits.len(), 20);
    assert!(
        plan.edits
            .iter()
            .all(|e| matches!(e.action, EnvEditAction::CommentLegacyLine { .. }))
    );
    let applied = e.persister.apply(&plan).await.unwrap();
    for (path, bytes) in &before {
        let expected = String::from_utf8(bytes.clone())
            .unwrap()
            .split_inclusive('\n')
            .map(|line| {
                if line.starts_with("export OPENAI_BASE_URL=")
                    || line.starts_with("set -gx OPENAI_BASE_URL ")
                    || line.starts_with("set -x OPENAI_BASE_URL ")
                {
                    format!("# [superba] {line}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<String>();
        assert_eq!(fs::read(e.home().join(path)).unwrap(), expected.as_bytes());
    }
    assert!(
        e.persister
            .inspect("OPENAI_BASE_URL")
            .await
            .unwrap()
            .foreign_lines
            .is_empty()
    );
    e.persister.revert(&applied).await.unwrap();
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn crlf_survives_takeover_update_clear_and_restore() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    write(&e.home().join(".bashrc"), &BASH.replace('\n', "\r\n"));
    write(&e.home().join(".zshrc"), &ZSH.replace('\n', "\r\n"));
    write(
        &e.home().join(".profile"),
        "# login\r\nexport OTHER=yes\r\n",
    );
    let before = files(e.home());
    let baseline = e.persister.inspect(VAR).await.unwrap();
    for value in [VALUE, NEXT] {
        e.apply(set(value)).await;
        assert_eq!(
            fs::read_to_string(e.home().join(".bashrc")).unwrap(),
            format!("{}\n{}", shell_block(value), commented(BASH)).replace('\n', "\r\n")
        );
        assert_eq!(
            fs::read_to_string(e.home().join(".profile")).unwrap(),
            format!("# login\nexport OTHER=yes\n\n{}", shell_block(value)).replace('\n', "\r\n")
        );
        for path in before.keys() {
            let text = fs::read_to_string(e.home().join(path)).unwrap();
            assert!(!text.replace("\r\n", "").contains(['\r', '\n']));
        }
    }
    e.apply(EnvOp::Clear).await;
    assert_eq!(
        fs::read_to_string(e.home().join(".bashrc")).unwrap(),
        BASH.replace('\n', "\r\n")
    );
    e.apply(EnvOp::RestoreBaseline {
        snapshots: snapshots_from(&baseline),
        legacy_lines: baseline.legacy_lines,
    })
    .await;
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn restore_baseline_uncomments_every_duplicate_legacy_assignment() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let original = format!("export {VAR}={OLD}\n# separator\nexport {VAR}={OLD}\n");
    write(&e.home().join(".bashrc"), &original);
    let baseline = e.persister.inspect(VAR).await.unwrap();
    assert_eq!(baseline.legacy_lines.len(), 2);
    e.apply(set(VALUE)).await;
    e.apply(EnvOp::RestoreBaseline {
        snapshots: snapshots_from(&baseline),
        legacy_lines: baseline.legacy_lines,
    })
    .await;
    assert_eq!(
        fs::read_to_string(e.home().join(".bashrc")).unwrap(),
        original
    );
}

#[tokio::test]
async fn clear_uncomments_every_duplicate_foreign_assignment() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let original = format!(
        "export {VAR}=https://example.com\n# separator\nexport {VAR}=https://example.com\n"
    );
    write(&e.home().join(".bashrc"), &original);
    e.apply(EnvOp::Set {
        value: VALUE.into(),
        take_over_foreign: true,
    })
    .await;
    e.apply(EnvOp::Clear).await;
    assert_eq!(
        fs::read_to_string(e.home().join(".bashrc")).unwrap(),
        original
    );
}

#[tokio::test]
async fn stale_comment_out_plan_still_comments_assignments_and_preserves_new_content() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let path = e.home().join(".bashrc");
    write(&path, "export OPENAI_BASE_URL=https://one.example\n");
    let plan = e
        .persister
        .plan_comment_out("OPENAI_BASE_URL")
        .await
        .unwrap();
    let changed = "# inserted\nexport OPENAI_BASE_URL=https://one.example\nexport OPENAI_BASE_URL=https://two.example\n";
    write(&path, changed);
    let applied = e.persister.apply(&plan).await.unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        changed.replace(
            "export OPENAI_BASE_URL=",
            "# [superba] export OPENAI_BASE_URL="
        )
    );
    e.persister.revert(&applied).await.unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
}

#[tokio::test]
async fn long_home_paths_keep_distinct_backups_for_byte_exact_revert() {
    let tmp = tempfile::Builder::new()
        .prefix(&"long-test-home-".repeat(7))
        .tempdir()
        .unwrap();
    let e = TestEnv::with(
        tmp,
        EnvPlatform::Linux,
        MapEnv::new().with("SHELL", "/bin/bash"),
        MemoryNativeEnv::new(),
    );
    e.seed_legacy();
    let before = files(e.home());
    let applied = e.apply(set(VALUE)).await;
    assert_eq!(applied.backups.iter().collect::<BTreeSet<_>>().len(), 2);
    e.persister.revert(&applied).await.unwrap();
    assert_eq!(files(e.home()), before);
}

#[tokio::test]
async fn later_legacy_lines_are_restored_by_clear_and_baseline_and_tracked_after_revert() {
    for clear in [false, true] {
        let e = TestEnv::new(EnvPlatform::Linux, "/bin/zsh");
        let baseline = e.persister.inspect(VAR).await.unwrap();
        assert!(baseline.legacy_lines.is_empty());
        e.apply(set(VALUE)).await;
        let path = e.home().join(".zshrc");
        let original = format!("# Added after takeover\nexport {VAR}=\"{OLD}\"\n");
        write(&path, &original);
        let switched = e.apply(set(NEXT)).await;
        assert_eq!(fs::read_to_string(&path).unwrap(), commented(&original));
        let op = if clear {
            EnvOp::Clear
        } else {
            EnvOp::RestoreBaseline {
                snapshots: snapshots_from(&baseline),
                legacy_lines: baseline.legacy_lines.clone(),
            }
        };
        // State persists across persister instances.
        let restarted =
            ShellEnvPersister::with_platform(e.ctx.clone(), EnvPlatform::Linux, e.native.clone());
        let plan = restarted.plan(VAR, op.clone()).await.unwrap();
        assert_eq!(
            plan.edits
                .iter()
                .filter(|edit| matches!(edit.action, EnvEditAction::UncommentLegacyLine { .. }))
                .count(),
            1
        );
        let restored = restarted.apply(&plan).await.unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(restarted.plan(VAR, op.clone()).await.unwrap().is_noop());
        // Undoing a restore must re-register the comment so Clear can undo it.
        restarted.revert(&restored).await.unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), commented(&original));
        let cleared = e.apply(EnvOp::Clear).await;
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        // Undoing the switch itself must not retain a stale comment record.
        e.persister.revert(&cleared).await.unwrap();
        e.persister.revert(&switched).await.unwrap();
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(e.ctx.data_dir.root().join("env-state.json")).unwrap(),
        )
        .unwrap();
        // No comment record is left; only the value of Superba's own block,
        // which the first switch wrote and nothing reverted (GH#5 detection).
        let var = &state["vars"][VAR];
        assert!(
            var.get("commented_legacy")
                .is_none_or(|v| v == &serde_json::json!([]))
        );
        assert!(
            var.get("commented_foreign")
                .is_none_or(|v| v == &serde_json::json!([]))
        );
        assert_eq!(var["written"], VALUE);
    }
}

#[tokio::test]
async fn restore_deduplicates_legacy_records_even_when_baseline_line_numbers_shifted() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let original = format!("export {VAR}={OLD}\nexport {VAR}={OLD}\n");
    let path = e.home().join(".bashrc");
    write(&path, &original);
    let mut baseline = e.persister.inspect(VAR).await.unwrap();
    e.apply(set(VALUE)).await;
    for line in &mut baseline.legacy_lines {
        line.line_no += 100;
    }
    let plan = e
        .plan(EnvOp::RestoreBaseline {
            snapshots: snapshots_from(&baseline),
            legacy_lines: baseline.legacy_lines,
        })
        .await;
    assert_eq!(
        plan.edits
            .iter()
            .filter(|edit| matches!(edit.action, EnvEditAction::UncommentLegacyLine { .. }))
            .count(),
        2
    );
    e.persister.apply(&plan).await.unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(e.plan(EnvOp::Clear).await.is_noop());
}

#[tokio::test]
async fn non_utf8_rc_files_are_skipped_with_a_notice_and_keep_every_byte() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let original = b"# caf\xe9\nexport OTHER=yes\n";
    // .bashrc is also a write location; .bash_login is scanned only.
    for name in [".bashrc", ".bash_login"] {
        fs::write(e.home().join(name), original).unwrap();
    }
    let baseline = e.persister.inspect(VAR).await.unwrap();
    let before = files(e.home());
    for op in [
        set(VALUE),
        EnvOp::RestoreBaseline {
            snapshots: snapshots_from(&baseline),
            legacy_lines: baseline.legacy_lines,
        },
        EnvOp::Clear,
    ] {
        let plan = e.plan(op).await;
        if matches!(plan.op, EnvOp::Set { .. }) {
            assert!(!plan.edits.is_empty());
        }
        assert!(
            plan.scope_notes
                .iter()
                .any(|note| note.key == "env.scope.non_utf8_skipped")
        );
        for edit in &plan.edits {
            if let EnvLocation::ShellRc { path, .. } = &edit.location {
                assert!(![e.home().join(".bashrc"), e.home().join(".bash_login")].contains(path));
            }
        }
        e.persister.apply(&plan).await.unwrap();
        for (path, bytes) in &before {
            assert_eq!(fs::read(e.home().join(path)).unwrap(), *bytes);
        }
    }
    assert_eq!(files(e.home()), before);
    let diagnostic = e.persister.plan_comment_out("OTHER").await.unwrap();
    assert!(diagnostic.edits.is_empty());
    assert_eq!(
        diagnostic
            .scope_notes
            .iter()
            .filter(|note| note.key == "env.scope.non_utf8_skipped")
            .count(),
        2
    );
}

#[tokio::test]
async fn stale_plan_does_not_overwrite_a_file_that_became_non_utf8() {
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/bash");
    let path = e.home().join(".bashrc");
    write(&path, "# valid\n");
    let plan = e.plan(set(VALUE)).await;
    let changed = b"# caf\xe9\n";
    fs::write(&path, changed).unwrap();
    e.persister.apply(&plan).await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), changed);
    assert!(
        fs::read_to_string(e.home().join(".profile"))
            .unwrap()
            .contains(VALUE)
    );
}

/// GH#5: a value someone else put where Superba had set its own is reported
/// (`superba_value`) and left alone by a clear or a restore, with a note.
#[tokio::test]
async fn clear_and_restore_leave_values_changed_since_superba_set_them() {
    // Windows: the user's environment store.
    let tmp = tempfile::tempdir().unwrap();
    let e = TestEnv::with(
        tmp,
        EnvPlatform::Windows,
        MapEnv::new(),
        MemoryNativeEnv::new().with(&EnvLocation::WindowsUserEnv, VAR, OLD),
    );
    e.apply(set(VALUE)).await;
    let report = e.persister.inspect(VAR).await.unwrap();
    assert_eq!(report.superba_value.as_deref(), Some(VALUE));
    // Another program sets its own value.
    e.native
        .set(
            &EnvLocation::WindowsUserEnv,
            VAR,
            "https://other.example.org",
        )
        .unwrap();
    let plan = e.plan(EnvOp::Clear).await;
    assert!(plan.is_noop(), "{:?}", plan.edits);
    assert!(
        plan.scope_notes
            .iter()
            .any(|n| n.key == "env.scope.kept_changed"),
        "{:?}",
        plan.scope_notes
    );
    e.persister.apply(&plan).await.unwrap();
    assert_eq!(
        e.native
            .get(&EnvLocation::WindowsUserEnv, VAR)
            .unwrap()
            .as_deref(),
        Some("https://other.example.org")
    );

    // Linux: a value edited inside Superba's own block stays too; an untouched
    // block is removed.
    let e = TestEnv::new(EnvPlatform::Linux, "/bin/zsh");
    e.apply(set(VALUE)).await;
    let zshenv = e.home().join(".zshenv");
    let edited = fs::read_to_string(&zshenv).unwrap().replace(VALUE, NEXT);
    write(&zshenv, &edited);
    let restore = EnvOp::RestoreBaseline {
        snapshots: Vec::new(),
        legacy_lines: Vec::new(),
    };
    let plan = e.plan(restore).await;
    assert!(
        plan.edits.iter().all(|edit| !matches!(
            &edit.location,
            EnvLocation::ShellRc { path, .. } if path == &zshenv
        )),
        "{:?}",
        plan.edits
    );
    e.persister.apply(&plan).await.unwrap();
    assert_eq!(fs::read_to_string(&zshenv).unwrap(), edited);
    let envd = e.home().join(".config/environment.d/60-superba.conf");
    assert!(!fs::read_to_string(&envd).is_ok_and(|t| t.contains(VALUE)));
}
