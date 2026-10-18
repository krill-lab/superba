//! `Switcher::plan` over in-memory fakes: every plan 7.1 rule as it arises from
//! real component observations, the destination resolution (profiles, restores,
//! migrations, remotes), plan choices, warnings and alternatives. Planning must
//! never write.

#![allow(missing_docs)]

mod fakes;

use std::collections::BTreeMap;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AuthApiHealth, AuthMode, AuthShape, BlockReason, CodexProcessKind, CodexVersion, ConsumerKind,
    EnvOp, Error, FullRestartReason, KeyAccess, LegacySessionChoice, Locale, MigrationSelector,
    PlanChoice, ProfileId, ProviderRestore, RcLine, RestartKind, RestoreTarget, SwitchIntent,
    SwitchPlan, SwitchRequest, SwitchService, TargetId, Timestamp, codex::LEGACY_KRILL_PROVIDER,
};

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn with_line(profile: &str, line: &str, auth: Option<AuthMode>) -> SwitchRequest {
    let mut r = req(profile);
    r.intent = SwitchIntent::Profile {
        profile_id: ProfileId::new(profile),
        line_id: Some(line.into()),
        auth_mode: auth,
    };
    r
}

fn restore(target: RestoreTarget) -> SwitchRequest {
    let mut r = req("work");
    r.intent = SwitchIntent::Restore { target };
    r
}

async fn plan(w: &World, r: SwitchRequest) -> SwitchPlan {
    let p = w.switcher().plan(r).await.expect("plan");
    assert!(w.writes().is_empty(), "planning wrote: {:?}", w.writes());
    p
}

fn full(reason: FullRestartReason) -> RestartKind {
    RestartKind::FullAppRestart { reason }
}

fn hot() -> RestartKind {
    RestartKind::AppServerHot {
        host_id: "local".into(),
        kill_remote: false,
    }
}

fn blocks(p: &SwitchPlan) -> Vec<BlockReason> {
    match &p.primary {
        RestartKind::Blocked { reasons } => reasons.clone(),
        other => panic!("expected blocked, got {other:?}"),
    }
}

fn warning_keys(p: &SwitchPlan) -> Vec<String> {
    p.warnings.iter().map(|w| w.text.key.clone()).collect()
}

// --- rule 8: the hot path ---------------------------------------------------

#[tokio::test]
async fn pat_profile_with_cdp_bridge_and_healthy_auth_address_is_hot() {
    let w = World::new();
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, hot());
    assert!(p.countdown_allowed());
    assert!(!p.needs_confirmation());
    assert!(p.choices.is_empty());
    let cfg = p.config.as_ref().expect("config plan");
    assert_eq!(cfg.auth_mode, Some(AuthMode::Pat));
    assert_eq!(
        w.config.last_desired.lock().unwrap().as_deref(),
        Some("token:Pat:https://api.krill-code.net/backend-api/codex")
    );
    // PAT persists the auth address of the destination line.
    assert_eq!(
        w.env.plans.lock().unwrap().last(),
        Some(&EnvOp::Set {
            value: OVERSEAS.into(),
            take_over_foreign: false
        })
    );
    assert!(p.env.is_some());
    assert_eq!(p.to.line_id.as_ref().map(|l| l.as_str()), Some("overseas"));
    assert_eq!(p.to.auth_mode, Some(AuthMode::Pat));
    // whoami was checked on the origin the running app was started with.
    assert_eq!(
        *w.prober.authapi_checks.lock().unwrap(),
        vec![OVERSEAS.to_owned()]
    );
    // The destination label renders with nested, translated names.
    let zh = superba_switch::messages::catalog(Locale::ZhCn);
    let label = zh.text(&p.to.label);
    assert!(label.contains("海外线路"), "{label}");
    assert_eq!(
        p.to.label.render_fallback(),
        "Krill · Overseas · PAT (personal access token)"
    );
}

#[tokio::test]
async fn hot_line_change_keeps_the_app_auth_address_and_says_so() {
    let w = World::new();
    let p = plan(&w, with_line("work", "cdn", None)).await;
    assert_eq!(p.primary, hot());
    assert!(warning_keys(&p).contains(&"switch.warn.authapi_line".to_owned()));
    // The env still follows the destination line for future launches.
    assert_eq!(
        w.env.plans.lock().unwrap().last(),
        Some(&EnvOp::Set {
            value: "https://api.cdn-krill-ai.com".into(),
            take_over_foreign: false
        })
    );
}

#[tokio::test]
async fn api_key_destination_ignores_the_auth_address_rules() {
    let w = World::new();
    w.set_session(Some(session(None, true)));
    let p = plan(&w, with_line("work", "overseas", Some(AuthMode::ApiKey))).await;
    assert_eq!(p.primary, hot());
    assert!(p.env.is_none(), "API key mode needs no env");
    assert!(w.prober.authapi_checks.lock().unwrap().is_empty());
}

