#![allow(missing_docs)]
//! The Tools page layout (ui-spec): settings rows that never squeeze their
//! label, one state per tool shared by the list, header and footer, the model
//! chooser's selection rules, and the whole page at 960x640 in both languages
//! and themes with the footer in view and no overlapping text.
mod a11y_harness;
mod router_harness;
mod support;
use a11y_harness::AppHarness;
use egui::accesskit::{Role, Toggled};
use std::sync::Arc;
use superba_core::harness::*;
use superba_core::{Locale, ModelList, ModelListSource, ModelSpec};
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use superba_gui::pages::tools::models::{self, Candidate};
use superba_gui::pages::tools::{self, Selection, State, ToolState, form, list};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn candidates(ids: &[&str]) -> Vec<Candidate> {
    ids.iter()
        .map(|id| Candidate {
            id: (*id).to_owned(),
            context: None,
        })
        .collect()
}

#[test]
fn rows_give_the_label_what_the_controls_leave_and_stack_wide_controls() {
    // 470 pt detail (960 px window), a 140 pt segmented control.
    let l = form::row_layout(470.0, 140.0);
    assert!(!l.stacked);
    assert_eq!(l.label_w, 470.0 - 140.0 - 16.0);
    // Controls up to everything but the narrowest label column stay beside it.
    let max = form::controls_max(470.0);
    assert_eq!(max, 470.0 - form::MIN_LABEL - 16.0);
    assert!(!form::row_layout(470.0, max).stacked);
    assert_eq!(form::row_layout(470.0, max).label_w, form::MIN_LABEL);
    // Wider controls go under the label, which then has the whole row.
    let l = form::row_layout(470.0, max + 20.0);
    assert!(l.stacked);
    assert_eq!(l.label_w, 470.0);
    // A row too narrow for any control column never gets a negative width.
    assert_eq!(form::controls_max(40.0), 0.0);
}

#[test]
fn context_windows_read_in_thousands_or_millions() {
    assert_eq!(form::context_label(272_000), "272K");
    assert_eq!(form::context_label(128_000), "128K");
    assert_eq!(form::context_label(131_072), "131K");
    assert_eq!(form::context_label(1_000_000), "1M");
    assert_eq!(form::context_label(1_048_576), "1M");
    assert_eq!(form::context_label(1_500_000), "1.5M");
    assert_eq!(form::context_label(2_000_000), "2M");
}

#[test]
fn one_state_per_tool_most_urgent_first() {
    use ToolState::*;
    // (applied, draft, out_of_sync, drift, invalid)
    assert_eq!(
        ToolState::of(false, false, false, false, false),
        NotConfigured
    );
    // What a first apply would write is not "pending" before anything was applied.
    assert_eq!(
        ToolState::of(false, false, true, false, false),
        NotConfigured
    );
    assert_eq!(ToolState::of(false, true, false, false, false), Pending);
    assert_eq!(ToolState::of(true, false, true, false, false), Pending);
    assert_eq!(ToolState::of(true, false, false, false, false), Applied);
    assert_eq!(ToolState::of(true, true, true, true, false), Changed);
    assert_eq!(ToolState::of(true, true, true, true, true), Invalid);
    assert_eq!(ToolState::of(false, false, false, false, true), Invalid);
    for (state, status) in [
        (NotConfigured, superba_gui::design::widgets::Status::Idle),
        (Pending, superba_gui::design::widgets::Status::Warn),
        (Changed, superba_gui::design::widgets::Status::Warn),
        (Invalid, superba_gui::design::widgets::Status::Warn),
        (Applied, superba_gui::design::widgets::Status::Ok),
    ] {
        assert_eq!(state.status(), status);
    }
    // The footer explains each state; "pending" without stale entries means
    // Codex switched providers.
    assert_eq!(tools::footer_text(Pending, 2).1, "tools.footer_out_of_sync");
    assert_eq!(tools::footer_text(Pending, 0).1, "tools.footer_follow");
    assert_eq!(tools::footer_text(Applied, 0).1, "tools.footer_in_sync");
    assert_eq!(
        tools::footer_text(NotConfigured, 3).1,
        "tools.footer_not_applied"
    );
    assert_eq!(tools::footer_text(Changed, 1).1, "tools.footer_drift");
    assert_eq!(tools::footer_text(Invalid, 1).1, "tools.footer_invalid");
    let tr = Tr::new(Locale::En);
    for state in [NotConfigured, Pending, Changed, Invalid, Applied] {
        assert_ne!(tr.t(state.key()), state.key(), "{state:?} has a label");
    }
}

