//! Read-only remote discovery from app state, live SSH links, and SSH config
//! (plan 8.1, spike S1).
//!
//! S1 status: the global-state keys and entry shapes below were taken from static
//! analysis of the desktop app 26.1013 bundle (the app's own loader accepts both the
//! current `{hostId, displayName, source, alias, hostname, sshPort, identity}` form
//! and the legacy `{sshAlias, sshHost, ...}` form, and derives discovered host ids
//! as `remote-ssh-discovered:<encodeURIComponent(alias)>`). They have not yet been
//! observed in a live file written by the app, so every field is parsed defensively,
//! and hosts reported by the renderer (`from_app`, via CDP) are merged in as an
//! independent source of host ids.
//!
//! Host kinds (user bug 36): `codex-managed-remote-connections` holds SSH
//! connections only (the app maps every entry to an SSH connection, ids
//! `remote-ssh-discovered:<alias>` / `remote-ssh-codex-managed:<uuid>` or legacy
//! ones). The renderer also reports WSL distributions (`remote-wsl:<distro>`),
//! ChatGPT remote-control devices (`remote-control:<environment id>`) and hosts
//! run by ChatGPT (`durable`, `chatgpt:<account>:<user>`); those keep their
//! [`HostKind`], never get an SSH endpoint and are never reached over SSH.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde_json::Value;
use superba_core::{
    CodexHome, CodexProcessKind, Error, HostKind, HostSource, ProcessInfo, RemoteHost, Result,
    SshEndpoint, TargetId, looks_like_host_id,
};

/// Global-state key containing the app's remote connections.
pub const KEY_CONNECTIONS: &str = "codex-managed-remote-connections";
/// Global-state key containing remote projects.
pub const KEY_PROJECTS: &str = "remote-projects";
/// Global-state key mapping host IDs to auto-connect preferences.
pub const KEY_AUTO_CONNECT: &str = "remote-connection-auto-connect-by-host-id";
/// Suffix appended to the global-state filename for its backup.
pub const GLOBAL_STATE_BAK_SUFFIX: &str = ".bak";
/// App host-ID prefix for an applied, discovered SSH alias.
pub const DISCOVERED_HOST_ID_PREFIX: &str = "remote-ssh-discovered:";

/// One entry of `codex-managed-remote-connections`, normalised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConnection {
    /// The app's non-empty host identifier.
    pub host_id: String,
    /// Optional display name supplied by the app.
    pub display_name: Option<String>,
    /// Source spelling supplied by the app, including future source values.
    pub source: Option<String>,
    /// SSH config alias, preferred over explicit connection parameters.
    pub alias: Option<String>,
    /// Host name or address without a `user@` prefix.
    pub host: Option<String>,
    /// Explicit user or the user extracted from the host string.
    pub user: Option<String>,
    /// Valid SSH port in the range 1 through 65535.
    pub port: Option<u16>,
    /// Identity path; discovery never reads its contents.
    pub identity: Option<PathBuf>,
    /// Auto-connect preference from [`KEY_AUTO_CONNECT`].
    pub auto_connect: Option<bool>,
    /// Matching projects' remote paths in file order, including duplicates.
    pub project_paths: Vec<String>,
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(key)?
            .as_str()
            .and_then(non_empty)
            .map(str::to_owned)
    })
}

fn split_user_host(value: &str) -> (Option<String>, Option<String>) {
    match value.rsplit_once('@') {
        Some((user, host)) => (
            non_empty(user).map(str::to_owned),
            non_empty(host).map(str::to_owned),
        ),
        None => (None, non_empty(value).map(str::to_owned)),
    }
}

fn valid_port(value: &str) -> Option<u16> {
    value.trim().parse().ok().filter(|port| *port != 0)
}

