//! Codex-only WSL trust, transported on stdin and journaled with the remote
//! config. No private key or system trust-store operation crosses the boundary.

use std::collections::BTreeSet;

use superba_core::{Error, FileChange, RemoteCertificatePlan, RemoteHost, Result, SecretBytes};
use tokio_rustls::rustls::{
    RootCertStore,
    pki_types::{CertificateDer, pem::PemObject},
};

use crate::{
    SshRemoteHosts,
    envrc::{CERT_ENV, restore_legacy},
    probe::Snapshot,
    records::FileRead,
    script,
    store::HostState,
    util::{rjoin, sha256_hex},
};

pub(crate) fn bundle_path(snap: &Snapshot) -> String {
    rjoin(&snap.state.codex_home, ".superba/router-ca.pem")
}

fn append(bundle: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    if bytes.len() > script::MAX_READ_BYTES as usize
        || bytes
            .windows(b"PRIVATE KEY".len())
            .any(|w| w == b"PRIVATE KEY")
    {
        return Err(Error::invalid("invalid or oversized Codex CA bundle"));
    }
    let certs = CertificateDer::pem_slice_iter(bytes)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::invalid("invalid Codex CA PEM"))?;
    if certs.is_empty() {
        return Err(Error::invalid("Codex CA bundle contains no certificates"));
    }
    let mut roots = RootCertStore::empty();
    for cert in certs {
        roots
            .add(cert)
            .map_err(|_| Error::invalid("invalid Codex CA certificate"))?;
    }
    if bundle.windows(bytes.len()).any(|part| part == bytes) {
        return Ok(());
    }
    if !bundle.is_empty() && bytes.windows(bundle.len()).any(|part| part == bundle) {
        *bundle = bytes.to_vec();
    } else {
        if !bundle.is_empty() {
            bundle.push(b'\n');
        }
        bundle.extend_from_slice(bytes);
    }
    if bundle.len() > script::MAX_READ_BYTES as usize {
        return Err(Error::invalid("combined Codex CA bundle is too large"));
    }
    Ok(())
}

fn source_path(value: &str, snap: &Snapshot) -> Result<String> {
    let path = ["${HOME}/", "$HOME/", "~/"]
        .into_iter()
        .find_map(|prefix| value.strip_prefix(prefix).map(|p| rjoin(&snap.home, p)))
        .unwrap_or_else(|| value.to_owned());
    if !path.starts_with('/') || path.chars().any(char::is_control) {
        return Err(Error::invalid(
            "Codex CA path must resolve to an absolute POSIX path",
        ));
    }
    Ok(path)
}

impl SshRemoteHosts {
    pub(crate) async fn plan_certificate(
        &self,
        host: &RemoteHost,
        snap: &Snapshot,
        state: &HostState,
        certificate: Option<&SecretBytes>,
    ) -> Result<Option<RemoteCertificatePlan>> {
        let path = bundle_path(snap);
        let before = snap.router_ca.bytes();
        let before_sha256 = before.map(|b| sha256_hex(b.expose()));
        if certificate.is_none() && state.router_ca_sha256.is_none() {
            return Ok(None);
        }
        if snap.router_ca.is_unavailable()
            || (before_sha256.is_some() && before_sha256 != state.router_ca_sha256)
        {
            return Err(Error::Conflict {
                message: "WSL Codex CA bundle was changed outside Superba; left untouched".into(),
            });
        }
        let change = if let Some(certificate) = certificate {
            let mut bundle = Vec::new();
            append(&mut bundle, certificate.expose())?;
            // Repeated switches keep the user's previously incorporated roots.
            if let Some(before) = before {
                append(&mut bundle, before.expose())?;
            }
            if let Some(source) = &snap.ca_path
                && source != &path
            {
                let bytes = snap.ca_source.bytes().ok_or_else(|| {
                    Error::invalid(
                        "cannot read the existing WSL Codex CA bundle; trust was not changed",
                    )
                })?;
                append(&mut bundle, bytes.expose())?;
            }
            // Keep roots used by other login shells too, including exports we
            // neutralised on a previous apply. Never evaluate shell text here.
            let mut sources = BTreeSet::new();
            for rc in &snap.rc {
                let text = String::from_utf8_lossy(rc.content.as_deref().unwrap_or_default());
                for line in text.lines() {
                    let restored = restore_legacy(line);
                    if let Some(value) = superba_core::env::export_value(
                        restored.as_deref().unwrap_or(line),
                        CERT_ENV,
                    ) {
                        let source = source_path(&value, snap)?;
                        if source != path && snap.ca_path.as_ref() != Some(&source) {
                            sources.insert(source);
                        }
                    }
                }
            }
            if !sources.is_empty() {
                let nonce = script::new_nonce();
                let sources: Vec<_> = sources.into_iter().collect();
                let recs = self
                    .run(
                        host,
                        &script::read_script(&nonce, &sources),
                        &nonce,
                        self.timeouts.write,
                    )
                    .await?;
                recs.require_done("read")?;
                for n in 0..sources.len() {
                    let bytes = recs
                        .file(&format!("f.{n}"))
                        .and_then(FileRead::bytes)
                        .ok_or_else(|| Error::invalid("cannot read a user WSL Codex CA bundle"))?;
                    append(&mut bundle, bytes.expose())?;
                }
            }
            if before.is_some_and(|b| b.expose() == bundle) {
                return Ok(None);
            }
            FileChange::Write {
                sha256: sha256_hex(&bundle),
                contents: SecretBytes::new(bundle),
            }
        } else {
            if before.is_none() {
                return Ok(None);
            }
            FileChange::Remove
        };
        Ok(Some(RemoteCertificatePlan {
            path,
            before_sha256,
            change,
        }))
    }
}
