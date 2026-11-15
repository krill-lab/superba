//! Desktop install discovery and file-format readers with explicit filesystem roots.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use superba_core::codex::ENV_SUPERBA_APP_PATH;
use superba_core::{AppInstall, AppLocationCheck, AppVariant, CodexVersion, EnvSource};
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

/// Every `<Application>` of an Appx manifest in document order, as
/// `(Id, Executable)` (the executable is package relative, `app/ChatGPT.exe`).
pub fn parse_appx_applications(xml: &str) -> Vec<(String, Option<String>)> {
    let mut remaining = xml;
    let mut apps = Vec::new();
    while let Some(tag) = next_xml_tag(&mut remaining) {
        if !tag.closing
            && tag.name.rsplit(':').next() == Some("Application")
            && let Some(id) = xml_attribute(tag.attributes, "Id")
        {
            apps.push((id, xml_attribute(tag.attributes, "Executable")));
        }
    }
    apps
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

/// Whether a macOS `CFBundleIdentifier` is the ChatGPT or Codex desktop app:
/// `com.openai.chat`, `com.openai.chatgpt`, `com.openai.codex` and its
/// dotted channels (`com.openai.codex.beta`), but not the bundled CLI
/// (`com.openai.codex.cli`) nor the installer (`com.openai.codex.installer`).
/// Exact, case-sensitive ids: a prefix such as `com.openai.codexfoo` or a
/// foreign Electron app (`com.anysphere.*`, Cursor) is not one.
pub fn is_desktop_bundle_id(id: &str) -> bool {
    match id {
        "com.openai.chat" | "com.openai.chatgpt" | "com.openai.codex" => true,
        "com.openai.codex.cli" | "com.openai.codex.installer" => false,
        _ => id.strip_prefix("com.openai.codex.").is_some_and(|channel| {
            !channel.is_empty()
                && channel
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
        }),
    }
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
    // Other Electron apps (Cursor's `com.anysphere.*`, VS Code, Slack...) have
    // the same layout: only the ChatGPT / Codex bundle ids are an install.
    if !is_desktop_bundle_id(&id) {
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

/// The registered MSIX package a directory is the install folder of: the
/// folder name is the package full name (`OpenAI.Codex_<ver>_x64__<publisher>`)
/// and the manifest sits in it. Only the name is parsed (nothing is listed).
fn package_of_root(root: &Path) -> Option<AppxPackage> {
    let name = root.file_name()?.to_str()?;
    let package = parse_appx_full_name(name, &root.to_string_lossy())?;
    root.join("AppxManifest.xml").is_file().then_some(package)
}

fn install_from_resources(root: &Path, executable: PathBuf, resources: &Path) -> AppInstall {
    let (version, brand) = fs::read_to_string(resources.join("linux-package-metadata.json"))
        .map(|text| parse_linux_package_metadata(&text))
        .unwrap_or_default();
    // An MSIX folder is named by its package family, as registry discovery does,
    // so a running app found by its executable has the same id as the install.
    let package = package_of_root(root);
    let id = match &package {
        Some(p) => p.family_name.clone(),
        None => format!("path:{}", root.display()),
    };
    let (bundled_codex, bundled_codex_version) = bundled_cli(resources);
    AppInstall {
        variant: variant(&id, &executable, brand.as_deref()),
        id,
        path: root.to_owned(),
        executable,
        app_version: read_owl(resources)
            .app_version
            .or(version)
            .or(package.and_then(|p| p.version)),
        bundled_codex,
        bundled_codex_version,
    }
}

/// True when `path` is a file, or exists but may not be inspected (a package
/// file under `WindowsApps` whose folder grants no read to this user).
fn present_file(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => meta.is_file(),
        Err(e) => e.kind() == std::io::ErrorKind::PermissionDenied,
    }
}

/// The install of a registered MSIX package, built from its manifest: the main
/// application's executable (`app/ChatGPT.exe`) and the `resources` folder next
/// to it. Only files named by the manifest are opened, by full path; the
/// package folder and `WindowsApps` are never listed. Without a readable
/// manifest the known layout of the OpenAI package is assumed.
pub fn appx_install(package: &AppxPackage) -> Option<AppInstall> {
    let root = crate::appx::plain_path(&package.install_location);
    let manifest = fs::read_to_string(root.join("AppxManifest.xml")).ok();
    let apps = manifest
        .as_deref()
        .map(parse_appx_applications)
        .unwrap_or_default();
    let relative = crate::appx::main_application(&apps)
        .and_then(|(_, exe)| exe.clone())
        .unwrap_or_else(|| "app/ChatGPT.exe".to_owned());
    let executable = crate::appx::package_file(&root, &relative)?;
    let resources = executable.parent()?.join("resources");
    let (mut bundled_codex, bundled_codex_version) = bundled_cli(&resources);
    if bundled_codex.is_none() && present_file(&resources.join("codex.exe")) {
        bundled_codex = Some(resources.join("codex.exe"));
    }
    let id = package.family_name.clone();
    Some(AppInstall {
        variant: variant(&id, &executable, None),
        id,
        path: root,
        executable,
        app_version: read_owl(&resources).app_version.or(package.version.clone()),
        bundled_codex,
        bundled_codex_version,
    })
}

/// Whether `id` names `install`: the same id, a package family in another
/// case, or a `path:` id (an older form, also with a verbatim `\\?\` prefix)
/// of its folder or executable.
pub fn install_matches(install: &AppInstall, id: &str) -> bool {
    if install.id == id || (crate::appx::is_family_name(id) && install.id.eq_ignore_ascii_case(id))
    {
        return true;
    }
    let Some(path) = id.strip_prefix("path:") else {
        return false;
    };
    let wanted = crate::appx::plain_path(Path::new(path));
    let same = |p: &Path| same_file_name(&crate::appx::plain_path(p), &wanted);
    same(&install.path)
        || same(&install.executable)
        || install
            .id
            .strip_prefix("path:")
            .is_some_and(|own| same(Path::new(own)))
}

/// Path equality, ignoring ASCII case on Windows (paths are case-insensitive there).
fn same_file_name(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
    } else {
        a == b
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
    let path = crate::appx::canonical(path).ok()?;
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
    explicit_install_among(env, &[])
}

/// [`explicit_install`] with a location inside an MSIX package folder mapped
/// to the installed version of that package's family ([`located_install`]).
fn explicit_install_among(env: &dyn EnvSource, packages: &[AppInstall]) -> Option<AppInstall> {
    [ENV_SUPERBA_APP_PATH, ENV_SUPERBA_APP_DIR]
        .into_iter()
        .filter_map(|key| env.var_os(key).filter(|value| !value.is_empty()))
        .find_map(|path| located_install(Path::new(&path), packages))
}

/// The OpenAI MSIX package whose folder `path` is in or below
/// (`…\WindowsApps\OpenAI.Codex_26.1103.4239.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe`):
/// its family, version and folder. Parsed from the text only (both separators,
/// any case), so it works for a folder that no longer exists.
pub fn windows_apps_package(path: &Path) -> Option<AppxPackage> {
    let text = path.to_string_lossy();
    let mut parts = Vec::new();
    let mut start = 0;
    for (at, c) in text.char_indices() {
        if c == '/' || c == '\\' {
            parts.push((start, at));
            start = at + 1;
        }
    }
    parts.push((start, text.len()));
    parts.windows(2).find_map(|pair| {
        let [(a, b), (c, d)] = pair else {
            return None;
        };
        if !text[*a..*b].eq_ignore_ascii_case("WindowsApps") {
            return None;
        }
        parse_appx_full_name(&text[*c..*d], &text[..*d])
    })
}

/// The install at a location the user gave (`SUPERBA_APP_PATH`, the app
/// location of the settings). A location inside an MSIX package folder names
/// one version of the package, whose folder an app update replaces: it stands
/// for its package family, so the registered version among `packages` is used
/// (a stale version's folder is never inspected); without one registered the
/// folder itself, while it is still there.
pub fn located_install(path: &Path, packages: &[AppInstall]) -> Option<AppInstall> {
    let Some(package) = windows_apps_package(path) else {
        return inspect_install(path);
    };
    if let Some(current) = packages
        .iter()
        .find(|i| i.id.eq_ignore_ascii_case(&package.family_name))
    {
        if !same_file_name(
            &crate::appx::plain_path(&current.path),
            &crate::appx::plain_path(&package.install_location),
        ) {
            tracing::info!(
                given = %path.display(),
                installed = %current.path.display(),
                "the app location names another version of the Store app's package; using the installed one"
            );
        }
        return Some(current.clone());
    }
    let found = inspect_install(path);
    if found.is_none() {
        tracing::warn!(
            given = %path.display(),
            family = %package.family_name,
            "the app location is a Store app package folder that is gone, and the package is not installed; ignored"
        );
    }
    found
}

/// What a location the user picked holds (Settings → ChatGPT app location):
/// [`inspect_install`] of it, told apart into a missing path, a path that is
/// not the desktop app, and an app without its bundled Codex. The bundled
/// Codex's version is read from its package JSON only; see
/// [`check_location_versions`].
pub fn check_location(path: &Path) -> AppLocationCheck {
    // A folder of the Store app's package: found without a location, and
    // replaced by every app update (a saved one would go stale).
    if let Some(package) = windows_apps_package(path) {
        return AppLocationCheck::StorePackage {
            family: package.family_name,
        };
    }
    if fs::symlink_metadata(path).is_err() {
        return AppLocationCheck::Missing;
    }
    match inspect_install(path) {
        Some(install) if install.bundled_codex.is_some() => AppLocationCheck::Install { install },
        Some(install) => AppLocationCheck::NoBundledCodex { install },
        None => AppLocationCheck::NotAnApp,
    }
}

/// [`check_location`] with the bundled Codex's version asked
/// ([`bundled_codex_version`]) when its package JSON has none, as detection does.
pub async fn check_location_versions(path: &Path, env: &dyn EnvSource) -> AppLocationCheck {
    let path = path.to_owned();
    let mut check = tokio::task::spawn_blocking(move || check_location(&path))
        .await
        .unwrap_or(AppLocationCheck::NotAnApp);
    if let AppLocationCheck::Install { install } = &mut check
        && install.bundled_codex_version.is_none()
        && let Some(binary) = install.bundled_codex.clone()
    {
        install.bundled_codex_version = bundled_codex_version(&binary, env).await;
    }
    check
}

/// The values of one `...\CurrentVersion\Uninstall\<key>` registry entry that
/// locate an install. Read on Windows by `registry_uninstall_entries`; the
/// parsing below is portable so Linux tests cover it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UninstallEntry {
    /// `DisplayName`.
    pub display_name: Option<String>,
    /// `Publisher`.
    pub publisher: Option<String>,
    /// `InstallLocation`.
    pub install_location: Option<String>,
    /// `DisplayIcon` (`"C:\...\ChatGPT.exe",0`).
    pub display_icon: Option<String>,
    /// `UninstallString` (`"C:\...\Uninstall ChatGPT.exe" /currentuser`).
    pub uninstall_string: Option<String>,
}

/// Whether an uninstall entry may be the ChatGPT / Codex desktop app: its name
/// names ChatGPT or Codex, or OpenAI published it. Superba's own entry is not.
/// Every location derived from it is still validated by [`inspect_install`].
pub fn uninstall_entry_matches(entry: &UninstallEntry) -> bool {
    let lower = |v: &Option<String>| v.as_deref().unwrap_or_default().to_lowercase();
    let name = lower(&entry.display_name);
    let publisher = lower(&entry.publisher);
    if name.contains("superba") {
        return false;
    }
    name.contains("chatgpt") || name.contains("codex") || publisher.contains("openai")
}

/// The install directories an uninstall entry points to, in this order and
/// without duplicates: `InstallLocation`, the folder of `DisplayIcon`, the
/// folder of the program of `UninstallString` (not `MsiExec.exe`). `%VAR%`
/// references (`REG_EXPAND_SZ`) are expanded from `env`; relative paths are
/// dropped.
pub fn uninstall_entry_dirs(entry: &UninstallEntry, env: &dyn EnvSource) -> Vec<PathBuf> {
    let mut dirs: Vec<String> = Vec::new();
    let mut push = |dir: Option<String>| {
        if let Some(dir) = dir.map(|d| trim_separators(&d))
            && is_absolute_any(&dir)
            && !dirs.iter().any(|d| d.eq_ignore_ascii_case(&dir))
        {
            dirs.push(dir);
        }
    };
    push(
        entry
            .install_location
            .as_deref()
            .map(|v| expand_percent_vars(&unquote(v), env))
            .filter(|v| !v.is_empty()),
    );
    push(
        entry
            .display_icon
            .as_deref()
            .map(icon_file)
            .map(|v| expand_percent_vars(&v, env))
            .and_then(|v| parent_dir(&v)),
    );
    push(
        entry
            .uninstall_string
            .as_deref()
            .and_then(command_program)
            .filter(|program| {
                !file_name_of(program).eq_ignore_ascii_case("msiexec.exe")
                    && !file_name_of(program).eq_ignore_ascii_case("msiexec")
            })
            .map(|v| expand_percent_vars(&v, env))
            .and_then(|v| parent_dir(&v)),
    );
    dirs.into_iter().map(PathBuf::from).collect()
}

/// The install directories of the matching entries, in entry order, without
/// duplicates.
pub fn uninstall_candidates(entries: &[UninstallEntry], env: &dyn EnvSource) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in entries
        .iter()
        .filter(|e| uninstall_entry_matches(e))
        .flat_map(|e| uninstall_entry_dirs(e, env))
    {
        if !out.iter().any(|d| same_file_name(d, &dir)) {
            out.push(dir);
        }
    }
    out
}

