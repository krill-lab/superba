//! Desktop install discovery and file-format readers with explicit filesystem roots.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use superba_core::codex::ENV_SUPERBA_APP_PATH;
use superba_core::{AppInstall, AppVariant, CodexVersion, EnvSource};
use tokio::process::Command;
use tokio::time::timeout;
use walkdir::WalkDir;

/// Alias of [`ENV_SUPERBA_APP_PATH`] accepted by appctl for a custom install directory.
pub const ENV_SUPERBA_APP_DIR: &str = "SUPERBA_APP_DIR";

/// Parsed `owl-app.ini` settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwlAppIni {
    /// Electron's user-data directory name.
    pub user_data_dir_name: Option<String>,
    /// Desktop app version, independent of the Electron runtime version.
    pub app_version: Option<String>,
}

/// Parse Owl settings in any section, tolerating comments, CRLF and surrounding spaces.
pub fn parse_owl_app_ini(text: &str) -> OwlAppIni {
    let mut result = OwlAppIni::default();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.starts_with(['#', ';', '[']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let comment = value.char_indices().find_map(|(i, c)| {
            (matches!(c, '#' | ';') && (i == 0 || value[..i].ends_with(char::is_whitespace)))
                .then_some(i)
        });
        let value = nonempty(&value[..comment.unwrap_or(value.len())]);
        match key.trim() {
            "UserDataDirectoryName" => result.user_data_dir_name = value,
            "AppVersion" => result.app_version = value,
            _ => {}
        }
    }
    result
}

/// Read `(version, codexAppBrand)` from Linux package metadata; invalid fields are absent.
pub fn parse_linux_package_metadata(text: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return (None, None);
    };
    (
        json_string(&value, "version"),
        json_string(&value, "codexAppBrand"),
    )
}

/// Read the nonempty `(version, entrypoint)` strings from `codex-package.json`.
pub fn parse_codex_package_json(text: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some((
        json_string(&value, "version")?,
        json_string(&value, "entrypoint")?,
    ))
}

fn nonempty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    nonempty(value.get(key)?.as_str()?)
}

// These small XML readers never resolve external entities or invoke platform tools.
struct XmlTag<'a> {
    name: &'a str,
    attributes: &'a str,
    closing: bool,
    empty: bool,
}

fn next_xml_tag<'a>(remaining: &mut &'a str) -> Option<XmlTag<'a>> {
    loop {
        let start = remaining.find('<')?;
        *remaining = &remaining[start + 1..];
        if remaining.starts_with("!--") {
            *remaining = &remaining[remaining.find("-->")? + 3..];
            continue;
        }
        let mut quote = None;
        let mut brackets = 0usize;
        let end = remaining.char_indices().find_map(|(i, c)| {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else {
                match c {
                    '\'' | '"' => quote = Some(c),
                    '[' => brackets += 1,
                    ']' => brackets = brackets.saturating_sub(1),
                    '>' if brackets == 0 => return Some(i),
                    _ => {}
                }
            }
            None
        })?;
        let body = remaining[..end].trim();
        *remaining = &remaining[end + 1..];
        if body.starts_with(['!', '?']) {
            continue;
        }
        let closing = body.starts_with('/');
        let empty = body.ends_with('/');
        let body = body.trim_start_matches('/').trim_end_matches('/').trim();
        let split = body.find(char::is_whitespace).unwrap_or(body.len());
        return Some(XmlTag {
            name: &body[..split],
            attributes: body[split..].trim(),
            closing,
            empty,
        });
    }
}

fn decode_xml(text: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find('&') {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let Some(end) = remaining.find(';') else {
            break;
        };
        let entity = &remaining[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|n| u32::from_str_radix(n, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(c) = decoded {
            result.push(c);
        } else {
            result.push_str(&remaining[..=end]);
        }
        remaining = &remaining[end + 1..];
    }
    result.push_str(remaining);
    result
}

/// Read a string immediately following a matching XML plist key, decoding XML entities.
pub fn parse_plist_string(xml: &str, key: &str) -> Option<String> {
    let mut remaining = xml;
    while let Some(tag) = next_xml_tag(&mut remaining) {
        if tag.name != "key" || tag.closing || tag.empty {
            continue;
        }
        let text = remaining.split_once('<')?.0;
        let end = next_xml_tag(&mut remaining)?;
        if end.name != "key" || !end.closing || decode_xml(text).trim() != key {
            continue;
        }
        let value = next_xml_tag(&mut remaining)?;
        if value.name != "string" || value.closing {
            return None;
        }
        if value.empty {
            return Some(String::new());
        }
        let text = remaining.split_once('<')?.0;
        let end = next_xml_tag(&mut remaining)?;
        return (end.name == "string" && end.closing).then(|| decode_xml(text));
    }
    None
}

fn desktop_exec_value(text: &str) -> Option<&str> {
    let mut in_entry = false;
    for line in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "Exec"
        {
            return Some(value.trim());
        }
    }
    None
}

fn desktop_unescape(value: &str) -> String {
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => result.push(' '),
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('r') => result.push('\r'),
            Some('\\') => result.push('\\'),
            Some(c) => {
                result.push('\\');
                result.push(c);
            }
            None => result.push('\\'),
        }
    }
    result
}

