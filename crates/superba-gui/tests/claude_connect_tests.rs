#![allow(missing_docs)]

//! Claude Code · 连接与模型 as one flow of sections (ui-spec, ui-claude): the
//! layout rules (setting rows keep their help as wide as their controls
//! allow, capability chips end in "+N", both tables fit the narrowest window),
//! the slot rows and their dropdowns, the `/model` table (tick, add by hand,
//! remove, reorder), and the whole page at 960×640 and 1280×800, light and
//! dark, Chinese and English: one short header line, nothing clipped or
//! overlapping, no page-level scroll fallback, every control named, and a
//! model put into a slot, edited and ticked into `/model` through the
//! accessibility tree.

mod a11y_harness;
mod router_harness;
mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::{Role, Toggled};
use egui::{Rect, pos2, vec2};
use router_harness::Harness;
use superba_core::{
    ClaudeCapability, ClaudeModelRow, ClaudeModels, ClaudeSlot, Locale, ModelListKind, SlotModel,
};
use superba_gui::design::layout;
use superba_gui::design::tokens::size;
use superba_gui::design::widgets::Table;
use superba_gui::design::{fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use superba_gui::pages::claude::{self, connect};
use support::fake::{FakeOptions, FakeService};

use connect::picker::{self, Line};
use connect::slots::{self, Choice, SlotKey};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn row(id: &str, usable: bool) -> ClaudeModelRow {
    ClaudeModelRow {
        id: id.into(),
        display_name: Some(id.to_uppercase()),
        lists: vec![ModelListKind::Standard],
        manual: false,
        usable,
        context_window: Some(400_000),
        reasoning_levels: Vec::new(),
        suggested: BTreeSet::from([ClaudeCapability::Effort]),
    }
}

// ---- layout rules ---------------------------------------------------------

#[test]
fn setting_rows_give_their_help_the_room_the_controls_leave() {
    // A dropdown row at 960×640 (680 points of column): the help keeps the
    // rest, not a fixed share.
    let w = 680.0;
    assert_eq!(connect::text_width(w, 260.0), w - 260.0 - 12.0);
    // An address with its buttons still leaves the help a readable column.
    assert!(connect::text_width(w, 380.0) >= 280.0);
    // Never below the minimum: the controls shrink (they elide) instead.
    assert_eq!(connect::text_width(400.0, 380.0), connect::TEXT_MIN);
    // Below the stacking width the controls go under the text.
    const { assert!(connect::STACK_BELOW > connect::TEXT_MIN + 300.0) };
}

#[test]
fn capability_chips_end_in_a_count_when_they_do_not_fit() {
    let widths = [50.0, 90.0, 80.0, 60.0];
    let more = |_n: usize| 28.0;
    // Everything fits: no "+N".
    assert_eq!(slots::chips_that_fit(&widths, 400.0, 6.0, more), 4);
    // Room for two and the "+2" chip after them.
    assert_eq!(slots::chips_that_fit(&widths, 190.0, 6.0, more), 2);
    // All four need 298: in 290 the fourth would fit but then "+1" would not.
    assert_eq!(slots::chips_that_fit(&widths, 298.0, 6.0, more), 4);
    assert_eq!(slots::chips_that_fit(&widths, 290.0, 6.0, more), 3);
    // Not even one: the count alone.
    assert_eq!(slots::chips_that_fit(&widths, 40.0, 6.0, more), 0);
    assert_eq!(slots::chips_that_fit(&[], 10.0, 6.0, more), 0);
}

#[test]
fn both_tables_fit_the_narrowest_window_without_squeezing() {
    // The page at 960×640, less the scroll bar's gutter and the table's
    // 12 point side padding.
    let central = Rect::from_min_max(pos2(size::SIDEBAR, 0.0), pos2(960.0, 640.0));
    let page = layout::page_rect(central, true);
    let room = page.width() - 14.0 - 24.0;
    let (_, slot_mins) = slots::COLUMNS;
    let (_, picker_mins) = picker::COLUMNS;
    assert!(
        slot_mins.iter().sum::<f32>() <= room,
        "slot table needs {} of {room}",
        slot_mins.iter().sum::<f32>()
    );
    assert!(
        picker_mins.iter().sum::<f32>() <= room,
        "/model table needs {} of {room}",
        picker_mins.iter().sum::<f32>()
    );
    // The model column holds a dropdown with an id like gpt-5.5-mini.
    assert!(slot_mins[1] - 12.0 >= 180.0);
}

#[test]
fn a_right_aligned_header_ends_where_its_numbers_end() {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(egui::Theme::Light));
    // As in the `/model` table, the context column is followed by the
    // (left-aligned) actions: its header and its numbers end
    // `Table::RIGHT_INSET` before the column's edge. A right-aligned last
    // column has only the row's padding after it and ends at its edge.
    let cases = [
        (
            Table::new(&[1.0, 0.0, 0.0])
                .min_widths(&[100.0, 80.0, 80.0])
                .right_aligned(&[1]),
            vec!["Model".to_owned(), "Context".into(), "Actions".into()],
            Table::RIGHT_INSET,
        ),
        (
            Table::new(&[1.0, 0.0])
                .min_widths(&[100.0, 80.0])
                .right_aligned(&[1]),
            vec!["Model".to_owned(), "Context".into()],
            0.0,
        ),
    ];
    for (table, labels, inset) in cases {
        assert!((table.right_inset(1) - inset).abs() < f32::EPSILON);
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0))),
                ..Default::default()
            },
            |ui| {
                ui.set_width(400.0);
                table.header(ui, &labels);
                table.row(ui, size::ROW, true, |ui, col| {
                    ui.label(["gpt-5.5", "200K", "Edit"][col]);
                });
            },
        );
        let texts: Vec<(String, Rect)> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                _ => None,
            })
            .collect();
        let cells = table.cells(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 32.0)));
        let find = |s: &str| texts.iter().find(|(t, _)| t == s).unwrap().1;
        let (model, context, number) = (find("Model"), find("Context"), find("200K"));
        assert!((model.min.x - cells[0].min.x).abs() < 0.5, "{model:?}");
        assert!(
            (context.max.x - (cells[1].max.x - inset)).abs() < 1.0,
            "{context:?} in {:?}",
            cells[1]
        );
        assert!(
            (number.max.x - context.max.x).abs() < 0.5,
            "200K ends at {} under a header ending at {}",
            number.max.x,
            context.max.x
        );
        out.drop_without_applying_deltas();
    }
}

