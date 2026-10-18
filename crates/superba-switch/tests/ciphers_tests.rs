//! Content another provider encrypted (GitHub issue 11) in direct mode: a
//! switch to a provider that cannot read it counts the recent conversations
//! it would break and says what to do; the Overview names conversations that
//! stopped for it. Only the local rollouts are read; nothing is sent.

mod fakes;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AuthMode, GenerationRecord, Notice, ProfileId, RequestKind, RequestStatus, SiteId, SwitchPlan,
    SwitchService, Timestamp,
};
use superba_sessions::usage::{ENCRYPTED_CONTENT_CODE, RolloutRequest};
use superba_switch::Switcher;
use superba_switch::ciphers::{CipherFamily, continued_elsewhere, family, refused_threads};
use superba_switch::usage::Attribution;
use url::Url;

fn req(profile: &str) -> superba_core::SwitchRequest {
    superba_core::SwitchRequest::profile(ProfileId::new(profile))
}

fn keys(p: &SwitchPlan) -> Vec<String> {
    p.warnings.iter().map(|w| w.text.key.clone()).collect()
}

fn args(n: &Notice) -> std::collections::BTreeMap<String, String> {
    n.text
        .args
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A compatible site and a profile `custom` on it, and a second Krill
/// profile `work2`.
fn add_profiles(w: &World) {
    let base = Url::parse("https://api.example.com/v1").unwrap();
    let site =
        superba_switch::new_compatible_site(SiteId::new("example"), "Example", &base).unwrap();
    let mut set = w.profiles.set.lock().unwrap();
    set.custom_sites.push(site);
    let mut p = krill_profile("custom", None, AuthMode::ApiKey);
    p.site_id = SiteId::new("example");
    p.name = "Example relay".into();
    set.profiles.push(p);
    let mut k = krill_profile("work2", Some("overseas"), AuthMode::ApiKey);
    k.name = "Krill API key".into();
    set.profiles.push(k);
    drop(set);
    let mut secrets = w.secrets.map.lock().unwrap();
    secrets.insert("profile:custom".into(), KEY.into());
    secrets.insert("profile:work2".into(), KEY.into());
}

/// Line `kind` with `payload` at `sec` seconds after T0 - 1 h.
fn line(sec: i64, kind: &str, payload: serde_json::Value) -> String {
    let at = Timestamp::from_millis(T0 - 3_600_000 + sec * 1000).to_rfc3339();
    serde_json::json!({"timestamp": at, "type": kind, "payload": payload}).to_string() + "\n"
}

/// A rollout of `thread`: one answered turn, then (with `refusal`) a turn the
/// provider refused for content another provider encrypted, as Codex 0.163
/// records it.
fn rollout(w: &World, thread: &str, refusal: bool) {
    let usage = serde_json::json!({"input_tokens": 900, "cached_input_tokens": 0,
        "cache_write_input_tokens": 0, "output_tokens": 30, "reasoning_output_tokens": 0,
        "total_tokens": 930});
    let mut s = line(
        0,
        "session_meta",
        serde_json::json!({"id": thread, "timestamp": "x", "cwd": "/w",
            "originator": "Codex Desktop", "cli_version": "0.163.0", "model_provider": "openai"}),
    );
    s += &line(
        1,
        "event_msg",
        serde_json::json!({"type": "task_started", "turn_id": "t1"}),
    );
    s += &line(
        1,
        "turn_context",
        serde_json::json!({"turn_id": "t1", "model": "gpt-x"}),
    );
    s += &line(
        2,
        "response_item",
        serde_json::json!({"type": "message", "role": "user", "content": []}),
    );
    s += &line(
        5,
        "token_usage_record",
        serde_json::json!({"thread_id": thread, "turn_id": "t1", "response_id": "resp_1", "usage": usage}),
    );
    s += &line(
        6,
        "event_msg",
        serde_json::json!({"type": "task_complete", "turn_id": "t1"}),
    );
    if refusal {
        let body = r#"{"error": {"type": "invalid_request_error", "code": "invalid_encrypted_content", "message": "The encrypted content for item amsg_01a14f90-4274-7462-ab32-fd7a6bb914ec could not be verified. Reason: encrypted content hydration failed: Encrypted content could not be decrypted or parsed.", "param": "input"}}"#;
        s += &line(
            60,
            "event_msg",
            serde_json::json!({"type": "task_started", "turn_id": "t2"}),
        );
        s += &line(
            61,
            "response_item",
            serde_json::json!({"type": "message", "role": "user", "content": []}),
        );
        s += &line(
            67,
            "event_msg",
            serde_json::json!({"type": "task_complete", "turn_id": "t2", "last_agent_message": null,
                "error": {"message": body, "codex_error_info": "other"}}),
        );
    }
    let dir = w.ctx.codex_home().sessions_dir().join("2025/10/09");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("rollout-2025-10-09T08-00-00-{thread}.jsonl")),
        s,
    )
    .unwrap();
}