/// Parse the flat global-state object, skipping entries without a valid host ID.
///
/// Missing or wrongly typed optional fields are treated as absent. Only invalid
/// JSON or a non-object top level returns a `codex global state` parse error.
pub fn parse_global_state(bytes: &[u8]) -> Result<Vec<AppConnection>> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| Error::parse("codex global state", error))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::parse("codex global state", "expected a JSON object"))?;

    let mut projects: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(entries) = object.get(KEY_PROJECTS).and_then(Value::as_array) {
        for entry in entries {
            match (
                string_field(entry, &["hostId"]),
                string_field(entry, &["remotePath"]),
            ) {
                (Some(id), Some(path)) => projects.entry(id).or_default().push(path),
                _ => tracing::debug!("Skipping malformed remote project"),
            }
        }
    }
    let auto_connect = object.get(KEY_AUTO_CONNECT).and_then(Value::as_object);
    let mut connections = Vec::new();
    if let Some(entries) = object.get(KEY_CONNECTIONS).and_then(Value::as_array) {
        for entry in entries {
            let Some(host_id) = string_field(entry, &["hostId"]) else {
                tracing::debug!("Skipping remote connection without a valid hostId");
                continue;
            };
            let (embedded_user, host) = string_field(entry, &["hostname", "sshHost"])
                .map(|host| split_user_host(&host))
                .unwrap_or_default();
            // Numbers are expected; numeric strings are tolerated (unverified schema).
            let port = match entry.get("sshPort") {
                Some(Value::Number(n)) => n
                    .as_u64()
                    .and_then(|port| u16::try_from(port).ok())
                    .filter(|port| *port != 0),
                Some(Value::String(s)) => valid_port(s),
                _ => None,
            };
            connections.push(AppConnection {
                display_name: string_field(entry, &["displayName"]),
                source: string_field(entry, &["source"]),
                alias: string_field(entry, &["alias", "sshAlias"]),
                host,
                user: string_field(entry, &["user", "sshUser"]).or(embedded_user),
                port,
                identity: string_field(entry, &["identity"]).map(PathBuf::from),
                auto_connect: auto_connect.and_then(|preferences| {
                    preferences.iter().find_map(|(id, preference)| {
                        (id.trim() == host_id)
                            .then(|| preference.as_bool())
                            .flatten()
                    })
                }),
                project_paths: projects.get(&host_id).cloned().unwrap_or_default(),
                host_id,
            });
        }
    }
    Ok(connections)
}

/// Return declared aliases in file order, deduplicated after trimming.
///
/// Invalid JSON, a non-object root, and malformed entries are ignored.
pub fn parse_codex_app_config(bytes: &[u8]) -> Vec<String> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut aliases = Vec::new();
    if let Some(entries) = value.get("remoteConnections").and_then(Value::as_array) {
        for entry in entries {
            let Some(alias) = string_field(entry, &["sshAlias"]) else {
                tracing::debug!("Skipping declarative remote connection without an alias");
                continue;
            };
            if seen.insert(alias.clone()) {
                aliases.push(alias);
            }
        }
    }
    aliases
}

/// The alias of a `remote-ssh-discovered:<encodeURIComponent(alias)>` host id
/// (JavaScript `decodeURIComponent`); `None` for other ids or a bad encoding.
pub fn discovered_alias(host_id: &str) -> Option<String> {
    superba_core::discovered_ssh_alias(host_id)
}

/// The kind of an entry of the app's saved connections: SSH, unless its id says
/// otherwise (the app never stores other kinds there; legacy ids are SSH).
pub fn saved_connection_kind(host_id: &str) -> HostKind {
    match HostKind::of_host_id(host_id) {
        HostKind::Unknown => HostKind::Ssh,
        kind => kind,
    }
}

/// Return `remote-ssh-discovered:` plus JavaScript `encodeURIComponent(alias)`.
pub fn discovered_host_id(alias: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut id = String::from(DISCOVERED_HOST_ID_PREFIX);
    for byte in alias.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            id.push(char::from(byte));
        } else {
            id.push('%');
            id.push(char::from(HEX[usize::from(byte >> 4)]));
            id.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    id
}

/// A concrete `Host` entry of an OpenSSH client config.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConfigHost {
    /// Concrete alias from a `Host` line.
    pub alias: String,
    /// First valid `HostName` in the block.
    pub host_name: Option<String>,
    /// First valid `User` in the block.
    pub user: Option<String>,
    /// First valid `Port` in the block.
    pub port: Option<u16>,
    /// First valid `IdentityFile` path, without reading or expanding it.
    pub identity_file: Option<String>,
}