fn strip_field_codes(arg: &str) -> Option<(String, bool)> {
    let mut result = String::new();
    let mut removed = false;
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
        } else {
            match chars.next()? {
                '%' => result.push('%'),
                'f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'i' | 'c' | 'k' | 'v' | 'm' => {
                    removed = true;
                }
                _ => return None,
            }
        }
    }
    Some((result, removed))
}

/// Split the Desktop Entry group's `Exec` using desktop quoting and escape rules.
/// Removes recognized field codes, turns `%%` into `%`, and rejects malformed commands.
pub fn parse_desktop_exec(text: &str) -> Option<Vec<String>> {
    let value = desktop_unescape(desktop_exec_value(text)?);
    let mut args = Vec::new();
    let mut arg = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' => {
                let next = chars.next()?;
                if quoted && !matches!(next, '"' | '`' | '$' | '\\') {
                    arg.push('\\');
                }
                arg.push(next);
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut arg));
                    started = false;
                }
            }
            c => {
                arg.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if started {
        args.push(arg);
    }
    let mut result = Vec::new();
    for arg in args {
        let (arg, removed) = strip_field_codes(&arg)?;
        if !arg.is_empty() || !removed {
            result.push(arg);
        }
    }
    Some(result)
}

fn xml_attribute(mut attributes: &str, key: &str) -> Option<String> {
    while !attributes.trim().is_empty() {
        let (name, tail) = attributes.trim_start().split_once('=')?;
        let tail = tail.trim_start();
        let quote = tail.chars().next()?;
        if !matches!(quote, '\'' | '"') {
            return None;
        }
        let tail = &tail[1..];
        let end = tail.find(quote)?;
        if name.trim() == key {
            return nonempty(&decode_xml(&tail[..end]));
        }
        attributes = &tail[end + 1..];
    }
    None
}

/// Read the first application `Id` from an Appx manifest, ignoring comments and namespaces.
pub fn parse_appx_application_id(xml: &str) -> Option<String> {
    let mut remaining = xml;
    while let Some(tag) = next_xml_tag(&mut remaining) {
        if !tag.closing && tag.name.rsplit(':').next() == Some("Application") {
            return xml_attribute(tag.attributes, "Id");
        }
    }
    None
}

/// One supported package reported by `Get-AppxPackage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppxPackage {
    /// Package name.
    pub name: String,
    /// Package family name, including publisher suffix.
    pub family_name: String,
    /// Package installation root.
    pub install_location: PathBuf,
    /// Package version, when available.
    pub version: Option<String>,
}

/// Parse an object or array of Appx packages, retaining only the three OpenAI desktop families.
/// Empty or invalid output produces an empty list; malformed entries are skipped individually.
pub fn parse_appx_packages_json(text: &str) -> Vec<AppxPackage> {
    let Ok(value) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
        return Vec::new();
    };
    let values = match &value {
        Value::Array(values) => values.as_slice(),
        Value::Object(_) => std::slice::from_ref(&value),
        _ => return Vec::new(),
    };
    values
        .iter()
        .filter_map(|value| {
            let family_name = json_string(value, "PackageFamilyName")?;
            let family = family_name.split_once('_')?.0;
            if !matches!(
                family,
                "OpenAI.Codex" | "OpenAI.CodexBeta" | "OpenAI.ChatGPT-Desktop"
            ) {
                return None;
            }
            Some(AppxPackage {
                name: json_string(value, "Name")?,
                family_name,
                install_location: PathBuf::from(json_string(value, "InstallLocation")?),
                version: json_string(value, "Version"),
            })
        })
        .collect()
}

