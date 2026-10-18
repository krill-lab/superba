//! Router member eligibility (user bug 28: the router could not be turned on and
//! nothing said that no member could carry traffic) and the router doctor
//! report built from `superba-router`'s facts.

mod fakes;

use std::path::PathBuf;

use fakes::*;
use superba_core::{
    AuthChoice, AuthMode, Error, Locale, ProfileId, RouterInstallRequest, Severity, SiteKind,
    SwitchService, catalog,
};
use superba_router::doctor::{
    ConfigFacts, ExeFacts, Finding, LiveFacts, LogFacts, MemberFacts, PortFacts, PortState,
    RouterFacts, ServiceFacts,
};
use superba_router::service::{
    CommandRecord, CommandRecordOutput, ServiceDiagnosis, ServiceKind, ServiceStatus,
};
use superba_switch::messages;
use superba_switch::router_doctor::{Eligibility, has_eligible_member, report, router_candidates};

fn set_work(w: &World, f: impl FnOnce(&mut superba_core::Profile)) {
    let mut set = w.profiles.set.lock().unwrap();
    f(set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap());
}

fn render(locale: Locale) -> impl Fn(&superba_core::LocalizedText) -> String {
    let catalog = messages::catalog(locale);
    move |t| catalog.text(t)
}

#[test]
fn every_kind_of_provider_gets_its_eligibility() {
    let w = World::new();
    let mut set = w.profiles.set.lock().unwrap().clone();
    let mut pat = krill_profile("pat", None, AuthMode::Pat);
    pat.set_auth(AuthChoice::Pat);
    let mut keyless = krill_profile("keyless", None, AuthMode::ApiKey);
    keyless.key_ref = None;
    let mut lost = krill_profile("lost", None, AuthMode::ApiKey);
    lost.site_id = "deleted-site".into();
    let mut official = catalog::builtin_profiles().remove(1);
    official.id = ProfileId::new("official");
    set.profiles.extend([pat, keyless, lost, official]);
    set.settings.router.members = vec![ProfileId::new("pat")];
    let sites = catalog::builtin_sites();
    let c = router_candidates(&set, &sites);
    let of = |id: &str| {
        c.iter()
            .find(|c| c.profile_id.as_ref().is_some_and(|p| p.as_str() == id))
            .unwrap_or_else(|| panic!("{id} listed"))
    };
    assert_eq!(
        c[0].profile_id.as_ref().unwrap().as_str(),
        "pat",
        "members first"
    );
    assert!(of("pat").member);
    // User bug 78: a profile set to PAT for direct switches still has its
    // key, which the router sends as an API key.
    assert_eq!(of("pat").eligibility, Eligibility::Eligible);
    assert_eq!(
        of("work").eligibility,
        Eligibility::Eligible,
        "Auto falls back to an API key"
    );
    assert!(of("work").addable());
    assert_eq!(of("keyless").eligibility, Eligibility::NoKey);
    assert_eq!(of("lost").eligibility, Eligibility::UnknownSite);
    // The official login is never routed: not listed unless it is a member.
    assert!(!c.iter().any(|c| {
        c.profile_id
            .as_ref()
            .is_some_and(|p| p.as_str() == "official")
    }));
    let official = set.profile(&ProfileId::new("official")).unwrap();
    let official_site = sites.iter().find(|s| s.id == official.site_id);
    assert_eq!(
        superba_switch::router_doctor::eligibility(official, official_site),
        Eligibility::OfficialLogin
    );
    assert!(has_eligible_member(&c), "the PAT member carries traffic");
    assert!(Eligibility::NoKey.needs_key());
    assert!(!Eligibility::Eligible.needs_key());
    // Every reason renders in both languages.
    for e in [
        Eligibility::Eligible,
        Eligibility::OfficialLogin,
        Eligibility::NoKey,
        Eligibility::NoApiKeyMode,
        Eligibility::UnknownSite,
    ] {
        assert!(messages::has_key(&e.text().key), "{e:?}");
        assert!(
            render(Locale::ZhCn)(&e.text())
                .chars()
                .any(|c| c > '\u{2e80}')
        );
    }
    // A Krill site nobody entered a key for is listed too (to point at the
    // Providers page key field).
    set.profiles.clear();
    set.settings.router.members.clear();
    let c = router_candidates(&set, &sites);
    let krill = c
        .iter()
        .find(|c| c.site_kind == Some(SiteKind::Krill))
        .unwrap();
    assert_eq!(
        (krill.profile_id.as_ref(), krill.eligibility),
        (None, Eligibility::NoKey)
    );
    assert!(!krill.addable());
}