// Tokenize config values without shell expansion. Quoted '#' and spaces remain
// literal; backslashes before ordinary characters (e.g. Windows paths) survive.
fn config_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && chars
                .peek()
                .is_some_and(|next| next.is_whitespace() || matches!(next, '\\' | '\'' | '"' | '#'))
        {
            if let Some(escaped) = chars.next() {
                word.push(escaped);
            }
        } else if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == '#' {
            break;
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    Some(words)
}

fn config_directive(line: &str) -> Option<(String, Vec<String>)> {
    let line = non_empty(line)?;
    if line.starts_with('#') {
        return None;
    }
    let boundary = line.find(|ch: char| ch.is_whitespace() || ch == '=')?;
    let (key, rest) = line.split_at(boundary);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    Some((key.to_ascii_lowercase(), config_words(rest)?))
}

#[derive(Default)]
struct SshConfigParser {
    hosts: Vec<SshConfigHost>,
    active: Vec<usize>,
    in_match: bool,
}

impl SshConfigParser {
    fn directive(&mut self, key: &str, values: &[String]) {
        match key {
            "host" => {
                self.in_match = false;
                self.active.clear();
                for alias in values.iter().filter_map(|value| non_empty(value)) {
                    if !alias.contains(['*', '?', '!']) {
                        self.active.push(self.hosts.len());
                        self.hosts.push(SshConfigHost {
                            alias: alias.to_owned(),
                            ..SshConfigHost::default()
                        });
                    }
                }
            }
            "match" => {
                self.in_match = true;
                self.active.clear();
            }
            _ => {
                let Some(value) = values.first().and_then(|value| non_empty(value)) else {
                    return;
                };
                for &index in &self.active {
                    let host = &mut self.hosts[index];
                    match key {
                        "hostname" if host.host_name.is_none() => {
                            host.host_name = Some(value.to_owned());
                        }
                        "user" if host.user.is_none() => host.user = Some(value.to_owned()),
                        "port" if host.port.is_none() => host.port = valid_port(value),
                        "identityfile" if host.identity_file.is_none() => {
                            host.identity_file = Some(value.to_owned());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Parse concrete SSH `Host` blocks and raw, unexpanded `Include` arguments.
///
/// Keywords ignore case; values support quotes, comments, and `Key=Value`.
/// Patterns containing `*`, `?`, or `!` are skipped. Each block retains its first
/// valid value per option, and `Match` options are ignored until the next `Host`.
pub fn parse_ssh_config(text: &str) -> (Vec<SshConfigHost>, Vec<String>) {
    let mut parser = SshConfigParser::default();
    let mut includes = Vec::new();
    for (key, values) in text.lines().filter_map(config_directive) {
        if key == "include" && !parser.in_match {
            includes.extend(values);
        } else {
            parser.directive(&key, &values);
        }
    }
    (parser.hosts, includes)
}

fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    let mut star = None;
    let mut retry = 0;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = n;
        } else if let Some(index) = star {
            p = index + 1;
            retry += 1;
            n = retry;
        } else {
            return false;
        }
    }
    while pattern.get(p) == Some(&'*') {
        p += 1;
    }
    p == pattern.len()
}

fn include_paths(argument: &str, home_dir: &Path) -> Vec<PathBuf> {
    let path = if let Some(relative) = argument.strip_prefix("~/") {
        home_dir.join(relative)
    } else {
        home_dir.join(".ssh").join(argument)
    };
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    // Expanding directory patterns is deliberately outside this parser's scope.
    if parent.to_string_lossy().contains(['*', '?']) {
        return Vec::new();
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut matches: Vec<_> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|file| wildcard_match(name, file))
        })
        .map(|entry| entry.path())
        .collect();
    matches.sort();
    matches
}

fn read_ssh_config(
    path: &Path,
    home_dir: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    parser: &mut SshConfigParser,
) {
    if depth > 4 {
        return;
    }
    let Ok(path) = fs::canonicalize(path) else {
        return;
    };
    if !visited.insert(path.clone()) {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for (key, values) in text.lines().filter_map(config_directive) {
        if key == "include" && !parser.in_match {
            for argument in values {
                for included in include_paths(&argument, home_dir) {
                    read_ssh_config(&included, home_dir, depth + 1, visited, parser);
                }
            }
        } else {
            parser.directive(&key, &values);
        }
    }
}

/// Read SSH config and Includes using only the supplied home directory.
///
/// Includes are visited in place, with sorted `*`/`?` globs in the last component.
/// Relative paths use `<home_dir>/.ssh`; `~/` uses `home_dir`. The root is depth
/// zero and at most four Include levels are read. Canonical paths are read once;
/// IO failures are ignored and the first occurrence of each alias wins.
pub fn load_ssh_config(home_dir: &Path) -> Vec<SshConfigHost> {
    let mut parser = SshConfigParser::default();
    read_ssh_config(
        &home_dir.join(".ssh/config"),
        home_dir,
        0,
        &mut HashSet::new(),
        &mut parser,
    );
    let mut aliases = HashSet::new();
    parser
        .hosts
        .retain(|host| aliases.insert(host.alias.clone()));
    parser.hosts
}

fn ssh_option(endpoint: &mut SshEndpoint, option: char, value: &str) {
    match option {
        'p' if endpoint.port.is_none() => endpoint.port = valid_port(value),
        'l' if endpoint.user.is_none() => {
            endpoint.user = non_empty(value).map(str::to_owned);
        }
        'i' if endpoint.identity.is_none() => {
            endpoint.identity = non_empty(value).map(PathBuf::from);
        }
        _ => {}
    }
}

fn destination_endpoint(
    destination: &str,
    mut endpoint: SshEndpoint,
    explicit: bool,
) -> Option<SshEndpoint> {
    let destination = non_empty(destination)?;
    let uri = destination.strip_prefix("ssh://");
    let address = uri.unwrap_or(destination);
    let (user, host) = split_user_host(address);
    let mut host = host?;
    endpoint.user = endpoint.user.or(user);
    if uri.is_some() {
        // A slash may terminate the authority, but paths/queries are not SSH
        // destinations. Brackets disambiguate an IPv6 address from its port.
        host = host.strip_suffix('/').unwrap_or(&host).to_owned();
        if host.contains(['/', '?', '#']) {
            return None;
        }
        if let Some(bracketed) = host.strip_prefix('[') {
            let (address, suffix) = bracketed.split_once(']')?;
            if !suffix.is_empty() && !suffix.starts_with(':') {
                return None;
            }
            endpoint.port = endpoint
                .port
                .or_else(|| suffix.strip_prefix(':').and_then(valid_port));
            host = address.to_owned();
        } else if host.matches(':').count() == 1 {
            let (address, port) = host.split_once(':')?;
            endpoint.port = endpoint.port.or_else(|| valid_port(port));
            host = address.to_owned();
        }
    }
    let host = non_empty(&host)?;
    if !explicit
        && uri.is_none()
        && !destination.contains('@')
        && !host.contains(['.', ':'])
        && host.parse::<IpAddr>().is_err()
    {
        endpoint.alias = Some(host.to_owned());
    } else {
        endpoint.host = Some(host.to_owned());
    }
    Some(endpoint)
}

/// Parse an OpenSSH argv into its destination, ignoring the remote command.
///
/// Supports argument-taking options, attached values, clustered flags, `--`,
/// `-o` connection options, `user@host`, and SSH URIs. Bare short destinations
/// without explicit identity, port, or user options are treated as aliases.
/// Non-SSH executables and incomplete command lines return `None`.
pub fn endpoint_from_ssh_argv(argv: &[String]) -> Option<SshEndpoint> {
    let executable = argv.first()?.rsplit(['/', '\\']).next()?;
    if executable != "ssh" && executable != "ssh.exe" {
        return None;
    }
    let mut endpoint = SshEndpoint::default();
    let mut explicit = false;
    let mut args = argv.iter().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--" {
            return destination_endpoint(args.next()?, endpoint, explicit);
        }
        let Some(options) = argument.strip_prefix('-') else {
            return destination_endpoint(argument, endpoint, explicit);
        };
        if options.is_empty() || options.starts_with('-') {
            return None;
        }
        for (index, option) in options.char_indices() {
            if "BbcDEeFIiJLlmOoPpQRSWw".contains(option) {
                let attached = &options[index + option.len_utf8()..];
                let value = if attached.is_empty() {
                    args.next()?
                } else {
                    attached
                };
                if option == 'o' {
                    if let Some((key, values)) = config_directive(value) {
                        let connection_option = match key.as_str() {
                            "port" => Some('p'),
                            "user" => Some('l'),
                            "identityfile" => Some('i'),
                            _ => None,
                        };
                        if let Some(option) = connection_option {
                            explicit = true;
                            if let Some(value) = values.first() {
                                ssh_option(&mut endpoint, option, value);
                            }
                        }
                    }
                } else if matches!(option, 'i' | 'p' | 'l') {
                    explicit = true;
                    ssh_option(&mut endpoint, option, value);
                }
                break;
            } else if !"46AaCfGgKkMNnqsTtVvXxYy".contains(option) {
                return None;
            }
        }
    }
    None
}

/// Convert app state into a host, preferring an alias over explicit parameters.
pub fn host_from_connection(c: &AppConnection) -> RemoteHost {
    let kind = saved_connection_kind(&c.host_id);
    let ssh = if !kind.is_managed() {
        SshEndpoint::default()
    } else if let Some(alias) = &c.alias {
        SshEndpoint {
            alias: Some(alias.clone()),
            ..SshEndpoint::default()
        }
    } else {
        SshEndpoint {
            alias: None,
            host: c.host.clone(),
            user: c.user.clone(),
            port: c.port,
            identity: c.identity.clone(),
        }
    };
    let label = c
        .display_name
        .clone()
        .filter(|name| !looks_like_host_id(name))
        .or_else(|| kind.is_ssh().then(|| ssh.label()))
        .unwrap_or_default();
    RemoteHost {
        target_id: TargetId::remote(&c.host_id),
        label,
        host_id: Some(c.host_id.clone()),
        kind,
        ssh,
        sources: vec![HostSource::GlobalState],
        connected: None,
        wsl: None,
    }
}

fn same_present(left: &Option<String>, right: &Option<String>) -> bool {
    left.as_ref()
        .is_some_and(|value| Some(value) == right.as_ref())
}

fn same_host(left: &RemoteHost, right: &RemoteHost) -> bool {
    same_present(&left.host_id, &right.host_id)
        || (left.kind == right.kind && same_endpoint(left, right))
}

fn same_endpoint(left: &RemoteHost, right: &RemoteHost) -> bool {
    same_present(&left.ssh.alias, &right.ssh.alias)
        || (same_present(&left.ssh.host, &right.ssh.host)
            && left.ssh.port.unwrap_or(22) == right.ssh.port.unwrap_or(22)
            && (left.ssh.user == right.ssh.user
                || left.ssh.user.is_none()
                || right.ssh.user.is_none()))
}

fn add_source(host: &mut RemoteHost, source: HostSource) {
    if !host.sources.contains(&source) {
        host.sources.push(source);
    }
}

fn fill_host(existing: &mut RemoteHost, incoming: RemoteHost) {
    existing.host_id = existing.host_id.take().or(incoming.host_id);
    // Only SSH hosts have an endpoint; the first record's kind stays.
    if existing.kind.is_ssh() && incoming.kind.is_ssh() {
        existing.ssh.alias = existing.ssh.alias.take().or(incoming.ssh.alias);
        existing.ssh.host = existing.ssh.host.take().or(incoming.ssh.host);
        existing.ssh.port = existing.ssh.port.or(incoming.ssh.port);
        existing.ssh.user = existing.ssh.user.take().or(incoming.ssh.user);
        existing.ssh.identity = existing.ssh.identity.take().or(incoming.ssh.identity);
    }
    if existing.label.is_empty() {
        existing.label = incoming.label;
    }
    existing.connected = existing.connected.or(incoming.connected);
    for source in incoming.sources {
        add_source(existing, source);
    }
}

fn merge_host(hosts: &mut Vec<RemoteHost>, mut incoming: RemoteHost, live: bool) {
    let existing = hosts.iter_mut().find(|host| {
        same_host(host, &incoming)
            || (live
                && incoming
                    .ssh
                    .alias
                    .as_ref()
                    .or(incoming.ssh.host.as_ref())
                    .is_some_and(|destination| {
                        host.ssh.alias.as_ref() == Some(destination)
                            || host.ssh.host.as_ref() == Some(destination)
                    }))
    });
    if let Some(existing) = existing {
        fill_host(existing, incoming);
        if live {
            existing.connected = Some(true);
        }
    } else {
        let mut sources = HashSet::new();
        incoming.sources.retain(|source| sources.insert(*source));
        hosts.push(incoming);
    }
}

/// A host reported by the renderer, normalised: the app's id prefix wins over
/// the reported kind when it names another kind (or the reported kind is
/// unknown); an SSH host whose id is
/// `remote-ssh-discovered:<alias>` gets that alias when no endpoint came with it;
/// a host of another kind never carries an SSH endpoint; a raw id is no label.
pub fn host_from_app(host: &RemoteHost) -> RemoteHost {
    let mut host = host.clone();
    if let Some(id) = host.host_id.as_deref() {
        let by_id = HostKind::of_host_id(id);
        let other_kind = matches!(
            by_id,
            HostKind::RemoteControl | HostKind::Wsl | HostKind::Cloud
        );
        if (host.kind == HostKind::Ssh && other_kind)
            || (host.kind == HostKind::Unknown && by_id != HostKind::Unknown)
        {
            host.kind = by_id;
        }
    }
    let mut from_id = false;
    if host.kind == HostKind::Wsl && host.wsl.is_none() {
        host.wsl = host
            .host_id
            .as_deref()
            .and_then(superba_core::wsl::distro_of_host_id)
            .map(superba_core::WslDistro::named);
    }
    if host.kind == HostKind::Wsl
        && (host.label.trim().is_empty() || looks_like_host_id(&host.label))
        && let Some(d) = &host.wsl
    {
        host.label = wsl_label(&d.name);
    }
    if host.kind.is_ssh() {
        if host.ssh.alias.is_none()
            && host.ssh.host.is_none()
            && let Some(alias) = host.host_id.as_deref().and_then(discovered_alias)
        {
            host.ssh.alias = Some(alias);
            from_id = true;
        }
    } else {
        host.ssh = SshEndpoint::default();
    }
    let label = host.label.trim();
    let raw = (label.is_empty() && from_id)
        || (!label.is_empty()
            && (looks_like_host_id(label)
                || host.host_id.as_deref().map(str::trim) == Some(label)));
    if raw {
        let has_endpoint = host.ssh.alias.is_some() || host.ssh.host.is_some();
        host.label = if host.kind.is_ssh() && has_endpoint {
            host.ssh.label()
        } else {
            String::new()
        };
    }
    host
}

fn endpoint_host(ssh: SshEndpoint, source: HostSource) -> RemoteHost {
    let label = ssh.label();
    RemoteHost {
        target_id: TargetId::remote(&label),
        label,
        host_id: None,
        kind: HostKind::Ssh,
        ssh,
        sources: vec![source],
        connected: None,
        wsl: None,
    }
}

/// Merge global state, renderer hosts, declared aliases, live links, and SSH hints.
///
/// Earlier records retain their fields and position. Later records fill missing
/// fields and append new sources; live links mark a host connected. SSH config
/// only adds provenance to existing aliases. Final IDs prefer a learned host ID,
/// then alias, host, or label. Declared aliases alone never acquire an app host ID.
pub fn merge_hosts(
    global: &[AppConnection],
    from_app: &[RemoteHost],
    declared_aliases: &[String],
    live: &[ProcessInfo],
    ssh_config: &[SshConfigHost],
) -> Vec<RemoteHost> {
    let mut hosts = Vec::new();
    for connection in global {
        merge_host(&mut hosts, host_from_connection(connection), false);
    }
    for host in from_app {
        merge_host(&mut hosts, host_from_app(host), false);
    }
    for alias in declared_aliases.iter().filter_map(|alias| non_empty(alias)) {
        let ssh = SshEndpoint {
            alias: Some(alias.to_owned()),
            ..SshEndpoint::default()
        };
        merge_host(
            &mut hosts,
            endpoint_host(ssh, HostSource::GlobalState),
            false,
        );
    }
    for process in live {
        let CodexProcessKind::RemoteLink { host } = &process.kind else {
            continue;
        };
        let Some(ssh) = endpoint_from_ssh_argv(&process.cmdline)
            .or_else(|| destination_endpoint(host, SshEndpoint::default(), false))
        else {
            continue;
        };
        let mut host = endpoint_host(ssh, HostSource::ProcessScan);
        host.connected = Some(true);
        merge_host(&mut hosts, host, true);
    }
    for config in ssh_config {
        let Some(alias) = non_empty(&config.alias) else {
            continue;
        };
        if let Some(host) = hosts
            .iter_mut()
            .find(|host| host.ssh.alias.as_deref() == Some(alias))
        {
            add_source(host, HostSource::SshConfig);
        } else {
            hosts.push(endpoint_host(
                SshEndpoint {
                    alias: Some(alias.to_owned()),
                    host: config.host_name.clone(),
                    user: config.user.clone(),
                    port: config.port,
                    identity: config.identity_file.as_ref().map(PathBuf::from),
                },
                HostSource::SshConfig,
            ));
        }
    }
    for host in &mut hosts {
        let key = host
            .host_id
            .as_deref()
            .or(host.ssh.alias.as_deref())
            .or(host.ssh.host.as_deref())
            .unwrap_or(&host.label);
        host.target_id = TargetId::remote(key);
    }
    hosts
}

fn read_global_state(codex_home: &CodexHome) -> Vec<AppConnection> {
    let main = codex_home.global_state_json();
    let mut backup = main.as_os_str().to_owned();
    backup.push(GLOBAL_STATE_BAK_SUFFIX);
    let mut parse_error = None;
    for path in [main, PathBuf::from(backup)] {
        if let Ok(bytes) = fs::read(path) {
            match parse_global_state(&bytes) {
                Ok(connections) => return connections,
                Err(error) => parse_error = Some(error),
            }
        }
    }
    if let Some(error) = parse_error {
        tracing::warn!(%error, "No usable Codex global state or backup; ignoring global state");
    }
    Vec::new()
}

/// Read app state (with `.bak` fallback), declared aliases, and optional SSH config.
///
/// Missing files and IO failures are ignored. Unparseable global state without a
/// usable backup is logged and treated as empty. No environment variables or
/// identity-file contents are read.
pub fn discover_from_files(
    codex_home: &CodexHome,
    home_dir: Option<&Path>,
    from_app: &[RemoteHost],
    live: &[ProcessInfo],
) -> Vec<RemoteHost> {
    let global = read_global_state(codex_home);
    let declared = fs::read(codex_home.root().join("codex-app/config.json"))
        .map(|bytes| parse_codex_app_config(&bytes))
        .unwrap_or_default();
    let ssh_config = home_dir.map(load_ssh_config).unwrap_or_default();
    merge_hosts(&global, from_app, &declared, live, &ssh_config)
}

/// The label of a distribution: the app's own display name (`WSL: <distro>`).
pub fn wsl_label(distro: &str) -> String {
    format!("WSL: {distro}")
}

/// Add the WSL distributions Windows lists (user bug 45) to `hosts`: a
/// distribution the app reported too (`remote-wsl:<distro>`) gets the listed
/// facts (WSL version, default, state) and the [`HostSource::Wsl`] source; one
/// the app does not know (WSL 1, the app's WSL connections off, a CLI-only
/// distribution) is added as a host of its own. A distribution the app reports
/// that Windows no longer lists keeps the app's record.
pub fn merge_wsl(hosts: &mut Vec<RemoteHost>, distros: &[superba_core::WslDistro]) {
    for distro in distros {
        let target = superba_core::wsl::target_id(&distro.name);
        if let Some(host) = hosts.iter_mut().find(|h| {
            h.kind == HostKind::Wsl
                && (h.target_id == target || h.wsl_distro().as_deref() == Some(&distro.name))
        }) {
            host.wsl = Some(distro.clone());
            host.host_id.get_or_insert_with(|| distro.host_id());
            host.target_id = target;
            if host.label.trim().is_empty() {
                host.label = wsl_label(&distro.name);
            }
            add_source(host, HostSource::Wsl);
            continue;
        }
        hosts.push(RemoteHost {
            target_id: target,
            label: wsl_label(&distro.name),
            host_id: Some(distro.host_id()),
            kind: HostKind::Wsl,
            ssh: SshEndpoint::default(),
            sources: vec![HostSource::Wsl],
            connected: None,
            wsl: Some(distro.clone()),
        });
    }
}

/// True when the host's only discovery source is SSH config.
pub fn is_suggestion(host: &RemoteHost) -> bool {
    host.sources == [HostSource::SshConfig]
}