fn read_owl(resources: &Path) -> OwlAppIni {
    fs::read_to_string(resources.join("owl-app.ini"))
        .map(|text| parse_owl_app_ini(&text))
        .unwrap_or_default()
}

fn child_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
}

fn variant(id: &str, executable: &Path, brand: Option<&str>) -> AppVariant {
    let id = id.to_ascii_lowercase();
    let file_name = executable.file_name().unwrap_or_default().to_string_lossy();
    let name = if file_name.to_ascii_lowercase().ends_with(".exe") {
        &file_name[..file_name.len() - 4]
    } else {
        &file_name
    };
    if !id.starts_with("path:") && (id.contains("codexbeta") || id.contains("codex.beta")) {
        AppVariant::CodexBeta
    } else if brand.is_some_and(|b| b.eq_ignore_ascii_case("chatgpt"))
        || name.eq_ignore_ascii_case("chatgpt")
    {
        AppVariant::ChatGpt
    } else if name.eq_ignore_ascii_case("codex") {
        AppVariant::Codex
    } else {
        AppVariant::Other(name.to_owned())
    }
}

fn relative_entrypoint(text: &str) -> Option<PathBuf> {
    let path = PathBuf::from(text.replace('\\', "/"));
    (!text.contains(':')
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)))
    .then_some(path)
}

fn bundled_cli(resources: &Path) -> (Option<PathBuf>, Option<CodexVersion>) {
    let cli_root = resources.join("codex-cli");
    let package = fs::read_to_string(cli_root.join("codex-package.json"))
        .ok()
        .and_then(|text| parse_codex_package_json(&text));
    let entrypoint = package
        .as_ref()
        .and_then(|(_, entrypoint)| relative_entrypoint(entrypoint))
        .map(|entrypoint| cli_root.join(entrypoint));
    // The path the app starts (on macOS the `bin/codex` launcher of the
    // `CodexCLI.app` binary; `crate::codexid` treats both as one Codex). Windows
    // ships the Linux `codex` for WSL next to `codex.exe`: the Windows one first.
    let binary = entrypoint
        .into_iter()
        .chain([
            cli_root.join("bin/codex"),
            cli_root.join("CodexCLI.app/Contents/MacOS/codex"),
            resources.join("codex.exe"),
            resources.join("codex"),
        ])
        .find(|path| path.is_file());
    let version = binary
        .as_ref()
        .and_then(|_| package.and_then(|(version, _)| CodexVersion::parse(&version).ok()));
    (binary, version)
}

fn inspect_bundle(root: &Path) -> Option<AppInstall> {
    if root
        .file_name()?
        .to_string_lossy()
        .eq_ignore_ascii_case("CodexCLI.app")
    {
        return None;
    }
    let contents = root.join("Contents");
    let resources = contents.join("Resources");
    if !resources.join("app.asar").is_file()
        || !child_paths(&contents.join("Frameworks"))
            .iter()
            .any(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with("Framework.framework"))
            })
    {
        return None;
    }
    let plist = fs::read_to_string(contents.join("Info.plist")).ok()?;
    let id = nonempty(&parse_plist_string(&plist, "CFBundleIdentifier")?)?;
    if id == "com.openai.codex.cli" {
        return None;
    }
    let name = parse_plist_string(&plist, "CFBundleExecutable")?;
    let executable = contents.join("MacOS").join(relative_entrypoint(&name)?);
    if !executable.is_file() {
        return None;
    }
    let app_version = parse_plist_string(&plist, "CFBundleShortVersionString")
        .and_then(|version| nonempty(&version))
        .or_else(|| read_owl(&resources).app_version);
    let (bundled_codex, bundled_codex_version) = bundled_cli(&resources);
    Some(AppInstall {
        variant: variant(&id, &executable, None),
        id,
        path: root.to_owned(),
        executable,
        app_version,
        bundled_codex,
        bundled_codex_version,
    })
}

fn install_from_resources(root: &Path, executable: PathBuf, resources: &Path) -> AppInstall {
    let (version, brand) = fs::read_to_string(resources.join("linux-package-metadata.json"))
        .map(|text| parse_linux_package_metadata(&text))
        .unwrap_or_default();
    let id = format!("path:{}", root.display());
    let (bundled_codex, bundled_codex_version) = bundled_cli(resources);
    AppInstall {
        variant: variant(&id, &executable, brand.as_deref()),
        id,
        path: root.to_owned(),
        executable,
        app_version: read_owl(resources).app_version.or(version),
        bundled_codex,
        bundled_codex_version,
    }
}

