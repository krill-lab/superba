//! Pure planning of remote shell rc edits for `CODEX_AUTHAPI_BASE_URL` (plan 8.5,
//! 9.9), using the marker-block format shared with the local persister
//! (`superba_core::env::{render_block, find_block}`).
//!
//! The desktop app starts the remote app-server with `$SHELL -l -i -c ...` (a login
//! *and* interactive shell), so the value must be visible to that shell:
//!
//! | Login shell | Files written |
//! |---|---|
//! | zsh | end of `${ZDOTDIR:-$HOME}/.zshenv` (read by every zsh) |
//! | bash | top of `~/.bashrc` (before the usual "return if not interactive"), and the end of the first existing of `~/.bash_profile`, `~/.bash_login`, `~/.profile` (else `~/.profile`) |
//! | sh, dash, ksh, other POSIX | end of `~/.profile` |
//! | fish | `${XDG_CONFIG_HOME:-~/.config}/fish/conf.d/superba.fish` |
//! | csh, tcsh, nu, xonsh | unsupported (no POSIX or fish syntax): PAT is blocked |
//!
//! `ZDOTDIR` and `XDG_CONFIG_HOME` are taken from the environment the ssh session
//! inherits (what zsh and fish see when they start). A `ZDOTDIR` set inside
//! `~/.zshenv` itself, or an `ENV` file of an interactive POSIX sh that sets the
//! variable again, is not inspected; the post-restart verification of the new
//! app-server's environment catches such overrides.
//!
//! Legacy Krill tutorial lines (`export CODEX_AUTHAPI_BASE_URL=<krill origin>`,
//! usually at the end of `~/.bashrc` / `~/.zshrc`, which would override our value)
//! are neutralised in every rc file read and restored on restore. In POSIX files the
//! line becomes `: # [superba] <line>` (a no-op command, so an `if` / function body
//! whose only statement was that line stays valid); in fish files, where empty
//! blocks are legal, `# [superba] <line>`. Both forms carry
//! `superba_core::env::LEGACY_COMMENT_PREFIX`. Lines pointing at other hosts are
//! left alone and reported.

use std::path::PathBuf;

use superba_core::codex::ENV_AUTHAPI_BASE_URL;
use superba_core::env::{
    LEGACY_COMMENT_PREFIX, export_value, find_block, legacy_krill_export_value, render_block,
};
use superba_core::{EnvEdit, EnvEditAction, EnvLocation, EnvOp, Error, Notice, Result, ShellKind};

use crate::util::{rjoin, sha256_hex};

/// Directory an rc file is relative to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcBase {
    /// `$HOME`.
    Home,
    /// `${ZDOTDIR:-$HOME}` (zsh startup files).
    ZDotDir,
    /// `${XDG_CONFIG_HOME:-$HOME/.config}` (fish).
    XdgConfig,
}

impl RcBase {
    /// The base as a POSIX shell expression (inside double quotes).
    pub fn shell_expr(self) -> &'static str {
        match self {
            RcBase::Home => "$HOME",
            RcBase::ZDotDir => "${ZDOTDIR:-$HOME}",
            RcBase::XdgConfig => "${XDG_CONFIG_HOME:-$HOME/.config}",
        }
    }
}

/// The base directories as reported by the host (`None` / empty = unset).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RcBases {
    /// `$HOME`.
    pub home: String,
    /// Inherited `ZDOTDIR`.
    pub zdotdir: Option<String>,
    /// Inherited `XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<String>,
}

impl RcBases {
    /// Absolute directory of `base`.
    pub fn dir(&self, base: RcBase) -> String {
        match base {
            RcBase::Home => self.home.clone(),
            RcBase::ZDotDir => self
                .zdotdir
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| self.home.clone()),
            RcBase::XdgConfig => self
                .xdg_config_home
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| rjoin(&self.home, ".config")),
        }
    }
}

/// One rc file Superba reads on a remote host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcSpec {
    /// Record name (no `:`).
    pub name: &'static str,
    /// Base directory.
    pub base: RcBase,
    /// Path relative to the base.
    pub rel: &'static str,
    /// Dialect of the file.
    pub fish: bool,
}

