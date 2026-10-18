//! `spb config`: Codex's own `config.toml` and `auth.json`, edited by hand
//! (GitHub issue 9).
//!
//! - `edit [config|auth] [--host H]` opens a private copy in `$VISUAL` /
//!   `$EDITOR` (notepad on Windows, `vi` elsewhere), checks it when the editor
//!   closes (syntax with line and column, the settings catalog of the Codex
//!   versions reading that home, the shape of `auth.json`), shows the diff
//!   (secrets masked unless `--show-secrets`) and what it changes in the keys
//!   Superba manages, asks, and saves through the service, which backs the old
//!   file up first (encrypted).
//! - `check` checks the file as it is; `backups` lists the backups (the
//!   editor's, Superba's switch pre-images, the originals); `restore <id>`
//!   shows the diff and saves the backup like an edit.

use std::path::{Path, PathBuf};

use superba_core::{
    CodexFileBackupQuery, CodexFileDoc, CodexFileKind, CodexFileSave, CodexFileSaved, Error,
    FileProblem, ProblemSeverity, SaveReason, SecretBytes, TargetId, auth_shape_label,
};
use superba_switch::codex_files::{self, CheckContext, Masker};

use super::{Failure, Result, remote, runtime, usage};
use crate::text::text;
use crate::{CodexFileArg, ConfigCommand, Output, Runtime, exit};

pub(super) async fn run(command: &ConfigCommand, rt: &Runtime, out: &mut Output) -> Result {
    if let ConfigCommand::Edit {
        file,
        host: Some(_),
        ..
    }
    | ConfigCommand::Check {
        file,
        host: Some(_),
    } = command
        && *file == CodexFileArg::Claude
    {
        return Err(usage(out, text("cli.claude.local_only")));
    }
    match command {
        ConfigCommand::Edit {
            file,
            host,
            yes,
            force,
            show_secrets,
        } => {
            edit(
                *file,
                host.as_deref(),
                Flags {
                    yes: *yes,
                    force: *force,
                    show_secrets: *show_secrets,
                },
                rt,
                out,
            )
            .await
        }
        ConfigCommand::Check { file, host } => check(*file, host.as_deref(), rt, out).await,
        ConfigCommand::Backups { file, host } => backups(*file, host.as_deref(), rt, out).await,
        ConfigCommand::Restore {
            id,
            yes,
            force,
            show_secrets,
        } => {
            restore(
                id,
                Flags {
                    yes: *yes,
                    force: *force,
                    show_secrets: *show_secrets,
                },
                rt,
                out,
            )
            .await
        }
    }
}

#[derive(Clone, Copy)]
struct Flags {
    yes: bool,
    force: bool,
    show_secrets: bool,
}

fn target(host: Option<&str>) -> TargetId {
    host.map_or_else(TargetId::local, remote::target)
}

fn failure(error: Error, code: u8) -> Failure {
    Failure { error, code }
}

fn where_label(out: &Output, target: &TargetId, label: &str) -> String {
    if target.is_local() {
        out.label("cli.config.this_computer")
    } else {
        label.to_owned()
    }
}

/// The editor command: `$VISUAL`, `$EDITOR`, notepad on Windows, `vi`.
fn editor(rt: &Runtime) -> String {
    rt.env
        .var_non_empty("VISUAL")
        .or_else(|| rt.env.var_non_empty("EDITOR"))
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".into()
            } else {
                "vi".into()
            }
        })
}