#[tokio::test]
async fn turning_the_router_on_without_an_eligible_member_says_why_and_what_to_do() {
    let w = World::new();
    let sw = w.switcher();
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    // The user's only member has no key (any more).
    set_work(&w, |p| p.key_ref = None);
    let e = sw.router_set_enabled(true).await.unwrap_err();
    let Error::Invalid { message } = &e else {
        panic!("expected Invalid, got {e:?}");
    };
    assert!(
        message.contains("needs at least one member with an API key"),
        "{message}"
    );
    assert!(message.contains("Need an API key: work"), "{message}");
    // Planning router mode says the same (the GUI shows the plan error).
    let mut request = superba_core::SwitchRequest::profile(ProfileId::new(""));
    request.intent = superba_core::SwitchIntent::Router { enable: true };
    let e = sw.plan(request).await.unwrap_err();
    assert!(e.to_string().contains("with an API key"), "{e}");
}

/// User bug 78: one `spb switch --auth pat` stored PAT as the profile's
/// direct auth, and the router then refused the profile ("PAT only; the
/// router needs an API key"). Eligibility is having a key: the member stays,
/// router mode routes it with the key as an API key, and a direct switch
/// afterwards keeps the chosen PAT.
#[tokio::test]
async fn a_member_set_to_pat_for_direct_switches_stays_routable() {
    let w = World::new();
    let sw = w.switcher();
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    // What `spb switch --auth pat` leaves behind: PAT for direct switches.
    set_work(&w, |p| p.set_auth(AuthChoice::Pat));
    let direct = sw
        .plan(superba_core::SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert_eq!(
        direct.to.auth_mode,
        Some(AuthMode::Pat),
        "{:?}",
        direct.primary
    );
    let set = w.profiles.set.lock().unwrap().clone();
    let c = router_candidates(&set, &catalog::builtin_sites());
    let work = c
        .iter()
        .find(|c| c.profile_id.as_ref().is_some_and(|p| p.as_str() == "work"))
        .unwrap();
    assert_eq!(work.eligibility, Eligibility::Eligible);
    assert!(has_eligible_member(&c));
    // Router mode: planned, not blocked, through the router with the API key.
    let mut on = superba_core::SwitchRequest::profile(ProfileId::new(""));
    on.intent = superba_core::SwitchIntent::Router { enable: true };
    let plan = sw.plan(on).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(plan.to.auth_mode, Some(AuthMode::ApiKey));
    assert_eq!(plan.to.label.key, "switch.destination.router");
    sw.router_set_enabled(true).await.unwrap();
    // The direct choice is untouched by the router.
    let set = w.profiles.set.lock().unwrap().clone();
    let work = set.profile(&ProfileId::new("work")).unwrap();
    assert_eq!(work.auth_choice(), AuthChoice::Pat);
}

#[tokio::test]
async fn doctor_without_router_facts_still_explains_the_members() {
    let w = World::new();
    let sw = w.switcher();
    // No members at all (the user's case): "work" could be one.
    let report = sw.router_doctor().await.unwrap();
    assert_eq!(report.findings[0].severity, Severity::Danger);
    assert_eq!(
        report.findings[0].text.key,
        "switch.router_eligibility.none_eligible"
    );
    let text = report.render_text(&render(Locale::ZhCn));
    assert!(
        text.contains("开启路由需要至少一个有 API key 的成员。"),
        "{text}"
    );
    assert!(text.contains("spb router enable --member work"), "{text}");
    assert!(text.contains("[供应商]"), "{text}");
    assert!(
        text.contains("不是成员 · 可以作为成员（有 API key）"),
        "{text}"
    );
    // The fake router has no facts: said so, not hidden.
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.text.key == "switch.router_doctor.finding.no_facts")
    );
}

fn record(command: &str, code: i32, stdout: &str, stderr: &str) -> CommandRecord {
    CommandRecord {
        command: command.into(),
        result: Ok(CommandRecordOutput {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }),
    }
}