const fn spec(name: &'static str, base: RcBase, rel: &'static str, fish: bool) -> RcSpec {
    RcSpec {
        name,
        base,
        rel,
        fish,
    }
}

/// Every rc file read by the probe (legacy lines are searched in all of them).
pub const RC_FILES: &[RcSpec] = &[
    spec("zshenv", RcBase::ZDotDir, ".zshenv", false),
    spec("zprofile", RcBase::ZDotDir, ".zprofile", false),
    spec("zshrc", RcBase::ZDotDir, ".zshrc", false),
    spec("zlogin", RcBase::ZDotDir, ".zlogin", false),
    spec("bashrc", RcBase::Home, ".bashrc", false),
    spec("bash_profile", RcBase::Home, ".bash_profile", false),
    spec("bash_login", RcBase::Home, ".bash_login", false),
    spec("profile", RcBase::Home, ".profile", false),
    spec("fish_config", RcBase::XdgConfig, "fish/config.fish", true),
    spec(
        "fish_superba",
        RcBase::XdgConfig,
        "fish/conf.d/superba.fish",
        true,
    ),
];

/// Current state of one rc file on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcFile {
    /// Which file.
    pub spec: RcSpec,
    /// Absolute remote path.
    pub path: String,
    /// Contents (`None` = absent).
    pub content: Option<Vec<u8>>,
    /// Exists but could not be read (or is too large).
    pub unreadable: bool,
}

/// Final contents of one rc file after planning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcWrite {
    /// Record name.
    pub name: &'static str,
    /// Absolute remote path.
    pub path: String,
    /// Contents before (`None` = absent).
    pub before: Option<Vec<u8>>,
    /// Contents after (`None` = remove the file).
    pub after: Option<Vec<u8>>,
}

/// Result of [`plan_env`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RcPlan {
    /// Edits for the preview / report (`before`/`after` are file sha256 values).
    pub edits: Vec<EnvEdit>,
    /// Files to write.
    pub writes: Vec<RcWrite>,
    /// Foreign lines and similar observations.
    pub warnings: Vec<Notice>,
}

/// Where the block goes in a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    Top,
    Bottom,
}

/// True if Superba can persist the variable for this login shell.
pub fn shell_supported(shell: &ShellKind) -> bool {
    match shell {
        ShellKind::Zsh | ShellKind::Bash | ShellKind::Fish | ShellKind::Sh => true,
        ShellKind::Other(name) => !matches!(name.as_str(), "csh" | "tcsh" | "nu" | "xonsh" | ""),
    }
}

/// The ShellKind used for the location of a file.
fn file_shell(spec: &RcSpec) -> ShellKind {
    if spec.fish {
        ShellKind::Fish
    } else if spec.name.starts_with('z') {
        ShellKind::Zsh
    } else if spec.name.starts_with("bash") {
        ShellKind::Bash
    } else {
        ShellKind::Sh
    }
}

fn targets(shell: &ShellKind, files: &[RcFile]) -> Vec<(&'static str, Placement)> {
    let exists = |name: &str| {
        files
            .iter()
            .any(|f| f.spec.name == name && f.content.is_some())
    };
    match shell {
        ShellKind::Zsh => vec![("zshenv", Placement::Bottom)],
        ShellKind::Bash => {
            let login = ["bash_profile", "bash_login", "profile"]
                .into_iter()
                .find(|n| exists(n))
                .unwrap_or("profile");
            vec![("bashrc", Placement::Top), (login, Placement::Bottom)]
        }
        ShellKind::Fish => vec![("fish_superba", Placement::Bottom)],
        _ => vec![("profile", Placement::Bottom)],
    }
}

fn utf8(f: &RcFile) -> Result<&str> {
    let bytes = f.content.as_deref().unwrap_or_default();
    std::str::from_utf8(bytes)
        .map_err(|_| Error::parse(f.path.clone(), "not UTF-8 text; Superba will not edit it"))
}

fn remove_block(text: &str) -> String {
    let mut out = text.to_owned();
    // Remove every block (a file may hold a duplicate from manual copying).
    while let Some(r) = find_block(&out) {
        out.replace_range(r, "");
    }
    out
}

