//! v0.3 host operations of [`SshRemoteHosts`]: scoped plans, Codex installs and
//! rollbacks, read-only session scans.

use std::path::Path;

use sha2::{Digest, Sha256};
use superba_core::{
    CodexVersion, Error, RemoteApplyScope, RemoteHost, RemoteInstallReport, RemoteOs, RemotePlan,
    ResolvedProfile, Result, SessionReport, codex_package_target,
};
use tokio::io::AsyncReadExt;

use crate::SshRemoteHosts;
use crate::install::{self, InstallSpec};
use crate::probe::Snapshot;
use crate::records::Records;
use crate::script::new_nonce;
use crate::sessions;

/// sha256 (hex) and size of a local file, read in chunks.
pub(crate) async fn file_digest(path: &Path) -> Result<(String, u64)> {
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| Error::io(path, &e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0_u8; 1 << 20];
    let mut size = 0_u64;
    loop {
        let n = f.read(&mut buf).await.map_err(|e| Error::io(path, &e))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        h.update(&buf[..n]);
    }
    Ok((hex::encode(h.finalize()), size))
}

/// The package target of a probed host (`uname -s` / `uname -m`).
pub(crate) fn host_target(snap: &Snapshot, host: &RemoteHost) -> Result<&'static str> {
    let os = match &snap.state.os {
        RemoteOs::Linux => "linux",
        RemoteOs::Darwin => "darwin",
        RemoteOs::Other(name) => {
            return Err(Error::Unsupported {
                what: format!("installing Codex on {} ({name})", host.label),
            });
        }
    };
    let arch = snap.state.arch.as_deref().unwrap_or_default();
    codex_package_target(os, arch).ok_or_else(|| Error::Unsupported {
        what: format!(
            "installing Codex on {} (architecture {arch:?} has no Codex package)",
            host.label
        ),
    })
}

impl SshRemoteHosts {
    /// [`superba_core::RemoteHosts::plan_apply_scoped`].
    pub(crate) async fn plan_scoped(
        &self,
        host: &RemoteHost,
        desired: &ResolvedProfile,
        scope: RemoteApplyScope,
    ) -> Result<RemotePlan> {
        self.plan_in_scope(host, desired, scope).await
    }

    /// The probed layout (Codex home, `codex` directory) of a host.
    async fn layout(&self, host: &RemoteHost) -> Result<(Snapshot, String)> {
        let snap = self.snapshot(host, true).await?;
        let bin_dir = snap
            .install_dir
            .clone()
            .unwrap_or_else(|| crate::util::rjoin(&snap.home, ".local/bin"));
        Ok((snap, bin_dir))
    }

    /// [`superba_core::RemoteHosts::install_codex`].
    pub(crate) async fn install(
        &self,
        host: &RemoteHost,
        package: &Path,
        version: &CodexVersion,
    ) -> Result<RemoteInstallReport> {
        let (snap, bin_dir) = self.layout(host).await?;
        let target = host_target(&snap, host)?;
        let (sha256, size) = file_digest(package).await?;
        let need = install::required_kib(size);
        if let Some(free) = snap.state.disk_free_kib
            && free < need
        {
            return Err(Error::Unsupported {
                what: format!(
                    "installing Codex on {}: {free} KiB free, about {need} KiB needed",
                    host.label
                ),
            });
        }
        let spec = InstallSpec {
            codex_home: snap.state.codex_home.clone(),
            bin_dir,
            version: version.clone(),
            target: target.to_owned(),
            sha256,
            size,
        };
        let nonce = new_nonce();
        let script = install::install_script(&nonce, &spec);
        tracing::info!(host = %host.label, %version, target, size, "installing Codex on a remote host");
        crate::ssh::require_ssh(host)?;
        let out = self
            .shell
            .run_with_payload(host, &script, package, self.timeouts.install)
            .await?;
        let recs = Records::parse(&out.stdout, &nonce)?;
        for removed in recs.all("removed") {
            tracing::info!(host = %host.label, release = removed, "old Superba release removed");
        }
        let outcome = install::parse_outcome(&recs, &host.label)?;
        Ok(RemoteInstallReport {
            target_id: host.target_id.clone(),
            version: version.clone(),
            executable: outcome.visible,
            previous: outcome.previous,
            at: self.clock.now(),
        })
    }

    /// [`superba_core::RemoteHosts::rollback_codex`].
    pub(crate) async fn rollback_install(&self, host: &RemoteHost) -> Result<RemoteInstallReport> {
        let (snap, bin_dir) = self.layout(host).await?;
        let nonce = new_nonce();
        let script = install::rollback_script(&nonce, &snap.state.codex_home, &bin_dir);
        let recs = self.run(host, &script, &nonce, self.timeouts.write).await?;
        let outcome = install::parse_outcome(&recs, &host.label)?;
        let version = outcome.version.ok_or_else(|| Error::Ssh {
            host: host.label.clone(),
            message: "the restored codex did not report its version".into(),
        })?;
        Ok(RemoteInstallReport {
            target_id: host.target_id.clone(),
            version,
            executable: outcome.visible,
            previous: outcome.previous,
            at: self.clock.now(),
        })
    }

    /// [`superba_core::RemoteHosts::scan_sessions`].
    pub(crate) async fn scan(&self, host: &RemoteHost) -> Result<SessionReport> {
        let st = self.store(host).load_state().await?;
        let codex_home = match st.codex_home {
            Some(ch) => ch,
            None => self.snapshot(host, false).await?.state.codex_home,
        };
        let nonce = new_nonce();
        let script = sessions::scan_script(&nonce, Some(&codex_home));
        let recs = self.run(host, &script, &nonce, self.timeouts.scan).await?;
        sessions::parse_report(&recs, self.clock.now())
    }
}
