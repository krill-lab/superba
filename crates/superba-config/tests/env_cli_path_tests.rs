//! v0.3 `CODEX_CLI_PATH` persistence (Updates page "persist" checkbox): the same
//! marker blocks as `CODEX_AUTHAPI_BASE_URL`, each variable managed on its own.

use std::fs;
use std::sync::Arc;

use superba_config::{EnvPlatform, MemoryNativeEnv, ShellEnvPersister};
use superba_core::{
    DataDir, EnvOp, EnvPersister, LocalContext, MapEnv,
    codex::{ENV_AUTHAPI_BASE_URL, ENV_CODEX_CLI_PATH},
};

fn persister(platform: EnvPlatform) -> (tempfile::TempDir, LocalContext, ShellEnvPersister) {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = LocalContext::new(
        tmp.path().join("home"),
        DataDir::new(tmp.path().join("data")),
        Arc::new(MapEnv::new().with("SHELL", "/bin/bash")),
    );
    fs::create_dir_all(&ctx.home_dir).unwrap();
    let p =
        ShellEnvPersister::with_platform(ctx.clone(), platform, Arc::new(MemoryNativeEnv::new()));
    (tmp, ctx, p)
}

async fn set(p: &ShellEnvPersister, var: &str, value: &str) {
    let plan = p
        .plan(
            var,
            EnvOp::Set {
                value: value.into(),
                take_over_foreign: false,
            },
        )
        .await
        .unwrap();
    p.apply(&plan).await.unwrap();
}

#[tokio::test]
async fn cli_path_shares_the_block_and_clears_alone() {
    for platform in [EnvPlatform::Linux, EnvPlatform::MacOs] {
        let (_tmp, ctx, p) = persister(platform);
        let bashrc = ctx.home_dir.join(".bashrc");
        fs::write(&bashrc, "# mine\nalias ll='ls -l'\n").unwrap();
        set(&p, ENV_AUTHAPI_BASE_URL, "https://api.krill-code.net").await;
        set(
            &p,
            ENV_CODEX_CLI_PATH,
            "/home/u/.local/share/superba/codex/0.162.0-x86_64-unknown-linux-musl/bin/codex",
        )
        .await;
        let text = fs::read_to_string(&bashrc).unwrap();
        assert!(text.contains("export CODEX_AUTHAPI_BASE_URL="), "{text}");
        assert!(text.contains("export CODEX_CLI_PATH="), "{text}");
        assert!(text.contains("alias ll='ls -l'"));
        let report = p.inspect(ENV_CODEX_CLI_PATH).await.unwrap();
        assert!(report.persisted.iter().any(|v| v.managed));

        let plan = p.plan(ENV_CODEX_CLI_PATH, EnvOp::Clear).await.unwrap();
        p.apply(&plan).await.unwrap();
        let text = fs::read_to_string(&bashrc).unwrap();
        assert!(!text.contains("CODEX_CLI_PATH"), "{text}");
        assert!(text.contains("export CODEX_AUTHAPI_BASE_URL="), "{text}");
        if platform == EnvPlatform::MacOs {
            let agents = ctx.home_dir.join("Library/LaunchAgents");
            let names: Vec<String> = fs::read_dir(&agents)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(names.len(), 1, "{names:?}");
        }
    }
}