/// `profile` (on `site`) applied since T0 - 2 h.
fn applied(w: &World, profile: &str, site: &str) {
    *w.config.generations.lock().unwrap() = vec![GenerationRecord {
        generation: 1,
        profile_id: Some(ProfileId::new(profile)),
        site_kind: None,
        applied_at: Timestamp(T0 - 7_200_000),
    }];
    w.config.state.lock().unwrap().active = Some(superba_core::ActiveState {
        profile_id: Some(ProfileId::new(profile)),
        site_id: Some(SiteId::new(site)),
        line_id: None,
        auth_mode: Some(AuthMode::ApiKey),
        generation: 1,
        applied_at: Timestamp(T0 - 7_200_000),
    });
}

const THREAD: &str = "01a14f90-2834-7af2-90de-0cb4d5117756";

#[tokio::test]
async fn a_switch_to_a_provider_that_cannot_read_them_counts_the_conversations() {
    let w = World::new();
    add_profiles(&w);
    let svc = Switcher::new(w.deps());
    rollout(&w, THREAD, false);
    // The conversation ran on Krill; the destination is a compatible site.
    applied(&w, "work", superba_core::catalog::KRILL_SITE_ID);
    let p = svc.plan(req("custom")).await.unwrap();
    let n = p
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.warn.ciphertext_elsewhere")
        .unwrap_or_else(|| panic!("{:?}", keys(&p)));
    let a = args(n);
    assert_eq!(a["count"], "1");
    assert_eq!(a["provider"], "work");
    assert_eq!(a["site"], "Example relay");
    assert!(!keys(&p).contains(&"switch.warn.cross_site".to_owned()));

    // Another Krill profile reads the same backend: nothing to say.
    let p = svc.plan(req("work2")).await.unwrap();
    assert!(
        !keys(&p)
            .iter()
            .any(|k| k.contains("ciphertext") || k == "switch.warn.cross_site"),
        "{:?}",
        keys(&p)
    );
}

#[tokio::test]
async fn back_from_a_compatible_site_the_same_note_names_it() {
    let w = World::new();
    add_profiles(&w);
    let svc = Switcher::new(w.deps());
    rollout(&w, THREAD, false);
    applied(&w, "custom", "example");
    let p = svc.plan(req("work")).await.unwrap();
    let n = p
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.warn.ciphertext_elsewhere")
        .unwrap_or_else(|| panic!("{:?}", keys(&p)));
    assert_eq!(args(n)["provider"], "Example relay");
}

#[tokio::test]
async fn without_recent_conversations_the_general_note_stays() {
    let w = World::new();
    add_profiles(&w);
    let svc = Switcher::new(w.deps());
    applied(&w, "work", superba_core::catalog::KRILL_SITE_ID);
    let p = svc.plan(req("custom")).await.unwrap();
    assert!(keys(&p).contains(&"switch.warn.cross_site".to_owned()));
    assert!(!keys(&p).contains(&"switch.warn.ciphertext_elsewhere".to_owned()));
}

