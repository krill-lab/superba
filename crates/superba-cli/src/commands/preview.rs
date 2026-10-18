use std::path::Path;

use superba_core::{
    AuthMode, EnvLocation, EnvOp, EnvPlan, KeyResolution, LegacySessionChoice, SwitchIntent,
    SwitchRequest,
};

use crate::{ExecArgs, Output, Runtime, text::text};

pub(super) fn environment(
    env: &EnvPlan,
    args: &ExecArgs,
    rt: &Runtime,
    out: &mut Output,
) -> superba_core::Result<()> {
    let home = rt
        .env
        .var_os("HOME")
        .or_else(|| rt.env.var_os("USERPROFILE"));
    let home = home.as_deref().map(Path::new);
    let locations = env
        .edits
        .iter()
        .map(|edit| location(&edit.location, home, out))
        .collect::<Vec<_>>();
    let mut unique = Vec::new();
    for location in &locations {
        if !unique.contains(location) {
            unique.push(location.clone());
        }
    }
    let value = match &env.op {
        EnvOp::Set { value, .. } => value.clone(),
        EnvOp::Clear => out.label("cli.env.clear"),
        EnvOp::RestoreBaseline { .. } => out.label("cli.env.restore"),
    };
    if !unique.is_empty() {
        out.line(
            text("cli.format.environment")
                .arg("var", &env.var)
                .arg("value", value)
                .arg("locations", unique.join(", ")),
        )?;
    } else {
        out.line(text("cli.env.unchanged").arg("var", &env.var))?;
    }
    if args.dry_run && out.verbose {
        for (edit, location) in env.edits.iter().zip(locations) {
            out.line(text("cli.format.section").arg("title", location))?;
            out.data_line(&edit.preview)?;
        }
    }
    for note in &env.scope_notes {
        out.line(note.clone())?;
    }
    Ok(())
}

fn location(location: &EnvLocation, home: Option<&Path>, out: &Output) -> String {
    match location {
        EnvLocation::ShellRc { path, .. }
        | EnvLocation::EnvironmentD { path }
        | EnvLocation::LaunchAgent { path } => {
            if let Some(relative) = home
                .filter(|home| home.is_absolute())
                .and_then(|home| path.strip_prefix(home).ok())
            {
                Path::new("~").join(relative).display().to_string()
            } else {
                path.display().to_string()
            }
        }
        EnvLocation::Launchctl => "launchctl".into(),
        EnvLocation::WindowsUserEnv => "HKCU\\Environment".into(),
        _ => out.label("cli.value.unknown"),
    }
}

/// Reconstruct the alternative from the typed request, keeping profile,
/// targets, answers and execution flags from this invocation. No secrets occur
/// in a request, and shell quoting prevents model names or paths becoming code.
pub(super) fn alternative(
    request: &SwitchRequest,
    args: &ExecArgs,
    out: &Output,
) -> Option<String> {
    let SwitchIntent::Profile {
        profile_id,
        line_id,
        auth_mode,
    } = &request.intent
    else {
        return None;
    };
    let mut words = vec![
        out.binary.to_owned(),
        "switch".into(),
        "--profile".into(),
        profile_id.to_string(),
    ];
    if let Some(line) = line_id {
        option(&mut words, "--line", line);
    }
    if let Some(mode) = auth_mode {
        option(
            &mut words,
            "--auth",
            match mode {
                AuthMode::Pat => "pat",
                AuthMode::ApiKey => "apikey",
                _ => return None,
            },
        );
    }
    if let Some(model) = &request.model_override {
        option(&mut words, "--model", model);
    }
    if !request.targets.local {
        words.push("--no-local".into());
    }
    for target in &request.targets.remotes {
        option(&mut words, "--remote", target);
    }
    if let Some(choice) = request.legacy_sessions {
        words.push(
            match choice {
                LegacySessionChoice::Migrate => "--migrate-legacy-sessions",
                LegacySessionChoice::Skip => "--skip-legacy-sessions",
            }
            .into(),
        );
    }
    if let Some(take_over) = request.take_over_foreign_env {
        words.push(
            if take_over {
                "--take-over-env"
            } else {
                "--keep-env"
            }
            .into(),
        );
    }
    for (enabled, flag) in [
        (request.accept_unknown_versions, "--accept-unknown-version"),
        (request.accept_shared_home, "--accept-shared-home"),
        (args.yes, "--yes"),
        (args.allow_full_restart, "--allow-full-restart"),
        (args.wait_turn, "--wait-turn"),
        (args.force, "--force"),
        (args.write_only, "--write-only"),
        (args.dry_run, "--dry-run"),
    ] {
        if enabled {
            words.push(flag.into());
        }
    }
    for (path, resolution) in &request.key_resolutions {
        if path == superba_core::ALL_CHANGED && *resolution == KeyResolution::Overwrite {
            words.push("--take-over".into());
            continue;
        }
        option(
            &mut words,
            match resolution {
                KeyResolution::KeepCurrent => "--keep-key",
                KeyResolution::Overwrite => "--overwrite-key",
            },
            path,
        );
    }
    for thread in &request.migration_exclude {
        option(&mut words, "--exclude-thread", thread);
    }
    if args.countdown > 0 {
        option(&mut words, "--countdown", args.countdown);
    }
    Some(
        words
            .iter()
            .map(|word| shell_word(word))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn option(words: &mut Vec<String>, flag: &str, value: impl ToString) {
    words.extend([flag.into(), value.to_string()]);
}

fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
    {
        return value.into();
    }
    // Single quotes work in POSIX shells; Windows suggestions target PowerShell.
    let escaped = if cfg!(windows) {
        value.replace('\'', "''")
    } else {
        value.replace('\'', "'\\''")
    };
    format!("'{escaped}'")
}