fn inspect_directory(root: &Path) -> Option<AppInstall> {
    let resources = root.join("resources");
    if resources.join("app.asar").is_file()
        && let Some(executable) = ["ChatGPT", "Codex", "chatgpt", "codex-desktop"]
            .into_iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file())
    {
        return Some(install_from_resources(root, executable, &resources));
    }

    // Windows packages can nest the main executable, but a resources/codex.exe is a CLI.
    // File-based inspection is portable so Linux tests can validate this layout too.
    for entry in WalkDir::new(root)
        .min_depth(1)
        .max_depth(3)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("resources")
        })
        .filter_map(Result::ok)
    {
        let name = entry.file_name().to_string_lossy();
        if !(name.eq_ignore_ascii_case("ChatGPT.exe") || name.eq_ignore_ascii_case("Codex.exe"))
            || !entry.path().is_file()
        {
            continue;
        }
        let adjacent = entry.path().parent()?.join("resources");
        for resources in [adjacent, root.join("resources")] {
            if resources.join("app.asar").is_file() {
                return Some(install_from_resources(
                    root,
                    entry.path().to_owned(),
                    &resources,
                ));
            }
        }
    }
    None
}

/// Inspect a desktop directory, executable, launcher, AppImage or path inside a macOS bundle.
/// Requires an Electron archive (and a framework for bundles), excluding native and CLI apps.
/// Reads app metadata only; a bundled CLI version is read solely from its package JSON.
pub fn inspect_install(path: &Path) -> Option<AppInstall> {
    if let Some(install) = appimage_install(path) {
        return Some(install);
    }
    let path = fs::canonicalize(path).ok()?;
    if let Some(bundle) = path.ancestors().find(|path| {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    }) {
        return inspect_bundle(bundle);
    }
    if path.is_dir() {
        return inspect_directory(&path);
    }
    let name = path.file_name()?.to_str()?;
    if !matches!(
        name,
        "ChatGPT" | "Codex" | "chatgpt" | "codex-launcher" | "codex-desktop"
    ) && !name.eq_ignore_ascii_case("ChatGPT.exe")
        && !name.eq_ignore_ascii_case("Codex.exe")
    {
        return None;
    }
    let parent = path.parent()?;
    let root = if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        parent
            .ancestors()
            .take(3)
            .find(|dir| dir.join("AppxManifest.xml").is_file())
            .unwrap_or(parent)
    } else {
        parent
    };
    inspect_directory(root)
}

/// Inspect the first valid explicit install, preferring `SUPERBA_APP_PATH` to `SUPERBA_APP_DIR`.
pub fn explicit_install(env: &dyn EnvSource) -> Option<AppInstall> {
    [ENV_SUPERBA_APP_PATH, ENV_SUPERBA_APP_DIR]
        .into_iter()
        .filter_map(|key| env.var_os(key).filter(|value| !value.is_empty()))
        .find_map(|path| inspect_install(Path::new(&path)))
}

pub(crate) struct CandidateRoots {
    pub(crate) installs: Vec<PathBuf>,
    pub(crate) desktop_dirs: Vec<PathBuf>,
    pub(crate) scan_dirs: Vec<PathBuf>,
}

// macOS paths are fixed (no XDG or Windows variables), so it is unused there.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn env_path(env: &dyn EnvSource, key: &str) -> Option<PathBuf> {
    env.var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "linux")]
