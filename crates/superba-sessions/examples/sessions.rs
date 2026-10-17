//! Developer tool: run the session migrator against a Codex home and print JSON.
//!
//! ```text
//! cargo run -p superba-sessions --example sessions -- <command> \
//!     --codex-home <dir> --data-dir <dir> [--sqlite-home <dir>] \
//!     [--from <provider>] [--to <provider>] [--live-only] [--id <migration id>]
//! commands: scan | plan | migrate | rollback | history | recover
//! ```
//!
//! `migrate` plans and executes in one go. Point it only at test homes.

use std::path::PathBuf;
use std::process::ExitCode;

use superba_core::{CodexHome, DataDir, MigrationRequest, MigrationSelector, SessionMigrator};
use superba_sessions::CodexSessionMigrator;

fn usage() -> ExitCode {
    eprintln!(
        "usage: sessions <scan|plan|migrate|rollback|history|recover> --codex-home DIR \
         --data-dir DIR [--sqlite-home DIR] [--from ID] [--to ID] [--live-only] [--id ID]"
    );
    ExitCode::from(2)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().cloned() else {
        return usage();
    };
    let mut codex_home = None;
    let mut sqlite_home = None;
    let mut data_dir = None;
    let mut from = "krill".to_owned();
    let mut to = "openai".to_owned();
    let mut include_archived = true;
    let mut id = None;
    let mut it = args.iter().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned();
        match flag.as_str() {
            "--codex-home" => codex_home = value().map(PathBuf::from),
            "--sqlite-home" => sqlite_home = value().map(PathBuf::from),
            "--data-dir" => data_dir = value().map(PathBuf::from),
            "--from" => from = value().unwrap_or_default(),
            "--to" => to = value().unwrap_or_default(),
            "--id" => id = value(),
            "--live-only" => include_archived = false,
            _ => return usage(),
        }
    }
    let (Some(codex_home), Some(data_dir)) = (codex_home, data_dir) else {
        return usage();
    };
    let mut home = CodexHome::new(codex_home);
    if let Some(dir) = sqlite_home {
        home = home.with_sqlite_home(dir);
    }
    let migrator = CodexSessionMigrator::new(DataDir::new(data_dir));
    let request = MigrationRequest {
        to_provider: to,
        selectors: vec![MigrationSelector::Provider { provider: from }],
        include_archived,
        exclude: vec![],
    };
    let result = match command.as_str() {
        "scan" => migrator
            .scan(&home)
            .await
            .map(|r| serde_json::to_value(r).unwrap_or_default()),
        "plan" => migrator
            .plan(&home, &request)
            .await
            .map(|r| serde_json::to_value(r).unwrap_or_default()),
        "migrate" => match migrator.plan(&home, &request).await {
            Ok(plan) => migrator
                .migrate(&home, &plan)
                .await
                .map(|r| serde_json::to_value(r).unwrap_or_default()),
            Err(e) => Err(e),
        },
        "rollback" => match id {
            Some(id) => migrator
                .rollback(&home, &id)
                .await
                .map(|r| serde_json::to_value(r).unwrap_or_default()),
            None => return usage(),
        },
        "history" => migrator
            .history(&home)
            .await
            .map(|r| serde_json::to_value(r).unwrap_or_default()),
        "recover" => migrator
            .recover(&home)
            .await
            .map(|r| serde_json::to_value(r).unwrap_or_default()),
        _ => return usage(),
    };
    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