fn mac_facts() -> RouterFacts {
    let exe = PathBuf::from("/Applications/Superba.app/Contents/MacOS/superba-router");
    let data = PathBuf::from("/Users/Jane Doe/Library/Application Support/com.krill-code.Superba");
    RouterFacts {
        superba_version: "0.1.0".into(),
        platform: "macos aarch64".into(),
        os_version: Some("14.6.1".into()),
        data_dir: data.clone(),
        exe: ExeFacts {
            path: exe.clone(),
            exists: true,
            executable: Some(true),
            size: Some(31_457_280),
            version: Some(record(
                "/Applications/Superba.app/Contents/MacOS/superba-router --version",
                0,
                "superba-router 0.1.0\n",
                "",
            )),
            quarantine: Some("0083;66f0a1b2;Safari;".into()),
            bundle: Some((PathBuf::from("/Applications/Superba.app"), None)),
            codesign: Some(record(
                "/usr/bin/codesign -dv --verbose=2 x",
                0,
                "",
                "Format=Mach-O universal (x86_64 arm64)\nCodeDirectory v=20400 flags=0x20002(adhoc,linker-signed)\nSignature=adhoc\n",
            )),
            candidates: vec![(exe.clone(), true)],
            ..ExeFacts::default()
        },
        service: ServiceFacts {
            kind: ServiceKind::LaunchAgent,
            status: Ok(ServiceStatus {
                kind: ServiceKind::LaunchAgent,
                installed: true,
                running: Some(false),
                pid: None,
                detail: Some("not running".into()),
            }),
            diagnosis: ServiceDiagnosis {
                definition_path: Some(PathBuf::from(
                    "/Users/Jane Doe/Library/LaunchAgents/com.krill.superba.router.plist",
                )),
                definition: Some("<plist>…</plist>".into()),
                program: Some(exe.clone()),
                query: Some(record(
                    "/bin/launchctl print gui/501/com.krill.superba.router",
                    0,
                    "",
                    "",
                )),
                fields: vec![
                    ("state".into(), "not running".into()),
                    ("runs".into(), "3".into()),
                    ("last exit code".into(), "78: EX_CONFIG".into()),
                ],
                loaded: Some(true),
                crash_looping: true,
                last_exit_code: Some(78),
                last_signal: None,
            },
        },
        config: ConfigFacts {
            config_exists: true,
            port: Some(54_321),
            members: vec![MemberFacts {
                id: "work".into(),
                name: "Work".into(),
                lines: 3,
                has_key: true,
            }],
            secrets_exists: true,
            secrets_mode: Some(0o600),
            ..ConfigFacts::default()
        },
        runtime: None,
        port: Some(PortFacts {
            port: 54_321,
            state: PortState::Free,
        }),
        live: LiveFacts {
            up: false,
            detail: Some("the router is not running (no runtime.json)".into()),
            version: None,
            members: None,
            uptime_ms: None,
            local: None,
        },
        log: LogFacts {
            path: data.join("router/router.log"),
            exists: true,
            size: Some(2048),
            tail: vec!["2026-10-17T03:12:44Z ERROR superba_router: cannot start error=…".into()],
        },
        foreground: vec![
            exe.to_string_lossy().into_owned(),
            "run".into(),
            "--data-dir".into(),
            data.to_string_lossy().into_owned(),
        ],
        findings: vec![
            Finding::Quarantined {
                path: PathBuf::from("/Applications/Superba.app"),
            },
            Finding::CrashLoop {
                exit: Some(78),
                signal: None,
            },
            Finding::RouterDown {
                detail: "the router is not running (no runtime.json)".into(),
            },
        ],
    }
}

#[tokio::test]
async fn doctor_report_from_mac_facts_renders_every_part_in_both_languages() {
    let w = World::new();
    let sw = w.switcher();
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    let set = w.profiles.set.lock().unwrap().clone();
    let candidates = router_candidates(&set, &catalog::builtin_sites());
    let facts = mac_facts();
    let report = report(Ok(&facts), &candidates);
    assert_eq!(report.foreground, facts.foreground);
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.text.key == "switch.router_eligibility.none_eligible"),
        "work is an eligible member"
    );
    assert_eq!(
        report.findings[0].text.key,
        "switch.router_doctor.finding.quarantined"
    );
    for (locale, needles) in [
        (
            Locale::En,
            vec![
                "Superba 0.1.0 · local router diagnostics · macos aarch64 14.6.1",
                "! macOS marks /Applications/Superba.app as downloaded (quarantine)",
                "xattr -dr com.apple.quarantine \"/Applications/Superba.app\"",
                "(last exit: 78)",
                "[Router program]",
                "--version: superba-router 0.1.0 (exit 0)",
                "Quarantine (program): 0083;66f0a1b2;Safari;",
                "Code signature: Format=Mach-O universal (x86_64 arm64) | CodeDirectory v=20400 flags=0x20002(adhoc,linker-signed) | Signature=adhoc",
                "$ /bin/launchctl print gui/501/com.krill.superba.router",
                "  last exit code = 78: EX_CONFIG",
                "Work (work): 3 line(s), key present",
                "member · can be a member (has an API key)",
                "On the port: free (nothing listens)",
                "[Log (last 1 lines)]",
                "'/Users/Jane Doe/Library/Application Support/com.krill-code.Superba'",
            ],
        ),
        (
            Locale::ZhCn,
            vec![
                "本地路由诊断",
                "macOS 把 /Applications/Superba.app 标记为下载文件",
                "[路由程序]",
                "[后台服务]",
                "成员 · 可以作为成员（有 API key）",
                "端口上: 空闲（没有程序监听）",
                "[在终端里运行（日志直接显示，Ctrl-C 停止）]",
            ],
        ),
    ] {
        let text = report.render_text(&render(locale));
        for needle in needles {
            assert!(
                text.contains(needle),
                "{locale:?}: {needle:?} missing in\n{text}"
            );
        }
        assert!(
            !text.contains("{value}") && !text.contains("{path}"),
            "{text}"
        );
        assert!(!text.contains(KEY), "never a key");
    }
}

