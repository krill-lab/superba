//! `spb notify`: channels, preferences, the test and send commands, the
//! harness hook entry points (`spb notify codex`, `spb notify hook <harness>`)
//! and the Codex hook installer (GH#12).
//!
//! The hook entry points never write to stdout (Codex and Claude Code read a
//! hook's stdout as its answer) and always exit 0: a notification problem must
//! never disturb the harness. With `--verbose` they report on stderr.

use superba_core::notify::{QuietHours, harness};
use superba_core::{
    ChannelKind, ChannelOutcome, EventKind, HookRequest, HookState, LocalizedText, NotifyChannel,
    NotifyEvent, NotifyReport, NotifySecrets, Secret, SendOptions,
};

use super::{Result, missing, usage};
use crate::{
    NotifyChannelsCommand, NotifyCommand, NotifyFieldArgs, NotifySetArgs, Output, Runtime, exit,
    text::text,
};

pub(super) async fn run(command: &NotifyCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        NotifyCommand::Status => status(rt, out).await,
        NotifyCommand::Channels { command } => match command {
            None | Some(NotifyChannelsCommand::List) => list(rt, out).await,
            Some(NotifyChannelsCommand::Kinds) => kinds(out),
            Some(NotifyChannelsCommand::Add { kind, fields }) => add(kind, fields, rt, out).await,
            Some(NotifyChannelsCommand::Edit {
                id,
                fields,
                enabled,
            }) => edit(id, fields, enabled.map(|e| e.is_on()), rt, out).await,
            Some(NotifyChannelsCommand::Remove { id }) => {
                rt.service.notify_delete_channel(id).await?;
                out.line(text("cli.notify.removed").arg("id", id))?;
                out.value(&serde_json::json!({ "removed": id }))?;
                Ok(exit::OK)
            }
        },
        NotifyCommand::Test { channel } => {
            let mut event = NotifyEvent::new(EventKind::Test, harness::SUPERBA, rt.clock.now());
            event.detail = Some(out.render(&LocalizedText::new(
                "notify.msg.test_body",
                "Superba can reach this channel.",
            )));
            let options = SendOptions {
                channel: channel.clone(),
                force: true,
            };
            let report = rt.service.notify_send(event, options).await?;
            report_lines(&report, out)?;
            out.value(&report)?;
            Ok(if report.sent() > 0 && report.failed() == 0 {
                exit::OK
            } else {
                exit::ERROR
            })
        }
        NotifyCommand::Send {
            text: body,
            event,
            harness: h,
            channel,
            force,
        } => {
            let kind = match event {
                None => EventKind::Message,
                Some(k) => EventKind::parse(k)
                    .ok_or_else(|| usage(out, text("cli.notify.unknown_event").arg("event", k)))?,
            };
            let mut e = NotifyEvent::new(kind, h.clone(), rt.clock.now());
            e.detail = Some(body.clone());
            let options = SendOptions {
                channel: channel.clone(),
                force: *force,
            };
            let report = rt.service.notify_send(e, options).await?;
            report_lines(&report, out)?;
            out.value(&report)?;
            Ok(if report.failed() == 0 {
                exit::OK
            } else {
                exit::ERROR
            })
        }
        NotifyCommand::Set(args) => set(args, rt, out).await,
        NotifyCommand::Install {
            harness: h,
            approvals,
            trust,
        } => {
            let code = install(h, *approvals, true, rt, out).await?;
            if *trust && h == "codex" {
                // Codex computes the hash; Superba records it like /hooks does.
                let hooks = rt
                    .service
                    .notify_set_hook(HookRequest {
                        harness: harness::CODEX.into(),
                        mechanism: superba_switch::notify::MECH_CODEX_TRUST.into(),
                        install: true,
                    })
                    .await?;
                if let Some(v) = hooks
                    .hooks
                    .iter()
                    .find(|v| v.mechanism == superba_switch::notify::MECH_CODEX_PERMISSION)
                {
                    out.field("cli.notify.hook_state", out.label(state_key(v.state)))?;
                }
            }
            Ok(code)
        }
        NotifyCommand::Uninstall {
            harness: h,
            approvals,
        } => install(h, *approvals, false, rt, out).await,
        NotifyCommand::Codex { args } => {
            let (chained, payload) = superba_switch::notify::split_codex_args(args);
            if let Some(payload) = &payload
                && !chained.is_empty()
            {
                run_chained(&chained, payload, out);
            }
            match payload {
                Some(p) => hook(harness::CODEX, &p, rt, out).await,
                None => Ok(exit::OK),
            }
        }
        NotifyCommand::Hook {
            harness: h,
            payload,
        } => {
            let payload = match payload {
                Some(p) => p.clone(),
                None => (rt.read_stdin_all)().unwrap_or_default(),
            };
            hook(h, &payload, rt, out).await
        }
        NotifyCommand::Event { kind, detail, id } => {
            let Some(kind) = EventKind::parse(kind) else {
                return Err(usage(
                    out,
                    text("cli.notify.unknown_event").arg("event", kind),
                ));
            };
            let mut e = NotifyEvent::new(kind, harness::SUPERBA, rt.clock.now());
            e.detail = detail.clone();
            e.turn_id = id.clone();
            let report = rt.service.notify_send(e, SendOptions::default()).await?;
            out.value(&report)?;
            Ok(exit::OK)
        }
    }
}