#[test]
fn list_rows_read_provider_route_state_or_one_word() {
    assert_eq!(
        list::summary_line(Some("Krill"), "Direct", Some("Applied"), None),
        "Krill · Direct · Applied"
    );
    // Codex has no state of its own.
    assert_eq!(
        list::summary_line(Some("Krill"), "Local router", None, None),
        "Krill · Local router"
    );
    // A tool without configuration says only why.
    assert_eq!(
        list::summary_line(Some("Krill"), "Direct", Some("x"), Some("Not installed")),
        "Not installed"
    );
    assert_eq!(list::summary_line(None, "Direct", None, None), "—");
}

#[test]
fn chosen_models_keep_one_and_move_the_default_off_an_unchecked_model() {
    let all = candidates(&["gpt-5.5", "gpt-6-astra", "gpt-5.5-mini"]);
    let mut s = HarnessSettings::default();
    // No explicit choice: every model is written.
    assert!(all.iter().all(|c| models::written(&s, &c.id)));
    assert_eq!(models::written_count(&s, &all), 3);
    assert_eq!(models::default_model(&s, &all).as_deref(), Some("gpt-5.5"));
    // Unchecking turns "all" into an explicit list without that model.
    s.default_model = Some("gpt-5.5".into());
    assert!(models::set_written(&mut s, &all, "gpt-5.5", false));
    assert_eq!(s.models, ["gpt-6-astra", "gpt-5.5-mini"]);
    assert_eq!(s.default_model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(models::written_count(&s, &all), 2);
    // Checking a written model again changes nothing.
    assert!(!models::set_written(&mut s, &all, "gpt-6-astra", true));
    assert!(models::set_written(&mut s, &all, "gpt-5.5-mini", false));
    // The last written model stays.
    assert!(!models::set_written(&mut s, &all, "gpt-6-astra", false));
    assert_eq!(s.models, ["gpt-6-astra"]);
    assert!(models::set_written(&mut s, &all, "gpt-5.5", true));
    assert_eq!(models::written_count(&s, &all), 2);
}

#[test]
fn candidates_keep_list_order_and_chosen_models_the_list_lost() {
    let spec = |slug: &str, context| {
        let mut m: ModelSpec = serde_json::from_value(serde_json::json!({ "slug": slug }))
            .expect("a minimal model spec");
        m.supported_in_api = true;
        m.context_window = context;
        m
    };
    let list = ModelList {
        source: ModelListSource::Union { members: vec![] },
        models: vec![spec("b-model", Some(400_000)), spec("a-model", None)],
        fallback_reason: None,
        sources: vec![],
    };
    let got = models::candidates(Some(&list), &["gone".to_owned(), "a-model".to_owned()]);
    let ids: Vec<_> = got.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["b-model", "a-model", "gone"]);
    assert_eq!(got[0].context, Some(400_000));
    assert_eq!(models::candidates(None, &[]), vec![]);
    let all = candidates(&["gpt-5.5", "GPT-6-astra", "claude"]);
    let shown: Vec<_> = models::filter(&all, " gpt ")
        .into_iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(shown, ["gpt-5.5", "GPT-6-astra"]);
    assert_eq!(models::filter(&all, "").len(), 3);
}