// --- rules 4-7 --------------------------------------------------------------

#[tokio::test]
async fn app_not_running_needs_no_restart() {
    let w = World::new();
    w.set_session(None);
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, RestartKind::None);
    assert!(!p.countdown_allowed());
    assert!(!p.needs_confirmation());
}

#[tokio::test]
async fn app_without_cdp_needs_a_full_restart_through_superba() {
    let w = World::new();
    w.set_session(Some(session(Some(OVERSEAS), false)));
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, full(FullRestartReason::NoCdp));
    assert!(p.needs_confirmation());
    assert!(warning_keys(&p).contains(&"switch.warn.app_not_ours".to_owned()));
}

#[tokio::test]
async fn unverified_cdp_owner_counts_as_no_cdp() {
    let w = World::new();
    let mut s = session(Some(OVERSEAS), true);
    s.cdp.as_mut().unwrap().owner_verified = false;
    w.set_session(Some(s));
    assert_eq!(
        plan(&w, req("work")).await.primary,
        full(FullRestartReason::NoCdp)
    );
}

#[tokio::test]
async fn missing_bridge_needs_a_full_restart() {
    let w = World::new();
    *w.app.bridge_ready.lock().unwrap() = false;
    assert_eq!(
        plan(&w, req("work")).await.primary,
        full(FullRestartReason::NoBridge)
    );
}

#[tokio::test]
async fn pat_without_auth_address_in_the_app_needs_a_full_restart() {
    let w = World::new();
    w.set_session(Some(session(None, true)));
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, full(FullRestartReason::AuthApiMissing));
    // Offered: hot switch with a plain API key instead.
    assert_eq!(p.alternatives.len(), 1);
    assert!(matches!(
        p.alternatives[0].request.intent,
        SwitchIntent::Profile {
            auth_mode: Some(AuthMode::ApiKey),
            ..
        }
    ));
}

#[tokio::test]
async fn pat_with_unhealthy_auth_address_offers_api_key_hot_switch() {
    let w = World::new();
    w.prober.authapi.lock().unwrap().insert(
        OVERSEAS.into(),
        AuthApiHealth::Unhealthy {
            reason: "timeout".into(),
        },
    );
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, full(FullRestartReason::AuthApiUnhealthy));
    assert_eq!(p.alternatives[0].label.key, "switch.action.hot_with_apikey");
    // The alternative plans hot.
    let alt = plan(&w, p.alternatives[0].request.clone()).await;
    assert_eq!(alt.primary, hot());
}

#[tokio::test]
async fn foreign_auth_address_origin_is_unhealthy_without_sending_the_key() {
    let w = World::new();
    w.set_session(Some(session(Some("not a url"), true)));
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, full(FullRestartReason::AuthApiUnhealthy));
    assert!(w.prober.authapi_checks.lock().unwrap().is_empty());
}

// --- rule 3 ------------------------------------------------------------------

#[tokio::test]
async fn official_login_to_token_while_running_is_cross_family() {
    let w = World::new();
    w.config.state.lock().unwrap().auth = AuthShape::ChatGpt {
        email: None,
        account_id: None,
    };
    assert_eq!(
        plan(&w, req("work")).await.primary,
        full(FullRestartReason::CrossAuthFamily)
    );
    // With the app stopped no process can race on auth.json.
    w.set_session(None);
    assert_eq!(plan(&w, req("work")).await.primary, RestartKind::None);
}

#[tokio::test]
async fn generation_restore_from_chatgpt_to_pat_requires_cross_family_restart() {
    let w = World::new();
    w.config.restore_token(AuthMode::Pat);
    w.config.state.lock().unwrap().auth = AuthShape::ChatGpt {
        email: None,
        account_id: None,
    };
    w.set_session(Some(session(None, true)));
    let p = plan(&w, restore(RestoreTarget::Generation { generation: 1 })).await;
    assert_eq!(p.to.auth_mode, Some(AuthMode::Pat));
    assert_eq!(p.primary, full(FullRestartReason::CrossAuthFamily));
}

#[tokio::test]
async fn pat_generation_restore_without_app_auth_env_requires_restart() {
    let w = World::new();
    w.config.restore_token(AuthMode::Pat);
    w.set_session(Some(session(None, true)));
    let p = plan(&w, restore(RestoreTarget::Generation { generation: 1 })).await;
    assert_eq!(p.primary, full(FullRestartReason::AuthApiMissing));
    assert!(
        matches!(p.env.as_ref().map(|e| &e.op), Some(EnvOp::Set { value, .. }) if value == OVERSEAS)
    );
}