fn place(text: &str, block: &str, placement: Placement) -> String {
    match placement {
        Placement::Top => format!("{block}{text}"),
        Placement::Bottom => {
            let mut s = text.to_owned();
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(block);
            s
        }
    }
}

/// True if `offset` lies inside a marker block of `text`.
fn in_block(text: &str, offset: usize) -> bool {
    find_block(text).is_some_and(|r| r.contains(&offset))
}

/// The neutralised form of a legacy line (`body` without its line break).
pub fn neutralise_legacy(body: &str, fish: bool) -> String {
    let indent_len = body.len() - body.trim_start().len();
    let (indent, rest) = body.split_at(indent_len);
    if fish {
        format!("{indent}{LEGACY_COMMENT_PREFIX}{rest}")
    } else {
        format!("{indent}: {LEGACY_COMMENT_PREFIX}{rest}")
    }
}

/// The original line if `body` is a legacy line Superba neutralised (any form).
pub fn restore_legacy(body: &str) -> Option<String> {
    let indent_len = body.len() - body.trim_start().len();
    let (indent, rest) = body.split_at(indent_len);
    let rest = rest.strip_prefix(": ").unwrap_or(rest);
    let original = rest.strip_prefix(LEGACY_COMMENT_PREFIX)?;
    export_value(original, ENV_AUTHAPI_BASE_URL)?;
    Some(format!("{indent}{original}"))
}

struct FileWork<'a> {
    file: &'a RcFile,
    text: String,
    edits: Vec<(EnvEditAction, String)>,
}

/// Plan the rc edits for `op` on a host whose login shell is `shell`. Pure.
/// `EnvOp::Clear` and `EnvOp::RestoreBaseline` both remove Superba's blocks and
/// restore the legacy lines Superba neutralised (on a remote host there are no
/// registry or launchctl values to restore).
pub fn plan_env(shell: &ShellKind, files: &[RcFile], op: &EnvOp) -> Result<RcPlan> {
    plan_env_for(std::slice::from_ref(shell), files, op)
}

