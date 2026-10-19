#![allow(missing_docs)]
//! Models page, per-model context window and auto-compact (user bug 7), driven
//! through real egui frames (pointer, keyboard and text events): what a row
//! shows (the value Codex uses, tagged Default / All models / Custom, `k`/`M`
//! labels), editing with validation, the echo right after Save while the view
//! reloads, the pending / applied / next switch / not used states, Reset,
//! persistence across a reload, why (and only when) Codex restarts, and the
//! personal-access-token scope.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Event, Key, Pos2, Rect, pos2};
use pretty_assertions::assert_eq;
use superba_core::{
    AuthChoice, AuthMode, Locale, ModelOverride, ModelsEdit, ProfileId, SwitchService, Timestamp,
};
use superba_gui::design::Palette;
use superba_gui::design::widgets::token_field::{exact_tokens, token_label};
use superba_gui::i18n::Tr;
use superba_gui::pages::models::{self as models_page, State};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

/// The page with its service, data, state and the actions it returned.
struct Page {
    h: Harness,
    tr: Tr,
    data: Data,
    fake: Arc<FakeService>,
    service: Arc<dyn SwitchService>,
    state: State,
    actions: Vec<Action>,
}

type Texts = Vec<(String, Rect)>;

impl Page {
    /// The fake Krill provider (applied) with an API key, or with its personal
    /// access token when `pat`.
    async fn new(pat: bool) -> Page {
        let fake = Arc::new(FakeService::new(FakeOptions {
            fast: true,
            ..Default::default()
        }));
        if !pat {
            let mut p = fake
                .profile_set()
                .await
                .unwrap()
                .profile(&ProfileId::new("krill"))
                .cloned()
                .unwrap();
            p.auth_mode = AuthMode::ApiKey;
            p.set_auth(AuthChoice::ApiKey);
            fake.save_profile(p, None).await.unwrap();
        }
        let mut page = Page {
            h: Harness::new([1100.0, 760.0], egui::Theme::Light),
            tr: Tr::new(Locale::En),
            data: Data::default(),
            service: fake.clone(),
            fake,
            state: State::default(),
            actions: Vec::new(),
        };
        page.refresh_data().await;
        page.state.set_price_table(None);
        page.wait(|p, t| p.state.view().is_some() && has(t, "gpt-5.5"));
        page
    }

    /// Reload the app data the way the shell does after `Action::Refresh`.
    async fn refresh_data(&mut self) {
        let mut data = Data {
            sites: self.fake.sites().await.unwrap(),
            ..Data::default()
        };
        data.overview.finish(self.fake.overview().await);
        data.profiles.finish(self.fake.profile_set().await);
        self.data = data;
    }

    fn frame(&mut self, events: Vec<Event>) -> Texts {
        let Page {
            h,
            tr,
            data,
            service,
            state,
            actions,
            ..
        } = self;
        let fonts = &h.fonts;
        h.frame(events, &mut |ui| {
            let cx = Cx {
                tr,
                pal: Palette::of(ui.ctx()),
                data,
                busy: false,
                now: Timestamp::now(),
                fonts,
                service,
                log: &[],
            };
            models_page::show(ui, &cx, state, actions);
        })
    }

    fn settle(&mut self) -> Texts {
        self.frame(Vec::new());
        self.frame(Vec::new())
    }

