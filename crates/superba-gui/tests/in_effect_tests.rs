#![allow(missing_docs)]
//! GH#2 and GH#4 in the GUI: the close / force-stop questions offer "Stop them
//! for me" with every process and running turn listed, ending by force only as
//! a second, danger answer; the Overview's to-do names the Codex processes that
//! still run the previous configuration; "Codex in effect" shows, per running
//! Codex, what it runs, whether a turn would be interrupted, and how it picks
//! the new configuration up (restart the app's core, guidance for terminals and
//! editors, an optional confirmed stop).

mod support;

use std::time::Duration;

use superba_core::{
    ActiveState, ActivitySignal, AuthMode, BusyState, CodexProcessKind, ConsumerRole,
    ConsumerRuntime, Decision, DecisionRequest, EnvObservation, IdeKind, Locale, LocalizedText,
    PlanDestination, ProcessInfo, RestartAdvice, RestartKind, RunningGeneration, RuntimeEffect,
    RuntimeView, SiteKind, SwitchPlan, SwitchRequest, SwitchService, ThreadActivity, Timestamp,
};
use superba_gui::design::Palette;
use superba_gui::flow::Flow;
use superba_gui::i18n::Tr;
use superba_gui::in_effect::{self, InEffect};
use superba_gui::pages::overview::todo::{self, Spot, TodoKind};
use superba_gui::pages::{Action, Cx, Data};
use superba_gui::runtime::{Command, PendingDecision};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;
use tokio::sync::oneshot;

const T: i64 = 1_760_000_000_000;

fn process(pid: u32, kind: CodexProcessKind, ide: Option<IdeKind>) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: None,
        kind,
        exe: None,
        cmdline: vec!["codex".into()],
        started_at: Some(Timestamp(T - 600_000)),
        authapi_env: EnvObservation::Unknown,
        codex_home: None,
        stale: false,
        ide,
        cwd: Some("/work/app".into()),
    }
}

fn turn() -> ThreadActivity {
    ThreadActivity {
        target_id: "local".into(),
        thread_id: Some("t1".into()),
        title: Some("Fix the parser".into()),
        rollout_path: None,
        last_event_at: None,
        signal: ActivitySignal::RolloutTail,
        originator: Some("codex_cli_rs".into()),
        cwd: Some("/work/app".into()),
    }
}

fn row(
    pid: u32,
    role: ConsumerRole,
    effect: RuntimeEffect,
    advice: RestartAdvice,
) -> ConsumerRuntime {
    ConsumerRuntime {
        role,
        pid,
        core_pid: None,
        started_at: Some(Timestamp(T - 600_000)),
        codex_home: None,
        cwd: Some("/work/app".into()),
        exe: None,
        cmdline: vec![],
        effect,
        busy: BusyState::Idle,
        advice,
        can_stop: true,
    }
}

fn previous() -> RuntimeEffect {
    RuntimeEffect::Previous {
        running: RunningGeneration {
            generation: Some(4),
            profile_id: None,
            site_kind: Some(SiteKind::Official),
            applied_at: Some(Timestamp(T - 3_600_000)),
        },
    }
}

fn view() -> RuntimeView {
    let mut tui = row(
        300,
        ConsumerRole::Terminal,
        previous(),
        RestartAdvice::Reopen,
    );
    tui.busy = BusyState::Running {
        threads: vec![turn()],
    };
    let mut app = row(
        100,
        ConsumerRole::DesktopApp,
        previous(),
        RestartAdvice::HotRestart,
    );
    app.can_stop = false;
    app.core_pid = Some(150);
    RuntimeView {
        checked_at: Timestamp(T),
        written: Some(ActiveState {
            profile_id: Some("work".into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::Pat),
            generation: 5,
            applied_at: Timestamp(T),
        }),
        consumers: vec![
            app,
            tui,
            row(
                40,
                ConsumerRole::IdeExtension {
                    ide: Some(IdeKind::VsCode),
                },
                previous(),
                RestartAdvice::ReloadIde {
                    ide: Some(IdeKind::VsCode),
                },
            ),
            row(
                500,
                ConsumerRole::Exec,
                RuntimeEffect::Current,
                RestartAdvice::None,
            ),
        ],
        busy_checked: true,
        edited: None,
    }
}

async fn fake_data(runtime: Option<RuntimeView>) -> Data {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    let mut overview = service.overview().await;
    if let Ok(o) = overview.as_mut() {
        o.runtime = runtime;
    }
    data.overview.finish(overview);
    data.profiles.finish(service.profile_set().await);
    data
}

fn with_cx<R>(data: &Data, locale: Locale, f: impl FnOnce(&Cx<'_>) -> R) -> R {
    let tr = Tr::new(locale);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let svc = support::idle_service();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp(T),
        fonts: &fonts,
        service: &svc,
        log: &[],
    };
    f(&cx)
}