#[tokio::test]
async fn same_family_generation_restore_checks_saved_key_and_stays_hot() {
    for auth in [AuthMode::Pat, AuthMode::ApiKey] {
        let w = World::new();
        w.config.restore_token(auth);
        let p = plan(&w, restore(RestoreTarget::Generation { generation: 1 })).await;
        assert_eq!(p.primary, hot());
        assert_eq!(p.to.auth_mode, Some(auth));
        assert_eq!(
            *w.prober.authapi_checks.lock().unwrap(),
            if auth.needs_authapi_env() {
                vec![OVERSEAS.to_owned()]
            } else {
                Vec::new()
            }
        );
    }
}

#[tokio::test]
async fn pat_generation_restore_rechecks_the_app_auth_address_health() {
    let w = World::new();
    w.config.restore_token(AuthMode::Pat);
    w.prober.authapi.lock().unwrap().insert(
        OVERSEAS.into(),
        AuthApiHealth::Unhealthy {
            reason: "timeout".into(),
        },
    );
    let p = plan(&w, restore(RestoreTarget::Generation { generation: 1 })).await;
    assert_eq!(p.primary, full(FullRestartReason::AuthApiUnhealthy));
}

#[tokio::test]
async fn generation_and_unknown_original_auth_fall_back_to_rendered_auth_json() {
    for target in [
        RestoreTarget::Generation { generation: 1 },
        RestoreTarget::Original {
            provider: ProviderRestore::KeepOpenai,
        },
    ] {
        let w = World::new();
        w.config.restore_token(AuthMode::Pat);
        *w.config.restored_auth_mode.lock().unwrap() = None;
        w.config.baseline.lock().unwrap().as_mut().unwrap().auth = AuthShape::Unknown {
            detail: "old baseline".into(),
        };
        w.config.state.lock().unwrap().auth = AuthShape::ChatGpt {
            email: None,
            account_id: None,
        };
        let p = plan(&w, restore(target)).await;
        assert_eq!(p.to.auth_mode, Some(AuthMode::Pat));
        assert_eq!(p.primary, full(FullRestartReason::CrossAuthFamily));
    }
}

