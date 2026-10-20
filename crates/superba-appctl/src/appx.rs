//! MSIX (Appx) packages on Windows: package paths, application ids, AUMIDs and
//! the activation command line, without listing `C:\Program Files\WindowsApps`.
//!
//! The ChatGPT app for Windows is the package `OpenAI.Codex` (Store product
//! 9PLM9XGG6VKS). A packaged app keeps its package identity only when it is
//! activated by its application user model id (`PackageFamilyName!AppId`)
//! through `IApplicationActivationManager`; its executable is never started by
//! path. Package facts come from the package APIs (`GetPackagesByPackageFamily`,
//! `GetPackagePathByFullName`, `GetPackageApplicationIds`), which read the
//! package repository and touch no file; the package's own `AppxManifest.xml` is
//! read by its full path only as a fallback. The WindowsApps root is never
//! listed (normal users may not), and paths are kept in their plain `C:\...`
//! form (no verbatim `\\?\` prefix).
//!
//! The parsers and the command-line builder are pure and tested on every
//! platform; the API calls are small and exercised on a real Windows runner by
//! `tests/windows_app_it.rs`.

use std::path::{Path, PathBuf};

/// The application id of the desktop app in the `OpenAI.Codex` manifest; the
/// package also declares `CodexCoreCommandRunner` (a helper without a window).
pub const MAIN_APP_ID: &str = "App";

/// `PackageFamilyName!AppId`.
pub fn aumid(family: &str, app_id: &str) -> String {
    format!("{family}!{app_id}")
}

/// A package family name (`Name_PublisherId`, the publisher id being 13
/// lower-case base-32 characters), as MSIX installs carry it for their id.
pub fn is_family_name(id: &str) -> bool {
    let Some((name, publisher)) = id.rsplit_once('_') else {
        return false;
    };
    !name.is_empty()
        && !name.contains(['_', ':', '\\', '/', '!'])
        && publisher.len() == 13
        && publisher
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The family of a package full name (`Name_Version_Arch_ResourceId_PublisherId`).
pub fn family_of_full_name(full_name: &str) -> Option<String> {
    let parts: Vec<&str> = full_name.split('_').collect();
    let [name, _version, _arch, _resource, publisher] = parts.as_slice() else {
        return None;
    };
    let family = format!("{name}_{publisher}");
    is_family_name(&family).then_some(family)
}

/// The version part of a package full name.
pub fn version_of_full_name(full_name: &str) -> Option<&str> {
    full_name.split('_').nth(1).filter(|v| !v.is_empty())
}

/// The AUMID to activate among the application ids a package reports (full
/// AUMIDs from `GetPackageApplicationIds`, or bare ids from the manifest):
/// the main application ([`MAIN_APP_ID`]) when present, else the first.
pub fn pick_aumid(family: &str, ids: &[String]) -> Option<String> {
    let full: Vec<String> = ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(|id| match id.split_once('!') {
            Some(_) => id.to_owned(),
            None => aumid(family, id),
        })
        .collect();
    full.iter()
        .find(|id| {
            id.split_once('!')
                .is_some_and(|(f, app)| f.eq_ignore_ascii_case(family) && app == MAIN_APP_ID)
        })
        .or_else(|| full.first())
        .cloned()
}

/// The application to activate from a manifest's `(Id, Executable)` list: the
/// one whose executable is the desktop app (`ChatGPT.exe` / `Codex.exe`), else
/// the first.
pub fn main_application(apps: &[(String, Option<String>)]) -> Option<&(String, Option<String>)> {
    apps.iter()
        .find(|(_, exe)| {
            exe.as_deref().is_some_and(|e| {
                let name = e.rsplit(['/', '\\']).next().unwrap_or(e);
                name.eq_ignore_ascii_case("ChatGPT.exe") || name.eq_ignore_ascii_case("Codex.exe")
            })
        })
        .or_else(|| apps.first())
}

/// A package-relative path from a manifest (`app/ChatGPT.exe`) joined to the
/// package root with the platform separator; `None` for absolute or escaping
/// paths.
pub fn package_file(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for part in relative.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => out.push(p),
        }
    }
    (out != root).then_some(out)
}