#[tokio::test]
async fn the_overview_names_conversations_that_stopped_for_it() {
    let w = World::new();
    add_profiles(&w);
    let svc = Switcher::new(w.deps());
    applied(&w, "work", superba_core::catalog::KRILL_SITE_ID);
    let notice = |o: &superba_core::Overview| {
        o.notices
            .iter()
            .find(|n| n.text.key == "switch.overview.encrypted_refused")
            .cloned()
    };
    rollout(&w, THREAD, true);
    let o = svc.overview().await.unwrap();
    let n = notice(&o).expect("the stopped conversation is named");
    assert_eq!(args(&n)["count"], "1");
    // Router mode repairs them: no notice.
    w.profiles.set.lock().unwrap().settings.router.enabled = true;
    assert_eq!(notice(&svc.overview().await.unwrap()), None);

    // A conversation that only ran fine: no notice.
    let w = World::new();
    let svc = Switcher::new(w.deps());
    rollout(&w, THREAD, false);
    assert_eq!(notice(&svc.overview().await.unwrap()), None);
}

fn row(thread: &str, at: i64, status: RequestStatus, provider: &str) -> RolloutRequest {
    RolloutRequest {
        id: -at,
        thread_id: thread.into(),
        turn_id: None,
        response_id: None,
        started_at: Timestamp(at),
        completed_at: Timestamp(at + 10),
        model: None,
        effort: None,
        service_tier: None,
        model_provider: Some(provider.into()),
        originator: None,
        cli_version: None,
        thread_source: None,
        kind: RequestKind::Turn,
        status,
        usage: None,
        per_turn: true,
    }
}

#[test]
fn only_a_conversation_whose_latest_turn_stopped_is_counted() {
    let refused = RequestStatus::Failed {
        code: Some(ENCRYPTED_CONTENT_CODE.into()),
    };
    let rows = vec![
        row("a", 1, RequestStatus::Ok, "openai"),
        row("a", 2, refused.clone(), "openai"),
        // Continued successfully afterwards (router, or switched back).
        row("b", 1, refused.clone(), "openai"),
        row("b", 2, RequestStatus::Ok, "openai"),
        row(
            "c",
            3,
            RequestStatus::Failed {
                code: Some("other".into()),
            },
            "openai",
        ),
    ];
    assert_eq!(refused_threads(&rows), 1);
}

#[test]
fn families_and_where_conversations_were_last_continued() {
    let krill = superba_core::catalog::krill_site();
    assert_eq!(family(&krill), CipherFamily::ChatGptBackend);
    let base = Url::parse("https://api.example.com/v1").unwrap();
    let site =
        superba_switch::new_compatible_site(SiteId::new("example"), "Example", &base).unwrap();
    assert_eq!(family(&site), CipherFamily::Site(SiteId::new("example")));

    let a = Attribution {
        generations: vec![
            GenerationRecord {
                generation: 1,
                profile_id: Some(ProfileId::new("custom")),
                site_kind: None,
                applied_at: Timestamp(0),
            },
            GenerationRecord {
                generation: 2,
                profile_id: Some(ProfileId::new("work")),
                site_kind: None,
                applied_at: Timestamp(100),
            },
        ],
        names: [
            (ProfileId::new("custom"), "Example relay".to_owned()),
            (ProfileId::new("work"), "Krill".to_owned()),
        ]
        .into_iter()
        .collect(),
        ..Attribution::default()
    };
    let rows = vec![
        // Last continued on the compatible site.
        row("a", 10, RequestStatus::Ok, "openai"),
        // Moved to Krill afterwards.
        row("b", 20, RequestStatus::Ok, "openai"),
        row("b", 120, RequestStatus::Ok, "openai"),
        // Recorded under a legacy provider id: its own family.
        row("c", 130, RequestStatus::Ok, "krill"),
    ];
    let family_of = |id: &ProfileId| match id.as_str() {
        "custom" => Some(CipherFamily::Site(SiteId::new("example"))),
        "work" => Some(CipherFamily::ChatGptBackend),
        _ => None,
    };
    assert_eq!(
        continued_elsewhere(&rows, &a, &CipherFamily::ChatGptBackend, family_of),
        Some((2, "Example relay".to_owned()))
    );
    assert_eq!(
        continued_elsewhere(
            &rows,
            &a,
            &CipherFamily::Site(SiteId::new("example")),
            family_of
        )
        .map(|(n, _)| n),
        Some(2)
    );
}