// ---- slots ----------------------------------------------------------------

#[test]
fn every_slot_row_writes_its_own_setting() {
    let settings: BTreeSet<&str> = SlotKey::ALL.iter().map(|k| k.setting()).collect();
    assert_eq!(settings.len(), 8);
    assert!(settings.contains("ANTHROPIC_DEFAULT_OPUS_MODEL"));
    assert!(settings.contains("ANTHROPIC_CUSTOM_MODEL_OPTION"));
    assert!(settings.contains("model"));
    assert!(settings.contains("CLAUDE_CODE_SUBAGENT_MODEL"));
    assert!(settings.contains("ANTHROPIC_SMALL_FAST_MODEL"));
    // Capabilities only for the /model slots.
    assert_eq!(
        SlotKey::ALL.iter().filter(|k| k.has_capabilities()).count(),
        5
    );
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        for k in SlotKey::ALL {
            assert_ne!(tr.t(k.label_key()), k.label_key(), "{k:?}");
            assert_ne!(tr.t(k.tip_key()), k.tip_key(), "{k:?}");
        }
    }
}

#[test]
fn a_slot_takes_a_model_and_a_new_model_drops_the_old_row_text() {
    let mut m = ClaudeModels::default();
    let opus = SlotKey::Slot(ClaudeSlot::Opus);
    opus.set(&mut m, Some("gpt-6.1-sol".into()));
    assert_eq!(opus.model(&m), Some("gpt-6.1-sol"));
    m.slots.get_mut(&ClaudeSlot::Opus).unwrap().name = Some("Sol".into());
    // The same model keeps its row text.
    opus.set(&mut m, Some("gpt-6.1-sol".into()));
    assert_eq!(m.slots[&ClaudeSlot::Opus].name.as_deref(), Some("Sol"));
    // Another model starts without it.
    opus.set(&mut m, Some("deepseek-v4".into()));
    assert_eq!(m.slots[&ClaudeSlot::Opus], SlotModel::new("deepseek-v4"));
    // Not set: gone; a blank id clears too.
    opus.set(&mut m, None);
    assert!(m.slots.is_empty());
    SlotKey::Main.set(&mut m, Some("kimi-k3".into()));
    SlotKey::Subagent.set(&mut m, Some("glm-5".into()));
    SlotKey::Background.set(&mut m, Some("  ".into()));
    assert_eq!(m.main.as_deref(), Some("kimi-k3"));
    assert_eq!(SlotKey::Subagent.model(&m), Some("glm-5"));
    assert_eq!(m.small_fast, None);
}

