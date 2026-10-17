//! Windows app packages downloaded by Superba: the packages already
//! registered (framework dependencies), the signature check before a package
//! is shown to the user, and handing it to its default handler (App
//! Installer), where the user clicks Install.
//!
//! - Registered packages come from the same AppModel repository key the MSIX
//!   install discovery reads ([`crate::install`]); no child process.
//! - The signature: `WinVerifyTrust` with `WINTRUST_ACTION_GENERIC_VERIFY_V2`
//!   (the Appx SIP checks MSIX / APPX packages and bundles), no UI, no
//!   revocation lookups and no network fetches (the package carries its chain
//!   and an RFC 3161 timestamp); then the signing certificate's subject and
//!   issuer (`CertNameToStrW`, X.500 form). Whether that signer is acceptable
//!   is decided by the caller (`superba_versions::winapp::trust`).
//! - Opening: `ShellExecuteExW` with the "open" verb on the file, exactly like
//!   a double click. Nothing is installed silently (`Add-AppxPackage` is never
//!   run) and nothing is elevated.
//!
//! The Windows parts cannot run in CI (Linux); they are kept small. Elsewhere
//! every function reports `Unsupported`.

use std::path::Path;

use superba_core::{InstalledPackage, Result};

/// Split a package full name (`Name_Version_Arch_ResourceId_PublisherId`).
pub fn parse_full_name(full_name: &str) -> Option<InstalledPackage> {
    let parts: Vec<&str> = full_name.split('_').collect();
    let [name, version, arch, _resource, publisher_id] = parts.as_slice() else {
        return None;
    };
    let ok = !name.is_empty()
        && !version.is_empty()
        && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        && !arch.is_empty()
        && publisher_id.len() == 13;
    ok.then(|| InstalledPackage {
        name: (*name).to_owned(),
        version: (*version).to_owned(),
        arch: arch.to_ascii_lowercase(),
        publisher_id: (*publisher_id).to_owned(),
    })
}

/// Every package registered for this user (all publishers), from the registry.
#[cfg(windows)]
pub fn installed_packages() -> Vec<InstalledPackage> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!(
        "Software\\Classes\\{}",
        crate::install::APPX_REPOSITORY_KEY
    )) else {
        return Vec::new();
    };
    key.enum_keys()
        .flatten()
        .filter_map(|full| parse_full_name(&full))
        .collect()
}

/// Every package registered for this user (none outside Windows).
#[cfg(not(windows))]
pub fn installed_packages() -> Vec<InstalledPackage> {
    Vec::new()
}

/// Check the signature of `path` with `WinVerifyTrust`; its signer.
#[cfg(windows)]
pub fn verify_signature(path: &Path) -> Result<superba_core::PackageSigner> {
    imp::verify(path)
}

/// Check the signature of `path` (Windows only).
#[cfg(not(windows))]
pub fn verify_signature(path: &Path) -> Result<superba_core::PackageSigner> {
    let _ = path;
    Err(superba_core::Error::Unsupported {
        what: "checking package signatures on this platform".into(),
    })
}

/// Open `path` with its default handler (App Installer for a package).
#[cfg(windows)]
pub fn open_with_default_handler(path: &Path) -> Result<()> {
    imp::open(path)
}