/// Start the user's own Codex `notify` program with the same JSON (not
/// waited for, like Codex does).
fn run_chained(chained: &[String], payload: &str, out: &Output) {
    let Some((program, args)) = chained.split_first() else {
        return;
    };
    let result = superba_core::process::command(program)
        .args(args)
        .arg(payload)
        .stdin(std::process::Stdio::null())
        .spawn();
    if let Err(e) = result
        && out_verbose(out)
    {
        eprintln!("notify: cannot start {program}: {e}");
    }
}

fn out_verbose(out: &Output) -> bool {
    out.verbose
}

async fn hook(h: &str, payload: &str, rt: &Runtime, out: &mut Output) -> Result {
    match rt.service.notify_hook(h, payload).await {
        Ok(report) if out_verbose(out) => {
            for line in report_text(&report, out) {
                eprintln!("{line}");
            }
        }
        Ok(_) => {}
        Err(e) if out_verbose(out) => eprintln!("notify: {e}"),
        Err(_) => {}
    }
    // Never an error for the harness.
    Ok(exit::OK)
}

fn report_text(report: &NotifyReport, out: &Output) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(reason) = report.skipped {
        lines.push(out.render(&text("cli.notify.skipped").arg_text("reason", reason.label())));
    }
    for c in &report.channels {
        let line = match &c.outcome {
            ChannelOutcome::Sent { ms } => {
                text("cli.notify.sent").arg("name", &c.name).arg("ms", ms)
            }
            ChannelOutcome::Skipped { reason } => text("cli.notify.channel_skipped")
                .arg("name", &c.name)
                .arg_text("reason", reason.label()),
            ChannelOutcome::Failed { error } => text("cli.notify.failed")
                .arg("name", &c.name)
                .arg("error", error),
        };
        lines.push(out.render(&line));
    }
    if report.skipped.is_none() && report.channels.is_empty() {
        lines.push(out.label("cli.notify.no_channels"));
    }
    lines
}

fn report_lines(report: &NotifyReport, out: &mut Output) -> Result<()> {
    for line in report_text(report, out) {
        out.data_line(&line)?;
    }
    Ok(())
}