#[tokio::test]
async fn restored_pat_key_is_never_probed_using_an_unrecognized_destination_site() {
    let w = World::new();
    w.config.restore_token(AuthMode::Pat);
    w.config
        .restored_files
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .config_toml = Some(superba_core::SecretBytes::new(
        b"openai_base_url = 'https://example.org/v1'\n".to_vec(),
    ));
    let p = plan(&w, restore(RestoreTarget::Generation { generation: 1 })).await;
    assert_eq!(p.primary, full(FullRestartReason::AuthApiUnhealthy));
    assert!(w.prober.authapi_checks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn released_baseline_still_restores_original_env_and_auth() {
    let w = World::new();
    let saved = {
        let mut baseline = w.config.baseline.lock().unwrap();
        let b = baseline.as_mut().unwrap();
        b.released = true;
        b.legacy_env_lines = vec![RcLine {
            path: w.ctx.home_dir.join(".zshrc"),
            line_no: 4,
            text: format!("export CODEX_AUTHAPI_BASE_URL={CN_FAST}"),
            value: CN_FAST.into(),
        }];
        b.clone()
    };
    let p = plan(
        &w,
        restore(RestoreTarget::Original {
            provider: ProviderRestore::KeepOpenai,
        }),
    )
    .await;
    assert_eq!(p.primary, full(FullRestartReason::CrossAuthFamily));
    assert_eq!(p.to.auth_mode, saved.auth.mode());
    assert_eq!(
        p.env.as_ref().unwrap().op,
        EnvOp::RestoreBaseline {
            snapshots: saved.env,
            legacy_lines: saved.legacy_env_lines,
        }
    );
}

#[tokio::test]
async fn restore_official_warns_about_an_old_login_and_is_cross_family() {
    let w = World::new();
    w.config
        .login_stash
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .stashed_at = Timestamp(T0 - 30 * 86_400_000);
    let p = plan(&w, restore(RestoreTarget::Official)).await;
    assert_eq!(p.primary, full(FullRestartReason::CrossAuthFamily));
    assert_eq!(p.to.auth_mode, Some(AuthMode::OfficialChatGpt));
    assert_eq!(
        w.config.last_desired.lock().unwrap().as_deref(),
        Some("official")
    );
    let warn = p
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.warn.login_stash_age")
        .expect("age warning");
    assert!(warn.text.render_fallback().contains("30 days"));
}

#[tokio::test]
async fn restore_official_without_stash_says_to_sign_in() {
    let w = World::new();
    *w.config.login_stash.lock().unwrap() = None;
    let p = plan(&w, restore(RestoreTarget::Official)).await;
    assert!(warning_keys(&p).contains(&"switch.warn.no_login_stash".to_owned()));
}

// --- rule 1: blocks ----------------------------------------------------------

#[tokio::test]
async fn unreachable_line_blocks() {
    let w = World::new();
    let mut caps = good_caps("krill", "overseas");
    caps.reachable = false;
    w.prober
        .caps
        .lock()
        .unwrap()
        .insert("overseas".into(), caps);
    let p = plan(&w, req("work")).await;
    assert_eq!(
        blocks(&p),
        vec![BlockReason::LineUnavailable {
            line_id: "overseas".into()
        }]
    );
    assert!(p.config.is_none(), "blocked plans compute no writes");
}

#[tokio::test]
async fn rejected_key_blocks_with_the_access_reason() {
    let w = World::new();
    let mut caps = good_caps("krill", "overseas");
    caps.key_access = Some(KeyAccess::QuotaExhausted);
    w.prober
        .caps
        .lock()
        .unwrap()
        .insert("overseas".into(), caps);
    assert_eq!(
        blocks(&plan(&w, req("work")).await),
        vec![BlockReason::KeyRejected {
            line_id: "overseas".into(),
            access: KeyAccess::QuotaExhausted
        }]
    );
}

#[tokio::test]
async fn missing_key_blocks() {
    let w = World::new();
    w.secrets.map.lock().unwrap().clear();
    assert!(blocks(&plan(&w, req("work")).await).contains(&BlockReason::MissingKey));
}

#[tokio::test]
async fn pending_journal_and_unparseable_config_block() {
    let w = World::new();
    {
        let mut st = w.config.state.lock().unwrap();
        st.journal_pending = true;
        st.parse_error = Some("line 3: expected =".into());
    }
    let b = blocks(&plan(&w, req("work")).await);
    assert!(b.contains(&BlockReason::JournalPending));
    assert!(
        b.iter()
            .any(|r| matches!(r, BlockReason::Unparseable { .. }))
    );
}

#[tokio::test]
async fn edge_blocked_pat_falls_back_to_api_key_with_an_explanation() {
    let w = World::new();
    let mut caps = good_caps("krill", "cn-fast");
    caps.pat_ok = false;
    caps.blocked_by_edge = true;
    w.prober.caps.lock().unwrap().insert("cn-fast".into(), caps);
    let p = plan(&w, with_line("work", "cn-fast", None)).await;
    assert_eq!(p.primary, hot());
    assert_eq!(p.to.auth_mode, Some(AuthMode::ApiKey));
    assert!(warning_keys(&p).contains(&"switch.warn.pat_fallback".to_owned()));
    assert!(p.env.is_none());
}

#[tokio::test]
async fn explicit_pat_on_an_edge_blocked_line_blocks_and_offers_api_key() {
    let w = World::new();
    let mut caps = good_caps("krill", "cn-fast");
    caps.pat_ok = false;
    w.prober.caps.lock().unwrap().insert("cn-fast".into(), caps);
    let p = plan(&w, with_line("work", "cn-fast", Some(AuthMode::Pat))).await;
    assert_eq!(
        blocks(&p),
        vec![BlockReason::AuthModeUnavailable {
            line_id: "cn-fast".into(),
            auth_mode: AuthMode::Pat
        }]
    );
    assert_eq!(p.alternatives.len(), 1);
}

#[tokio::test]
async fn codex_v1_fallback_is_used_for_api_key_only() {
    let w = World::new();
    let mut caps = good_caps("krill", "cdn");
    caps.codex_path = Some(superba_core::CodexPath::CodexV1);
    caps.pat_ok = false;
    w.prober.caps.lock().unwrap().insert("cdn".into(), caps);
    plan(&w, with_line("work", "cdn", Some(AuthMode::ApiKey))).await;
    assert_eq!(
        w.config.last_desired.lock().unwrap().as_deref(),
        Some("token:ApiKey:https://api.cdn-krill-ai.com/codex/v1")
    );
}

#[tokio::test]
async fn probe_failures_other_than_missing_implementations_block_the_line() {
    let w = World::new();
    *w.prober.probe_error.lock().unwrap() = Some(Error::Network {
        url: OVERSEAS.into(),
        message: "connection refused".into(),
    });
    assert!(
        blocks(&plan(&w, req("work")).await).contains(&BlockReason::LineUnavailable {
            line_id: "overseas".into()
        })
    );
    // A prober that is not written yet only produces an informational note.
    *w.prober.probe_error.lock().unwrap() = Some(Error::not_implemented("net"));
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, hot());
    assert!(warning_keys(&p).contains(&"switch.warn.check_skipped".to_owned()));
}

// --- version gates (plan 9.8) -----------------------------------------------

/// Make the `work` profile a manual PAT profile (Automatic falls back to an API
/// key on a Codex too old for PAT instead of being blocked).
fn manual_pat(w: &World) {
    let mut set = w.profiles.set.lock().unwrap();
    let p = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap();
    p.set_auth(superba_core::AuthChoice::Pat);
}

#[tokio::test]
async fn old_bundled_codex_blocks_manual_pat() {
    let w = World::new();
    manual_pat(&w);
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 150, 0));
    let b = blocks(&plan(&w, req("work")).await);
    assert!(
        matches!(&b[0], BlockReason::VersionTooOld { consumer, .. } if consumer == "ChatGPT app")
    );
}

