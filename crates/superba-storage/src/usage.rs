use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use superba_core::LocalContext;
use superba_core::data::{DataCategoryUsage, DataLocation, DataUsage};

use crate::location::{LOCATION_FILE, fixed_file};

pub const CATEGORIES: [&str; 9] = [
    "requests",
    "imported_usage",
    "session_backups",
    "config_backups",
    "cache",
    "logs",
    "downloads",
    "programs",
    "settings",
];

fn category(path: &Path) -> &'static str {
    let mut parts = path.components();
    let first = parts
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    let second = parts
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    match first.as_str() {
        "router" if second.starts_with("usage.sqlite3") => "requests",
        f if f.starts_with("imported-usage.sqlite3") => "imported_usage",
        "backups" if second.starts_with("migrate-") => "session_backups",
        "backups" | "journal" => "config_backups",
        "cache" | "feeds" => "cache",
        "logs" => "logs",
        "router" if second.ends_with(".log") => "logs",
        "notify" if second.ends_with(".jsonl") || second.ends_with(".log") => "logs",
        "downloads" => "downloads",
        "codex" | "tools" => "programs",
        _ => "settings",
    }
}

fn walk(root: &Path, visit: &mut impl FnMut(&Path, u64), skipped: &mut u64) {
    let mut todo = vec![root.to_path_buf()];
    while let Some(path) = todo.pop() {
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if path == root && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                *skipped += 1;
                continue;
            }
        };
        if crate::validation::is_link(&meta) {
            *skipped += 1;
            continue;
        }
        if meta.is_file() {
            visit(&path, meta.len());
        } else if meta.is_dir() {
            match fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(e) => todo.push(e.path()),
                            Err(_) => *skipped += 1,
                        }
                    }
                }
                Err(_) => *skipped += 1,
            }
        } else {
            *skipped += 1;
        }
    }
}

/// Logical file sizes without following links or reading message contents.
/// Missing directories are zero; inaccessible files mark the result partial.
pub fn usage(ctx: &LocalContext) -> DataUsage {
    let mut groups: BTreeMap<&str, DataCategoryUsage> = CATEGORIES
        .into_iter()
        .map(|c| {
            (
                c,
                DataCategoryUsage {
                    category: c.into(),
                    bytes: 0,
                    files: 0,
                },
            )
        })
        .collect();
    let mut skipped = 0;
    walk(
        ctx.data_dir.root(),
        &mut |path, bytes| {
            let rel = path
                .strip_prefix(ctx.data_dir.root())
                .expect("walk stays in root");
            if fixed_file(rel) {
                return;
            }
            let group = groups.get_mut(category(rel)).expect("known category");
            group.bytes = group.bytes.saturating_add(bytes);
            group.files += 1;
        },
        &mut skipped,
    );
    let home = ctx.codex_home();
    let mut codex_home_bytes = 0u64;
    let mut codex_sessions_bytes = 0u64;
    walk(
        home.root(),
        &mut |path, bytes| {
            codex_home_bytes = codex_home_bytes.saturating_add(bytes);
            if path.starts_with(home.sessions_dir())
                || path.starts_with(home.archived_sessions_dir())
            {
                codex_sessions_bytes = codex_sessions_bytes.saturating_add(bytes);
            }
        },
        &mut skipped,
    );
    let mut codex_sqlite_bytes = 0u64;
    if home.sqlite_home() != home.root() {
        for suffix in ["", "-wal", "-shm"] {
            let file = home.sqlite_home().join(format!(
                "{}{}",
                superba_core::paths::STATE_DB_FILE,
                suffix
            ));
            walk(
                &file,
                &mut |_, bytes| codex_sqlite_bytes += bytes,
                &mut skipped,
            );
        }
    }
    let categories: Vec<_> = CATEGORIES
        .into_iter()
        .map(|key| groups.remove(key).unwrap())
        .collect();
    DataUsage {
        location: DataLocation {
            directory: ctx.data_dir.root().to_path_buf(),
            pointer: ctx.data_dir.anchor().join(LOCATION_FILE),
            codex_home: home.root().to_path_buf(),
        },
        total_bytes: categories.iter().map(|c| c.bytes).sum(),
        categories,
        codex_home_bytes,
        codex_sessions_bytes,
        codex_sqlite_bytes,
        skipped,
    }
}