/// [`plan_env`] for a host whose consumers start through several shells: the
/// block goes into the files of each (a WSL distribution: the desktop app's
/// `/usr/bin/bash -l` and the user's own login shell for the Codex CLI). The
/// first shell must be supported; others that are not are left out.
pub fn plan_env_for(shells: &[ShellKind], files: &[RcFile], op: &EnvOp) -> Result<RcPlan> {
    let shell = shells.first().cloned().unwrap_or(ShellKind::Sh);
    let mut work: Vec<FileWork<'_>> = Vec::new();
    let mut warnings = Vec::new();
    for f in files {
        if f.unreadable {
            continue;
        }
        let text = match (&f.content, utf8(f)) {
            (None, _) => String::new(),
            (Some(_), Ok(t)) => t.to_owned(),
            (Some(_), Err(_)) => continue,
        };
        work.push(FileWork {
            file: f,
            text,
            edits: Vec::new(),
        });
    }
    match op {
        EnvOp::Set { value, .. } => {
            if !shell_supported(&shell) {
                return Err(Error::Unsupported {
                    what: format!(
                        "persisting {ENV_AUTHAPI_BASE_URL} for the {shell:?} login shell"
                    ),
                });
            }
            // 1. Neutralise legacy lines, report foreign ones.
            for w in &mut work {
                let mut out = String::with_capacity(w.text.len() + 64);
                let mut offset = 0;
                for (idx, line) in w.text.split_inclusive('\n').enumerate() {
                    let body = line.trim_end_matches(['\n', '\r']);
                    let eol = &line[body.len()..];
                    if !in_block(&w.text, offset) {
                        if legacy_krill_export_value(body).is_some() {
                            let new = neutralise_legacy(body, w.file.spec.fish);
                            out.push_str(&new);
                            out.push_str(eol);
                            w.edits.push((
                                EnvEditAction::CommentLegacyLine {
                                    line_no: idx + 1,
                                    original: body.to_owned(),
                                },
                                new,
                            ));
                            offset += line.len();
                            continue;
                        }
                        if let Some(v) = export_value(body, ENV_AUTHAPI_BASE_URL)
                            && v != *value
                        {
                            warnings.push(Notice::warning(crate::texts::foreign_env(
                                &w.file.path,
                                idx + 1,
                                &v,
                            )));
                        }
                    }
                    out.push_str(line);
                    offset += line.len();
                }
                w.text = out;
            }
            // 2. Write the block into the target files.
            let mut wanted: Vec<(&'static str, Placement)> = Vec::new();
            for sh in shells.iter().filter(|sh| shell_supported(sh)) {
                for t in targets(sh, files) {
                    if !wanted.iter().any(|(n, _)| *n == t.0) {
                        wanted.push(t);
                    }
                }
            }
            for (name, placement) in wanted {
                let Some(w) = work.iter_mut().find(|w| w.file.spec.name == name) else {
                    let path = files
                        .iter()
                        .find(|f| f.spec.name == name)
                        .map(|f| f.path.clone())
                        .unwrap_or_else(|| name.to_owned());
                    return Err(Error::invalid(format!(
                        "{path} cannot be read or is not UTF-8 text; Superba will not edit it"
                    )));
                };
                let dialect = file_shell(&w.file.spec);
                let block = render_block(&dialect, &[(ENV_AUTHAPI_BASE_URL, value.as_str())]);
                let new = place(&remove_block(&w.text), &block, placement);
                if new != w.text {
                    w.text = new;
                    w.edits.push((EnvEditAction::WriteBlock, block));
                }
            }
        }
        EnvOp::Clear | EnvOp::RestoreBaseline { .. } => {
            for w in &mut work {
                if w.file.content.is_none() {
                    continue;
                }
                let stripped = remove_block(&w.text);
                if stripped != w.text {
                    w.edits.push((EnvEditAction::RemoveBlock, String::new()));
                }
                let mut out = String::with_capacity(stripped.len());
                for (idx, line) in stripped.split_inclusive('\n').enumerate() {
                    let body = line.trim_end_matches(['\n', '\r']);
                    if let Some(orig) = restore_legacy(body) {
                        out.push_str(&orig);
                        out.push_str(&line[body.len()..]);
                        w.edits.push((
                            EnvEditAction::UncommentLegacyLine {
                                line_no: idx + 1,
                                original: orig.clone(),
                            },
                            orig,
                        ));
                    } else {
                        out.push_str(line);
                    }
                }
                w.text = out;
            }
        }
    }
    // 3. Collect writes and edits with per-step hashes.
    let mut plan = RcPlan {
        warnings,
        ..RcPlan::default()
    };
    for w in work {
        if w.edits.is_empty() {
            continue;
        }
        let before = w.file.content.clone();
        // Superba's own fish file is removed once nothing but whitespace is left.
        let after = if w.file.spec.name == "fish_superba" && w.text.trim().is_empty() {
            None
        } else {
            Some(w.text.clone().into_bytes())
        };
        let location = EnvLocation::ShellRc {
            path: PathBuf::from(&w.file.path),
            shell: file_shell(&w.file.spec),
        };
        let mut prev = before.as_deref().map(sha256_hex);
        let n = w.edits.len();
        for (i, (action, preview)) in w.edits.into_iter().enumerate() {
            // Intermediate states are not materialised; the last edit carries the
            // final file hash so revert can compare against it.
            let next = if i + 1 == n {
                after.as_deref().map(sha256_hex)
            } else {
                prev.clone()
            };
            plan.edits.push(EnvEdit {
                location: location.clone(),
                action,
                preview,
                before: prev.clone(),
                after: next.clone(),
            });
            prev = next;
        }
        plan.writes.push(RcWrite {
            name: w.file.spec.name,
            path: w.file.path.clone(),
            before,
            after,
        });
    }
    Ok(plan)
}

/// Absolute remote path of an rc file.
pub fn rc_path(bases: &RcBases, spec: &RcSpec) -> String {
    rjoin(&bases.dir(spec.base), spec.rel)
}