#[tokio::test]
async fn old_cli_blocks_manual_pat_unless_switching_for_the_app_only() {
    let w = World::new();
    manual_pat(&w);
    w.app.binaries.lock().unwrap()[0].version = Some(CodexVersion::new(0, 155, 0));
    let b = blocks(&plan(&w, req("work")).await);
    assert!(
        matches!(&b[0], BlockReason::VersionTooOld { consumer, .. } if consumer == "/usr/local/bin/codex")
    );
    let mut r = req("work");
    r.accept_unknown_versions = true;
    let p = plan(&w, r).await;
    assert_eq!(p.primary, hot());
    assert!(warning_keys(&p).contains(&"switch.warn.cli_too_old".to_owned()));
}

#[tokio::test]
async fn unknown_app_version_asks_before_continuing() {
    let w = World::new();
    w.app.installs.lock().unwrap()[0].bundled_codex_version = None;
    let p = plan(&w, req("work")).await;
    assert_eq!(
        p.choices,
        vec![PlanChoice::UnknownVersion {
            consumer: "ChatGPT app".into()
        }]
    );
    assert!(!p.countdown_allowed());
    let mut r = req("work");
    r.accept_unknown_versions = true;
    assert!(plan(&w, r).await.choices.is_empty());
}

#[tokio::test]
async fn official_destination_has_no_version_gate() {
    let w = World::new();
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 1, 0));
    let p = plan(&w, restore(RestoreTarget::Official)).await;
    assert!(!p.is_blocked());
}

// --- sessions (plan 10) -------------------------------------------------------

fn legacy_world() -> World {
    let w = World::new();
    w.config.state.lock().unwrap().model_provider = Some(LEGACY_KRILL_PROVIDER.into());
    let home = w.ctx.codex_home();
    *w.sessions.report.lock().unwrap() =
        session_report(home.root(), &[("krill", 128), ("openai", 2)]);
    w
}

#[tokio::test]
async fn legacy_provider_sessions_force_a_choice() {
    let w = legacy_world();
    let p = plan(&w, req("work")).await;
    assert_eq!(
        p.choices,
        vec![PlanChoice::LegacySessions {
            providers: BTreeMap::from([("krill".to_owned(), 128)])
        }]
    );
}

#[tokio::test]
async fn migrating_legacy_sessions_is_a_confirmed_full_restart() {
    let w = legacy_world();
    let mut r = req("work");
    r.legacy_sessions = Some(LegacySessionChoice::Migrate);
    r.migration_exclude = vec!["t-9".into()];
    let p = plan(&w, r).await;
    assert_eq!(p.primary, full(FullRestartReason::SessionMigration));
    assert!(p.needs_confirmation());
    let m = p.migration.as_ref().expect("migration plan");
    assert_eq!(m.request.to_provider, "openai");
    assert_eq!(
        m.request.selectors,
        vec![MigrationSelector::Provider {
            provider: "krill".into()
        }]
    );
    assert_eq!(m.request.exclude, vec!["t-9".to_owned()]);
    assert!(warning_keys(&p).contains(&"switch.warn.migration".to_owned()));
    // Session migration needs every consumer stopped, even with the app closed.
    w.set_session(None);
    let mut r = req("work");
    r.legacy_sessions = Some(LegacySessionChoice::Migrate);
    assert_eq!(
        plan(&w, r).await.primary,
        full(FullRestartReason::SessionMigration)
    );
}

#[tokio::test]
async fn skipping_legacy_sessions_warns_and_stays_hot() {
    let w = legacy_world();
    let mut r = req("work");
    r.legacy_sessions = Some(LegacySessionChoice::Skip);
    let p = plan(&w, r).await;
    assert_eq!(p.primary, hot());
    assert!(p.migration.is_none());
    assert!(warning_keys(&p).contains(&"switch.warn.legacy_skipped".to_owned()));
}

#[tokio::test]
async fn migrate_sessions_intent_plans_a_migration() {
    let w = legacy_world();
    let mut r = req("work");
    r.intent = SwitchIntent::MigrateSessions {
        request: superba_core::MigrationRequest {
            to_provider: "openai".into(),
            selectors: vec![MigrationSelector::Provider {
                provider: "krill".into(),
            }],
            include_archived: true,
            exclude: Vec::new(),
        },
    };
    let p = plan(&w, r).await;
    assert_eq!(p.primary, full(FullRestartReason::SessionMigration));
    assert!(p.config.is_none());
    assert_eq!(p.migration.as_ref().unwrap().thread_count, 128);
}

// --- restore original (plan 9.6 / 10.4) --------------------------------------

