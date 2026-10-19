//! Locate the two executables of a Superba installation.

use std::path::{Path, PathBuf};
use superba_core::LocalContext;

/// Executable being located; overrides apply only to macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Executable {
    /// Desktop application (`SUPERBA_GUI_EXE`).
    Gui,
    /// Standalone command line (`SUPERBA_SPB_EXE`).
    Spb,
}

impl Executable {
    fn name(self) -> &'static str {
        match self {
            Self::Gui => "superba",
            Self::Spb => "spb",
        }
    }
    fn override_key(self) -> &'static str {
        match self {
            Self::Gui => "SUPERBA_GUI_EXE",
            Self::Spb => "SUPERBA_SPB_EXE",
        }
    }
}

/// macOS lookup shared by login registration and the Superba overview.
/// Bundle lookup is lazy: an explicit override or sibling needs no OS query.
pub fn macos_executable(
    current: &Path,
    ctx: &LocalContext,
    kind: Executable,
    installed_bundle: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let valid = |p: &PathBuf| p.is_absolute() && p.is_file();
    if let Some(path) = ctx.env.var_os(kind.override_key()).map(PathBuf::from) {
        // A broken explicit override must not silently select another install.
        return valid(&path).then_some(path);
    }
    let dir = current.parent()?;
    let name = kind.name();
    let mut candidates = vec![
        dir.join(name),
        dir.join("Superba.app/Contents/MacOS").join(name),
    ];
    // In the DMG the standalone CLI is beside the app bundle.
    if let Some(bundle) = current
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        && let Some(parent) = bundle.parent()
    {
        candidates.push(parent.join(name));
    }
    if let Some(path) = candidates.into_iter().find(valid) {
        return Some(path);
    }
    if let Some(bundle) = installed_bundle() {
        let path = bundle.join("Contents/MacOS").join(name);
        if valid(&path) {
            return Some(path);
        }
        if kind == Executable::Spb
            && let Some(parent) = bundle.parent()
        {
            let path = parent.join(name);
            if valid(&path) {
                return Some(path);
            }
        }
    }
    for bundle in [
        ctx.home_dir.join("Applications/Superba.app"),
        PathBuf::from("/Applications/Superba.app"),
    ] {
        let path = bundle.join("Contents/MacOS").join(name);
        if valid(&path) {
            return Some(path);
        }
    }
    if kind == Executable::Spb {
        // Finder apps do not inherit the user's shell PATH. This is the
        // standalone CLI location documented in the DMG installation steps.
        let path = ctx.home_dir.join(".local/bin/spb");
        if valid(&path) {
            return Some(path);
        }
        return path_executable(ctx, name);
    }
    None
}

fn path_executable(ctx: &LocalContext, name: &str) -> Option<PathBuf> {
    ctx.env.var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|p| p.join(name))
            .find(|p| p.is_file())
    })
}

/// Find the standalone CLI, including its README install location on macOS.
pub fn spb_executable(current: &Path, ctx: &LocalContext) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        macos_executable(current, ctx, Executable::Spb, installed_macos_bundle)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let name = format!("spb{}", std::env::consts::EXE_SUFFIX);
        current
            .parent()
            .map(|p| p.join(&name))
            .filter(|p| p.is_file())
            .or_else(|| path_executable(ctx, &name))
    }
}

/// Ask LaunchServices for the installed application, including Finder moves.
#[cfg(target_os = "macos")]
pub fn installed_macos_bundle() -> Option<PathBuf> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;
    NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(crate::autostart::BUNDLE_ID))?
        .path()
        .map(|p| PathBuf::from(p.to_string()))
}
