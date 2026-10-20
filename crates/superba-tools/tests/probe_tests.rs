//! A transient failure must not hide the version of an unchanged executable.

#![cfg(unix)]

mod support;

use std::sync::Arc;

use superba_core::NetworkSettings;
use superba_tools::{NoHost, ToolManager, probe_version};

#[tokio::test]
async fn failed_probes_can_recover_without_replacing_the_program() {
    let root = tempfile::tempdir().unwrap();
    let ctx = support::context(root.path(), &[]);
    let tm = ToolManager::new(&ctx, NetworkSettings::default(), Arc::new(NoHost));
    let exe = root.path().join("tool");
    support::script(
        &exe,
        r#"echo attempt >> "$HOME/attempts"
if [ ! -f "$HOME/failed" ]; then
    echo failed > "$HOME/failed"
    echo '1.3.0'; echo 'startup failed' >&2; exit 7
fi
if [ ! -f "$HOME/ready" ]; then
    echo ready > "$HOME/ready"
    echo 'still starting'; exit 0
fi
echo '1.3.0'"#,
    );
    assert_eq!(probe_version(tm.ctx(), &exe, &["--version"]).await, None);
    assert_eq!(probe_version(tm.ctx(), &exe, &["--version"]).await, None);
    assert_eq!(
        probe_version(tm.ctx(), &exe, &["--version"])
            .await
            .as_deref(),
        Some("1.3.0")
    );
    assert_eq!(
        probe_version(tm.ctx(), &exe, &["--version"])
            .await
            .as_deref(),
        Some("1.3.0")
    );
    assert_eq!(
        std::fs::read_to_string(ctx.home_dir.join("attempts")).unwrap(),
        "attempt\nattempt\nattempt\n"
    );
}