/// One argument quoted for a Windows command line so that
/// `CommandLineToArgvW` (which Chromium uses) gives it back unchanged.
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// The argument string for `ActivateApplication`.
pub fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_windows_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

pub use superba_core::paths::{canonical, plain_path};

/// True for a path inside a `WindowsApps` package store (any drive, any case).
pub fn in_windows_apps(path: &Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    text.contains("\\windowsapps\\")
}

/// The copies of its bundled `codex.exe` the Windows app runs: it relocates
/// the binary out of the read-only package before starting it, to
/// `%LOCALAPPDATA%\OpenAI\Codex\bin\<16 hex digits>\codex.exe` (seen with
/// 26.1013; a plain `bin\codex.exe` is accepted too). Existing files only,
/// the most recently written first.
pub fn relocated_codex_candidates(local_app_data: &Path) -> Vec<PathBuf> {
    let bin = local_app_data.join("OpenAI").join("Codex").join("bin");
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::iter::once(bin.join("codex.exe"))
        .chain(
            std::fs::read_dir(&bin)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path().join("codex.exe")),
        )
        .filter_map(|p| {
            let meta = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
            Some((meta.modified().unwrap_or(std::time::UNIX_EPOCH), p))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, p)| p).collect()
}

#[cfg(windows)]
pub use imp::{application_ids, family_full_names, package_path, resolve_aumid};

#[cfg(windows)]
mod imp {
    //! The package API calls (kernel32 / appmodel, Windows 8.1+). Each one is
    //! called twice: for the size, then into a buffer of that size.
    use std::path::PathBuf;