async fn status(rt: &Runtime, out: &mut Output) -> Result {
    let set = rt.service.profile_set().await?;
    let n = &set.settings.notify;
    let hooks = rt.service.notify_hooks().await?;
    out.field("cli.notify.enabled", out.boolean(n.enabled))?;
    let on: Vec<String> = EventKind::ALL
        .into_iter()
        .filter(|k| !k.always_sent() && n.triggers.wants(*k))
        .map(|k| out.render(&k.label()))
        .collect();
    out.field("cli.notify.events", on.join(", "))?;
    out.field(
        "cli.notify.quiet",
        if n.quiet_hours.enabled {
            n.quiet_hours.display()
        } else {
            out.label("cli.value.off")
        },
    )?;
    out.field(
        "cli.notify.rate",
        if n.rate_limit.per_minute == 0 {
            out.label("cli.value.off")
        } else {
            out.render(&text("cli.notify.per_minute").arg("n", n.rate_limit.per_minute))
        },
    )?;
    out.field("cli.notify.summary", out.boolean(n.content.summary))?;
    out.field("cli.notify.channels", n.channels.len())?;
    for c in &n.channels {
        out.data_line(&format!(
            "  {} · {} · {}{}",
            c.id,
            c.display_name(),
            out.render(&c.kind.label()),
            if c.enabled {
                String::new()
            } else {
                format!(" ({})", out.label("cli.value.off"))
            }
        ))?;
    }
    out.field("cli.notify.program", &hooks.program)?;
    for h in &hooks.hooks {
        let state = out.label(state_key(h.state));
        out.data_line(&format!(
            "  {} {} · {}",
            harness::display_name(&h.harness),
            h.mechanism,
            state
        ))?;
    }
    out.value(&serde_json::json!({ "settings": n, "hooks": hooks }))?;
    Ok(exit::OK)
}

async fn list(rt: &Runtime, out: &mut Output) -> Result {
    let set = rt.service.profile_set().await?;
    let channels = &set.settings.notify.channels;
    if channels.is_empty() {
        out.line(text("cli.notify.no_channels"))?;
    }
    let rows: Vec<Vec<String>> = channels
        .iter()
        .map(|c| {
            let mut fields: Vec<String> =
                c.params.iter().map(|(k, v)| format!("{k}={v}")).collect();
            fields.extend(c.secret_hints.iter().map(|(k, v)| format!("{k}={v}")));
            vec![
                c.id.clone(),
                c.display_name(),
                out.render(&c.kind.label()),
                out.boolean(c.enabled),
                fields.join(" "),
            ]
        })
        .collect();
    out.table(
        &[
            "cli.notify.col_id",
            "cli.notify.col_name",
            "cli.notify.col_kind",
            "cli.notify.col_on",
            "cli.notify.col_fields",
        ],
        rows,
    )?;
    out.value(channels)?;
    Ok(exit::OK)
}