pub(crate) fn candidate_roots(env: &dyn EnvSource, home: &Path) -> CandidateRoots {
    CandidateRoots {
        installs: [
            "/usr/lib/chatgpt",
            "/usr/lib/codex",
            "/opt/ChatGPT",
            "/opt/chatgpt",
            "/opt/Codex",
            "/opt/codex",
        ]
        .into_iter()
        .map(PathBuf::from)
        .chain([home.join(".local/lib/chatgpt")])
        .collect(),
        desktop_dirs: vec![
            env_path(env, "XDG_DATA_HOME")
                .unwrap_or_else(|| home.join(".local/share"))
                .join("applications"),
            PathBuf::from("/usr/share/applications"),
            PathBuf::from("/usr/local/share/applications"),
            PathBuf::from("/var/lib/flatpak/exports/share/applications"),
            home.join(".local/share/flatpak/exports/share/applications"),
        ],
        scan_dirs: vec![
            home.join("Applications"),
            home.join(".local/bin"),
            PathBuf::from("/opt"),
        ],
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn candidate_roots(_env: &dyn EnvSource, home: &Path) -> CandidateRoots {
    // Bundle metadata, rather than the displayed application name, identifies the app.
    CandidateRoots {
        installs: Vec::new(),
        desktop_dirs: Vec::new(),
        scan_dirs: vec![PathBuf::from("/Applications"), home.join("Applications")],
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn candidate_roots(env: &dyn EnvSource, home: &Path) -> CandidateRoots {
    // MSIX roots are obtained separately from PowerShell.
    let local = env_path(env, "LOCALAPPDATA").unwrap_or_else(|| home.join("AppData/Local"));
    CandidateRoots {
        installs: vec![local.join("Programs/ChatGPT"), local.join("Programs/Codex")],
        desktop_dirs: Vec::new(),
        scan_dirs: Vec::new(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn candidate_roots(_env: &dyn EnvSource, _home: &Path) -> CandidateRoots {
    CandidateRoots {
        installs: Vec::new(),
        desktop_dirs: Vec::new(),
        scan_dirs: Vec::new(),
    }
}

/// Derive candidate install directories from matching `.desktop` files in the supplied roots.
/// Only desktop programs with a directory component are usable without an injected PATH.
/// Entries mentioning Superba in `Exec` are ignored; results are ordered and deduplicated.
pub fn desktop_entry_targets(dirs: &[PathBuf]) -> Vec<PathBuf> {
    desktop_targets(desktop_entry_programs(dirs))
}

fn desktop_entry_programs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut results = Vec::new();
    for path in dirs.iter().flat_map(|dir| child_paths(dir)) {
        if path.extension().is_none_or(|ext| ext != "desktop") {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        if desktop_exec_value(&text)
            .is_none_or(|exec| exec.to_ascii_lowercase().contains("superba"))
        {
            continue;
        }
        let Some(argv) = parse_desktop_exec(&text) else {
            continue;
        };
        let Some(program) = argv.first().map(Path::new) else {
            continue;
        };
        if argv
            .iter()
            .any(|arg| arg.to_ascii_lowercase().contains("superba"))
            || !matches!(
                program.file_name().and_then(|name| name.to_str()),
                Some("ChatGPT" | "chatgpt" | "Codex" | "codex-launcher" | "codex-desktop")
            )
        {
            continue;
        }
        results.push(program.to_owned());
    }
    results
}

fn desktop_targets(programs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for program in programs {
        // A bare command needs the caller's PATH; never resolve it via the process cwd.
        if !program.is_absolute() {
            continue;
        }
        let program = fs::canonicalize(&program).unwrap_or(program);
        if let Some(parent) = program
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            && seen.insert(parent.to_owned())
        {
            results.push(parent.to_owned());
        }
    }
    results
}

fn appimage_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    name.ends_with(".appimage") && (name.starts_with("chatgpt") || name.starts_with("codex"))
}

/// Build an install for a ChatGPT/Codex AppImage file, with no inferred versions or bundled CLI.
pub fn appimage_install(path: &Path) -> Option<AppInstall> {
    if !appimage_name(path) || !path.is_file() {
        return None;
    }
    let variant = if path
        .file_name()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .starts_with("chatgpt")
    {
        AppVariant::ChatGpt
    } else {
        AppVariant::Codex
    };
    let path = fs::canonicalize(path).ok()?;
    Some(AppInstall {
        id: format!("path:{}", path.display()),
        variant,
        executable: path.clone(),
        path,
        app_version: None,
        bundled_codex: None,
        bundled_codex_version: None,
    })
}

/// List platform candidate locations using only the supplied environment and home for user roots.
pub fn candidate_paths(env: &dyn EnvSource, home_dir: &Path) -> Vec<PathBuf> {
    let roots = candidate_roots(env, home_dir);
    let mut paths = roots.installs;
    let mut programs = desktop_entry_programs(&roots.desktop_dirs);
    if let Some(path_value) = env.var_os("PATH") {
        for program in &mut programs {
            if program.components().count() == 1
                && let Some(resolved) = std::env::split_paths(&path_value)
                    .map(|dir| {
                        if dir.is_absolute() {
                            dir
                        } else {
                            home_dir.join(dir)
                        }
                    })
                    .map(|dir| dir.join(&*program))
                    .find(|path| path.is_file())
            {
                *program = resolved;
            }
        }
    }
    paths.extend(desktop_targets(programs));
    for path in roots.scan_dirs.iter().flat_map(|dir| child_paths(dir)) {
        #[cfg(target_os = "linux")]
        if appimage_name(&path) && path.is_file() {
            paths.push(path);
        }
        #[cfg(target_os = "macos")]
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
            && let Ok(plist) = fs::read_to_string(path.join("Contents/Info.plist"))
            && let Some(id) = parse_plist_string(&plist, "CFBundleIdentifier")
            && (matches!(id.as_str(), "com.openai.chat" | "com.openai.chatgpt")
                || (id.starts_with("com.openai.codex") && id != "com.openai.codex.cli"))
        {
            paths.push(path);
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = path;
    }
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

/// Registry key (under `HKCU` and `HKLM\\SOFTWARE\\Classes`) that lists the registered
/// MSIX packages by full name, each with a `PackageRootFolder` value.
#[cfg(target_os = "windows")]
const APPX_REPOSITORY_KEY: &str =
    "Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";

/// Split an MSIX package full name (`Name_Version_Arch_ResourceId_PublisherId`) into
/// the package it names. Resource packages (a non-empty `ResourceId`) and anything
/// that is not one of the OpenAI desktop families give `None`.
pub fn parse_appx_full_name(full_name: &str, root: &str) -> Option<AppxPackage> {
    let parts: Vec<&str> = full_name.split('_').collect();
    let [name, version, _arch, resource, publisher] = parts.as_slice() else {
        return None;
    };
    if !resource.is_empty()
        || !matches!(
            *name,
            "OpenAI.Codex" | "OpenAI.CodexBeta" | "OpenAI.ChatGPT-Desktop"
        )
        || root.is_empty()
    {
        return None;
    }
    Some(AppxPackage {
        name: (*name).to_owned(),
        family_name: format!("{name}_{publisher}"),
        install_location: PathBuf::from(root),
        version: Some((*version).to_owned()),
    })
}

/// Of several registered versions of one family keep the newest.
pub fn newest_appx_per_family(mut packages: Vec<AppxPackage>) -> Vec<AppxPackage> {
    fn key(package: &AppxPackage) -> Vec<u64> {
        package
            .version
            .as_deref()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    }
    packages.sort_by_key(|package| std::cmp::Reverse(key(package)));
    let mut seen = HashSet::new();
    packages.retain(|package| seen.insert(package.family_name.clone()));
    packages
}

/// The registered OpenAI MSIX packages, read from the registry: no child process.
/// `None` when the repository key cannot be opened at all.
#[cfg(target_os = "windows")]
fn registry_appx_packages() -> Option<Vec<AppxPackage>> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let hives = [
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!("Software\\Classes\\{APPX_REPOSITORY_KEY}")),
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!("SOFTWARE\\Classes\\{APPX_REPOSITORY_KEY}")),
    ];
    let mut readable = false;
    let mut packages = Vec::new();
    for hive in hives.into_iter().flatten() {
        readable = true;
        for full_name in hive.enum_keys().flatten() {
            if !full_name.starts_with("OpenAI.") {
                continue;
            }
            let Ok(sub) = hive.open_subkey(&full_name) else {
                continue;
            };
            let Ok(root) = sub.get_value::<String, _>("PackageRootFolder") else {
                continue;
            };
            packages.extend(parse_appx_full_name(&full_name, &root));
        }
    }
    readable.then(|| newest_appx_per_family(packages))
}

/// Subkey count and last write time of the MSIX package repository keys
/// (`HKCU` and `HKLM`): they change when a package is added, updated or
/// removed. One `RegQueryInfoKey` per hive, no enumeration.
#[cfg(target_os = "windows")]
pub(crate) fn registry_appx_stamp() -> Vec<(u32, u32, u32)> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    [
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!("Software\\Classes\\{APPX_REPOSITORY_KEY}")),
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!("SOFTWARE\\Classes\\{APPX_REPOSITORY_KEY}")),
    ]
    .into_iter()
    .map(|key| {
        key.and_then(|k| k.query_info())
            .map(|info| {
                let at = &*info.last_write_time;
                (info.sub_keys, at.dwHighDateTime, at.dwLowDateTime)
            })
            .unwrap_or_default()
    })
    .collect()
}

/// `Get-AppxPackage` through a hidden PowerShell: only when the registry cannot be
/// read, and then once until [`forget_probe_caches`] (an explicit refresh), so a
/// detection repeated on a timer never starts PowerShell.
#[cfg(target_os = "windows")]
async fn powershell_appx_packages(env: &dyn EnvSource, home_dir: &Path) -> Vec<AppxPackage> {
    use std::sync::Mutex;

    static CACHE: Mutex<Option<(u64, Vec<AppxPackage>)>> = Mutex::new(None);
    let epoch = probe_epoch();
    if let Ok(cache) = CACHE.lock()
        && let Some((at, packages)) = cache.as_ref()
        && *at == epoch
    {
        return packages.clone();
    }
    // Resolve PowerShell from the supplied environment, never the process PATH.
    let powershell = env_path(env, "SystemRoot")
        .map(|root| root.join("System32/WindowsPowerShell/v1.0/powershell.exe"))
        .filter(|path| path.is_file())
        .or_else(|| {
            std::env::split_paths(&env.var_os("PATH")?)
                .map(|dir| {
                    if dir.is_absolute() {
                        dir
                    } else {
                        home_dir.join(dir)
                    }
                })
                .map(|dir| dir.join("powershell.exe"))
                .find(|path| path.is_file())
        });
    let packages = match powershell {
        None => Vec::new(),
        Some(powershell) => {
            let mut command = Command::from(superba_core::process::command(powershell));
            command.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command",
                "Get-AppxPackage -Name 'OpenAI.*' | Select-Object Name,PackageFamilyName,InstallLocation,Version | ConvertTo-Json -Compress"])
                .stdin(Stdio::null()).kill_on_drop(true);
            match timeout(Duration::from_secs(10), command.output()).await {
                Ok(Ok(output)) if output.status.success() => {
                    parse_appx_packages_json(&String::from_utf8_lossy(&output.stdout))
                }
                _ => Vec::new(),
            }
        }
    };
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((epoch, packages.clone()));
    }
    packages
}