#[tokio::test]
async fn restore_original_without_baseline_is_blocked() {
    let w = World::new();
    *w.config.baseline.lock().unwrap() = None;
    let p = plan(
        &w,
        restore(RestoreTarget::Original {
            provider: ProviderRestore::KeepOpenai,
        }),
    )
    .await;
    assert_eq!(blocks(&p), vec![BlockReason::NoBaseline]);
}

#[tokio::test]
async fn original_profile_with_legacy_baseline_asks_about_the_provider() {
    let w = World::new();
    w.config
        .baseline
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .provider_key = Some("krill".into());
    let p = plan(&w, req("original")).await;
    assert!(matches!(
        &p.choices[..],
        [PlanChoice::RestoreProvider { baseline_provider, .. }] if baseline_provider == "krill"
    ));
    // Restoring env as well (legacy tutorial lines come back).
    assert!(matches!(
        w.env.plans.lock().unwrap().last(),
        Some(EnvOp::RestoreBaseline { .. })
    ));
}

#[tokio::test]
async fn full_restore_with_reverse_migration_selects_ledgers_and_krill_generations() {
    let w = World::new();
    w.config
        .baseline
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .provider_key = Some("krill".into());
    w.sessions
        .history
        .lock()
        .unwrap()
        .push(superba_core::MigrationReceipt {
            id: "takeover".into(),
            codex_home: w.ctx.codex_home().root().to_path_buf(),
            migrated_at: Timestamp(T0 - 10),
            to_provider: "openai".into(),
            by_source_provider: BTreeMap::from([("krill".to_owned(), 5)]),
            threads: 5,
            rollouts: 5,
            backup_dir: "/b".into(),
            ledger_path: "/l".into(),
            rolled_back: false,
        });
    *w.config.generations.lock().unwrap() = vec![
        superba_core::GenerationRecord {
            generation: 1,
            profile_id: Some("work".into()),
            site_kind: Some(superba_core::SiteKind::Krill),
            applied_at: Timestamp(T0 - 5_000),
        },
        superba_core::GenerationRecord {
            generation: 2,
            profile_id: Some("official".into()),
            site_kind: Some(superba_core::SiteKind::Official),
            applied_at: Timestamp(T0 - 1_000),
        },
    ];
    let p = plan(
        &w,
        restore(RestoreTarget::Original {
            provider: ProviderRestore::Baseline {
                reverse_migrate: true,
            },
        }),
    )
    .await;
    assert_eq!(p.primary, full(FullRestartReason::SessionMigration));
    let req = w.sessions.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(req.to_provider, "krill");
    assert_eq!(
        req.selectors,
        vec![
            MigrationSelector::Ledger {
                migration_id: "takeover".into()
            },
            MigrationSelector::CreatedBetween {
                provider: "openai".into(),
                from: Timestamp(T0 - 5_000),
                to: Some(Timestamp(T0 - 1_000)),
            },
        ]
    );
}

// --- questions -----------------------------------------------------------------

#[tokio::test]
async fn edited_owned_keys_raise_a_conflict_question() {
    let w = World::new();
    *w.config.fail_plan.lock().unwrap() = Some(Error::Conflict {
        message: "drift".into(),
    });
    w.config.state.lock().unwrap().drift = vec![superba_core::KeyChange {
        path: "openai_base_url".into(),
        before: Some("\"a\"".into()),
        after: Some("\"b\"".into()),
    }];
    let p = plan(&w, req("work")).await;
    assert!(
        matches!(&p.choices[..], [PlanChoice::OwnedKeyConflict { conflicts, .. }] if conflicts[0].path == "openai_base_url")
    );
}

#[tokio::test]
async fn foreign_env_lines_ask_and_the_answer_takes_them_over() {
    let w = World::new();
    w.env.report.lock().unwrap().foreign_lines = vec![RcLine {
        path: "/home/u/.bashrc".into(),
        line_no: 7,
        text: "export CODEX_AUTHAPI_BASE_URL=https://example.org".into(),
        value: "https://example.org".into(),
    }];
    let p = plan(&w, req("work")).await;
    assert!(matches!(&p.choices[..], [PlanChoice::ForeignEnv { .. }]));
    let mut r = req("work");
    r.take_over_foreign_env = Some(true);
    let p = plan(&w, r).await;
    assert!(p.choices.is_empty());
    assert_eq!(
        w.env.plans.lock().unwrap().last(),
        Some(&EnvOp::Set {
            value: OVERSEAS.into(),
            take_over_foreign: true
        })
    );
}

