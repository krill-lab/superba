//! GH#5 in `spb`: `--take-over` on switch / restore / uninstall, `spb
//! hand-off`, the conflict rows with who changed what, the interactive answer
//! at a terminal, and the external change in `spb status`.

mod common;

use std::sync::Arc;

use clap::Parser;
use common::*;
use superba_cli::{Cli, Output};
use superba_core::*;

fn conflict() -> PlanChoice {
    PlanChoice::OwnedKeyConflict {
        conflicts: vec![KeyChange {
            path: "model_provider".into(),
            before: Some("\"openai\"".into()),
            after: Some("\"custom\"".into()),
        }],
        auth: Some(AuthDrift {
            written: AuthShape::Pat {
                key_last4: "1111".into(),
            },
            live: AuthShape::Missing,
        }),
        source: ChangeSource::CcSwitch {
            provider: Some("Kimi".into()),
        },
    }
}

fn external() -> ExternalChange {
    ExternalChange {
        codex_home: "/fake/.codex".into(),
        keys: vec![KeyChange {
            path: "model_provider".into(),
            before: Some("\"openai\"".into()),
            after: Some("\"custom\"".into()),
        }],
        auth: Some(AuthDrift {
            written: AuthShape::Pat {
                key_last4: "1111".into(),
            },
            live: AuthShape::Missing,
        }),
        env: Vec::new(),
        source: ChangeSource::CcSwitch {
            provider: Some("Kimi".into()),
        },
        evidence: vec![
            LocalizedText::new(
                "ownership.evidence.store_current",
                "{tool}'s current Codex provider “{provider}” uses this address.",
            )
            .arg("tool", "cc-switch")
            .arg("provider", "Kimi"),
        ],
        fingerprint: "f1".into(),
    }
}

/// A service whose plans raise the conflict until it is taken over.
fn conflicting() -> Arc<FakeService> {
    let service = FakeService::new();
    service.script(|request| {
        let mut p = plan(request.clone());
        if !request.key_resolutions.contains_key(ALL_CHANGED)
            && !matches!(request.intent, SwitchIntent::HandOff { .. })
        {
            p.choices = vec![conflict()];
        }
        Ok(p)
    });
    service
}

#[tokio::test]
async fn take_over_answers_everything_on_switch_restore_and_uninstall() {
    for args in [
        vec!["switch", "--profile", "work", "--take-over", "--yes"],
        vec!["restore", "--take-over", "--yes"],
        vec!["uninstall", "--take-over", "--yes"],
    ] {
        let service = conflicting();
        let (code, _, stderr) = invoke(service.clone(), &args).await;
        assert_eq!(code, 0, "{args:?}: {stderr}");
        let calls = service.calls();
        let request = calls.plans.last().unwrap();
        assert_eq!(
            request.key_resolutions.get(ALL_CHANGED),
            Some(&KeyResolution::Overwrite),
            "{args:?}"
        );
        assert_eq!(calls.executions.len(), 1, "{args:?}");
    }
}

#[tokio::test]
async fn an_unanswered_conflict_shows_who_and_what_and_the_ways_forward() {
    let service = conflicting();
    let (code, stdout, _) = invoke(service.clone(), &["switch", "--profile", "work"]).await;
    assert_eq!(code, 4);
    assert!(
        stdout.contains("Changed outside Superba by cc-switch (provider “Kimi”):"),
        "{stdout}"
    );
    assert!(
        stdout.contains("model_provider: \"openai\" → \"custom\""),
        "{stdout}"
    );
    assert!(
        stdout.contains("auth.json: PAT …1111 → no auth.json"),
        "{stdout}"
    );
    assert!(
        stdout.contains("--take-over") && stdout.contains("hand-off"),
        "{stdout}"
    );
    assert!(service.calls().executions.is_empty());
}

async fn answered(answer: &'static str) -> (u8, String, Arc<FakeService>) {
    let service = conflicting();
    let mut rt = runtime(service.clone());
    rt.read_stdin_line = Arc::new(move || Ok(answer.to_owned()));
    let cli = Cli::try_parse_from(["spb", "switch", "--profile", "work", "--yes"]).unwrap();
    let (out, stdout, _) = Output::capture();
    let mut out = out.with_terminals(true, true);
    let code = superba_cli::run(&cli, &rt, &mut out).await;
    (code, stdout.contents(), service)
}

#[tokio::test]
async fn at_a_terminal_the_user_takes_over_hands_off_or_cancels() {
    let (code, stdout, service) = answered("t").await;
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("[t] take over"), "{stdout}");
    // A changed credential cannot be kept by a switch: no [k].
    assert!(!stdout.contains("[k]"), "{stdout}");
    let calls = service.calls();
    assert_eq!(
        calls.plans.last().unwrap().key_resolutions.get(ALL_CHANGED),
        Some(&KeyResolution::Overwrite)
    );
    assert_eq!(calls.executions.len(), 1);

    let (code, _, service) = answered("h").await;
    assert_eq!(code, 0);
    let calls = service.calls();
    assert!(matches!(
        &calls.plans.last().unwrap().intent,
        SwitchIntent::HandOff {
            to: ChangeSource::CcSwitch { provider: Some(p) }
        } if p == "Kimi"
    ));

    let (code, stdout, service) = answered("").await;
    assert_eq!(code, 5, "{stdout}");
    assert!(service.calls().executions.is_empty());
}

#[tokio::test]
async fn hand_off_names_the_tool_seen_or_the_one_given() {
    let service = FakeService::new();
    service.overview.lock().unwrap().external = Some(external());
    let (code, _, stderr) = invoke(service.clone(), &["hand-off", "--yes"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(matches!(
        &service.calls().plans.last().unwrap().intent,
        SwitchIntent::HandOff {
            to: ChangeSource::CcSwitch { provider: Some(p) }
        } if p == "Kimi"
    ));
    let (code, _, _) = invoke(
        service.clone(),
        &["hand-off", "--to", "ai-toolbox", "--yes"],
    )
    .await;
    assert_eq!(code, 0);
    assert!(matches!(
        &service.calls().plans.last().unwrap().intent,
        SwitchIntent::HandOff {
            to: ChangeSource::AiToolbox { provider: None }
        }
    ));
}

#[tokio::test]
async fn status_lists_the_external_change() {
    let service = FakeService::new();
    service.overview.lock().unwrap().external = Some(external());
    let (code, stdout, _) = invoke(service.clone(), &["status"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Changed outside Superba by cc-switch (provider “Kimi”):"),
        "{stdout}"
    );
    assert!(
        stdout.contains("why: cc-switch's current Codex provider “Kimi” uses this address."),
        "{stdout}"
    );
    assert!(stdout.contains("spb switch --take-over"), "{stdout}");
    let (_, stdout, _) = invoke(service, &["status", "--json"]).await;
    assert_eq!(one_json(&stdout)["external"]["source"]["kind"], "cc_switch");
}