#[cfg(target_os = "windows")]
async fn msix_installs(env: &dyn EnvSource, home_dir: &Path) -> Vec<AppInstall> {
    let packages = match registry_appx_packages() {
        Some(packages) => packages,
        None => powershell_appx_packages(env, home_dir).await,
    };
    packages
        .into_iter()
        .filter_map(|package| {
            let mut install = inspect_install(&package.install_location)?;
            install.id = package.family_name;
            install.variant = variant(&install.id, &install.executable, None);
            install.app_version = install.app_version.or(package.version);
            Some(install)
        })
        .collect()
}

/// Detect explicit, platform and MSIX installs, deduplicating by canonical main executable.
/// Probe missing bundled CLI versions with a five-second timeout after discovery.
pub async fn detect_installs(env: &dyn EnvSource, home_dir: &Path) -> Vec<AppInstall> {
    let mut installs: Vec<_> = explicit_install(env).into_iter().collect();
    installs.extend(
        candidate_paths(env, home_dir)
            .iter()
            .filter_map(|path| inspect_install(path)),
    );
    #[cfg(target_os = "windows")]
    installs.extend(msix_installs(env, home_dir).await);
    let mut seen = HashMap::<PathBuf, usize>::new();
    let mut unique: Vec<AppInstall> = Vec::new();
    for install in installs {
        let executable =
            fs::canonicalize(&install.executable).unwrap_or_else(|_| install.executable.clone());
        if let Some(&index) = seen.get(&executable) {
            let first = &mut unique[index];
            // An explicit MSIX path keeps its priority but gains the discovered family id.
            if first.id.starts_with("path:") && !install.id.starts_with("path:") {
                first.id = install.id;
                first.variant = install.variant;
            }
            first.app_version = first.app_version.take().or(install.app_version);
            first.bundled_codex_version = first
                .bundled_codex_version
                .take()
                .or(install.bundled_codex_version);
        } else {
            seen.insert(executable, unique.len());
            unique.push(install);
        }
    }
    for install in &mut unique {
        if install.bundled_codex_version.is_none()
            && let Some(binary) = &install.bundled_codex
        {
            install.bundled_codex_version = codex_version(binary).await;
        }
    }
    unique
}