/// A running router that ran short of descriptors: the report says so (a
/// warning, no line blamed) and shows its capacity, in both languages.
#[tokio::test]
async fn doctor_reports_local_shortages_of_a_running_router() {
    let mut facts = mac_facts();
    facts.live = LiveFacts {
        up: true,
        detail: None,
        version: Some("0.1.0".into()),
        members: Some(1),
        uptime_ms: Some(90_000),
        local: Some(superba_router::LocalStatus {
            open_files_limit: Some(256),
            open_files_limit_now: Some(256),
            max_concurrent: 64,
            in_flight: 64,
            max_connections: 128,
            connections: 128,
            busy: 17,
            last_busy_ms: Some(1_760_000_000_000),
            file_limit_errors: 3,
            last_file_limit: Some(superba_router::LocalProblem {
                at_ms: 1_760_000_000_000,
                place: "connecting to work/cdn".into(),
                detail: "tcp open error: Too many open files (os error 24)".into(),
            }),
        }),
    };
    facts.findings = superba_router::doctor::findings(&facts, true, None);
    let local = facts
        .findings
        .iter()
        .find(|f| matches!(f, Finding::LocalLimits { .. }))
        .expect("a local-limits finding");
    assert!(!local.is_error(), "a warning: the router runs");
    assert!(local.english().contains("no line was marked down"));
    let w = World::new();
    let set = w.profiles.set.lock().unwrap().clone();
    let doc = report(
        Ok(&facts),
        &router_candidates(&set, &catalog::builtin_sites()),
    );
    let finding = doc
        .findings
        .iter()
        .find(|f| f.text.key == "switch.router_doctor.finding.local_limits_last")
        .expect("the finding is in the report");
    assert_eq!(finding.severity, Severity::Warning);
    for (locale, needles) in [
        (
            Locale::En,
            vec![
                "3 \"too many open files\" errors (open-file limit 256) and 17 requests",
                "Last: connecting to work/cdn: tcp open error: Too many open files (os error 24).",
                "Local capacity: 64 / 64 requests in progress, 128 / 128 connections, open-file limit 256",
            ],
        ),
        (
            Locale::ZhCn,
            vec![
                "出现 3 次「打开的文件太多」（打开文件上限 256），17 个请求",
                "没有线路因此被标为故障",
                "进行中 64 / 64 个请求，连接 128 / 128，打开文件上限 256",
            ],
        ),
    ] {
        let text = doc.render_text(&render(locale));
        for needle in needles {
            assert!(
                text.contains(needle),
                "{locale:?}: {needle:?} missing in\n{text}"
            );
        }
        assert!(
            !text.contains("{limit}") && !text.contains("{last}"),
            "{text}"
        );
        assert!(!text.contains(KEY), "never a key");
    }
    // Without a limit (Windows) the text says so.
    if let Some(l) = facts.live.local.as_mut() {
        l.open_files_limit = None;
        l.open_files_limit_now = None;
        l.last_file_limit = None;
    }
    facts.findings = superba_router::doctor::findings(&facts, false, None);
    let doc = report(
        Ok(&facts),
        &router_candidates(&set, &catalog::builtin_sites()),
    );
    let text = doc.render_text(&render(Locale::En));
    assert!(text.contains("(open-file limit unlimited)"), "{text}");
    assert!(
        text.contains("connections, open-file limit unlimited"),
        "{text}"
    );
}