/// Every row of the page at 960x640 (and 1280x800): no fallback scroll of the
/// whole page, nothing right of it, no two visible texts on top of each other,
/// the footer's Apply in view for a tool.
#[test]
fn every_row_fits_960x640_without_overlap_in_both_languages_and_themes() {
    let svc = fake();
    let data = router_harness::data_from(&svc);
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                for selected in [Selection::Codex, Selection::Claude]
                    .into_iter()
                    .chain(HarnessId::ALL.map(Selection::Tool))
                {
                    let mut state = State::default();
                    state.selected = selected;
                    for tool in HarnessId::ALL {
                        state.set_view(svc.tool_fixture(tool));
                    }
                    let report = support::fit::render_fit_min_height(
                        window,
                        locale,
                        theme,
                        &data,
                        Page::Tools.min_height(),
                        |ui, cx, out| tools::show(ui, cx, &mut state, out),
                    );
                    let at = format!("{selected:?} {window:?} {locale:?} {theme:?}");
                    assert!(!report.scrolled, "{at}: the whole page scrolled");
                    assert!(report.fits(), "{at}");
                    assert!(
                        report.beyond_right().is_empty(),
                        "{at}: {:?}",
                        report.beyond_right()
                    );
                    let texts = report.visible_texts();
                    for (i, (a, ra)) in texts.iter().enumerate() {
                        for (b, rb) in &texts[i + 1..] {
                            assert!(
                                !ra.shrink(1.0).intersects(rb.shrink(1.0)),
                                "{at}: {a:?} {ra:?} overlaps {b:?} {rb:?}"
                            );
                        }
                    }
                    if selected.harness().is_some() {
                        let apply = tr.t("tools.apply");
                        assert!(
                            texts
                                .iter()
                                .any(|(t, r)| *t == apply && r.max.y <= report.rect.max.y),
                            "{at}: Apply is out of view"
                        );
                        for key in ["tools.provider", "tools.protocol", "tools.route"] {
                            assert!(report.whole(&tr.t(key)), "{at}: {key} is cut");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn the_chooser_keeps_one_model_selects_all_and_closes_without_writing() {
    let svc = fake();
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(svc.clone(), Locale::En, Page::Tools);
    h.until("models", |t| {
        t.with_role(Role::ComboBox)
            .into_iter()
            .any(|id| t.node(id).value().is_some_and(|v| v.starts_with("gpt-")))
    });
    h.click(Some(Role::Button), &tr.t("tools.choose_models"));
    let t = h.until("chooser", |t| t.has(&tr.t("tools.models_dialog")));
    // Every check box is a model: opencode has no other.
    let boxes: Vec<_> = t
        .with_role(Role::CheckBox)
        .into_iter()
        .filter_map(|id| t.name(id))
        .collect();
    assert!(boxes.len() >= 2, "{}", t.dump());
    // Uncheck every model: the last one stays checked.
    for name in &boxes {
        let t = h.frame();
        let id = t.find(Some(Role::CheckBox), name).unwrap();
        h.click_id(id);
    }
    let t = h.frame();
    let checked: Vec<_> = boxes
        .iter()
        .filter(|n| {
            let id = t.find(Some(Role::CheckBox), n).unwrap();
            t.node(id).toggled() == Some(Toggled::True)
        })
        .collect();
    assert_eq!(checked.len(), 1, "one model is always written");
    // Select all checks every model again.
    let t = h.click(Some(Role::Button), &tr.t("tools.models_select_all"));
    for name in &boxes {
        let id = t.find(Some(Role::CheckBox), name).unwrap();
        assert_eq!(t.node(id).toggled(), Some(Toggled::True), "{name}");
    }
    // The search narrows the list.
    let t = h.set_value(&tr.t("tools.search"), "astra");
    let shown: Vec<_> = t
        .with_role(Role::CheckBox)
        .into_iter()
        .filter_map(|id| t.name(id))
        .filter(|n| n.starts_with("gpt-"))
        .collect();
    assert_eq!(shown, ["gpt-6-astra"]);
    let t = h.click(Some(Role::Button), &tr.t("tools.done"));
    assert!(!t.has(&tr.t("tools.models_dialog")));
    assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
    assert_eq!(svc.harness_counts(), (0, 0), "choosing never writes");
}

#[test]
fn summaries_have_one_action_and_tools_show_one_stats_row() {
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(fake(), Locale::ZhCn, Page::Tools);
    let t = h.until("loaded", |t| t.has(&tr.t("tools.stats_row")));
    // The reply statistics line is one row that links to the Router page.
    assert_eq!(t.find_all(None, &tr.t("tools.stats_row")).len(), 1);
    assert!(
        t.find(Some(Role::Button), &tr.t("tools.stats_link"))
            .is_some()
    );
    for (name, key) in [
        ("Codex", "tools.open_codex"),
        ("Claude Code", "tools.open_claude"),
    ] {
        let t = h.frame();
        let row = t
            .with_role(Role::ListItem)
            .into_iter()
            .find(|id| {
                t.name(*id)
                    .is_some_and(|n| n.starts_with(&format!("{name},")))
            })
            .unwrap();
        h.click_id(row);
        let t = h.until("summary", |t| t.has(&tr.t(key)));
        let buttons: Vec<_> = t
            .with_role(Role::Button)
            .into_iter()
            .filter_map(|id| t.name(id))
            .filter(|n| {
                ![
                    "tools.reload",
                    "tools.updates",
                    // A value's own affordance, not an action.
                    "tools.copy_address",
                    "shell.window_close",
                    "shell.window_maximize",
                    "shell.window_minimize",
                    "shell.window_restore",
                ]
                .iter()
                .any(|key| &tr.t(key) == n)
            })
            .collect();
        assert_eq!(buttons.len(), 1, "{name}: {buttons:?}");
    }
}