    /// Frames until `done` (at most 5 s); the last frame's texts.
    fn wait(&mut self, mut done: impl FnMut(&Page, &Texts) -> bool) -> Texts {
        let start = Instant::now();
        loop {
            let texts = self.frame(Vec::new());
            if done(self, &texts) || start.elapsed() > Duration::from_secs(5) {
                return texts;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    fn click(&mut self, pos: Pos2) {
        let button = |pressed| Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![Event::PointerMoved(pos), button(true)]);
        self.frame(vec![button(false)]);
        self.frame(Vec::new());
    }

    fn key(&mut self, key: Key) {
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }]);
        self.frame(Vec::new());
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.to_owned())]);
    }

    /// Click the field of `slug`'s row in the column headed `column`, type
    /// `text` (replacing the selected value) and press Enter.
    fn enter(&mut self, slug: &str, column: &str, text: &str) {
        let pos = self.cell(slug, column);
        self.click(pos);
        if text.is_empty() {
            self.key(Key::Backspace);
        } else {
            self.type_text(text);
        }
        self.key(Key::Enter);
        self.settle();
    }

    /// Centre of the field of a row (`slug`, or "All models (default)") in the
    /// column headed `column`.
    fn cell(&mut self, slug: &str, column: &str) -> Pos2 {
        let texts = self.settle();
        let header = find(&texts, column).unwrap_or_else(|| panic!("no column {column}"));
        let row = find(&texts, slug).unwrap_or_else(|| panic!("no row {slug}"));
        // The field starts at the column's left edge and is centred on the
        // row (slug on the first of two lines).
        pos2(header.min.x + 30.0, row.max.y + 3.0)
    }

    /// The texts drawn on `slug`'s row (same band as its slug).
    fn row(&mut self, slug: &str) -> Vec<String> {
        let texts = self.settle();
        let row = find(&texts, slug).unwrap();
        let band = (row.min.y - 6.0)..(row.max.y + 22.0);
        let mut on_row: Vec<(f32, String)> = texts
            .iter()
            .filter(|(_, r)| band.contains(&r.center().y))
            .map(|(t, r)| (r.min.x, t.clone()))
            .collect();
        on_row.sort_by(|a, b| a.0.total_cmp(&b.0));
        on_row.into_iter().map(|(_, t)| t).collect()
    }

    fn click_text(&mut self, text: &str) {
        let texts = self.settle();
        let rect = texts
            .iter()
            .rev()
            .find(|(t, _)| t == text)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("no {text:?} in {:?}", all(&texts)));
        self.click(rect.center());
    }

    /// Run a page command (as the palette or the shell does).
    fn command(&mut self, command: &str) {
        let cx = Cx {
            tr: &self.tr,
            pal: &superba_gui::design::tokens::LIGHT,
            data: &self.data,
            busy: false,
            now: Timestamp::now(),
            fonts: &self.h.fonts,
            service: &self.service,
            log: &[],
        };
        models_page::command(&mut self.state, &cx, command, &mut Vec::new());
    }

    fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }
}

/// A text of the English catalog (the locale these tests run in).
fn en(key: &str) -> String {
    Tr::new(Locale::En).t(key)
}

fn find(texts: &Texts, text: &str) -> Option<Rect> {
    texts.iter().find(|(t, _)| t == text).map(|(_, r)| *r)
}

fn has(texts: &Texts, text: &str) -> bool {
    texts.iter().any(|(t, _)| t == text)
}