#[test]
fn a_slot_dropdown_lists_served_models_first_and_keeps_an_unlisted_value() {
    let rows = [
        row("gpt-6-astra", false),
        row("gpt-5.5", true),
        row("deepseek-v4", true),
    ];
    let ids = |c: &[Choice]| c.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
    // Served models in list order, then the Codex-only one (shown disabled).
    let c = slots::choices(&rows, None);
    assert_eq!(ids(&c), ["gpt-5.5", "deepseek-v4", "gpt-6-astra"]);
    assert!(!c[2].usable);
    assert_eq!(c[0].display.as_deref(), Some("GPT-5.5"));
    // A value no list has (set by hand, or the list has not loaded) is first.
    let c = slots::choices(&rows, Some("kimi-k3"));
    assert_eq!(ids(&c)[0], "kimi-k3");
    assert_eq!(c.len(), 4);
    // A listed value is not repeated.
    assert_eq!(slots::choices(&rows, Some("gpt-5.5")).len(), 3);
    assert_eq!(ids(&slots::choices(&[], Some("x"))), ["x"]);
}

#[test]
fn chips_show_the_users_capabilities_else_the_suggestion() {
    let rows = [row("gpt-5.5", true)];
    let mut m = ClaudeModels::default();
    let (caps, own) = slots::capabilities(&m, &rows, "gpt-5.5");
    assert!(!own);
    assert_eq!(caps, BTreeSet::from([ClaudeCapability::Effort]));
    m.capabilities.insert(
        "GPT-5.5".into(),
        BTreeSet::from([ClaudeCapability::Thinking, ClaudeCapability::MaxEffort]),
    );
    let (caps, own) = slots::capabilities(&m, &rows, "gpt-5.5");
    assert!(own, "matched without case");
    assert!(caps.contains(&ClaudeCapability::MaxEffort));
    // An id no list has: the family's suggestion.
    let (caps, own) = slots::capabilities(&m, &rows, "deepseek-v4");
    assert!(!own);
    assert!(caps.contains(&ClaudeCapability::Thinking));
}

// ---- /model ----------------------------------------------------------------

#[test]
fn the_model_table_ticks_adds_removes_and_reorders_rows() {
    let rows = vec![
        row("gpt-5.5", true),
        row("gpt-5.5-mini", true),
        row("gpt-6-astra", false),
    ];
    let mut m = ClaudeModels::default();
    picker::set_shown(&mut m, "gpt-5.5-mini", true);
    picker::set_shown(&mut m, "gpt-5.5", true);
    // Ticked rows know their place in /model (in the order they were ticked).
    let l = picker::lines(&rows, &m, "");
    let pos: Vec<Option<usize>> = l.iter().map(|l: &Line| l.position).collect();
    assert_eq!(pos, [Some(1), Some(0), None]);
    // Search by id or display name, without case.
    assert_eq!(picker::lines(&rows, &m, "MINI").len(), 1);
    assert_eq!(picker::lines(&rows, &m, "gpt-6 ").len(), 1);
    assert!(picker::lines(&rows, &m, "claude").is_empty());
    // Reorder: up and down within the list, not past its ends.
    assert!(picker::move_row(&mut m, "gpt-5.5", true));
    assert_eq!(m.picker[0].model, "gpt-5.5");
    assert!(!picker::move_row(&mut m, "gpt-5.5", true));
    assert!(!picker::move_row(&mut m, "gpt-5.5-mini", false));
    assert!(!picker::move_row(&mut m, "absent", false));
    // Untick.
    picker::set_shown(&mut m, "gpt-5.5-mini", false);
    assert_eq!(m.picker.len(), 1);
    // Add by hand: trimmed, listed in /model, kept as an extra id only when
    // no list has it; blank ids are refused.
    assert!(picker::add_manual(&mut m, &rows, "  qwen3.7-plus "));
    assert_eq!(m.extra_models, ["qwen3.7-plus"]);
    assert!(m.picker.iter().any(|p| p.model == "qwen3.7-plus"));
    assert!(picker::add_manual(&mut m, &rows, "gpt-5.5-mini"));
    assert_eq!(m.extra_models, ["qwen3.7-plus"]);
    assert!(!picker::add_manual(&mut m, &rows, "   "));
    let all = connect::rows(None, &m);
    let lines = picker::lines(&all, &m, "qwen");
    assert!(lines[0].removable && lines[0].row.manual);
    picker::remove_manual(&mut m, "qwen3.7-plus");
    assert!(m.extra_models.is_empty());
    assert!(!m.picker.iter().any(|p| p.model == "qwen3.7-plus"));
}