    use superba_core::AppInstall;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
    use windows_sys::Win32::Storage::Packaging::Appx::{
        _PACKAGE_INFO_REFERENCE, ClosePackageInfo, GetPackageApplicationIds,
        GetPackagePathByFullName, GetPackagesByPackageFamily, OpenPackageInfoByFullName,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A NUL-terminated wide string.
    ///
    /// SAFETY: `p` must point to a NUL-terminated UTF-16 string that stays
    /// valid for the call.
    unsafe fn from_wide(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        // SAFETY: the caller guarantees a terminating NUL within the buffer.
        unsafe {
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        }
    }

    /// Full names of the packages of `family` registered for the current user.
    pub fn family_full_names(family: &str) -> Vec<String> {
        let family_w = wide(family);
        let mut count = 0u32;
        let mut length = 0u32;
        // SAFETY: size query: null output buffers with zero lengths.
        let status = unsafe {
            GetPackagesByPackageFamily(
                family_w.as_ptr(),
                &mut count,
                std::ptr::null_mut(),
                &mut length,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_INSUFFICIENT_BUFFER || count == 0 || length == 0 {
            return Vec::new();
        }
        let mut names = vec![std::ptr::null_mut::<u16>(); count as usize];
        let mut buffer = vec![0u16; length as usize];
        // SAFETY: `names` holds `count` pointers and `buffer` `length` WCHARs, as
        // the size query asked for; the pointers written point into `buffer`.
        let status = unsafe {
            GetPackagesByPackageFamily(
                family_w.as_ptr(),
                &mut count,
                names.as_mut_ptr(),
                &mut length,
                buffer.as_mut_ptr(),
            )
        };
        if status != ERROR_SUCCESS {
            return Vec::new();
        }
        names
            .iter()
            .take(count as usize)
            // SAFETY: each pointer points to a NUL-terminated name in `buffer`,
            // which is alive here.
            .map(|p| unsafe { from_wide(*p) })
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// The install folder of a package (no file is touched).
    pub fn package_path(full_name: &str) -> Option<PathBuf> {
        let name = wide(full_name);
        let mut length = 0u32;
        // SAFETY: size query with a null buffer.
        let status =
            unsafe { GetPackagePathByFullName(name.as_ptr(), &mut length, std::ptr::null_mut()) };
        if status != ERROR_INSUFFICIENT_BUFFER || length == 0 {
            return None;
        }
        let mut buffer = vec![0u16; length as usize];
        // SAFETY: `buffer` holds `length` WCHARs as requested.
        let status =
            unsafe { GetPackagePathByFullName(name.as_ptr(), &mut length, buffer.as_mut_ptr()) };
        if status != ERROR_SUCCESS {
            return None;
        }
        // SAFETY: the API wrote a NUL-terminated path into `buffer`.
        let path = unsafe { from_wide(buffer.as_ptr()) };
        (!path.is_empty()).then(|| super::plain_path(std::path::Path::new(&path)))
    }

    /// The application user model ids a package declares, in manifest order.
    pub fn application_ids(full_name: &str) -> Vec<String> {
        let name = wide(full_name);
        let mut info: *mut _PACKAGE_INFO_REFERENCE = std::ptr::null_mut();
        // SAFETY: `info` receives a reference that is closed below.
        if unsafe { OpenPackageInfoByFullName(name.as_ptr(), 0, &mut info) } != ERROR_SUCCESS
            || info.is_null()
        {
            return Vec::new();
        }
        let mut ids = Vec::new();
        let mut length = 0u32;
        let mut count = 0u32;
        // SAFETY: size query with a null buffer; `info` is open.
        let status = unsafe {
            GetPackageApplicationIds(info, &mut length, std::ptr::null_mut(), &mut count)
        };
        if status == ERROR_INSUFFICIENT_BUFFER && length > 0 {
            // Pointer-aligned storage: the buffer starts with `count` pointers to
            // the strings that follow them.
            let words = (length as usize).div_ceil(std::mem::size_of::<usize>());
            let mut buffer = vec![0usize; words];
            // SAFETY: `buffer` holds at least `length` bytes, pointer aligned.
            let status = unsafe {
                GetPackageApplicationIds(info, &mut length, buffer.as_mut_ptr().cast(), &mut count)
            };
            if status == ERROR_SUCCESS {
                let pointers = buffer.as_ptr().cast::<*const u16>();
                for i in 0..(count as usize).min(words) {
                    // SAFETY: the first `count` entries are pointers into `buffer`
                    // to NUL-terminated strings; `buffer` is alive here.
                    ids.push(unsafe { from_wide(*pointers.add(i)) });
                }
            }
        }
        // SAFETY: `info` was opened above and is closed once.
        unsafe {
            ClosePackageInfo(info);
        }
        ids.retain(|id| !id.is_empty());
        ids
    }

    /// The AUMID of an MSIX install (its id is the package family name): from
    /// the package APIs, else from the package's manifest file, else
    /// `<family>!App`. `None` for installs that are not packaged.
    pub fn resolve_aumid(install: &AppInstall) -> Option<String> {
        let family = install.id.as_str();
        if !super::is_family_name(family) {
            return None;
        }
        let mut full_names = family_full_names(family);
        // The registered version whose folder is this install first.
        full_names.sort_by_key(|n| {
            package_path(n).is_none_or(|p| {
                !p.as_os_str()
                    .eq_ignore_ascii_case(super::plain_path(&install.path).as_os_str())
            })
        });
        for full in &full_names {
            if let Some(id) = super::pick_aumid(family, &application_ids(full)) {
                return Some(id);
            }
        }
        let manifest = std::fs::read_to_string(install.path.join("AppxManifest.xml")).ok();
        let apps = manifest
            .as_deref()
            .map(crate::install::parse_appx_applications)
            .unwrap_or_default();
        let app_id = super::main_application(&apps)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| super::MAIN_APP_ID.to_owned());
        Some(super::aumid(family, &app_id))
    }
}
