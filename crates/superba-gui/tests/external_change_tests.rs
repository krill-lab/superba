#![allow(missing_docs)]
//! GH#5 in the GUI: a change another tool made to what Superba manages is a
//! to-do of the Overview (not twice through the diagnostics), and a dialog
//! offered once per state of the files shows who changed what and plans
//! "take over again" (the applied profile, everything answered `Overwrite`)
//! or "let <tool> manage it" (a hand-off); "Not now" is remembered.

mod support;

use superba_core::{
    ALL_CHANGED, AuthDrift, AuthShape, ChangeSource, DoctorFinding, ExternalChange, KeyChange,
    KeyResolution, Locale, LocalizedText, Severity, SwitchIntent, SwitchService, Timestamp,
};
use superba_gui::design::widgets::Status;
use superba_gui::external::{self, ExternalDialog};
use superba_gui::i18n::Tr;
use superba_gui::pages::overview::todo::{self, Spot, TodoKind};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

fn change(fingerprint: &str) -> ExternalChange {
    ExternalChange {
        codex_home: "/home/u/.codex".into(),
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
        evidence: Vec::new(),
        fingerprint: fingerprint.into(),
    }
}

async fn data_with(x: Option<ExternalChange>) -> Data {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    service.set_external(x);
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
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
        now: Timestamp::now(),
        fonts: &fonts,
        service: &svc,
        log: &[],
    };
    f(&cx)
}

#[tokio::test]
async fn the_overview_lists_the_change_once_with_a_review_button() {
    let mut data = data_with(Some(change("f1"))).await;
    data.doctor.finish(Ok(vec![DoctorFinding {
        id: "external_change".into(),
        severity: Severity::Warning,
        title: LocalizedText::new("switch.doctor.external", "x"),
        detail: None,
        fixable: false,
    }]));
    for locale in [Locale::En, Locale::ZhCn] {
        with_cx(&data, locale, |cx| {
            let todos = todo::todos(cx, None);
            let ours: Vec<_> = todos
                .iter()
                .filter(|t| t.kind == TodoKind::ExternalChange)
                .collect();
            assert_eq!(ours.len(), 1);
            assert!(
                !todos
                    .iter()
                    .any(|t| t.kind == TodoKind::Finding("external_change".into()))
            );
            let t = ours[0];
            assert_eq!(t.status, Status::Warn);
            assert_eq!(t.spot, Some(Spot::ThisComputer));
            assert!(t.title.contains("cc-switch"), "{}", t.title);
            assert!(matches!(t.fix(cx), Action::ShowExternal));
            if locale == Locale::En {
                assert_eq!(
                    t.title,
                    "cc-switch (provider “Kimi”) changed Codex's configuration that Superba manages"
                );
                assert_eq!(t.fix_label(cx), "Review");
            }
        });
    }
    let data = data_with(None).await;
    with_cx(&data, Locale::En, |cx| {
        assert!(
            !todo::todos(cx, None)
                .iter()
                .any(|t| t.kind == TodoKind::ExternalChange)
        );
    });
}

#[tokio::test]
async fn the_dialog_is_offered_once_per_state_and_plans_the_answers() {
    let data = data_with(Some(change("f1"))).await;
    let x = data.overview.value.as_ref().unwrap().external.clone();
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
    let mut state = ExternalDialog::default();
    // Not while something else asks for attention.
    state.offer(x.as_ref(), false);
    assert!(!state.is_open());
    state.offer(x.as_ref(), true);
    assert!(state.is_open());

    let mut actions: Vec<Action> = Vec::new();
    let texts = with_cx(&data, Locale::En, |cx| {
        harness.settle(&mut |ui| external::show(ui.ctx(), cx, &mut state, &mut actions))
    });
    let shown: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
    for want in [
        "Codex's configuration was changed outside Superba",
        "model_provider",
        "auth.json",
        "Take over again",
        "Let cc-switch manage it",
        "Not now",
    ] {
        assert!(
            shown.iter().any(|t| t.contains(want)),
            "{want} in {shown:?}"
        );
    }

    // Let cc-switch manage it: a hand-off naming cc-switch's provider.
    actions.clear();
    with_cx(&data, Locale::En, |cx| {
        assert!(harness.click_text("Let cc-switch manage it", &mut |ui| {
            external::show(ui.ctx(), cx, &mut state, &mut actions)
        }));
    });
    let [Action::Plan(request)] = actions.as_slice() else {
        panic!("{actions:?}");
    };
    assert!(matches!(
        &request.intent,
        SwitchIntent::HandOff {
            to: ChangeSource::CcSwitch { provider: Some(p) }
        } if p == "Kimi"
    ));
    assert!(!state.is_open());
    // Answered: not offered again for the same state of the files ...
    state.offer(x.as_ref(), true);
    assert!(!state.is_open());
    // ... but for a new one.
    let newer = change("f2");
    state.offer(Some(&newer), true);
    assert!(state.is_open());

    // Take over again: the applied profile with everything overwritten.
    let data = data_with(Some(change("f2"))).await;
    actions.clear();
    with_cx(&data, Locale::En, |cx| {
        assert!(harness.click_text("Take over again", &mut |ui| {
            external::show(ui.ctx(), cx, &mut state, &mut actions)
        }));
    });
    let [Action::Plan(request)] = actions.as_slice() else {
        panic!("{actions:?}");
    };
    assert!(
        matches!(&request.intent, SwitchIntent::Profile { profile_id, .. } if profile_id == "krill")
    );
    assert_eq!(
        request.key_resolutions.get(ALL_CHANGED),
        Some(&KeyResolution::Overwrite)
    );

    // Not now: closes, remembered.
    let mut state = ExternalDialog::default();
    state.offer(x.as_ref(), true);
    let data = data_with(Some(change("f1"))).await;
    actions.clear();
    with_cx(&data, Locale::En, |cx| {
        assert!(harness.click_text("Not now", &mut |ui| {
            external::show(ui.ctx(), cx, &mut state, &mut actions)
        }));
    });
    assert!(actions.is_empty() && !state.is_open());
    state.offer(x.as_ref(), true);
    assert!(!state.is_open());
}