/// Bumped by [`forget_probe_caches`]: cached probe answers from an earlier epoch
/// are asked again.
static PROBE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current probe epoch.
pub(crate) fn probe_epoch() -> u64 {
    PROBE_EPOCH.load(std::sync::atomic::Ordering::SeqCst)
}

/// Forget every remembered `--version` answer (and, on Windows, the PowerShell
/// package list): the next detection asks each binary again. For an explicit
/// refresh only; detection on a timer relies on file stamps instead.
pub fn forget_probe_caches() {
    PROBE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// What identifies the answer of `binary --version`: the stamp of the file and,
/// for a launcher script, of the program it runs.
fn version_key(
    binary: &Path,
) -> Option<(
    crate::codexbin::FileStamp,
    Option<crate::codexbin::FileStamp>,
)> {
    let own = crate::codexbin::FileStamp::of(binary)?;
    let target = crate::codexid::resolve_binary(binary);
    let target = (target != binary)
        .then(|| crate::codexbin::FileStamp::of(&target))
        .flatten();
    Some((own, target))
}

/// Run the supplied binary with `--version`, null stdin and a five-second deadline.
/// A launcher script is not run when the program it execs is known
/// ([`crate::codexid::resolve_binary`]): that program is asked, and the
/// launcher only if it does not answer.
/// A timed-out child is killed; unsuccessful commands and unparseable output yield `None`.
///
/// Answers (also "no answer") are remembered per binary file and, for a
/// launcher, the program it runs (path, size, mtime) until
/// [`forget_probe_caches`]: the same file always reports the same version, so a
/// detection repeated on a timer spawns nothing, and an updated binary (new
/// stamp) is asked again at once.
pub async fn codex_version(binary: &Path) -> Option<CodexVersion> {
    use std::sync::Mutex;

    type Key = (
        crate::codexbin::FileStamp,
        Option<crate::codexbin::FileStamp>,
    );
    type Cache = Mutex<HashMap<Key, (u64, Option<CodexVersion>)>>;
    static CACHE: std::sync::LazyLock<Cache> = std::sync::LazyLock::new(Default::default);

    let epoch = probe_epoch();
    let key = version_key(binary);
    if let Some(key) = &key
        && let Ok(cache) = CACHE.lock()
        && let Some((at, version)) = cache.get(key)
        && *at == epoch
    {
        return version.clone();
    }
    let version = probe_codex_version(binary).await;
    if let Some(key) = key
        && let Ok(mut cache) = CACHE.lock()
    {
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, (epoch, version.clone()));
    }
    version
}