#[tokio::test]
async fn missing_model_gets_a_replacement_from_the_catalog() {
    let w = World::new();
    w.config.state.lock().unwrap().model = Some("gpt-old-codex".into());
    *w.prober.models.lock().unwrap() = Ok(vec!["gpt-5.5".into(), "gpt-old-mini".into()]);
    let p = plan(&w, req("work")).await;
    let mc = p.model_check.as_ref().unwrap();
    assert!(!mc.available);
    assert_eq!(
        mc.suggestions,
        vec!["gpt-old-mini".to_owned(), "gpt-5.5".to_owned()]
    );
    assert!(warning_keys(&p).contains(&"switch.warn.model_missing".to_owned()));
    assert!(p.choices.is_empty());
    // An explicit replacement that is not offered asks again.
    let mut r = req("work");
    r.model_override = Some("nope".into());
    let p = plan(&w, r).await;
    assert!(matches!(
        &p.choices[..],
        [PlanChoice::ReplacementModel { .. }]
    ));
}

// --- consumers and remotes ----------------------------------------------------

#[tokio::test]
async fn cli_processes_and_daemon_get_their_own_restart_decisions() {
    let w = World::new();
    w.app.processes.lock().unwrap().extend([
        cli_process(300, CodexProcessKind::CliTui, T0 - 1),
        cli_process(301, CodexProcessKind::CliExec, T0 - 1),
        cli_process(302, CodexProcessKind::CliDaemon, T0 - 1),
    ]);
    let p = plan(&w, req("work")).await;
    let by: BTreeMap<String, (RestartKind, Vec<u32>)> = p
        .restarts
        .iter()
        .map(|r| {
            (
                format!("{:?}", r.consumer),
                (r.restart.clone(), r.pids.clone()),
            )
        })
        .collect();
    assert_eq!(by["DesktopApp"], (hot(), vec![100]));
    assert_eq!(
        by["CliProcesses"],
        (RestartKind::NextLaunch, vec![300, 301])
    );
    assert_eq!(by["CliDaemon"], (RestartKind::DaemonRestart, vec![302]));
    let keys = warning_keys(&p);
    assert!(keys.contains(&"switch.warn.cli_processes".to_owned()));
    assert!(keys.contains(&"switch.warn.daemon_running".to_owned()));
}

/// GH#4: the plan names an editor's extension apart from terminals (it keeps
/// the previous configuration until the editor restarts it).
#[tokio::test]
async fn editor_extensions_get_their_own_note() {
    let w = World::new();
    let mut ide = cli_process(310, CodexProcessKind::IdeAppServer, T0 - 1);
    ide.ide = Some(superba_core::IdeKind::VsCode);
    w.app.processes.lock().unwrap().push(ide);
    let p = plan(&w, req("work")).await;
    let keys = warning_keys(&p);
    assert!(
        keys.contains(&"switch.warn.ide_processes".to_owned()),
        "{keys:?}"
    );
    assert!(
        !keys.contains(&"switch.warn.cli_processes".to_owned()),
        "{keys:?}"
    );
}

#[tokio::test]
async fn remote_hosts_are_planned_with_their_own_decisions() {
    let w = World::new();
    *w.remote.hosts.lock().unwrap() = vec![
        remote_host("devbox", Some("h-devbox")),
        remote_host("gpu", None),
    ];
    let mut r = req("work");
    r.targets.remotes = vec![
        TargetId::remote("devbox"),
        TargetId::remote("gpu"),
        TargetId::remote("ghost"),
    ];
    let p = plan(&w, r).await;
    assert_eq!(p.primary, hot());
    assert_eq!(p.remotes.len(), 2);
    let remote = |key: &str| {
        p.restarts
            .iter()
            .find(|c| {
                c.consumer
                    == ConsumerKind::Remote {
                        target_id: TargetId::remote(key),
                    }
            })
            .map(|c| c.restart.clone())
            .unwrap()
    };
    assert_eq!(
        remote("devbox"),
        RestartKind::AppServerHot {
            host_id: "h-devbox".into(),
            kill_remote: true
        }
    );
    assert_eq!(remote("gpu"), RestartKind::NextLaunch);
    assert!(remote("ghost").is_blocked());
    let keys = warning_keys(&p);
    assert!(keys.contains(&"switch.warn.remote_no_host_id".to_owned()));
    assert!(keys.contains(&"switch.warn.remotes".to_owned()));
}

#[tokio::test]
async fn remote_unknown_version_and_shared_home_ask() {
    let w = World::new();
    *w.remote.hosts.lock().unwrap() = vec![remote_host("devbox", Some("h1"))];
    *w.remote.version.lock().unwrap() = None;
    *w.remote.shared_home.lock().unwrap() = true;
    let mut r = req("work");
    r.targets.remotes = vec![TargetId::remote("devbox")];
    let p = plan(&w, r.clone()).await;
    assert!(p.choices.contains(&PlanChoice::UnknownVersion {
        consumer: "devbox".into()
    }));
    assert!(p.choices.contains(&PlanChoice::SharedHome {
        targets: vec![TargetId::remote("devbox")]
    }));
    r.accept_unknown_versions = true;
    r.accept_shared_home = true;
    assert!(plan(&w, r).await.choices.is_empty());
}