#[tokio::test]
async fn the_overview_names_the_codex_still_on_the_previous_configuration() {
    let data = fake_data(Some(view())).await;
    for locale in [Locale::En, Locale::ZhCn] {
        with_cx(&data, locale, |cx| {
            let todos = todo::todos(cx, None);
            let t = todos
                .iter()
                .find(|t| t.kind == TodoKind::OldConfig)
                .expect("an old-config to-do");
            assert_eq!(
                t.title,
                cx.tr.f("overview.todo.old_config", &[("n", &3)]),
                "{locale:?}"
            );
            let detail = t.detail.clone().unwrap();
            assert!(
                detail.contains(&cx.tr.text(&ConsumerRole::Terminal.text())),
                "{detail}"
            );
            assert_eq!(t.spot, Some(Spot::ThisComputer));
            assert!(matches!(t.fix(cx), Action::ShowRuntime));
            assert_eq!(t.fix_label(cx), cx.tr.t("overview.todo.fix.details"));
        });
    }
    // Nothing on an old configuration: no to-do.
    let mut calm = view();
    for c in &mut calm.consumers {
        c.effect = RuntimeEffect::Current;
    }
    let data = fake_data(Some(calm)).await;
    with_cx(&data, Locale::En, |cx| {
        assert!(
            !todo::todos(cx, None)
                .iter()
                .any(|t| t.kind == TodoKind::OldConfig)
        );
    });
}

/// Draw the "Codex in effect" dialog with `state`.
macro_rules! dialog {
    ($data:expr, $tr:expr, $state:expr, $out:expr) => {{
        let svc = support::idle_service();
        let fonts = superba_gui::design::fonts::FontReport::default();
        let (data, tr, state, out) = ($data, $tr, $state, $out);
        move |ui: &mut egui::Ui| {
            let cx = Cx {
                tr,
                pal: Palette::of(ui.ctx()),
                data,
                busy: false,
                now: Timestamp(T),
                fonts: &fonts,
                service: &svc,
                log: &[],
            };
            in_effect::show(ui.ctx(), &cx, state, out);
        }
    }};
}

#[tokio::test]
async fn codex_in_effect_lists_what_each_codex_runs_and_how_to_switch_it() {
    let data = fake_data(None).await;
    let tr = Tr::new(Locale::En);
    let mut state = InEffect::default();
    state.open_with(view());
    let mut out = Vec::new();
    let h = Harness::new([1280.0, 900.0], egui::Theme::Light);
    // The modal grows to its content over a few frames.
    h.settle(&mut dialog!(&data, &tr, &mut state, &mut out));
    let texts: Vec<String> = h
        .settle(&mut dialog!(&data, &tr, &mut state, &mut out))
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    let has = |s: &str| texts.iter().any(|t| t == s);
    let contains = |s: &str| texts.iter().any(|t| t.contains(s));
    assert!(has("Codex in effect"), "{texts:?}");
    assert!(contains("generation 5"), "{texts:?}");
    assert!(has("ChatGPT desktop app"));
    assert!(has("codex in a terminal"));
    assert!(has("VS Code · Codex extension"));
    assert!(has("Still runs the previous configuration"));
    assert!(has("Uses the new configuration"), "the exec row");
    assert!(
        contains("Runs generation 4 · Official ChatGPT"),
        "{texts:?}"
    );
    // The app's core is restarted by Superba; the terminal and the editor get
    // their command to copy; the running turn is named with its warning.
    assert!(has("Restart ChatGPT's Codex core"));
    assert!(has("Copy “codex resume --last”"));
    assert!(has("Copy “Developer: Restart Extension Host”"));
    assert!(contains("Fix the parser"));
    assert!(has(
        "A turn is running in it: restarting it now interrupts that turn."
    ));
    assert!(contains("pid 150"), "the app's core pid: {texts:?}");
    // Stopping asks first, with what it does.
    assert!(h.click_text("Stop…", &mut dialog!(&data, &tr, &mut state, &mut out)));
    let texts: Vec<String> = h
        .settle(&mut dialog!(&data, &tr, &mut state, &mut out))
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert!(
        texts.iter().any(|t| t == "Stop this Codex process?"),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("codex resume --last` continues it"))
    );
    // Restarting the app's core plans the re-apply of the applied profile.
    assert!(h.click_text(
        "Restart ChatGPT's Codex core",
        &mut dialog!(&data, &tr, &mut state, &mut out)
    ));
    assert!(
        out.iter().any(|a| matches!(a, Action::Switch { .. })),
        "{out:?}"
    );
    assert!(!state.is_open(), "the switch takes over");
}

