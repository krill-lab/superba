#![allow(missing_docs)]

use superba_core::app::redact_argv;
use superba_core::*;

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn redact_argv_hides_secret_flag_values_and_key_like_arguments() {
    let argv = strings(&[
        "codex",
        "exec",
        "--api-key",
        "plain-value",
        "--token=abc123",
        "--model=gpt-5",
        "sk-live-1234567890",
        "--auth",
        "x",
        "prompt text",
    ]);
    assert_eq!(
        redact_argv(&argv),
        strings(&[
            "codex",
            "exec",
            "--api-key",
            "***",
            "--token=***",
            "--model=gpt-5",
            "***",
            "--auth",
            "***",
            "prompt text",
        ])
    );
    assert!(redact_argv(&[]).is_empty());
}

#[test]
fn close_consumers_decision_defaults_to_cancel_and_allows_continue() {
    let request = DecisionRequest::CloseConsumers {
        processes: vec![],
        busy: vec![],
    };
    assert_eq!(request.safe_default(), Decision::Cancel);
    assert!(request.allowed().contains(&Decision::Continue));
    assert!(request.allowed().contains(&request.safe_default()));
}

#[test]
fn env_set_defaults_to_not_taking_over_foreign_lines() {
    let op: EnvOp =
        serde_json::from_str(r#"{"op":"set","value":"https://api.krill-code.net"}"#).unwrap();
    assert_eq!(
        op,
        EnvOp::Set {
            value: "https://api.krill-code.net".into(),
            take_over_foreign: false
        }
    );
    let restore = EnvOp::RestoreBaseline {
        snapshots: vec![],
        legacy_lines: vec![],
    };
    let json = serde_json::to_value(&restore).unwrap();
    assert_eq!(json["op"], "restore_baseline");
}

#[test]
fn stash_scope_serializes_with_kind_and_id() {
    let local = StashScope::Local("/home/u/.codex".into());
    let remote = StashScope::Remote(TargetId::remote("devbox"));
    assert_eq!(
        serde_json::to_value(&local).unwrap(),
        serde_json::json!({"kind":"local","id":"/home/u/.codex"})
    );
    let back: StashScope = serde_json::from_value(serde_json::to_value(&remote).unwrap()).unwrap();
    assert_eq!(back, remote);
}

#[test]
fn switch_request_new_fields_default_when_absent() {
    let json = serde_json::json!({
        "intent": {"kind": "profile", "profile_id": "work"}
    });
    let req: SwitchRequest = serde_json::from_value(json).unwrap();
    assert_eq!(req, SwitchRequest::profile(ProfileId::new("work")));
    assert!(req.targets.local);
    assert!(req.key_resolutions.is_empty());
    assert!(!req.accept_shared_home);
}

#[test]
fn restore_and_uninstall_intents_round_trip() {
    for intent in [
        SwitchIntent::Restore {
            target: RestoreTarget::Generation { generation: 3 },
        },
        SwitchIntent::Uninstall {
            delete_backups: false,
            provider: Some(ProviderRestore::KeepOpenai),
        },
        SwitchIntent::RestartApp { install_id: None },
        SwitchIntent::RollbackRemote {
            target_id: TargetId::remote("gpu-01"),
        },
    ] {
        let v = serde_json::to_value(&intent).unwrap();
        let back: SwitchIntent = serde_json::from_value(v).unwrap();
        assert_eq!(back, intent);
    }
}
