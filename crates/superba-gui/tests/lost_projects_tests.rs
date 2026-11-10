#![allow(missing_docs)]
//! User bugs 135/136 follow-up in the GUI: folders ChatGPT dropped are one
//! to-do of the Overview; its dialog lists them with where each was found,
//! ticks the ones from ChatGPT's project list, and answers "bring back the
//! ticked ones", "don't" or "not now". The answer reaches the service.

mod support;

use superba_core::{
    Locale, LostProject, LostProjectSource, LostProjects, LostProjectsAnswer, SwitchService,
    Timestamp,
};
use superba_gui::design::widgets::Status;
use superba_gui::i18n::Tr;
use superba_gui::lost_projects::{self, LostProjectsDialog};
use superba_gui::pages::overview::todo::{self, Spot, TodoKind};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

fn lost() -> LostProjects {
    LostProjects {
        codex_home: "/home/u/.codex".into(),
        candidates: vec![
            LostProject {
                path: "/home/u/work/proj-c".into(),
                extra_roots: Vec::new(),
                name: "proj-c".into(),
                last_used: Some(Timestamp::from_millis(1_762_700_000_000)),
                sources: vec![
                    LostProjectSource::ProjectList,
                    LostProjectSource::Chats { count: 3 },
                ],
                app_id: Some("877476ef".into()),
            },
            LostProject {
                path: "/home/u/src/repo".into(),
                extra_roots: Vec::new(),
                name: "repo".into(),
                last_used: None,
                sources: vec![LostProjectSource::Chats { count: 1 }],
                app_id: None,
            },
        ],
        fingerprint: "fp1".into(),
    }
}

async fn service_with(l: Option<LostProjects>) -> (FakeService, Data) {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    service.set_lost_projects(l);
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    (service, data)
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
        now: Timestamp::now(),
        fonts: &fonts,
        service: &svc,
        log: &[],
    };
    f(&cx)
}

#[tokio::test]
async fn the_overview_lists_the_dropped_folders_once_with_a_review_button() {
    let (_s, data) = service_with(Some(lost())).await;
    for locale in [Locale::En, Locale::ZhCn] {
        with_cx(&data, locale, |cx| {
            let ours: Vec<_> = todo::todos(cx, None)
                .into_iter()
                .filter(|t| t.kind == TodoKind::LostProjects)
                .collect();
            assert_eq!(ours.len(), 1);
            let t = &ours[0];
            assert_eq!(t.status, Status::Warn);
            assert_eq!(t.spot, Some(Spot::ThisComputer));
            assert!(matches!(t.fix(cx), Action::ShowLostProjects));
            if locale == Locale::En {
                assert_eq!(
                    t.title,
                    "ChatGPT dropped 1 project folder Superba can bring back"
                );
                assert_eq!(t.fix_label(cx), "Review");
            } else {
                assert!(t.title.contains("1 个项目文件夹"), "{}", t.title);
            }
        });
    }
    let (_s, data) = service_with(None).await;
    with_cx(&data, Locale::En, |cx| {
        assert!(
            !todo::todos(cx, None)
                .iter()
                .any(|t| t.kind == TodoKind::LostProjects)
        );
    });
}

#[tokio::test]
async fn the_dialog_ticks_the_project_list_folders_and_sends_the_answer() {
    let (service, data) = service_with(Some(lost())).await;
    let l = data
        .overview
        .value
        .as_ref()
        .unwrap()
        .lost_projects
        .clone()
        .unwrap();
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
    let mut state = LostProjectsDialog::default();
    assert!(!state.is_open());
    state.open(&l);
    assert!(state.is_open());
    assert_eq!(
        state.ticked(&l),
        [std::path::PathBuf::from("/home/u/work/proj-c")]
    );

    let mut actions: Vec<Action> = Vec::new();
    let texts = with_cx(&data, Locale::En, |cx| {
        harness.settle(&mut |ui| lost_projects::show(ui.ctx(), cx, &mut state, &mut actions))
    });
    let shown: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
    for want in [
        "Bring back project folders",
        "proj-c",
        "/home/u/work/proj-c",
        "In ChatGPT's project list · 3 chats ran here",
        "/home/u/src/repo",
        "1 chat ran here",
        "Bring back 1 folder",
        "Don't bring back",
        "Not now",
    ] {
        assert!(
            shown.iter().any(|t| t.contains(want)),
            "{want} in {shown:?}"
        );
    }

    // Tick the chat folder too, then bring both back.
    state.set(&"/home/u/src/repo".into(), true);
    with_cx(&data, Locale::En, |cx| {
        assert!(harness.click_text("Bring back 2 folders", &mut |ui| {
            lost_projects::show(ui.ctx(), cx, &mut state, &mut actions)
        }));
    });
    let [
        Action::AnswerLostProjects {
            fingerprint,
            answer,
        },
    ] = actions.as_slice()
    else {
        panic!("{actions:?}");
    };
    assert_eq!(fingerprint, "fp1");
    assert_eq!(
        answer,
        &LostProjectsAnswer::Restore {
            paths: vec!["/home/u/work/proj-c".into(), "/home/u/src/repo".into()]
        }
    );
    assert!(!state.is_open());
    // What the runtime does with it.
    service
        .answer_lost_projects(fingerprint.clone(), answer.clone())
        .await
        .unwrap();
    assert_eq!(service.lost_project_answers().len(), 1);
    assert!(service.overview().await.unwrap().lost_projects.is_none());

    // Nothing ticked: "bring back" does nothing.
    let mut state = LostProjectsDialog::default();
    state.open(&l);
    state.set(&"/home/u/work/proj-c".into(), false);
    assert!(state.restore_action(&l).is_none());

    // Don't bring back: a dismissal of this list.
    actions.clear();
    with_cx(&data, Locale::En, |cx| {
        assert!(harness.click_text("Don't bring back", &mut |ui| {
            lost_projects::show(ui.ctx(), cx, &mut state, &mut actions)
        }));
    });
    assert!(matches!(
        actions.as_slice(),
        [Action::AnswerLostProjects {
            answer: LostProjectsAnswer::Dismiss,
            ..
        }]
    ));

    // Not now: closes without an answer (the to-do stays).
    let mut state = LostProjectsDialog::default();
    state.open(&l);
    actions.clear();
    with_cx(&data, Locale::En, |cx| {
        assert!(harness.click_text("Not now", &mut |ui| {
            lost_projects::show(ui.ctx(), cx, &mut state, &mut actions)
        }));
    });
    assert!(actions.is_empty() && !state.is_open());
}

#[tokio::test]
async fn the_dialog_closes_when_the_list_changed() {
    let (_s, data) = service_with(Some(lost())).await;
    let mut other = lost();
    other.fingerprint = "fp2".into();
    let mut state = LostProjectsDialog::default();
    state.open(&other);
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
    let mut actions = Vec::new();
    with_cx(&data, Locale::En, |cx| {
        harness.settle(&mut |ui| lost_projects::show(ui.ctx(), cx, &mut state, &mut actions))
    });
    assert!(!state.is_open());
}