/// A private directory for the copy the editor opens, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        for n in 0..32u32 {
            let dir = base.join(format!(
                "spb-edit-{}-{:08x}",
                std::process::id(),
                nanos.wrapping_add(n)
            ));
            let made = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    std::fs::DirBuilder::new().mode(0o700).create(&dir)
                }
                #[cfg(not(unix))]
                {
                    std::fs::create_dir(&dir)
                }
            };
            match made {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other("no free temporary directory"))
    }

    fn write(&self, name: &str, text: &str) -> std::io::Result<PathBuf> {
        let path = self.0.join(name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write as _;
        opts.open(&path)?.write_all(text.as_bytes())?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run the editor on a private copy of `text`; the text after it closed.
fn edit_in_editor(rt: &Runtime, out: &Output, name: &str, contents: &str) -> Result<String> {
    let io = |e: std::io::Error| failure(Error::io(Path::new(name), &e), exit::ERROR);
    let scratch = Scratch::new().map_err(io)?;
    let path = scratch.write(name, contents).map_err(io)?;
    let command = editor(rt);
    match (rt.edit_file)(&command, &path) {
        Ok(true) => {}
        Ok(false) => {
            return Err(failure(
                Error::other(out.render(&text("cli.config.editor_failed").arg("editor", &command))),
                exit::ERROR,
            ));
        }
        Err(e) => {
            return Err(failure(
                Error::other(
                    out.render(
                        &text("cli.config.editor_missing")
                            .arg("editor", &command)
                            .arg("error", e),
                    ),
                ),
                exit::ERROR,
            ));
        }
    }
    let bytes = std::fs::read(&path).map_err(io)?;
    superba_core::codex_files::text_of(&bytes).ok_or_else(|| {
        failure(
            Error::invalid(out.label("cli.config.not_utf8")),
            exit::ERROR,
        )
    })
}

fn severity_label(out: &Output, s: ProblemSeverity) -> String {
    out.label(match s {
        ProblemSeverity::Error => "cli.config.error",
        ProblemSeverity::Warning => "cli.config.warning",
        ProblemSeverity::Info => "cli.config.info",
    })
}

fn print_problems(
    out: &mut Output,
    file: &str,
    problems: &[FileProblem],
) -> superba_core::Result<()> {
    for p in problems
        .iter()
        .filter(|p| p.severity != ProblemSeverity::Info)
    {
        out.line(
            text("cli.config.problem")
                .arg("file", file)
                .arg("line", p.line)
                .arg("column", p.column)
                .arg("severity", severity_label(out, p.severity))
                .arg_text("message", p.message.clone()),
        )?;
    }
    Ok(())
}

/// Ask a yes/no question; `default` when the answer is empty.
fn ask(
    rt: &Runtime,
    out: &mut Output,
    question: superba_core::LocalizedText,
    default: bool,
) -> bool {
    if out.line(question).is_err() {
        return false;
    }
    let answer = (rt.read_stdin_line)()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match answer.as_str() {
        "" => default,
        "y" | "yes" | "是" => true,
        _ => false,
    }
}

fn interactive(rt: &Runtime, out: &Output) -> bool {
    rt.interactive && !out.json
}

fn check_context<'a>(doc: &'a CodexFileDoc, owner: Option<&'a str>) -> CheckContext<'a> {
    CheckContext {
        schema: Some(&doc.schema),
        owned: &doc.owned,
        owner,
        masker: None,
    }
}

/// The diff `old` -> `new` (masked for `auth.json` unless revealed).
fn print_diff(
    out: &mut Output,
    doc: &CodexFileDoc,
    old: &str,
    new: &str,
    show_secrets: bool,
) -> superba_core::Result<()> {
    let name = doc.kind.file_name();
    let secrets = matches!(
        doc.kind,
        CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings
    );
    let (old, new) = if secrets && !show_secrets {
        let mut m = Masker::for_kind(doc.kind);
        (m.mask(old), m.mask(new))
    } else {
        (old.to_owned(), new.to_owned())
    };
    let diff = codex_files::unified(
        &format!("{name} ({})", out.label("cli.config.current")),
        &format!("{name} ({})", out.label("cli.config.new")),
        &old,
        &new,
        3,
    );
    for line in diff.lines() {
        out.data_line(line)?;
    }
    if secrets && !show_secrets {
        out.line(text("cli.config.secrets_masked"))?;
    }
    Ok(())
}

/// What the edit changes in what Superba manages (GH#5).
fn print_ownership(
    out: &mut Output,
    doc: &CodexFileDoc,
    old: &str,
    new: &str,
) -> superba_core::Result<()> {
    let changed = match doc.kind {
        CodexFileKind::ConfigToml => codex_files::owned_changes(old, new, &doc.owned),
        CodexFileKind::ClaudeSettings => {
            superba_switch::claude_files::owned_changes(old, new, &doc.owned)
        }
        CodexFileKind::AuthJson => Vec::new(),
    };
    let credential = (doc.kind == CodexFileKind::AuthJson && doc.owns_credential)
        .then(|| codex_files::credential_change(Some(old.as_bytes()), Some(new.as_bytes())))
        .flatten();
    if changed.is_empty() && credential.is_none() {
        return Ok(());
    }
    out.line(text("cli.config.owned_title"))?;
    let none = out.label("cli.config.unset");
    for k in &changed {
        out.line(
            text("cli.config.owned_key")
                .arg("path", &k.path)
                .arg("before", k.before.as_deref().unwrap_or(&none))
                .arg("after", k.after.as_deref().unwrap_or(&none)),
        )?;
    }
    if let Some(c) = &credential {
        out.line(
            text("cli.config.owned_credential")
                .arg_text("before", auth_shape_label(&c.written))
                .arg_text("after", auth_shape_label(&c.live)),
        )?;
    }
    out.line(superba_core::codex_files::texts::owned_promise())
}

fn print_saved(out: &mut Output, saved: &CodexFileSaved) -> superba_core::Result<()> {
    out.line(text("cli.config.saved").arg("path", &saved.path))?;
    match &saved.backup {
        Some(b) => out.line(text("cli.config.backup").arg("id", &b.id))?,
        None => {
            if !saved
                .notes
                .iter()
                .any(|n| n.text.key == "switch.codex_files.unchanged")
            {
                out.line(text("cli.config.no_backup"))?;
            }
        }
    }
    for n in &saved.notes {
        out.notice(n)?;
    }
    if let Some(view) = &saved.runtime
        && !view.consumers.is_empty()
    {
        runtime::render(view, out)?;
    }
    Ok(())
}

async fn open(rt: &Runtime, target: TargetId, kind: CodexFileKind) -> Result<CodexFileDoc> {
    let doc = rt.service.codex_file(target, kind).await?;
    Ok(doc)
}

/// Save `new` over the opened `doc` after the checks, the diff and the
/// question; shared by `edit` and `restore`.
async fn confirm_and_save(
    rt: &Runtime,
    out: &mut Output,
    doc: &CodexFileDoc,
    new: &str,
    reason: SaveReason,
    flags: Flags,
    force: bool,
) -> Result {
    let old = doc.text().unwrap_or_default();
    print_diff(out, doc, &old, new, flags.show_secrets)?;
    print_ownership(out, doc, &old, new)?;
    if !flags.yes {
        if !interactive(rt, out) {
            out.line(text("cli.config.needs_yes"))?;
            return Ok(exit::NEEDS_CONFIRMATION);
        }
        if !ask(
            rt,
            out,
            text("cli.config.confirm").arg("file", doc.kind.file_name()),
            false,
        ) {
            out.line(text("cli.config.not_saved"))?;
            return Ok(exit::CANCELLED);
        }
    }
    let saved = rt
        .service
        .save_codex_file(CodexFileSave {
            target: doc.target.clone(),
            kind: doc.kind,
            contents: SecretBytes::new(new.as_bytes().to_vec()),
            base_sha256: doc.sha256.clone(),
            reason,
            force,
        })
        .await?;
    print_saved(out, &saved)?;
    out.value(&saved)?;
    Ok(exit::OK)
}

async fn edit(
    file: CodexFileArg,
    host: Option<&str>,
    flags: Flags,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let kind = file.kind();
    let doc = open(rt, target(host), kind).await?;
    if let Some(why) = &doc.read_only {
        return Err(failure(Error::other(out.render(why)), exit::ERROR));
    }
    let name = kind.file_name();
    out.line(
        text("cli.config.opening")
            .arg("path", &doc.path)
            .arg("where", where_label(out, &doc.target, &doc.target_label))
            .arg("editor", editor(rt)),
    )?;
    let original = doc.text().unwrap_or_default();
    let owner = doc.profile.as_ref().map(ToString::to_string);
    let mut current = original.clone();
    let mut force = flags.force;
    loop {
        current = edit_in_editor(rt, out, name, &current)?;
        if current == original {
            out.line(text("cli.config.unchanged"))?;
            return Ok(exit::OK);
        }
        let problems =
            codex_files::validate(kind, &current, &check_context(&doc, owner.as_deref()));
        print_problems(out, name, &problems)?;
        if problems.iter().any(FileProblem::blocks_save) {
            if interactive(rt, out) && ask(rt, out, text("cli.config.edit_again"), true) {
                continue;
            }
            out.line(text("cli.config.not_saved"))?;
            return Ok(exit::ERROR);
        }
        if !force && problems.iter().any(FileProblem::needs_force) {
            if interactive(rt, out) {
                match ask(rt, out, text("cli.config.save_anyway"), false) {
                    true => force = true,
                    false => {
                        if ask(rt, out, text("cli.config.edit_again"), true) {
                            continue;
                        }
                        out.line(text("cli.config.not_saved"))?;
                        return Ok(exit::CANCELLED);
                    }
                }
            } else {
                out.line(text("cli.config.needs_force"))?;
                return Ok(exit::NEEDS_CONFIRMATION);
            }
        }
        break;
    }
    confirm_and_save(rt, out, &doc, &current, SaveReason::Edit, flags, force).await
}

async fn check(file: CodexFileArg, host: Option<&str>, rt: &Runtime, out: &mut Output) -> Result {
    let kind = file.kind();
    let doc = open(rt, target(host), kind).await?;
    let Some(text_now) = doc.text() else {
        return Err(failure(
            Error::invalid(out.label("cli.config.not_utf8")),
            exit::ERROR,
        ));
    };
    let owner = doc.profile.as_ref().map(ToString::to_string);
    let problems = codex_files::validate(kind, &text_now, &check_context(&doc, owner.as_deref()));
    print_problems(out, kind.file_name(), &problems)?;
    let owned: Vec<&str> = problems
        .iter()
        .filter(|p| p.severity == ProblemSeverity::Info)
        .filter_map(|p| p.path.as_deref())
        .collect();
    if !owned.is_empty() {
        out.line(text("cli.config.owned_list").arg("keys", owned.join(", ")))?;
    }
    let bad = problems.iter().any(|p| p.blocks_save() || p.needs_force());
    if problems.iter().all(|p| p.severity == ProblemSeverity::Info) {
        out.line(text("cli.config.no_problems").arg("path", &doc.path))?;
    }
    out.value(&problems)?;
    Ok(if bad { exit::ERROR } else { exit::OK })
}

async fn backups(
    file: Option<CodexFileArg>,
    host: Option<&str>,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let list = rt
        .service
        .codex_file_backups(CodexFileBackupQuery {
            target: host.map(remote::target),
            kind: file.map(CodexFileArg::kind),
        })
        .await?;
    let rows = list
        .iter()
        .map(|b| {
            vec![
                b.id.clone(),
                out.time(b.at),
                b.kind.file_name().to_owned(),
                where_label(out, &b.target, &b.target_label),
                out.render(&b.source.text()),
                b.size.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.config.backups_title",
        &[
            "cli.config.col_id",
            "cli.config.col_time",
            "cli.config.col_file",
            "cli.config.col_where",
            "cli.config.col_source",
            "cli.config.col_size",
        ],
        rows,
    )?;
    if !list.is_empty() {
        out.line(text("cli.config.restore_hint"))?;
    }
    out.value(&list)?;
    Ok(exit::OK)
}

async fn restore(id: &str, flags: Flags, rt: &Runtime, out: &mut Output) -> Result {
    let backup = rt.service.codex_file_backup(id).await?;
    let doc = open(rt, backup.backup.target.clone(), backup.backup.kind).await?;
    if let Some(why) = &doc.read_only {
        return Err(failure(Error::other(out.render(why)), exit::ERROR));
    }
    let Some(new) = superba_core::codex_files::text_of(backup.contents.expose()) else {
        return Err(failure(
            Error::invalid(out.label("cli.config.not_utf8")),
            exit::ERROR,
        ));
    };
    out.line(
        text("cli.config.restoring")
            .arg("id", id)
            .arg_text("source", backup.backup.source.text())
            .arg("time", out.time(backup.backup.at))
            .arg("path", &doc.path),
    )?;
    if doc.text().as_deref() == Some(new.as_str()) {
        out.line(text("cli.config.same_as_backup"))?;
        return Ok(exit::OK);
    }
    let owner = doc.profile.as_ref().map(ToString::to_string);
    let problems = codex_files::validate(doc.kind, &new, &check_context(&doc, owner.as_deref()));
    print_problems(out, doc.kind.file_name(), &problems)?;
    if problems.iter().any(FileProblem::blocks_save) {
        out.line(text("cli.config.not_saved"))?;
        return Ok(exit::ERROR);
    }
    let mut force = flags.force;
    if !force && problems.iter().any(FileProblem::needs_force) {
        if interactive(rt, out) && ask(rt, out, text("cli.config.save_anyway"), false) {
            force = true;
        } else {
            out.line(text("cli.config.needs_force"))?;
            return Ok(exit::NEEDS_CONFIRMATION);
        }
    }
    confirm_and_save(
        rt,
        out,
        &doc,
        &new,
        SaveReason::Restore {
            backup: id.to_owned(),
        },
        flags,
        force,
    )
    .await
}