#[test]
fn context_windows_are_short() {
    assert_eq!(picker::context_text(400_000), "400K");
    assert_eq!(picker::context_text(128_000), "128K");
    assert_eq!(picker::context_text(1_048_576), "1M");
    assert_eq!(picker::context_text(1_500_000), "1.5M");
    assert_eq!(picker::context_text(512), "512");
}

// ---- the page ----------------------------------------------------------------

/// Texts drawn inside their clip and inside the page.
fn visible(f: &router_harness::Frame) -> Vec<(String, Rect)> {
    f.clipped_texts
        .iter()
        .filter(|(t, r, clip)| {
            !t.trim().is_empty() && clip.expand(0.5).contains_rect(*r) && f.rect.contains_rect(*r)
        })
        .map(|(t, r, _)| (t.clone(), *r))
        .collect()
}

#[test]
fn the_page_fits_every_size_theme_and_language_without_clipping_or_overlap() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        for size in [[960.0, 640.0], [1280.0, 800.0]] {
            for dark in [false, true] {
                let mut h = Harness::new(size, locale, fake(), true)
                    .min_height(Page::ClaudeCode.min_height());
                if dark {
                    h = h.dark();
                }
                let mut state = claude::State::default();
                h.until(
                    |ui, cx, out| claude::show(ui, cx, &mut state, Page::ClaudeCode, out),
                    |f| f.has(&tr.t("claude.slots.title")) && f.has("Codex"),
                );
                let f =
                    h.settle(|ui, cx, out| claude::show(ui, cx, &mut state, Page::ClaudeCode, out));
                let at = format!("{locale:?} {size:?} dark={dark}");
                // The page itself never falls back to scrolling as a whole: its
                // sections scroll, the header and the apply bar stay.
                assert!(!f.scrolled, "{at}: page-level scroll fallback");
                assert!(f.fits(), "{at}: drawn below the page");
                // One short header line, never cut.
                let subtitle = tr.t("page.claude_code.subtitle");
                assert!(f.has(&subtitle), "{at}");
                assert!(!f.elided.contains(&subtitle), "{at}: subtitle elided");
                // The section titles and every row title of 连接 are whole.
                let texts = visible(&f);
                for key in [
                    "claude.section.connection",
                    "claude.provider.title",
                    "claude.route.title",
                    "claude.endpoint.title",
                    "claude.slots.title",
                ] {
                    let t = tr.t(key);
                    assert!(
                        texts.iter().any(|(x, _)| x == &t),
                        "{at}: {t:?} not visible: {}",
                        f.all_text()
                    );
                    assert!(!f.elided.contains(&t), "{at}: {t:?} elided");
                }
                // Help text wraps instead of being cut.
                for key in ["claude.provider.help", "claude.route.help_soon"] {
                    let t = tr.t(key);
                    assert!(
                        !f.elided
                            .iter()
                            .any(|e| t.starts_with(e.trim_end_matches('…'))),
                        "{at}: {t:?} elided"
                    );
                }
                // No two visible texts overlap.
                for (i, (a, ra)) in texts.iter().enumerate() {
                    for (b, rb) in &texts[i + 1..] {
                        assert!(
                            !ra.shrink(1.0).intersects(rb.shrink(1.0)),
                            "{at}: {a:?} {ra:?} overlaps {b:?} {rb:?}"
                        );
                    }
                }
                // The apply bar stays at the bottom with its three states.
                let apply = tr.t("claude.apply.button");
                let (_, r) = texts
                    .iter()
                    .find(|(t, _)| t == &apply)
                    .unwrap_or_else(|| panic!("{at}: no visible Apply"));
                assert!(r.max.y <= f.rect.max.y && r.min.y > f.rect.max.y - size::FOOTER - 8.0);
            }
        }
    }
}