fn plan() -> SwitchPlan {
    SwitchPlan {
        id: "in-effect-plan".into(),
        created_at: Timestamp::EPOCH,
        request: SwitchRequest::profile("krill".into()),
        from: None,
        to: PlanDestination {
            profile_id: Some("krill".into()),
            site_id: Some("krill".into()),
            line_id: None,
            auth_mode: Some(AuthMode::Pat),
            label: LocalizedText::new("test.destination", "Krill"),
        },
        primary: RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        },
        restarts: vec![],
        config: None,
        env: None,
        migration: None,
        remotes: vec![],
        active_threads: vec![],
        model_check: None,
        choices: vec![],
        alternatives: vec![],
        warnings: vec![],
        cleanup: vec![],
    }
}

fn running_flow() -> Flow {
    let p = plan();
    let mut flow = Flow::new(0);
    assert!(flow.request(p.request.clone(), Duration::ZERO));
    let _ = flow.take_commands();
    flow.on_planned(1, Ok(p), Duration::ZERO);
    let commands = flow.take_commands();
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::Execute { .. })),
        "{commands:?}"
    );
    flow
}

macro_rules! decision {
    ($data:expr, $tr:expr, $flow:expr) => {{
        let svc = support::idle_service();
        let fonts = superba_gui::design::fonts::FontReport::default();
        let (data, tr, flow) = ($data, $tr, $flow);
        move |ui: &mut egui::Ui| {
            let cx = Cx {
                tr,
                pal: Palette::of(ui.ctx()),
                data,
                busy: true,
                now: Timestamp(T),
                fonts: &fonts,
                service: &svc,
                log: &[],
            };
            superba_gui::shell::decision_dialog(ui.ctx(), &cx, flow, Duration::ZERO);
        }
    }};
}

#[tokio::test]
async fn the_close_question_offers_stop_for_me_and_force_is_a_second_question() {
    let data = fake_data(None).await;
    let tr = Tr::new(Locale::En);
    let h = Harness::new([1280.0, 900.0], egui::Theme::Light);
    let processes = vec![
        process(300, CodexProcessKind::CliTui, None),
        process(40, CodexProcessKind::IdeAppServer, Some(IdeKind::VsCode)),
    ];
    let mut flow = running_flow();
    let (tx, mut rx) = oneshot::channel();
    flow.on_decision(
        PendingDecision::new(
            DecisionRequest::CloseConsumers {
                processes: processes.clone(),
                busy: vec![turn()],
            },
            tx,
        ),
        Duration::ZERO,
    );
    let texts: Vec<String> = h
        .settle(&mut decision!(&data, &tr, &mut flow))
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    let has = |s: &str| texts.iter().any(|t| t == s);
    assert!(has("Close these Codex processes first"), "{texts:?}");
    assert!(has("codex in a terminal"));
    assert!(has("VS Code · Codex extension"));
    assert!(
        texts
            .iter()
            .any(|t| t.contains("VS Code itself stays open"))
    );
    assert!(texts.iter().any(|t| t.contains("Fix the parser")));
    assert!(has("Stopping it now loses the output of the running turn."));
    assert!(has("I closed them — check again"));
    assert!(!has("End them by force"), "force is never offered first");
    assert!(h.click_text("Stop them for me", &mut decision!(&data, &tr, &mut flow)));
    assert_eq!(rx.try_recv(), Ok(Decision::StopProcesses));

    let (tx, mut rx) = oneshot::channel();
    flow.on_decision(
        PendingDecision::new(
            DecisionRequest::ForceStop {
                processes: processes[..1].to_vec(),
                busy: vec![],
            },
            tx,
        ),
        Duration::ZERO,
    );
    let texts: Vec<String> = h
        .settle(&mut decision!(&data, &tr, &mut flow))
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t == "These Codex processes are still running"),
        "{texts:?}"
    );
    assert!(h.click_text("End them by force", &mut decision!(&data, &tr, &mut flow)));
    assert_eq!(rx.try_recv(), Ok(Decision::Force));
}

#[test]
fn the_written_line_and_running_generation_read_well_in_chinese() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = rt.block_on(fake_data(None));
    with_cx(&data, Locale::ZhCn, |cx| {
        let line = in_effect::written_line(cx, &view());
        assert!(line.contains("第 5 代"), "{line}");
        let running = in_effect::running_text(
            cx,
            &RunningGeneration {
                generation: None,
                profile_id: None,
                site_kind: None,
                applied_at: None,
            },
        );
        assert_eq!(running, cx.tr.t("in_effect.runs_original"));
        let (_, label) = in_effect::effect_label(cx, &previous());
        assert_eq!(label, "仍在使用旧配置");
    });
}
