//! Reviewed configuration cleanup through the file editor's backup/save path.

use superba_config::codex_files::{CheckContext, validate};
use superba_config::toml_keys::ConfigDoc;
use superba_core::*;

use crate::Switcher;

pub(super) fn is_cleanup_notice(problem: &FileProblem) -> bool {
    problem.severity == ProblemSeverity::Warning
        && matches!(
            problem.message.key.as_str(),
            "codex_files.deprecated"
                | "codex_files.removed"
                | "codex_files.unknown_key"
                | "codex_files.not_in_version"
        )
}

pub(super) fn entries(
    doc: &CodexFileDoc,
    problems: &[FileProblem],
) -> Vec<TroubleshootingConfigEntry> {
    let Some(catalog) = doc.schema.catalog.as_deref() else {
        return Vec::new();
    };
    // The selected desktop app is the current consumer; use the CLI only when
    // there is no desktop installation. Other installed versions do not veto it.
    let Some(current) = doc
        .schema
        .consumers
        .iter()
        .find(|c| c.consumer == ConsumerKind::DesktopApp)
        .or_else(|| doc.schema.consumers.first())
    else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for problem in problems.iter().filter(|p| is_cleanup_notice(p)) {
        let Some(path) = &problem.path else {
            continue;
        };
        let support =
            superba_config::settings::key_support(path, std::slice::from_ref(current), catalog);
        if !support.iter().any(|s| {
            s.version.is_some()
                && matches!(
                    s.support,
                    KeySupport::Deprecated | KeySupport::Removed | KeySupport::Missing
                )
        }) {
            continue;
        }
        let entry = TroubleshootingConfigEntry {
            path: path.clone(),
            start: problem.start,
            end: problem.end,
        };
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    entries
}

impl Switcher {
    pub(crate) async fn troubleshooting_cleanup_impl(
        &self,
        request: TroubleshootingCleanup,
    ) -> Result<CodexFileSaved> {
        let doc = self
            .codex_file_impl(TargetId::local(), CodexFileKind::ConfigToml)
            .await?;
        if doc.sha256.as_deref() != Some(request.sha256.as_str()) {
            return Err(Error::Conflict { message: "config.toml changed after the diagnosis; check it again before removing entries.".into() });
        }
        let text = doc
            .text()
            .ok_or_else(|| Error::invalid("config.toml is not readable UTF-8"))?;
        let problems = validate(
            CodexFileKind::ConfigToml,
            &text,
            &CheckContext {
                schema: Some(&doc.schema),
                owned: &doc.owned,
                ..Default::default()
            },
        );
        let available = entries(&doc, &problems);
        if request.entries.is_empty()
            || request
                .entries
                .iter()
                .any(|entry| !available.contains(entry))
        {
            return Err(Error::Conflict {
                message:
                    "The reviewed configuration entries no longer match the diagnosis; check again."
                        .into(),
            });
        }
        let mut edited = ConfigDoc::parse(
            doc.contents
                .as_ref()
                .map(SecretBytes::expose)
                .unwrap_or_default(),
        )?;
        let mut entries = request.entries;
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.start));
        entries.dedup();
        for entry in entries {
            if !edited.remove_key_at(entry.start, entry.end) {
                return Err(Error::invalid(format!(
                    "Could not remove the reviewed configuration entry: {}",
                    entry.path
                )));
            }
        }
        self.save_codex_file_impl(CodexFileSave {
            target: TargetId::local(),
            kind: CodexFileKind::ConfigToml,
            contents: SecretBytes::new(edited.to_bytes()),
            base_sha256: doc.sha256,
            reason: SaveReason::Edit,
            force: false,
        })
        .await
    }
}