#[test]
fn a_slot_is_set_edited_and_its_model_ticked_through_the_accessibility_tree() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(fake(), locale, Page::ClaudeCode);
        h.until("models listed", |t| t.has("gpt-6.1-sol"));
        let opus = tr.t("claude.slot.opus");
        // Eight slot dropdowns, each named after its row.
        let t = h.settle();
        for k in SlotKey::ALL {
            let name = tr.f("claude.slots.model_of", &[("slot", &tr.t(k.label_key()))]);
            assert!(
                t.find(Some(Role::ComboBox), &name).is_some(),
                "{name}\n{}",
                t.dump()
            );
        }
        // Nothing to edit before a model is chosen.
        let edit = tr.f("claude.slots.edit_named", &[("slot", &opus)]);
        assert!(!t.has(&edit));
        // Pick gpt-6.1-sol for Opus from its dropdown.
        h.click(
            Some(Role::ComboBox),
            &tr.f("claude.slots.model_of", &[("slot", &opus)]),
        );
        let t = h.settle();
        let option = t
            .find_all(None, "gpt-6.1-sol")
            .into_iter()
            .find(|id| t.node(*id).role() != Role::Label)
            .unwrap_or_else(|| panic!("no option:\n{}", t.dump()));
        let t = h.click_id(option);
        let combo = t
            .find(
                Some(Role::ComboBox),
                &tr.f("claude.slots.model_of", &[("slot", &opus)]),
            )
            .unwrap();
        assert_eq!(t.node(combo).value(), Some("gpt-6.1-sol"), "{}", t.dump());
        assert!(t.has_containing(&tr.t("claude.footer.unsaved")));
        // Its row text and capabilities in the dialog.
        let t = h.click(Some(Role::Button), &edit);
        assert!(
            t.has(&tr.f("claude.slot_dialog.title", &[("slot", &opus)])),
            "{}",
            t.dump()
        );
        assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
        let t = h.click(Some(Role::CheckBox), "temperature");
        let cb = t.find(Some(Role::CheckBox), "temperature").unwrap();
        assert_eq!(t.node(cb).toggled(), Some(Toggled::True));
        h.click(Some(Role::Button), &tr.t("claude.slot_dialog.done"));
        // Tick the Anthropic-only model into /model: its row can be edited then.
        let show = tr.f("claude.picker.show_named", &[("model", &"deepseek-v4")]);
        let t = h.click(Some(Role::CheckBox), &show);
        let cb = t.find(Some(Role::CheckBox), &show).unwrap();
        assert_eq!(t.node(cb).toggled(), Some(Toggled::True));
        let row = tr.f("claude.picker.edit_named", &[("model", &"deepseek-v4")]);
        assert!(t.find(Some(Role::Button), &row).is_some(), "{}", t.dump());
        // The Codex-only model cannot be ticked.
        let astra = tr.f("claude.picker.show_named", &[("model", &"gpt-6-astra")]);
        let cb = t.find(Some(Role::CheckBox), &astra).unwrap();
        assert!(t.node(cb).is_disabled());
        // Add an id by hand: listed and ticked.
        h.set_value(&tr.t("claude.models.add_field"), "qwen3.7-plus");
        let t = h.click(Some(Role::Button), &tr.t("claude.models.add"));
        let added = tr.f("claude.picker.show_named", &[("model", &"qwen3.7-plus")]);
        let cb = t
            .find(Some(Role::CheckBox), &added)
            .unwrap_or_else(|| panic!("{}", t.dump()));
        assert_eq!(t.node(cb).toggled(), Some(Toggled::True));
        assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
    }
}