fn kinds(out: &mut Output) -> Result {
    let rows: Vec<Vec<String>> = ChannelKind::ALL
        .iter()
        .map(|k| {
            let fields: Vec<String> = k
                .fields()
                .iter()
                .map(|f| {
                    format!(
                        "{}{}{}",
                        f.key,
                        if f.secret { "*" } else { "" },
                        if f.required { "" } else { "?" }
                    )
                })
                .collect();
            vec![
                k.id().to_owned(),
                out.render(&k.label()),
                fields.join(" "),
                out.render(&k.route_note()),
            ]
        })
        .collect();
    out.table(
        &[
            "cli.notify.col_kind",
            "cli.notify.col_name",
            "cli.notify.col_fields",
            "cli.notify.col_route",
        ],
        rows,
    )?;
    out.line(text("cli.notify.kinds_legend"))?;
    let json: Vec<serde_json::Value> = ChannelKind::ALL
        .iter()
        .map(|k| {
            serde_json::json!({
                "kind": k.id(),
                "third_party_relay": k.third_party_relay(),
                "fields": k.fields().iter().map(|f| serde_json::json!({
                    "key": f.key, "secret": f.secret, "required": f.required, "default": f.default,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    out.value(&json)?;
    Ok(exit::OK)
}

fn apply_fields(
    channel: &mut NotifyChannel,
    fields: &NotifyFieldArgs,
    rt: &Runtime,
    out: &Output,
) -> std::result::Result<NotifySecrets, super::Failure> {
    if let Some(name) = &fields.name {
        channel.name = name.clone();
    }
    let field = |key: &str, out: &Output| {
        channel
            .kind
            .field(key)
            .ok_or_else(|| usage(out, text("cli.notify.unknown_field").arg("field", key)))
    };
    let mut params = Vec::new();
    for p in &fields.params {
        let (k, v) = p
            .split_once('=')
            .ok_or_else(|| usage(out, text("cli.notify.bad_param").arg("param", p)))?;
        if field(k, out)?.secret {
            return Err(usage(
                out,
                text("cli.notify.secret_on_argv").arg("field", k),
            ));
        }
        params.push((k.to_owned(), v.to_owned()));
    }
    let mut secrets = NotifySecrets::default();
    for s in &fields.secret_env {
        let (k, var) = s
            .split_once('=')
            .ok_or_else(|| usage(out, text("cli.notify.bad_param").arg("param", s)))?;
        if !field(k, out)?.secret {
            return Err(usage(out, text("cli.notify.not_secret").arg("field", k)));
        }
        let value = rt
            .env
            .var_non_empty(var)
            .ok_or_else(|| usage(out, text("cli.error.key_env").arg("var", var)))?;
        secrets.set(k, Secret::new(value));
    }
    if let Some(k) = &fields.secret_stdin {
        if !field(k, out)?.secret {
            return Err(usage(out, text("cli.notify.not_secret").arg("field", k)));
        }
        let line = (rt.read_stdin_line)()
            .map_err(|_| superba_core::Error::other(out.label("cli.error.stdin")))?;
        secrets.set(k.clone(), Secret::new(line));
    }
    for (k, v) in params {
        if v.is_empty() {
            channel.params.remove(&k);
        } else {
            channel.params.insert(k, v);
        }
    }
    Ok(secrets)
}

async fn add(kind: &str, fields: &NotifyFieldArgs, rt: &Runtime, out: &mut Output) -> Result {
    let kind = ChannelKind::parse(kind)
        .ok_or_else(|| usage(out, text("cli.notify.unknown_kind").arg("kind", kind)))?;
    let mut channel = NotifyChannel::new("", kind, "");
    let secrets = apply_fields(&mut channel, fields, rt, out)?;
    let saved = rt.service.notify_save_channel(channel, secrets).await?;
    saved_lines(&saved, out)?;
    out.value(&saved)?;
    Ok(exit::OK)
}

async fn edit(
    id: &str,
    fields: &NotifyFieldArgs,
    enabled: Option<bool>,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let set = rt.service.profile_set().await?;
    let mut channel = set
        .settings
        .notify
        .channel(id)
        .cloned()
        .ok_or_else(|| missing(out, text("cli.notify.no_channel").arg("id", id)))?;
    let secrets = apply_fields(&mut channel, fields, rt, out)?;
    if let Some(on) = enabled {
        channel.enabled = on;
    }
    let saved = rt.service.notify_save_channel(channel, secrets).await?;
    saved_lines(&saved, out)?;
    out.value(&saved)?;
    Ok(exit::OK)
}

fn saved_lines(c: &NotifyChannel, out: &mut Output) -> Result<()> {
    out.line(
        text("cli.notify.saved")
            .arg("name", c.display_name())
            .arg("id", &c.id),
    )?;
    if c.kind.third_party_relay() {
        out.line(c.kind.route_note())?;
    }
    for (k, v) in &c.secret_hints {
        out.data_line(&format!("  {k}: {v}"))?;
    }
    Ok(())
}

fn on_off(v: &str) -> Option<bool> {
    match v {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

async fn set(args: &NotifySetArgs, rt: &Runtime, out: &mut Output) -> Result {
    let mut settings = rt.service.profile_set().await?.settings;
    let n = &mut settings.notify;
    if let Some(e) = args.enabled {
        n.enabled = e.is_on();
    }
    for (list, on) in [(&args.on, true), (&args.off, false)] {
        for k in list {
            let kind = EventKind::parse(k)
                .ok_or_else(|| usage(out, text("cli.notify.unknown_event").arg("event", k)))?;
            n.triggers.set(kind, on);
        }
    }
    if let Some(q) = &args.quiet_hours {
        if on_off(q) == Some(false) {
            n.quiet_hours.enabled = false;
        } else {
            let parsed = QuietHours::parse(q)
                .ok_or_else(|| usage(out, text("cli.notify.bad_quiet").arg("value", q)))?;
            n.quiet_hours.start_min = parsed.start_min;
            n.quiet_hours.end_min = parsed.end_min;
            n.quiet_hours.enabled = true;
        }
    }
    if let Some(u) = args.urgent_in_quiet {
        n.quiet_hours.allow_urgent = u.is_on();
    }
    if let Some(r) = args.rate {
        n.rate_limit.per_minute = r;
    }
    if let Some(s) = args.min_turn_secs {
        n.triggers.min_turn_secs = s;
    }
    if let Some(s) = args.summary {
        n.content.summary = s.is_on();
    }
    for h in &args.mute {
        n.triggers.muted_harnesses.insert(h.clone());
    }
    for h in &args.unmute {
        n.triggers.muted_harnesses.remove(h);
    }
    let snapshot = n.clone();
    rt.service.save_settings(settings).await?;
    out.line(text("cli.notify.settings_saved"))?;
    out.value(&snapshot)?;
    Ok(exit::OK)
}

async fn install(h: &str, approvals: bool, on: bool, rt: &Runtime, out: &mut Output) -> Result {
    match h {
        "codex" => {
            let mechanism = if approvals {
                superba_switch::notify::MECH_CODEX_PERMISSION
            } else {
                superba_switch::notify::MECH_CODEX_NOTIFY
            };
            let hooks = rt
                .service
                .notify_set_hook(HookRequest {
                    harness: harness::CODEX.into(),
                    mechanism: mechanism.into(),
                    install: on,
                })
                .await?;
            for view in hooks.hooks.iter().filter(|v| v.mechanism == mechanism) {
                out.field("cli.notify.hook_state", out.label(state_key(view.state)))?;
                if let Some(file) = &view.file {
                    out.field("cli.notify.hook_file", file)?;
                }
                for note in &view.notes {
                    out.line(note.clone())?;
                }
            }
            out.value(&hooks)?;
            Ok(exit::OK)
        }
        "claude-code" | "claude" => {
            // The Claude Code settings writer owns settings.json: print the
            // entries it merges.
            let hooks = rt.service.notify_hooks().await?;
            let program = hooks.program.clone();
            let fragment = superba_switch::notify::claude_code_hooks(&program);
            out.line(text("cli.notify.claude_fragment"))?;
            if !out.json {
                out.data_line(&serde_json::to_string_pretty(&fragment).unwrap_or_default())?;
            }
            out.value(&serde_json::json!({ "hooks": fragment, "install": on }))?;
            Ok(exit::OK)
        }
        other => Err(usage(
            out,
            text("cli.notify.unknown_harness").arg("harness", other),
        )),
    }
}

fn state_key(state: HookState) -> &'static str {
    match state {
        HookState::Installed => "cli.notify.hook_installed",
        HookState::Untrusted => "cli.notify.hook_untrusted",
        HookState::Missing => "cli.notify.hook_missing",
        HookState::Foreign => "cli.notify.hook_foreign",
        HookState::Unreadable => "cli.notify.hook_unreadable",
        _ => "cli.notify.hook_external",
    }
}