#[tokio::test]
async fn remote_version_too_old_blocks_only_that_host() {
    let w = World::new();
    *w.remote.hosts.lock().unwrap() = vec![remote_host("devbox", Some("h1"))];
    *w.remote.version.lock().unwrap() = Some(CodexVersion::new(0, 120, 0));
    let mut r = req("work");
    r.targets.remotes = vec![TargetId::remote("devbox")];
    let p = plan(&w, r).await;
    assert_eq!(p.primary, hot());
    assert!(!p.remotes[0].blocked.is_empty());
}

#[tokio::test]
async fn remote_only_switch_leaves_the_local_target_alone() {
    let w = World::new();
    *w.remote.hosts.lock().unwrap() = vec![remote_host("devbox", Some("h1"))];
    let mut r = req("work");
    r.targets.local = false;
    r.targets.remotes = vec![TargetId::remote("devbox")];
    let p = plan(&w, r).await;
    assert_eq!(p.primary, RestartKind::None);
    assert!(p.config.is_none());
    assert!(
        p.restarts
            .iter()
            .all(|c| c.consumer != ConsumerKind::DesktopApp)
    );
}

#[tokio::test]
async fn busy_threads_at_plan_time_are_listed() {
    let w = World::new();
    w.push_turns(vec![vec![busy("local", "Refactor")]]);
    let p = plan(&w, req("work")).await;
    assert_eq!(p.active_threads.len(), 1);
}

// --- restart app ----------------------------------------------------------------

#[tokio::test]
async fn restart_app_intent() {
    let w = World::new();
    let mut r = req("work");
    r.intent = SwitchIntent::RestartApp { install_id: None };
    assert_eq!(
        plan(&w, r.clone()).await.primary,
        full(FullRestartReason::Requested)
    );
    w.set_session(Some(session(None, false)));
    assert_eq!(
        plan(&w, r.clone()).await.primary,
        full(FullRestartReason::NoCdp)
    );
    w.set_session(None);
    assert_eq!(plan(&w, r).await.primary, RestartKind::None);
}

#[tokio::test]
async fn unknown_profile_is_an_error_and_cross_site_switch_warns() {
    let w = World::new();
    let err = w.switcher().plan(req("nope")).await.unwrap_err();
    assert_eq!(err.code(), "not_found");
    w.config.state.lock().unwrap().active = Some(superba_core::ActiveState {
        profile_id: Some("other".into()),
        site_id: Some("compatible-x".into()),
        line_id: None,
        auth_mode: Some(AuthMode::ApiKey),
        generation: 3,
        applied_at: Timestamp(T0 - 1),
    });
    let p = plan(&w, req("work")).await;
    assert!(warning_keys(&p).contains(&"switch.warn.cross_site".to_owned()));
    assert_eq!(p.from.as_ref().unwrap().generation, 3);
}

#[tokio::test]
async fn an_app_on_another_codex_home_is_not_a_consumer() {
    let w = World::new();
    let mut s = session(Some(OVERSEAS), true);
    s.codex_home = Some("/elsewhere/.codex".into());
    w.set_session(Some(s));
    let p = plan(&w, req("work")).await;
    assert_eq!(p.primary, RestartKind::None);
    assert!(warning_keys(&p).contains(&"switch.doctor.codex_home_mismatch".to_owned()));
    // The same home is fine.
    let mut s = session(Some(OVERSEAS), true);
    s.codex_home = Some(w.ctx.codex_home().root().to_path_buf());
    w.set_session(Some(s));
    assert_eq!(plan(&w, req("work")).await.primary, hot());
}

#[tokio::test]
async fn nothing_to_write_and_a_current_core_needs_no_restart() {
    let w = World::new();
    w.profiles.set.lock().unwrap().profiles.push(krill_profile(
        "keyed",
        Some("overseas"),
        AuthMode::ApiKey,
    ));
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:keyed".into(), "sk-keyed-key-4444".into());
    let sw = w.switcher();
    let p = sw.plan(req("keyed")).await.unwrap();
    assert!(p.primary.is_hot(), "{:?}", p.primary);
    sw.execute(
        p,
        superba_core::ExecuteOptions::default(),
        std::sync::Arc::new(superba_core::SafeDefaults),
        std::sync::Arc::new(superba_core::NoEvents),
    )
    .await
    .unwrap();

    // The same destination again: nothing to write, the core already reads it.
    *w.config.noop_plan.lock().unwrap() = true;
    let p = sw.plan(req("keyed")).await.unwrap();
    assert_eq!(p.primary, RestartKind::None);

    // A core older than the applied configuration still gets the restart.
    {
        let mut procs = w.app.processes.lock().unwrap();
        procs.retain(|p| p.kind != CodexProcessKind::AppServer);
        procs.push(app_server(900, 100, T0 - 600_000));
    }
    let p = sw.plan(req("keyed")).await.unwrap();
    assert!(p.primary.is_hot(), "{:?}", p.primary);
}
