//! Per-install loopback TLS. Only Codex trusts this certificate, through its
//! additive `CODEX_CA_CERTIFICATE` bundle; no system trust store is changed.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use superba_core::{Error, Result};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{
    ServerConfig,
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use zeroize::Zeroizing;

use crate::config::write_private;
use crate::paths::RouterPaths;

/// Variable understood by Codex's HTTP and WebSocket transports.
pub const CERT_ENV: &str = "CODEX_CA_CERTIFICATE";

#[derive(Serialize, Deserialize)]
struct Identity {
    certificate: String,
    key: String,
}

impl Drop for Identity {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
    }
}

/// Ensure a stable identity exists, then load it for the loopback listener.
/// The certificate and key are published together, before the public PEM.
pub(crate) fn acceptor(paths: &RouterPaths) -> Result<TlsAcceptor> {
    identity_acceptor(paths, None)
}

/// A separate stable identity for each WSL NAT listener. Its SAN is the actual
/// adapter address; changing adapters does not invalidate the loopback identity.
pub(crate) fn wsl_acceptor(
    paths: &RouterPaths,
    address: std::net::Ipv4Addr,
) -> Result<TlsAcceptor> {
    if !crate::config::wsl_listen_allowed(address) {
        return Err(Error::invalid("invalid WSL TLS listener address"));
    }
    identity_acceptor(paths, Some(address))
}

/// The public certificate of a running router listener, for distribution to
/// WSL. Only the listener creates identities, avoiding concurrent key creation.
pub fn wsl_certificate(paths: &RouterPaths, address: std::net::Ipv4Addr) -> Result<Vec<u8>> {
    let path = if address.is_loopback() {
        paths.certificate_pem()
    } else if crate::config::wsl_listen_allowed(address) {
        paths.dir().join(format!("certificate-wsl-{address}.pem"))
    } else {
        return Err(Error::invalid("invalid WSL TLS listener address"));
    };
    std::fs::read(&path).map_err(|e| Error::io(&path, &e))
}

fn identity_acceptor(
    paths: &RouterPaths,
    address: Option<std::net::Ipv4Addr>,
) -> Result<TlsAcceptor> {
    let (file, public, names) = match address {
        Some(address) => (
            paths.dir().join(format!("tls-wsl-{address}.json")),
            paths.dir().join(format!("certificate-wsl-{address}.pem")),
            vec![address.to_string()],
        ),
        None => (
            paths.dir().join("tls-identity.json"),
            paths.certificate_pem(),
            vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
        ),
    };
    let identity: Identity = match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&Zeroizing::new(bytes))
            .map_err(|_| Error::invalid("invalid router TLS identity"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let certified = rcgen::generate_simple_self_signed(names)
                .map_err(|_| Error::other("cannot create router TLS identity"))?;
            let identity = Identity {
                certificate: certified.cert.pem(),
                key: certified.signing_key.serialize_pem(),
            };
            let bytes = Zeroizing::new(
                serde_json::to_vec(&identity)
                    .map_err(|_| Error::other("cannot serialize router TLS identity"))?,
            );
            write_private(&file, &bytes)?;
            identity
        }
        Err(e) => return Err(Error::io(&file, &e)),
    };
    let certificate = CertificateDer::from_pem_slice(identity.certificate.as_bytes())
        .map_err(|_| Error::invalid("invalid router TLS certificate"))?;
    let key = PrivateKeyDer::from_pem_slice(identity.key.as_bytes())
        .map_err(|_| Error::invalid("invalid router TLS private key"))?;
    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::other("cannot configure router TLS"))?
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .map_err(|_| Error::invalid("router TLS certificate does not match its private key"))?;
    if std::fs::read(&public).ok().as_deref() != Some(identity.certificate.as_bytes()) {
        write_private(&public, identity.certificate.as_bytes())?;
    }
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Prepare Codex's additive trust bundle, preserving any custom CA files the
/// user already selected. `extend` retains roots from the active router bundle.
/// Files are private and this never changes the OS trust store.
pub fn prepare_client_bundle(
    paths: &RouterPaths,
    extra: &[std::path::PathBuf],
    extend: bool,
) -> Result<()> {
    // Normally the service already created this. Also repair a removed public
    // certificate from the persisted identity before launching a new client.
    let _ = acceptor(paths)?;
    let public = paths.certificate_pem();
    let target = paths.client_certificate_pem();
    let mut bundle = std::fs::read(&public).map_err(|e| Error::io(&public, &e))?;
    let mut sources = extra.to_vec();
    if extend && target.exists() {
        sources.push(target.clone());
    }
    sources.sort();
    sources.dedup();
    for path in sources {
        if path == public || (path == target && !extend) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, &e))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(Error::invalid("custom Codex CA bundle exceeds 8 MiB"));
        }
        let certificates = CertificateDer::pem_slice_iter(&bytes)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::invalid("invalid custom Codex CA bundle"))?;
        if certificates.is_empty() {
            return Err(Error::invalid(
                "custom Codex CA bundle contains no certificates",
            ));
        }
        // Avoid growing the bundle on every launch or router-member change.
        if bytes == bundle || bundle.windows(bytes.len()).any(|part| part == bytes) {
            continue;
        }
        // Existing bundles include our own root; retain the previous bundle as
        // the base when it already contains the current root.
        if bytes.windows(bundle.len()).any(|part| part == bundle) {
            bundle = bytes;
        } else {
            bundle.push(b'\n');
            bundle.extend_from_slice(&bytes);
        }
    }
    if std::fs::read(&target).ok().as_deref() != Some(bundle.as_slice()) {
        write_private(&target, &bundle)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::{
        TlsConnector,
        rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
    };

    async fn connect(acceptor: TlsAcceptor, pem: &[u8], name: &'static str) -> bool {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            acceptor.accept(stream).await.is_ok()
        });
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(pem).unwrap())
            .unwrap();
        let config = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let result = TlsConnector::from(Arc::new(config))
            .connect(
                ServerName::try_from(name).unwrap(),
                TcpStream::connect(address).await.unwrap(),
            )
            .await
            .is_ok();
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn wsl_nat_identity_matches_only_its_address_and_keeps_loopback_stable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = RouterPaths::from_dir(dir.path());
        let _ = acceptor(&paths).unwrap();
        let loopback = wsl_certificate(&paths, std::net::Ipv4Addr::LOCALHOST).unwrap();
        let address = "172.25.80.1".parse().unwrap();
        let tls = wsl_acceptor(&paths, address).unwrap();
        let pem = wsl_certificate(&paths, address).unwrap();
        assert!(connect(tls.clone(), &pem, "172.25.80.1").await);
        assert!(!connect(tls.clone(), &pem, "172.25.80.2").await);
        assert!(!connect(tls, &loopback, "172.25.80.1").await);
        let tls = wsl_acceptor(&paths, address).unwrap();
        assert_eq!(wsl_certificate(&paths, address).unwrap(), pem);
        assert_eq!(
            wsl_certificate(&paths, std::net::Ipv4Addr::LOCALHOST).unwrap(),
            loopback
        );
        assert!(connect(tls, &pem, "172.25.80.1").await);
        assert!(wsl_acceptor(&paths, "8.8.8.8".parse().unwrap()).is_err());
        assert!(!String::from_utf8_lossy(&pem).contains("PRIVATE KEY"));
    }
}
