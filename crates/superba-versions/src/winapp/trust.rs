//! What a downloaded Windows package must be before it is opened: the right
//! identity in its manifest, and a signature Windows accepted from the
//! expected publisher.
//!
//! - The app: `OpenAI.Codex` of OpenAI's Microsoft Store publisher
//!   ([`OPENAI_STORE_PUBLISHER`], publisher id [`OPENAI_PUBLISHER_ID`]); the
//!   Store re-signs submitted packages, so the signing certificate's subject is
//!   that publisher identity (a GUID), issued by a "Microsoft Marketplace CA".
//!   The packages on OpenAI's own server are the same Store-signed files.
//! - A framework dependency: the package the manifest names, from
//!   [`MICROSOFT_PUBLISHER`].
//!
//! In both cases App Installer itself also requires the manifest's
//! `Publisher` to equal the signer's subject; Superba checks it first so a
//! wrong file is refused before anything is shown to the user.

use superba_core::{Error, MsixDependency, MsixIdentity, PackageSigner, Result};

use super::msix::PackageManifest;
use super::{
    MICROSOFT_PUBLISHER, MICROSOFT_PUBLISHER_ID, OPENAI_PUBLISHER_ID, OPENAI_STORE_PUBLISHER,
    PACKAGE_NAME, publisher_id, version_cmp,
};

/// Whose signature a package must carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerRole {
    /// The ChatGPT app.
    App,
    /// A Microsoft framework package.
    Dependency,
}

fn refuse(message: String) -> Error {
    Error::Invalid { message }
}

/// The attributes of a distinguished name (`CN=a, O=b`), keys upper-cased
/// (`ST` as `S`), values unquoted, sorted: two spellings of one name compare
/// equal whatever their order (Windows APIs print them reversed).
pub fn dn_attributes(dn: &str) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in dn.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' | ';' | '+' if !quoted => parts.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    parts.push(cur);
    let mut out: Vec<(String, String)> = parts
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            let k = k.trim().to_ascii_uppercase();
            let k = if k == "ST" { "S".to_owned() } else { k };
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v)
                .replace("\"\"", "\"");
            (!k.is_empty()).then_some((k, v))
        })
        .collect();
    out.sort();
    out
}

/// Two distinguished names are the same name.
pub fn same_dn(a: &str, b: &str) -> bool {
    let (a, b) = (dn_attributes(a), dn_attributes(b));
    !a.is_empty() && a == b
}

fn arch_fits(package: &str, machine: &str) -> bool {
    package == machine || package == "neutral" || (machine == "arm64" && package == "x64")
}

/// The manifest of the app package is the ChatGPT app for an MSIX
/// architecture, of `version` when one is expected (the file name's or the
/// release's).
pub fn check_app_identity(m: &PackageManifest, arch: &str, version: Option<&str>) -> Result<()> {
    let id = &m.identity;
    if id.name != PACKAGE_NAME {
        return Err(refuse(format!(
            "the package is {}, not the ChatGPT app ({PACKAGE_NAME})",
            id.name
        )));
    }
    if id.publisher_id != OPENAI_PUBLISHER_ID || !same_dn(&id.publisher, OPENAI_STORE_PUBLISHER) {
        return Err(refuse(format!(
            "the package's publisher is {:?}, not OpenAI's Store publisher {OPENAI_STORE_PUBLISHER}",
            id.publisher
        )));
    }
    if !arch_fits(&id.arch, arch) {
        return Err(refuse(format!(
            "the package is built for {}, this computer is {arch}",
            id.arch
        )));
    }
    let actual = m.bundle_version.as_deref().unwrap_or(&id.version);
    if let Some(want) = version
        && version_cmp(actual, want).is_ne()
    {
        return Err(refuse(format!(
            "the package is version {actual}, {want} was expected"
        )));
    }
    Ok(())
}

/// The manifest of a downloaded framework package is the dependency `dep` for
/// an app of architecture `arch`.
pub fn check_dependency_identity(
    m: &PackageManifest,
    dep: &MsixDependency,
    arch: &str,
) -> Result<()> {
    let id = &m.identity;
    let ok = id.name.eq_ignore_ascii_case(&dep.name)
        && id.publisher_id == publisher_id(&dep.publisher)
        && id.publisher_id == MICROSOFT_PUBLISHER_ID
        && (id.arch == arch || id.arch == "neutral")
        && version_cmp(&id.version, &dep.min_version).is_ge();
    if ok {
        Ok(())
    } else {
        Err(refuse(format!(
            "{} {} ({}) is not the dependency {} >= {} from Microsoft",
            id.name, id.version, id.arch, dep.name, dep.min_version
        )))
    }
}

/// The signer Windows accepted is the expected publisher of `identity`.
pub fn check_signer(
    identity: &MsixIdentity,
    signer: &PackageSigner,
    role: SignerRole,
) -> Result<()> {
    let (expected, issuer_must) = match role {
        SignerRole::App => (OPENAI_STORE_PUBLISHER, "Microsoft Marketplace CA"),
        SignerRole::Dependency => (MICROSOFT_PUBLISHER, "Microsoft"),
    };
    if !same_dn(&identity.publisher, expected) {
        return Err(refuse(format!(
            "the package's publisher {:?} is not the expected {expected}",
            identity.publisher
        )));
    }
    if !same_dn(&signer.subject, expected) {
        return Err(refuse(format!(
            "the package is signed by {:?}, not by {expected}",
            signer.subject
        )));
    }
    let issuer_cn = signer
        .issuer
        .as_deref()
        .map(dn_attributes)
        .and_then(|attrs| attrs.into_iter().find(|(k, _)| k == "CN").map(|(_, v)| v));
    match issuer_cn {
        Some(cn) if cn.starts_with(issuer_must) => Ok(()),
        other => Err(refuse(format!(
            "the signing certificate of {} was issued by {:?}, not by a {issuer_must} authority",
            identity.name,
            other.unwrap_or_default()
        ))),
    }
}