/// Open `path` with its default handler (Windows only).
#[cfg(not(windows))]
pub fn open_with_default_handler(path: &Path) -> Result<()> {
    let _ = path;
    Err(superba_core::Error::Unsupported {
        what: "opening package installers on this platform".into(),
    })
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use superba_core::{Error, PackageSigner, Result};
    use windows_sys::Win32::Foundation::{HWND, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Cryptography::{
        CERT_X500_NAME_STR, CRYPT_INTEGER_BLOB, CertNameToStrW, PKCS_7_ASN_ENCODING,
        X509_ASN_ENCODING,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO,
        WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
        WTD_STATEACTION_VERIFY, WTD_UI_NONE, WTHelperGetProvCertFromChain,
        WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
    };
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    /// An encoded certificate name as `CN=..., O=...`.
    ///
    /// SAFETY: `blob` points at a valid `CERT_NAME_BLOB` of a live certificate.
    unsafe fn name(blob: *const CRYPT_INTEGER_BLOB) -> Option<String> {
        let enc = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
        // SAFETY: a null buffer asks for the length (characters incl. the NUL).
        let n = unsafe { CertNameToStrW(enc, blob, CERT_X500_NAME_STR, std::ptr::null_mut(), 0) };
        if n <= 1 {
            return None;
        }
        let mut buf = vec![0_u16; n as usize];
        // SAFETY: `buf` holds `n` characters.
        let got = unsafe { CertNameToStrW(enc, blob, CERT_X500_NAME_STR, buf.as_mut_ptr(), n) };
        let len = (got as usize).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    /// The signer of a verified state: the first signer's certificate.
    ///
    /// SAFETY: `state` is the `hWVTStateData` of a successful
    /// `WTD_STATEACTION_VERIFY` that has not been closed.
    unsafe fn signer(state: windows_sys::Win32::Foundation::HANDLE) -> Option<PackageSigner> {
        // SAFETY: the provider data lives until the state is closed; every
        // pointer is checked before it is read.
        unsafe {
            let prov = WTHelperProvDataFromStateData(state);
            if prov.is_null() {
                return None;
            }
            let sgnr = WTHelperGetProvSignerFromChain(prov, 0, 0, 0);
            if sgnr.is_null() {
                return None;
            }
            let cert = WTHelperGetProvCertFromChain(sgnr, 0);
            if cert.is_null() || (*cert).pCert.is_null() || (*(*cert).pCert).pCertInfo.is_null() {
                return None;
            }
            let info = (*(*cert).pCert).pCertInfo;
            Some(PackageSigner {
                subject: name(&raw const (*info).Subject)?,
                issuer: name(&raw const (*info).Issuer),
            })
        }
    }

    pub(super) fn verify(path: &Path) -> Result<PackageSigner> {
        let file_path = wide(path);
        let mut file = WINTRUST_FILE_INFO {
            cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: file_path.as_ptr(),
            hFile: std::ptr::null_mut(),
            pgKnownSubject: std::ptr::null_mut(),
        };
        let mut data = WINTRUST_DATA {
            cbStruct: size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: WTD_CHOICE_FILE,
            dwStateAction: WTD_STATEACTION_VERIFY,
            dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
            ..Default::default()
        };
        data.Anonymous.pFile = &raw mut file;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // INVALID_HANDLE_VALUE as the window: no UI at all.
        let no_ui = INVALID_HANDLE_VALUE as HWND;
        // SAFETY: `data` and `file` (and the path buffer) outlive both calls;
        // the state opened by VERIFY is closed by the CLOSE call below.
        let (status, signer) = unsafe {
            let status = WinVerifyTrust(no_ui, &raw mut action, (&raw mut data).cast::<c_void>());
            let signer = if status == 0 {
                signer(data.hWVTStateData)
            } else {
                None
            };
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            WinVerifyTrust(no_ui, &raw mut action, (&raw mut data).cast::<c_void>());
            (status, signer)
        };
        if status != 0 {
            return Err(Error::Invalid {
                message: format!(
                    "Windows did not accept the signature of {} (WinVerifyTrust 0x{:08X}: {})",
                    path.display(),
                    status as u32,
                    trust_error(status as u32)
                ),
            });
        }
        signer.ok_or_else(|| Error::Invalid {
            message: format!(
                "{} is signed but its signer could not be read",
                path.display()
            ),
        })
    }

    fn trust_error(code: u32) -> &'static str {
        match code {
            0x800B_0100 => "not signed",
            0x8009_6010 => "the signature does not match the file (tampered or corrupted)",
            0x800B_0109 => "the certificate chain ends in an untrusted root",
            0x800B_0101 => "the certificate has expired",
            0x800B_010C => "the certificate was revoked",
            0x800B_0111 => "the publisher is explicitly distrusted",
            0x800B_0003 | 0x800B_0001 => "this file type cannot be checked",
            _ => "signature check failed",
        }
    }

    pub(super) fn open(path: &Path) -> Result<()> {
        use windows::Win32::System::Com::{
            COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
            CoUninitialize,
        };
        let verb = wide("open");
        let file = wide(path);
        let dir = path.parent().map(wide);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: verb.as_ptr(),
            lpFile: file.as_ptr(),
            lpDirectory: dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            nShow: SW_SHOWNORMAL,
            ..Default::default()
        };
        // SAFETY: COM is initialized for this call as ShellExecuteEx asks
        // (handlers may be COM objects) and uninitialized only when this call
        // initialized it; every string outlives the call.
        let ok = unsafe {
            let init = CoInitializeEx(
                None,
                COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0),
            );
            let ok = ShellExecuteExW(&raw mut info);
            if init.is_ok() {
                CoUninitialize();
            }
            ok
        };
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            return Err(Error::Process {
                message: format!(
                    "Windows could not open {} with its installer (App Installer): {e}",
                    path.display()
                ),
            });
        }
        Ok(())
    }
}