fn all(texts: &Texts) -> String {
    texts
        .iter()
        .map(|(t, _)| t.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn toasts(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Toast { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn switches(actions: &[Action]) -> usize {
    actions
        .iter()
        .filter(|a| matches!(a, Action::Switch { .. }))
        .count()
}

const CONTEXT: &str = "Context length";
const COMPACT: &str = "Auto-trim";
const SPEED: &str = "Speed mode";

#[test]
fn token_labels_use_k_and_m_with_the_exact_value_for_tooltips() {
    for (n, label) in [
        (999, "999"),
        (16_000, "16K"),
        (200_000, "200K"),
        (244_800, "244.8K"),
        (117_283, "117.3K"),
        (999_960, "1M"),
        (1_000_000, "1M"),
        (1_050_000, "1.05M"),
        (1_234_567, "1.23M"),
        (10_000_000, "10M"),
    ] {
        assert_eq!(token_label(n), label, "{n}");
    }
    assert_eq!(exact_tokens(244_800), "244,800");
}

#[tokio::test]
async fn rows_show_what_codex_uses_tagged_default_or_custom() {
    let mut page = Page::new(false).await;
    // The auto-trim column sits behind "More columns" (task-20) and this suite
    // is about the compaction values, so the extra columns are on.
    page.state.more_columns = true;
    // gpt-5.5 lists a 272K window (maximum 272K): Codex compacts at 90 %.
    let row = page.row("gpt-5.5");
    for text in ["272K", "Default", "max 272K", "244.8K", "trims at 90%"] {
        assert!(row.contains(&text.to_owned()), "{text}: {row:?}");
    }
    assert!(!row.contains(&"Custom".to_owned()), "{row:?}");
    // The all-models row is unset: each model keeps its own values.
    let row = page.row("All models (default)");
    assert!(row.contains(&"—".to_owned()), "{row:?}");
    assert!(row.contains(&"Default".to_owned()), "{row:?}");
}

#[tokio::test]
async fn an_edit_is_echoed_at_once_and_stays_while_the_view_reloads() {
    let mut page = Page::new(false).await;
    // The extra columns carry the compaction values this test reads.
    page.state.more_columns = true;
    page.enter("gpt-5.5", CONTEXT, "200k");
    let row = page.row("gpt-5.5");
    for text in ["200K", "Custom", "Unsaved", "180K"] {
        assert!(row.contains(&text.to_owned()), "{text}: {row:?}");
    }
    let texts = page.settle();
    assert!(
        has(
            &texts,
            "Saving restarts Codex: Krill is the provider in use"
        ),
        "{}",
        all(&texts)
    );
    // The reload after the save takes its time (provider list, local Codex):
    // the row must keep the saved value meanwhile, never the old default.
    page.fake
        .delay_models_view(Some(Duration::from_millis(1500)));
    page.click_text("Save & apply");
    let texts = page.wait(|p, _| !p.actions.is_empty());
    let row = page.row("gpt-5.5");
    assert!(row.contains(&"200K".to_owned()), "{row:?} {}", all(&texts));
    assert!(row.contains(&"Custom".to_owned()), "{row:?}");
    // Saved, not applied yet (Codex restarts to apply it), and the page says why.
    assert!(row.contains(&en("models.state.not_applied")), "{row:?}");
    let actions = page.take_actions();
    assert!(
        toasts(&actions).contains(
            &"Saved. Krill is in use, so Codex restarts (hot) to load the new model settings."
                .to_owned()
        ),
        "{actions:?}"
    );
    assert_eq!(switches(&actions), 1);
    let edit = page.fake.models_edits().pop().unwrap();
    assert_eq!(edit.set["gpt-5.5"].context_window, Some(200_000));
    assert_eq!(edit.set["gpt-5.5"].auto_compact, None);
    // After the reload, still the saved value; once the switch wrote it, applied.
    page.fake.delay_models_view(None);
    page.wait(|p, _| {
        p.state
            .view()
            .is_some_and(|v| v.overrides.contains_key("gpt-5.5"))
    });
    page.fake.apply_models(&ProfileId::new("krill"));
    page.command("refresh");
    // The view had applied values before the switch already: wait for the
    // refreshed view, whose catalog entry carries the saved window.
    page.wait(|p, _| {
        p.state
            .view()
            .and_then(|v| v.applied.as_ref())
            .and_then(|a| a.models.get("gpt-5.5"))
            .is_some_and(|m| m.context_window == Some(200_000))
    });
    let row = page.row("gpt-5.5");
    assert!(row.contains(&"200K".to_owned()), "{row:?}");
    assert!(row.contains(&"Applied".to_owned()), "{row:?}");
}

#[tokio::test]
async fn a_saved_value_survives_a_restart_of_the_page() {
    let mut page = Page::new(false).await;
    // Auto-trim is behind "More columns" (task-20).
    page.state.more_columns = true;
    page.enter("gpt-6-astra", CONTEXT, "400k");
    page.enter("gpt-6-astra", COMPACT, "75%");
    page.click_text("Save & apply");
    page.wait(|p, _| !p.actions.is_empty());
    // A fresh page (Superba restarted) loads the profile store again.
    page.state = State::default();
    page.state.set_price_table(None);
    // A restart comes back with the default (narrow) column set, but this test
    // keeps reading the compaction values: show them again.
    page.state.more_columns = true;
    page.refresh_data().await;
    page.wait(|p, t| p.state.view().is_some() && has(t, "gpt-6-astra"));
    let row = page.row("gpt-6-astra");
    for text in ["400K", "300K", "trims at 75%"] {
        assert!(row.contains(&text.to_owned()), "{text}: {row:?}");
    }
    // Saved, but Codex still reads the previous values.
    assert!(row.contains(&en("models.state.not_applied")), "{row:?}");
    assert_eq!(
        row.iter().filter(|t| *t == "Custom").count(),
        2,
        "window and compaction: {row:?}"
    );
}

#[tokio::test]
async fn escape_cancels_and_an_empty_field_goes_back_to_the_default() {
    let mut page = Page::new(false).await;
    // Escape: nothing changes.
    let pos = page.cell("gpt-5.5", CONTEXT);
    page.click(pos);
    page.type_text("100k");
    page.key(Key::Escape);
    let row = page.row("gpt-5.5");
    assert!(row.contains(&"272K".to_owned()), "{row:?}");
    assert_eq!(models_page::status(&page.state).pending, 0);
    // Typing the default value back is no override either.
    page.enter("gpt-5.5", CONTEXT, "272000");
    assert_eq!(models_page::status(&page.state).pending, 0);
    // Save a custom value, then clear the field: back to the default, unsaved.
    page.enter("gpt-5.5", CONTEXT, "200k");
    page.click_text("Save & apply");
    page.wait(|p, _| !p.actions.is_empty());
    page.take_actions();
    page.enter("gpt-5.5", CONTEXT, "");
    let row = page.row("gpt-5.5");
    for text in ["272K", "Default", "Unsaved"] {
        assert!(row.contains(&text.to_owned()), "{text}: {row:?}");
    }
    page.click_text("Save & apply");
    page.wait(|p, _| !p.actions.is_empty());
    let edit = page.fake.models_edits().pop().unwrap();
    assert_eq!(edit.set["gpt-5.5"], ModelOverride::default());
}

#[tokio::test]
async fn reset_restores_the_default() {
    let mut page = Page::new(false).await;
    // The reset icon is the trailing column, past auto-trim and speed.
    page.state.more_columns = true;
    page.fake
        .save_models(ModelsEdit {
            profile_id: Some(ProfileId::new("krill")),
            set: [(
                "gpt-5.5".to_owned(),
                ModelOverride {
                    context_window: Some(200_000),
                    auto_compact: Some(150_000),
                    ..ModelOverride::default()
                },
            )]
            .into(),
            ..ModelsEdit::default()
        })
        .await
        .unwrap();
    page.command("refresh");
    page.wait(|p, _| {
        p.state
            .view()
            .is_some_and(|v| v.overrides.contains_key("gpt-5.5"))
    });
    // The reset icon is the row's last column, right of the speed column.
    let texts = page.settle();
    let speed = find(&texts, SPEED).unwrap();
    let y = page.cell("gpt-5.5", CONTEXT).y;
    page.click(pos2(speed.min.x + 132.0 + 14.0, y));
    let row = page.row("gpt-5.5");
    for text in ["272K", "244.8K", "Unsaved"] {
        assert!(row.contains(&text.to_owned()), "{text}: {row:?}");
    }
    page.click_text("Save & apply");
    page.wait(|p, _| !p.actions.is_empty());
    let edit = page.fake.models_edits().pop().unwrap();
    assert!(edit.set["gpt-5.5"].is_empty(), "{edit:?}");
    assert!(page.state.view().unwrap().overrides.is_empty());
}

#[tokio::test]
async fn invalid_values_are_named_and_block_saving() {
    let mut page = Page::new(false).await;
    // The compaction field this test types into is behind "More columns".
    page.state.more_columns = true;
    // Above the model's maximum.
    page.enter("gpt-5.5", CONTEXT, "300k");
    let texts = page.settle();
    assert!(
        has(&texts, "gpt-5.5: Above this model's maximum 272K"),
        "{}",
        all(&texts)
    );
    // The rejected text stays in the field.
    assert!(page.row("gpt-5.5").contains(&"300k".to_owned()));
    // A valid window, then a compaction not below it.
    page.enter("gpt-5.5", CONTEXT, "200k");
    page.enter("gpt-5.5", COMPACT, "200k");
    let texts = page.settle();
    assert!(
        has(
            &texts,
            "gpt-5.5: The auto-trim limit must be below the context window (200K)"
        ),
        "{}",
        all(&texts)
    );
    page.click_text("Save & apply");
    page.settle();
    assert!(page.fake.models_edits().is_empty(), "nothing saved");
    // A threshold that fits, then a lower window that makes it invalid: the
    // compaction field is flagged and saving stays blocked.
    page.enter("gpt-5.5", COMPACT, "150k");
    page.enter("gpt-5.5", CONTEXT, "128k");
    let texts = page.settle();
    assert!(
        has(
            &texts,
            "gpt-5.5: The auto-trim limit must be below the context window (128K)"
        ),
        "{}",
        all(&texts)
    );
    page.click_text("Save & apply");
    page.settle();
    assert!(page.fake.models_edits().is_empty());
    // Fixed: saved.
    page.enter("gpt-5.5", COMPACT, "100k");
    page.click_text("Save & apply");
    page.wait(|p, _| !p.actions.is_empty());
    let edit = page.fake.models_edits().pop().unwrap();
    assert_eq!(edit.set["gpt-5.5"].context_window, Some(128_000));
    assert_eq!(edit.set["gpt-5.5"].auto_compact, Some(100_000));
}

#[tokio::test]
async fn a_provider_not_in_use_is_only_saved() {
    let mut page = Page::new(false).await;
    let mut other = page
        .fake
        .profile_set()
        .await
        .unwrap()
        .profile(&ProfileId::new("krill"))
        .cloned()
        .unwrap();
    other.id = ProfileId::new("krill-2");
    other.name = "Krill 2".into();
    page.fake.save_profile(other, None).await.unwrap();
    page.refresh_data().await;
    page.command("profile:krill-2");
    page.wait(|p, _| {
        p.state
            .view()
            .is_some_and(|v| v.profile_id.as_str() == "krill-2")
    });
    page.enter("gpt-5.5", CONTEXT, "200k");
    let texts = page.settle();
    assert!(
        has(
            &texts,
            "Takes effect the next time you switch to Krill 2; Codex is not restarted"
        ),
        "{}",
        all(&texts)
    );
    page.click_text("Save");
    page.wait(|p, _| !p.actions.is_empty());
    let actions = page.take_actions();
    assert_eq!(switches(&actions), 0, "{actions:?}");
    assert!(
        toasts(&actions).contains(&"Saved; applies on the next switch to Krill 2.".to_owned()),
        "{actions:?}"
    );
    let row = page.row("gpt-5.5");
    assert!(row.contains(&"200K".to_owned()), "{row:?}");
    assert!(row.contains(&en("models.state.next_switch")), "{row:?}");
}

#[tokio::test]
async fn with_a_personal_access_token_only_the_all_models_row_reaches_codex() {
    let mut page = Page::new(true).await;
    // A per-model value saved earlier is shown as not used; the row shows what
    // Codex uses.
    page.fake
        .save_models(ModelsEdit {
            profile_id: Some(ProfileId::new("krill")),
            set: [(
                "gpt-5.5".to_owned(),
                ModelOverride {
                    context_window: Some(200_000),
                    ..ModelOverride::default()
                },
            )]
            .into(),
            ..ModelsEdit::default()
        })
        .await
        .unwrap();
    page.command("refresh");
    page.wait(|p, _| p.state.view().is_some_and(|v| !v.overrides.is_empty()));
    let texts = page.settle();
    assert!(has(&texts, &en("models.pat_scope")), "{}", all(&texts));
    let row = page.row("gpt-5.5");
    assert!(row.contains(&"272K".to_owned()), "{row:?}");
    assert!(row.contains(&en("models.state.not_used")), "{row:?}");
    // Per-model fields are read-only: typing changes nothing.
    page.enter("gpt-6-astra", CONTEXT, "100k");
    assert_eq!(models_page::status(&page.state).pending, 0);
    // The all-models row works and restarts Codex (the provider is in use).
    page.enter("All models (default)", CONTEXT, "200k");
    let row = page.row("gpt-6-astra");
    assert!(row.contains(&"200K".to_owned()), "{row:?}");
    assert!(row.contains(&"All models".to_owned()), "{row:?}");
    page.click_text("Save & apply");
    page.wait(|p, _| !p.actions.is_empty());
    let actions = page.take_actions();
    assert_eq!(switches(&actions), 1, "{actions:?}");
    assert_eq!(
        page.fake
            .models_edits()
            .pop()
            .unwrap()
            .defaults
            .and_then(|d| d.context_window),
        Some(200_000)
    );
}

#[tokio::test]
async fn saved_values_codex_does_not_use_yet_offer_apply_now() {
    let mut page = Page::new(false).await;
    page.enter("gpt-5.5", CONTEXT, "200k");
    page.click_text("Save & apply");
    page.wait(|p, _| !p.actions.is_empty());
    page.take_actions();
    // The switch was cancelled: Codex still reads the old values.
    page.fake.apply_models(&ProfileId::new("krill"));
    page.fake
        .save_models(ModelsEdit {
            profile_id: Some(ProfileId::new("krill")),
            set: [(
                "gpt-5.5".to_owned(),
                ModelOverride {
                    context_window: Some(180_000),
                    ..ModelOverride::default()
                },
            )]
            .into(),
            ..ModelsEdit::default()
        })
        .await
        .unwrap();
    page.command("refresh");
    page.wait(|p, _| {
        p.state
            .view()
            .is_some_and(|v| v.overrides["gpt-5.5"].context_window == Some(180_000))
    });
    let texts = page.settle();
    assert!(
        has(&texts, "Saved, not used by Codex yet: 1"),
        "{}",
        all(&texts)
    );
    page.click_text("Apply now");
    assert_eq!(switches(&page.take_actions()), 1);
}

#[tokio::test]
async fn the_table_shows_three_columns_until_more_columns_is_picked() {
    let mut page = Page::new(false).await;
    // Three columns by default: the model, the context length and the price —
    // the three a reader decides with. Auto-trim and speed are behind the "More
    // columns" switch. The price label is wider than its column, so it is
    // matched by its start (task-20).
    let texts = page.settle();
    for key in ["models.col.model", "models.col.context"] {
        assert!(has(&texts, &en(key)), "{key}: {}", all(&texts));
    }
    assert!(
        all(&texts).contains(&en("models.col.price")),
        "{}",
        all(&texts)
    );
    for key in ["models.col.compact", "models.col.speed"] {
        assert!(!has(&texts, &en(key)), "{key}: {}", all(&texts));
    }
    page.click_text(&en("models.more_columns"));
    let texts = page.settle();
    for key in ["models.col.compact", "models.col.speed"] {
        assert!(has(&texts, &en(key)), "{key}: {}", all(&texts));
    }
    page.click_text(&en("models.more_columns"));
    let texts = page.settle();
    for key in ["models.col.compact", "models.col.speed"] {
        assert!(!has(&texts, &en(key)), "{key}: {}", all(&texts));
    }
}

#[tokio::test]
async fn every_row_says_what_its_numbers_mean() {
    let mut page = Page::new(false).await;
    // gpt-5.5 lists a 272K window: Codex trims at 244.8K, and the row says so
    // in one line under the slug.
    let summary = Tr::new(Locale::En).f(
        "models.summary",
        &[("window", &"272K"), ("compact", &"244.8K")],
    );
    let texts = page.settle();
    let slug = find(&texts, "gpt-5.5").unwrap();
    let (line, rect) = texts
        .iter()
        .find(|(t, r)| t.ends_with(&summary) && r.center().y > slug.max.y)
        .map(|(t, r)| (t.clone(), *r))
        .unwrap_or_else(|| panic!("no line with {summary:?} on gpt-5.5's row: {}", all(&texts)));
    assert!(
        rect.max.y <= slug.max.y + 22.0,
        "the line sits inside the row: {rect:?} {slug:?}"
    );
    assert!(
        line.contains(" · ") && line.len() > summary.len(),
        "{line:?}"
    );
    // Nothing is set on the all-models row: it states the override rule only.
    assert!(has(&texts, &en("models.all_hint")), "{}", all(&texts));
    // Setting a default window makes that row state its numbers too.
    page.enter("All models (default)", CONTEXT, "200k");
    let expected = Tr::new(Locale::En).f(
        "models.summary",
        &[("window", &"200K"), ("compact", &"180K")],
    );
    let texts = page.settle();
    assert!(
        texts.iter().any(|(t, _)| t.ends_with(&expected)),
        "{expected:?}: {}",
        all(&texts)
    );
}
