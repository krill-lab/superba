//! The ChatGPT desktop app package for Windows: downloaded, verified and
//! handed to App Installer (the user installs it; nothing is installed here).
//!
//! Sources (Preferences -> Network, [`superba_core::WindowsAppSource`]):
//!
//! - **Official** ([`official`]): OpenAI's `windows-release.json` on
//!   `persistent.oaistatic.com` lists `packageVersion` and one `.msix` per
//!   architecture. Only https URLs on that host (or the e2e override base) are
//!   accepted. No checksum is published; the size comes from the response and
//!   the package signature is the proof.
//! - **Store mirror** ([`mirror`]): store.rg-adguard.net, a third-party
//!   resolver, turns the Store product id into temporary links on Microsoft's
//!   CDN. Only links on the CDN hosts in [`mirror::is_store_cdn_url`] are
//!   followed; the listed SHA-1 and size are checked (and the CDN's own `ETag`,
//!   the base64 SHA-1, when it sends one).
//!
//! Facts (2026-10-17, captured in `tests/fixtures/`): both sources serve the
//! same file, the Store-signed package `OpenAI.Codex_<ver>_<arch>__2p2nqsd0c76g0`
//! (the oaistatic x64 msix of 26.1013.4894.0 has the SHA-1 rg-adguard lists and
//! the CDN's `ETag`). Its signer is OpenAI's Store publisher identity
//! [`OPENAI_STORE_PUBLISHER`] issued by "Microsoft Marketplace CA G 024" (not a
//! certificate with "OpenAI" in its subject), with an RFC 3161 timestamp; the
//! manifest declares no package dependencies. The CDN serves plain http only
//! (its TLS certificate does not cover `tlu.dl.delivery.mp.microsoft.com`), and
//! rg-adguard sits behind a Cloudflare challenge that answers automated
//! clients with `403` + `cf-mitigated: challenge` after a request or two, so
//! its answer is cached until the links expire.
//!
//! Every file is verified before it is reported: size, checksum (when one is
//! listed), the manifest identity ([`trust::check_app_identity`]); the caller
//! then checks the signature (`WinVerifyTrust` in `superba-appctl`) against
//! [`trust::check_signer`] before opening it.

pub mod download;
pub mod flow;
pub mod mirror;
pub mod msix;
pub mod official;
pub mod trust;

/// Microsoft Store product id of the ChatGPT desktop app (also in
/// `windows-store-update.json`).
pub const STORE_PRODUCT_ID: &str = "9PLM9XGG6VKS";

/// MSIX package name of the app.
pub const PACKAGE_NAME: &str = "OpenAI.Codex";

/// Publisher of the app's package (its Microsoft Store publisher identity; the
/// manifest's `Publisher` and the signing certificate's subject).
pub const OPENAI_STORE_PUBLISHER: &str = "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B";

/// Publisher id of [`OPENAI_STORE_PUBLISHER`] (family `OpenAI.Codex_2p2nqsd0c76g0`).
pub const OPENAI_PUBLISHER_ID: &str = "2p2nqsd0c76g0";

/// Publisher of Microsoft's framework packages (VCLibs, UI.Xaml, ...).
pub const MICROSOFT_PUBLISHER: &str =
    "CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US";

/// Publisher id of [`MICROSOFT_PUBLISHER`].
pub const MICROSOFT_PUBLISHER_ID: &str = "8wekyb3d8bbwe";

/// The publisher id Windows derives from a publisher distinguished name: the
/// first 8 bytes of the SHA-256 of its UTF-16LE form, as 13 characters of
/// Crockford-style base32 (the part after `_` in a package family name).
pub fn publisher_id(publisher: &str) -> String {
    use sha2::Digest;
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let utf16: Vec<u8> = publisher
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let digest = sha2::Sha256::digest(&utf16);
    let mut first = [0_u8; 8];
    first.copy_from_slice(&digest[..8]);
    // 64 bits plus one zero bit = 13 groups of 5 bits.
    let bits = u128::from(u64::from_be_bytes(first)) << 1;
    (0..13)
        .map(|i| char::from(ALPHABET[((bits >> (5 * (12 - i))) & 31) as usize]))
        .collect()
}

/// The MSIX architecture name of a Rust / OS architecture (`x86_64` -> `x64`).
pub fn msix_arch(arch: &str) -> Option<&'static str> {
    match arch.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" | "x64" => Some("x64"),
        "aarch64" | "arm64" => Some("arm64"),
        "x86" | "i686" | "i586" => Some("x86"),
        _ => None,
    }
}

/// Compare dotted package versions (`26.1013.4894.0`) numerically.
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    crate::app::app_version_cmp(a, b)
}