/// A value without surrounding whitespace and, when it starts with one, the
/// text inside its first pair of double quotes.
fn unquote(value: &str) -> String {
    let value = value.trim();
    match value.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default().trim().to_owned(),
        None => value.to_owned(),
    }
}

/// The file of a `DisplayIcon` value: quotes and the icon index (`,0`, `,-101`)
/// removed.
fn icon_file(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('"') {
        return unquote(value);
    }
    match value.rsplit_once(',') {
        Some((file, index))
            if !index.trim().is_empty()
                && index
                    .trim()
                    .trim_start_matches('-')
                    .bytes()
                    .all(|b| b.is_ascii_digit()) =>
        {
            file.trim().to_owned()
        }
        _ => value.to_owned(),
    }
}

/// The program of a command line: the quoted first part, else everything up to
/// and including the first `.exe`, else the first word.
fn command_program(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if command.starts_with('"') {
        return Some(unquote(command)).filter(|p| !p.is_empty());
    }
    if let Some(at) = command.to_ascii_lowercase().find(".exe") {
        return Some(command[..at + 4].to_owned());
    }
    command.split_whitespace().next().map(str::to_owned)
}

/// Replace `%NAME%` with the value of `NAME` in `env`; unknown names stay.
fn expand_percent_vars(value: &str, env: &dyn EnvSource) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        match env
            .var_os(name)
            .filter(|v| !name.is_empty() && !v.is_empty())
        {
            Some(v) => {
                out.push_str(&v.to_string_lossy());
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                out.push_str(name);
                rest = &after[end..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A Windows or Unix path is absolute: `C:\`, `C:/`, `\\server\share` or `/`.
fn is_absolute_any(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with("\\\\")
        || path.starts_with('/')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

/// Trailing `\` / `/` removed, except from a root (`C:\`, `/`).
fn trim_separators(path: &str) -> String {
    let path = path.trim();
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return path.chars().take(1).collect();
    }
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return format!("{trimmed}\\");
    }
    trimmed.to_owned()
}

/// The last component of a path with `\` or `/` separators.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// The folder of a file path with `\` or `/` separators (a path is parsed by
/// its text, so a Windows value reads the same on every platform).
fn parent_dir(path: &str) -> Option<String> {
    let path = path.trim().trim_end_matches(['\\', '/']);
    let at = path.rfind(['\\', '/'])?;
    Some(trim_separators(&path[..=at]))
}

/// The uninstall entries of the registry (`HKCU` and `HKLM`, both views:
/// `Software\Microsoft\Windows\CurrentVersion\Uninstall` and its
/// `WOW6432Node` twin). Only `DisplayName` and `Publisher` are read for every
/// entry; the location values only for the ones that match.
#[cfg(target_os = "windows")]
fn registry_uninstall_entries() -> Vec<UninstallEntry> {
    use winreg::RegKey;

    let mut entries = Vec::new();
    for (hive, path) in uninstall_keys() {
        let Ok(key) = RegKey::predef(hive).open_subkey(path) else {
            continue;
        };
        for name in key.enum_keys().flatten() {
            let Ok(sub) = key.open_subkey(&name) else {
                continue;
            };
            let value = |name: &str| {
                sub.get_value::<String, _>(name)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            };
            let mut entry = UninstallEntry {
                display_name: value("DisplayName"),
                publisher: value("Publisher"),
                ..Default::default()
            };
            if !uninstall_entry_matches(&entry) {
                continue;
            }
            entry.install_location = value("InstallLocation");
            entry.display_icon = value("DisplayIcon");
            entry.uninstall_string = value("UninstallString");
            entries.push(entry);
        }
    }
    entries
}

/// The uninstall keys [`registry_uninstall_entries`] reads.
#[cfg(target_os = "windows")]
fn uninstall_keys() -> [(winreg::HKEY, &'static str); 4] {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    const NATIVE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
    const WOW: &str = "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
    [
        (HKEY_CURRENT_USER, NATIVE),
        (HKEY_LOCAL_MACHINE, NATIVE),
        (HKEY_LOCAL_MACHINE, WOW),
        (HKEY_CURRENT_USER, WOW),
    ]
}

/// Subkey count and last write time of the uninstall keys: they change when an
/// installer adds, updates or removes its entry. One `RegQueryInfoKey` each.
#[cfg(target_os = "windows")]
pub(crate) fn registry_uninstall_stamp() -> Vec<(u32, u32, u32)> {
    use winreg::RegKey;

    uninstall_keys()
        .into_iter()
        .map(|(hive, path)| {
            RegKey::predef(hive)
                .open_subkey(path)
                .and_then(|k| k.query_info())
                .map(|info| {
                    let at = &*info.last_write_time;
                    (info.sub_keys, at.dwHighDateTime, at.dwLowDateTime)
                })
                .unwrap_or_default()
        })
        .collect()
}

/// The executables of running desktop app main processes, and the ones seen
/// running earlier in this process: an app started from a folder no other
/// source names stays known after it quits (still validated at every
/// detection, so a removed one drops out).
#[cfg(target_os = "windows")]
async fn running_app_executables() -> Vec<PathBuf> {
    use std::sync::Mutex;

    static SEEN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let running = tokio::task::spawn_blocking(|| {
        let snap = crate::procs::snapshot(crate::procs::ReadOpts { environ: false });
        snap.app_mains()
            .into_iter()
            .filter_map(|pid| snap.all.get(&pid).and_then(|e| e.exe.clone()))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    for exe in running {
        if !seen.iter().any(|s| same_file_name(s, &exe)) {
            seen.push(exe);
        }
    }
    seen.clone()
}

/// Install locations found outside the default folders: on Windows the
/// uninstall entries of the registry, then the running app's executable.
/// Nothing elsewhere (Linux and macOS scan their usual folders and `.desktop`
/// entries, [`candidate_paths`]).
async fn discovered_paths(env: &dyn EnvSource) -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let entries = tokio::task::spawn_blocking(registry_uninstall_entries)
            .await
            .unwrap_or_default();
        let mut paths = uninstall_candidates(&entries, env);
        paths.extend(running_app_executables().await);
        paths
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = env;
        Vec::new()
    }
}

pub(crate) struct CandidateRoots {
    pub(crate) installs: Vec<PathBuf>,
    pub(crate) desktop_dirs: Vec<PathBuf>,
    pub(crate) scan_dirs: Vec<PathBuf>,
}

// Linux and Windows only (macOS paths come from the home directory).
#[cfg(not(target_os = "macos"))]
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
    let local = env_path(env, "LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"));
    CandidateRoots {
        installs: vec![
            local.join("Programs").join("ChatGPT"),
            local.join("Programs").join("Codex"),
        ],
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
        let program = superba_core::paths::canonical(&program).unwrap_or(program);
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
    let path = crate::appx::canonical(path).ok()?;
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
            && is_desktop_bundle_id(&id)
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
pub(crate) const APPX_REPOSITORY_KEY: &str =
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
        .map(|root| {
            root.join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
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
    packages.iter().filter_map(appx_install).collect()
}

/// Detect installs, deduplicating by canonical main executable, in this order
/// (the first is the default install):
///
/// 1. `SUPERBA_APP_PATH` / `SUPERBA_APP_DIR` ([`explicit_install`]);
/// 2. `location`, the app location of Superba's settings;
/// 3. the platform's default locations ([`candidate_paths`]), then on Windows
///    the registered MSIX packages;
/// 4. installs discovered elsewhere: on Windows the uninstall entries of the
///    registry and the running app's executable.
///
/// Every location is validated by [`inspect_install`]. Missing bundled CLI
/// versions are probed with a five-second timeout after discovery.
pub async fn detect_installs(
    env: &dyn EnvSource,
    home_dir: &Path,
    location: Option<&Path>,
) -> Vec<AppInstall> {
    let discovered = discovered_paths(env).await;
    detect_installs_from(env, home_dir, location, &discovered).await
}

/// [`detect_installs`] with the discovered locations (step 4) given, so the
/// order and the deduplication are testable without a registry or processes.
pub async fn detect_installs_from(
    env: &dyn EnvSource,
    home_dir: &Path,
    location: Option<&Path>,
    discovered: &[PathBuf],
) -> Vec<AppInstall> {
    #[cfg(target_os = "windows")]
    let packages = msix_installs(env, home_dir).await;
    #[cfg(not(target_os = "windows"))]
    let packages = Vec::new();
    detect_installs_among(env, home_dir, location, discovered, packages).await
}

/// [`detect_installs_from`] with the installs of the registered MSIX
/// packages given (step 3 on Windows), so a location inside a package folder
/// ([`located_install`]) is testable on every platform.
pub async fn detect_installs_among(
    env: &dyn EnvSource,
    home_dir: &Path,
    location: Option<&Path>,
    discovered: &[PathBuf],
    packages: Vec<AppInstall>,
) -> Vec<AppInstall> {
    let mut installs: Vec<_> = explicit_install_among(env, &packages).into_iter().collect();
    installs.extend(
        location
            .filter(|path| !path.as_os_str().is_empty())
            .and_then(|path| located_install(path, &packages)),
    );
    installs.extend(
        candidate_paths(env, home_dir)
            .iter()
            .filter_map(|path| inspect_install(path)),
    );
    installs.extend(packages);
    installs.extend(discovered.iter().filter_map(|path| inspect_install(path)));
    let mut seen = HashMap::<PathBuf, usize>::new();
    let mut unique: Vec<AppInstall> = Vec::new();
    for install in installs {
        let executable = crate::appx::canonical(&install.executable)
            .unwrap_or_else(|_| install.executable.clone());
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
            install.bundled_codex_version = bundled_codex_version(binary, env).await;
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

/// The version of an app's bundled Codex: its `--version`. A binary inside an
/// MSIX package (`WindowsApps`), which Windows does not let Superba start, is
/// never run: its version is asked from the app's extracted copy with the same
/// SHA-256 ([`crate::runnable::runnable_codex`], user bug 143), else read from
/// the binary ([`crate::codexbin::scanned_version`], nothing is run).
pub async fn bundled_codex_version(binary: &Path, env: &dyn EnvSource) -> Option<CodexVersion> {
    if !crate::runnable::is_packaged(binary) {
        return codex_version(binary).await;
    }
    let local = crate::runnable::local_app_data(env);
    let path = binary.to_path_buf();
    let copy = tokio::task::spawn_blocking(move || {
        crate::runnable::resolve_packaged(&path, local.as_deref()).ok()
    })
    .await
    .ok()
    .flatten();
    if let Some(copy) = copy
        && let Some(version) = codex_version(&copy).await
    {
        tracing::debug!(copy = %copy.display(), "bundled Codex version from the app's copy");
        return Some(version);
    }
    let path = binary.to_path_buf();
    let version = tokio::task::spawn_blocking(move || crate::codexbin::scanned_version(&path))
        .await
        .ok()
        .flatten();
    if version.is_some() {
        tracing::debug!(binary = %binary.display(), "bundled Codex version read from the binary");
    }
    version
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
    let binary = superba_core::paths::canonical(binary).ok()?;
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
