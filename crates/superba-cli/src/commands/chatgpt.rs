//! `spb chatgpt`: the saved official ChatGPT accounts of this computer (PR #20,
//! issue #21): list them, add one through the browser sign-in, save the
//! current login, switch to one (an ordinary switch: preview, turns, restart,
//! verification, rollback), forget one and read their five-hour and weekly
//! Codex limits. No credential is ever printed.

use std::sync::Arc;

use superba_core::{
    ChatGptAccount, ChatGptLoginPrompt, ChatGptQuota, ChatGptWindow, RestoreTarget, SwitchIntent,
};

use super::{Result, execute, missing, request};
use crate::{ChatGptCommand, Output, Runtime, exit, text::text};

pub(super) async fn run(command: &ChatGptCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        ChatGptCommand::List => {
            let accounts = rt.service.chatgpt_accounts().await?;
            table(&accounts, &[], out)?;
            out.value(&accounts)?;
            Ok(exit::OK)
        }
        ChatGptCommand::Add => {
            // `spb` never opens a browser itself (it runs on servers too): it
            // prints the sign-in page as soon as Codex gives it.
            let (pages, mut page) = tokio::sync::mpsc::unbounded_channel::<String>();
            let events: superba_core::ChatGptLoginEvents = Arc::new(move |prompt| {
                if let Some(ChatGptLoginPrompt { url }) = prompt {
                    let _ = pages.send(url);
                }
                true
            });
            // The channel closes when the service drops `events` (done).
            let add = rt.service.chatgpt_add_account(events);
            let show = async {
                while let Some(url) = page.recv().await {
                    out.progress(text("cli.chatgpt.sign_in_page").arg("url", url))?;
                    out.progress(text("cli.chatgpt.add_waiting"))?;
                }
                Ok::<_, superba_core::Error>(())
            };
            let (added, shown) = tokio::join!(add, show);
            shown?;
            let added = added?;
            out.line(text("cli.chatgpt.saved").arg("email", &added.email))?;
            out.value(&added)?;
            Ok(exit::OK)
        }
        ChatGptCommand::Save => {
            let saved = rt.service.chatgpt_save_current().await?;
            out.line(text("cli.chatgpt.saved").arg("email", &saved.email))?;
            out.value(&saved)?;
            Ok(exit::OK)
        }
        ChatGptCommand::Use { account, exec } => {
            let accounts = rt.service.chatgpt_accounts().await?;
            let chosen = find(&accounts, account, out)?;
            let intent = SwitchIntent::Restore {
                target: RestoreTarget::Account {
                    identity: chosen.id.clone(),
                },
            };
            execute::run(request(intent, exec), exec, rt, out).await
        }
        ChatGptCommand::Remove { account } => {
            let accounts = rt.service.chatgpt_accounts().await?;
            let chosen = find(&accounts, account, out)?;
            rt.service.chatgpt_remove_account(&chosen.id).await?;
            out.line(text("cli.chatgpt.removed").arg("email", &chosen.email))?;
            out.value(chosen)?;
            Ok(exit::OK)
        }
        ChatGptCommand::Quota { account } => {
            let accounts = rt.service.chatgpt_accounts().await?;
            let chosen: Vec<&ChatGptAccount> = match account {
                Some(a) => vec![find(&accounts, a, out)?],
                None => accounts.iter().collect(),
            };
            let mut quotas = Vec::new();
            let mut failed = false;
            for a in chosen {
                out.progress(text("cli.chatgpt.reading").arg("email", &a.email))?;
                match rt.service.chatgpt_quota(&a.id).await {
                    Ok(q) => quotas.push(q),
                    Err(e) => {
                        failed = true;
                        out.line(
                            text("cli.chatgpt.read_failed")
                                .arg("email", &a.email)
                                .arg_text("error", e.to_text()),
                        )?;
                    }
                }
            }
            table(&accounts, &quotas, out)?;
            out.value(&quotas)?;
            Ok(if failed { exit::ERROR } else { exit::OK })
        }
    }
}

/// An account by its number in `spb chatgpt list`, its e-mail (any case) or
/// its id.
fn find<'a>(
    accounts: &'a [ChatGptAccount],
    wanted: &str,
    out: &Output,
) -> Result<&'a ChatGptAccount> {
    let wanted = wanted.trim();
    let by_number = wanted
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| accounts.get(i));
    by_number
        .or_else(|| {
            accounts
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case(wanted) || a.id == wanted)
        })
        .ok_or_else(|| missing(out, text("cli.chatgpt.unknown").arg("account", wanted)).into())
}

fn table(accounts: &[ChatGptAccount], quotas: &[ChatGptQuota], out: &mut Output) -> Result<()> {
    if accounts.is_empty() {
        out.line(text("cli.chatgpt.none"))?;
        return Ok(());
    }
    let window = |w: Option<&ChatGptWindow>, out: &Output| match w {
        Some(w) => match w.resets_at {
            Some(t) => out.render(
                &text("cli.chatgpt.window_reset")
                    .arg("percent", format!("{:.0}", w.remaining))
                    .arg("time", out.time(t)),
            ),
            None => out
                .render(&text("cli.chatgpt.window").arg("percent", format!("{:.0}", w.remaining))),
        },
        None => out.none(),
    };
    let rows: Vec<Vec<String>> = accounts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let q = quotas.iter().find(|q| q.account_id == a.id);
            let plan = q
                .and_then(|q| q.plan_type.as_deref())
                .or(a.plan_type.as_deref())
                .and_then(superba_core::chatgpt::plan_label)
                .unwrap_or_else(|| out.none());
            let mut row = vec![
                (i + 1).to_string(),
                a.email.clone(),
                plan,
                out.boolean(a.active),
                out.date(a.added_at),
            ];
            if !quotas.is_empty() {
                row.push(window(q.and_then(|q| q.five_hour.as_ref()), out));
                row.push(window(q.and_then(|q| q.weekly.as_ref()), out));
                row.push(q.map_or_else(|| out.none(), |q| out.time(q.fetched_at)));
            }
            row
        })
        .collect();
    let mut headers = vec![
        "cli.chatgpt.col.number",
        "cli.chatgpt.col.email",
        "cli.chatgpt.col.plan",
        "cli.chatgpt.col.in_use",
        "cli.chatgpt.col.added",
    ];
    if !quotas.is_empty() {
        headers.extend([
            "cli.chatgpt.col.five_hour",
            "cli.chatgpt.col.weekly",
            "cli.chatgpt.col.read_at",
        ]);
    }
    out.table(&headers, rows)?;
    Ok(())
}