async fn probe_codex_version(binary: &Path) -> Option<CodexVersion> {
    let target = crate::codexid::resolve_binary(binary);
    if target != binary
        && let Some(version) = version_of(&target).await
    {
        return Some(version);
    }
    version_of(binary).await
}

async fn version_of(binary: &Path) -> Option<CodexVersion> {
    let binary = fs::canonicalize(binary).ok()?;
    let mut command = Command::from(superba_core::process::command(binary));
    command
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    // A binary that is being rewritten (an npm / brew update, or a writer that
    // still holds it open) fails with ETXTBSY for a moment: retry briefly.
    let mut attempt = 0;
    let output = loop {
        attempt += 1;
        match timeout(Duration::from_secs(5), command.output())
            .await
            .ok()?
        {
            Ok(output) => break output,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 5 => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(_) => return None,
        }
    };
    if !output.status.success() {
        return None;
    }
    CodexVersion::parse(&String::from_utf8_lossy(&output.stdout))
        .or_else(|_| CodexVersion::parse(&String::from_utf8_lossy(&output.stderr)))
        .ok()
}

/// Resolve Electron's user-data directory for DevToolsActivePort using the supplied app home.
/// Owl's directory name defaults to `Codex`; Windows returns `None` because of MSIX virtualization.
pub fn user_data_dir(
    install: &AppInstall,
    env: &dyn EnvSource,
    home_dir: &Path,
) -> Option<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let resources = [
            install.path.join("Contents/Resources"),
            install.executable.parent()?.join("resources"),
            install.path.join("resources"),
        ]
        .into_iter()
        .find(|path| path.is_dir());
        let name = resources
            .as_deref()
            .map(read_owl)
            .unwrap_or_default()
            .user_data_dir_name
            .filter(|name| {
                let mut components = Path::new(name).components();
                matches!(components.next(), Some(Component::Normal(_)))
                    && components.next().is_none()
            })
            .unwrap_or_else(|| "Codex".to_owned());
        #[cfg(target_os = "linux")]
        let base = env_path(env, "XDG_CONFIG_HOME").unwrap_or_else(|| home_dir.join(".config"));
        #[cfg(target_os = "macos")]
        let base = {
            let _ = env;
            home_dir.join("Library/Application Support")
        };
        Some(base.join(name))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (install, env, home_dir);
        None
    }
}
