use superba_core::{MigrationRequest, MigrationSelector, SwitchIntent};

use super::{Result, execute, request};
use crate::{Output, Runtime, SessionsCommand, exit};

pub(super) async fn run(
    command: Option<&SessionsCommand>,
    host: Option<&str>,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    if let Some(h) = host.filter(|h| *h != "local") {
        if command.is_some_and(|c| !matches!(c, SessionsCommand::Scan)) {
            return Err(super::usage(
                out,
                crate::text::text("cli.hosts.sessions_read_only"),
            ));
        }
        let id = if h.contains(':') {
            superba_core::TargetId::new(h)
        } else {
            superba_core::TargetId::remote(h)
        };
        let sessions = rt.service.host_sessions(&id).await?;
        let rows = sessions
            .report
            .by_provider
            .iter()
            .map(|(provider, counts)| {
                vec![
                    provider.clone(),
                    counts.distinct.to_string(),
                    counts.rollouts.to_string(),
                    counts.archived.to_string(),
                ]
            })
            .collect::<Vec<_>>();
        out.table(
            &[
                "cli.label.provider",
                "cli.label.distinct",
                "cli.label.rollouts",
                "cli.label.archived",
            ],
            rows,
        )?;
        out.line(crate::text::text("cli.hosts.sessions_note"))?;
        if let Some(cmd) = &sessions.migrate_command {
            out.line(crate::text::text("cli.hosts.sessions_command"))?;
            out.data_line(cmd)?;
        }
        out.value(&sessions)?;
        return Ok(exit::OK);
    }
    match command {
        None | Some(SessionsCommand::Scan) => {
            let (report, history) = rt.service.sessions().await?;
            let rows = report
                .by_provider
                .iter()
                .map(|(provider, counts)| {
                    vec![
                        provider.clone(),
                        counts.distinct.to_string(),
                        counts.threads.to_string(),
                        counts.rollouts.to_string(),
                        counts.archived.to_string(),
                        counts.compressed.to_string(),
                    ]
                })
                .collect::<Vec<_>>();
            out.table(
                &[
                    "cli.label.provider",
                    "cli.label.distinct",
                    "cli.label.threads",
                    "cli.label.rollouts",
                    "cli.label.archived",
                    "cli.label.compressed",
                ],
                rows,
            )?;
            out.field("cli.label.inconsistent", report.inconsistent)?;
            for notice in &report.warnings {
                out.notice(notice)?;
            }
            let rows = history
                .iter()
                .map(|entry| {
                    vec![
                        entry.id.clone(),
                        entry.migrated_at.to_string(),
                        entry.to_provider.clone(),
                        entry
                            .by_source_provider
                            .iter()
                            .map(|(p, n)| format!("{p}: {n}"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        entry.threads.to_string(),
                        entry.rollouts.to_string(),
                        out.boolean(entry.rolled_back),
                    ]
                })
                .collect::<Vec<_>>();
            out.table(
                &[
                    "cli.label.id",
                    "cli.label.migrated_at",
                    "cli.label.to_provider",
                    "cli.label.by_source",
                    "cli.label.db_rows",
                    "cli.label.rollouts",
                    "cli.label.rolled_back",
                ],
                rows,
            )?;
            out.value(&serde_json::json!({ "report": report, "history": history }))?;
            Ok(exit::OK)
        }
        Some(SessionsCommand::Migrate {
            from,
            to,
            no_archived,
            exclude_thread,
            exec,
        }) => {
            let intent = SwitchIntent::MigrateSessions {
                request: MigrationRequest {
                    to_provider: to.clone(),
                    selectors: vec![MigrationSelector::Provider {
                        provider: from.clone(),
                    }],
                    include_archived: !no_archived,
                    exclude: exclude_thread.clone(),
                },
            };
            execute::run(request(intent, exec), exec, rt, out).await
        }
        Some(SessionsCommand::Rollback { migration_id, exec }) => {
            execute::run(
                request(
                    SwitchIntent::RollbackMigration {
                        migration_id: migration_id.clone(),
                    },
                    exec,
                ),
                exec,
                rt,
                out,
            )
            .await
        }
    }
}
