#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::path::Path;

use egui::{Color32, FontFamily, Rect, Sense, Theme, pos2, vec2};
use pretty_assertions::assert_eq;
use superba_gui::design::fonts::{self, CjkFace, FontReport};
use superba_gui::design::icons::{self, Icon};
use superba_gui::design::tokens::{DARK, LIGHT, Palette, contrast_ratio};
use superba_gui::design::widgets::{
    Badge, Button, Card, CountdownBar, IconButton, KeyInput, Latency, LatencyBadge, Meter,
    Segmented, SplitBar, Status, StatusDot, Table, Text, Toast, Toasts, Toggle, card, dialog, kbd,
    link_button,
};

#[test]
fn body_and_muted_text_meet_contrast_on_every_background() {
    for (theme, palette) in [("dark", DARK), ("light", LIGHT)] {
        for (name, color) in [("text", palette.text), ("text_muted", palette.text_muted)] {
            for (background, surface) in [
                ("bg", palette.bg),
                ("bg_subtle", palette.bg_subtle),
                ("surface", palette.surface),
                ("hover", palette.hover),
            ] {
                let ratio = contrast_ratio(color, surface);
                assert!(
                    ratio >= 4.5,
                    "{theme}: {name} on {background} has contrast {ratio}, expected >= 4.5"
                );
            }
        }
    }
}

#[test]
fn subtle_text_meets_secondary_contrast() {
    for (theme, palette) in [("dark", DARK), ("light", LIGHT)] {
        for (name, surface) in [("bg", palette.bg), ("surface", palette.surface)] {
            let ratio = contrast_ratio(palette.text_subtle, surface);
            assert!(
                ratio >= 3.0,
                "{theme}: text_subtle on {name} has contrast {ratio}, expected >= 3"
            );
        }
    }
}

#[test]
fn button_text_meets_primary_and_danger_contrast() {
    for (theme, palette) in [("dark", DARK), ("light", LIGHT)] {
        let primary = contrast_ratio(palette.on_primary, palette.primary);
        let danger = contrast_ratio(palette.on_danger, palette.red);
        assert!(primary >= 4.5, "{theme}: primary button contrast {primary}");
        assert!(danger >= 3.0, "{theme}: danger button contrast {danger}");
    }
}

#[test]
fn contrast_has_known_endpoints_and_is_symmetric() {
    assert!((contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.001);
    for a in [Color32::BLACK, Color32::WHITE, DARK.text, LIGHT.red] {
        assert!((contrast_ratio(a, a) - 1.0).abs() < 0.00001);
        for b in [Color32::BLACK, Color32::WHITE, DARK.bg, LIGHT.bg_subtle] {
            assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 0.00001);
        }
    }
}

#[test]
fn default_font_definitions_register_five_geist_faces_and_families() {
    let definitions = fonts::definitions(&FontReport::default());
    assert_eq!(
        definitions
            .font_data
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "geist-regular",
            "geist-medium",
            "geist-semibold",
            "geist-mono",
            "geist-mono-medium",
        ])
    );
    let expected = [
        (FontFamily::Proportional, "geist-regular"),
        (FontFamily::Monospace, "geist-mono"),
        (FontFamily::Name("geist-medium".into()), "geist-medium"),
        (FontFamily::Name("geist-semibold".into()), "geist-semibold"),
        (
            FontFamily::Name("geist-mono-medium".into()),
            "geist-mono-medium",
        ),
    ];
    assert_eq!(definitions.families.len(), expected.len());
    for (family, primary) in expected {
        let chain = definitions
            .families
            .get(&family)
            .expect("registered font family");
        assert_eq!(chain.first().map(String::as_str), Some(primary));
        for face in chain {
            assert!(
                definitions.font_data.contains_key(face),
                "{family:?} references missing face {face}"
            );
        }
    }
}

#[test]
fn missing_cjk_font_is_skipped_without_dangling_family_references() {
    let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nonexistent-cjk-font.ttf");
    assert!(!missing.try_exists().unwrap());
    let report = FontReport {
        cjk_regular: Some(CjkFace {
            family: "Missing CJK".into(),
            path: missing,
            index: 0,
            weight: 400,
        }),
        ..FontReport::default()
    };
    let definitions = fonts::definitions(&report);
    assert!(!definitions.font_data.contains_key("cjk-regular"));
    assert_eq!(definitions.font_data.len(), 5);
    assert_eq!(
        definitions.families,
        fonts::definitions(&FontReport::default()).families
    );
}

#[test]
fn empty_font_database_has_no_cjk_fallback() {
    assert_eq!(
        fonts::pick_cjk(&fontdb::Database::new()),
        FontReport::default()
    );
}

#[test]
fn cjk_candidate_paths_are_absolute() {
    for path in fonts::candidate_paths() {
        assert!(
            path.is_absolute(),
            "CJK candidate must be absolute: {}",
            path.display()
        );
    }
}

/// Every icon, the new dashboard and sidebar ones included.
const ALL_ICONS: [Icon; 52] = Icon::ALL;

fn widget_frame(theme: Theme) {
    let context = egui::Context::default();
    context.set_theme(theme);
    context.set_fonts(fonts::definitions(&FontReport::default()));
    let mut toasts = Toasts::<()>::default();
    toasts.push(
        Toast::new(Status::Ok, "Switch complete")
            .body("The new line is ready")
            .action("Details", ()),
    );
    toasts.push(Toast::new(Status::Error, "Retry needed").sticky());
    let input = egui::RawInput {
        // Keep every control in the clip rectangle so its painting code runs.
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 2400.0))),
        time: Some(1.25),
        ..egui::RawInput::default()
    };
    let output = context.run_ui(input, |ui| {
        let pal = Palette::of(ui.ctx());
        ui.set_width(1000.0);
        ui.horizontal(|ui| {
            assert_eq!(ui.add(Button::new("Secondary")).rect.height(), 32.0);
            assert_eq!(ui.add(Button::new("Small").small()).rect.height(), 28.0);
            assert_eq!(ui.add(Button::new("Large").large()).rect.height(), 40.0);
            ui.add(
                Button::primary("Switch")
                    .icon(Icon::Bolt)
                    .trailing(Icon::ArrowRight)
                    .kbd("Enter")
                    .min_width(120.0),
            );
            ui.add(Button::ghost("Cancel"));
            ui.add(Button::danger("Force"));
        });
        ui.horizontal(|ui| {
            let loading = ui.add(Button::primary("Loading").loading(true));
            assert!(!loading.sense.senses_click());
            let disabled = ui.add(Button::new("Disabled").enabled(false));
            assert!(!disabled.sense.senses_click());
            ui.add(IconButton::new(Icon::Refresh, "Refresh"));
            ui.add(
                IconButton::new(Icon::Close, "Disabled close")
                    .enabled(false)
                    .size(28.0),
            );
        });
        let mut selected = 0;
        let segmented = Segmented::new("smoke-segments", &mut selected)
            .option(0, "First")
            .option(1, "Second")
            .disabled_option(2, "Unavailable", "Unavailable in this fixture")
            .min_segment(80.0)
            .full_width()
            .show(ui);
        assert!(!segmented.changed());
        assert_eq!(selected, 0);
        Segmented::new("smoke-small-segments", &mut selected)
            .option(0, "First")
            .option(1, "Second")
            .small()
            .show(ui);

        let mut enabled = true;
        let mut disabled = false;
        ui.horizontal(|ui| {
            ui.add(Toggle::new(&mut enabled, "Switch together"));
            ui.add(Toggle::new(&mut disabled, "Disabled setting").enabled(false));
        });
        assert!(enabled);
        assert!(!disabled);
        let mut key = "test-key".to_owned();
        let mut revealed = false;
        KeyInput::new(
            "smoke-key",
            &mut key,
            &mut revealed,
            "Enter key",
            "Reveal key",
        )
        .width(360.0)
        .show(ui);
        let mut invalid_key = String::new();
        let mut invalid_revealed = true;
        KeyInput::new(
            "smoke-invalid-key",
            &mut invalid_key,
            &mut invalid_revealed,
            "Enter key",
            "Hide key",
        )
        .invalid(true)
        .width(360.0)
        .show(ui);
        assert_eq!(key, "test-key");
        assert!(!revealed);
        ui.horizontal(|ui| {
            ui.add(Badge::new("Available").status(Status::Ok));
            ui.add(Badge::new("0.162.0").mono().outline());
            let plain = ui.add(Badge::new("Routed").status(Status::Ok)).rect.width();
            let with_icon = ui
                .add(
                    Badge::new("Routed")
                        .icon(Icon::ArrowRightLeft)
                        .status(Status::Ok),
                )
                .rect
                .width();
            assert_eq!(
                with_icon,
                plain + 16.0,
                "a badge icon takes 12 px plus a gap"
            );
            for latency in [
                Latency::Probing,
                Latency::Ms(38),
                Latency::Ms(300),
                Latency::Ms(700),
                Latency::Unreachable,
                Latency::Unknown,
            ] {
                ui.add(LatencyBadge::new(latency));
            }
        });
        ui.add(CountdownBar::new(0.4));
        ui.add(CountdownBar::new(0.6).paused(true).height(5.0));
        for usage in [0.0, 0.5, 0.85, 0.98] {
            ui.add(Meter::new(usage));
        }
        ui.add(SplitBar::new(&[128, 46, 8]));
        ui.add(SplitBar::new(&[0, 0]));
        ui.horizontal(|ui| {
            for status in [
                Status::Ok,
                Status::Warn,
                Status::Error,
                Status::Idle,
                Status::Busy,
            ] {
                ui.add(StatusDot::new(status).size(10.0));
            }
        });
        let body = Card::new()
            .padding(12)
            .fill(pal.surface)
            .selected(true)
            .show(ui, |ui| {
                ui.label(Text::title("Card body", pal));
                42
            });
        assert_eq!(body.inner, 42);
        let mut footer_shown = false;
        let card_with_footer = Card::new().show_with_footer(
            ui,
            |ui| {
                ui.label(Text::body("Body with footer", pal));
                7
            },
            |ui| {
                footer_shown = true;
                ui.add(Button::new("Footer action").small());
            },
        );
        assert_eq!(card_with_footer.inner, 7);
        assert!(footer_shown);
        card(ui, |ui| {
            ui.label("Convenience card");
        });

        let table = Table::new(&[2.0, 1.0]).min_widths(&[120.0, 80.0]);
        table.header(ui, &["Target".into(), "Status".into()]);
        let mut cells = Vec::new();
        for last in [false, true] {
            let row = table.row(ui, 36.0, last, |ui, column| {
                cells.push(column);
                ui.label(if column == 0 { "Local" } else { "Ready" });
            });
            assert_eq!(row.rect.height(), 36.0);
        }
        assert_eq!(cells, [0, 1, 0, 1]);

        for text in [
            Text::h1("Page title", pal),
            Text::h2("Section", pal),
            Text::title("Title", pal),
            Text::body("Body", pal),
            Text::strong("Strong", pal),
            Text::muted("Muted", pal),
            Text::small("Small", pal),
            Text::caption("Caption", pal),
            Text::mono("Mono", pal),
            Text::mono_muted("Muted mono", pal),
            Text::display("128", pal),
        ] {
            ui.label(text);
        }
        ui.horizontal(|ui| {
            kbd(ui, "Esc");
            link_button(ui, "Details", false);
            link_button(ui, "External documentation", true);
        });
        ui.horizontal_wrapped(|ui| {
            for icon in ALL_ICONS {
                let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                assert!(
                    ui.is_rect_visible(rect),
                    "icon must be inside the test viewport"
                );
                icons::paint(ui.painter(), rect, icon, pal.text);
            }
            let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
            icons::spinner(ui.painter(), rect, pal.text, 0.25);
        });
        assert!(
            ui.min_rect().bottom() < ui.clip_rect().bottom(),
            "smoke frame must paint every widget"
        );
        assert_eq!(toasts.show(ui.ctx(), "Close"), None);
        assert_eq!(toasts.len(), 2);
        let modal = dialog(ui.ctx(), egui::Id::unique("smoke-dialog"), 320.0, |ui| {
            ui.label("Confirm the switch");
            23
        });
        assert_eq!(modal.inner, 23);
    });
    assert!(
        !output.shapes.is_empty(),
        "headless frame must produce paint commands"
    );
    output.drop_without_applying_deltas();
}

#[test]
fn every_widget_and_icon_renders_in_a_dark_headless_frame() {
    widget_frame(Theme::Dark);
}

#[test]
fn every_widget_and_icon_renders_in_a_light_headless_frame() {
    widget_frame(Theme::Light);
}

fn cjk_database(weights: &[u16]) -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_font_file(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Geist-Regular.ttf"))
        .unwrap();
    let template = db.faces().next().unwrap().clone();
    for weight in weights {
        let mut face = template.clone();
        face.families = vec![(
            "Noto Sans CJK SC".into(),
            fontdb::Language::English_UnitedStates,
        )];
        face.weight = fontdb::Weight(*weight);
        face.index = 0;
        db.push_face_info(face);
    }
    db
}

#[test]
fn cjk_medium_is_discovered_and_used_in_both_medium_chains() {
    for weights in [&[400, 500, 700][..], &[400, 700][..], &[400][..]] {
        let report = fonts::pick_cjk(&cjk_database(weights));
        assert_eq!(report.cjk_regular.as_ref().unwrap().weight, 400);
        let expected = if weights.contains(&500) {
            500
        } else if weights.contains(&700) {
            700
        } else {
            400
        };
        assert_eq!(report.medium_face().unwrap().weight, expected);
        assert_eq!(report.cjk_medium.is_some(), weights.contains(&500));
        let definitions = fonts::definitions(&report);
        let fallback = match expected {
            500 => "cjk-medium",
            700 => "cjk-bold",
            _ => "cjk-regular",
        };
        for family in [fonts::family::MEDIUM, fonts::family::MONO_MEDIUM] {
            assert_eq!(
                definitions.families[&FontFamily::Name(family.into())][1],
                fallback
            );
        }
    }
}

// --- v0.3 page layout and widgets (UX §2.3, §4) -------------------------------------

mod v3 {
    use super::*;
    use egui::Key;
    use pretty_assertions::assert_eq;
    use superba_core::SpeedTier;
    use superba_gui::design::layout::{self, Track};
    use superba_gui::design::tokens::size;
    use superba_gui::design::widgets::{
        Choice, ChoiceCard, LineTile, PagedState, PagedTable, Pager, PendingAction, PendingBar,
        PendingLabels, SpeedTiers, TierText, choice_cards, pager, rows_per_page, speed_tiers,
        tile_columns,
    };

    fn central(window: [f32; 2]) -> Rect {
        Rect::from_min_max(pos2(size::SIDEBAR, 0.0), pos2(window[0], window[1]))
    }

    #[test]
    fn page_rect_follows_the_ux_width_and_height_budget() {
        use superba_gui::design::tokens::chrome::{BUTTONS, Buttons};
        // Inside the content card (user bug 85): 28 px from the card's sides;
        // the body is 22 px lower where the card sits below the title band.
        let band = match BUTTONS {
            Buttons::TrafficLights => 0.0,
            Buttons::Caption | Buttons::Round => 22.0,
        };
        for (window, width, body) in [
            ([1280.0, 800.0], 994.0, 684.0 - band),
            ([1440.0, 900.0], 1154.0, 784.0 - band),
        ] {
            let fit = layout::page_rect(central(window), true);
            assert_eq!(fit.width(), width, "{window:?}");
            assert_eq!(fit.min.y, size::PAGE_TOP);
            assert_eq!(fit.height() - size::PAGE_HEADER, body, "{window:?}");
            let scroll = layout::page_rect(central(window), false);
            assert_eq!(scroll.width(), size::CONTENT_MAX);
            assert!(
                (scroll.center().x - fit.center().x).abs() < 0.01,
                "both centered"
            );
        }
        // Very wide windows cap the width; narrow ones keep 18 px gutters.
        assert_eq!(
            layout::page_rect(central([2560.0, 1440.0]), true).width(),
            size::CONTENT_WIDE
        );
        let narrow = layout::page_rect(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 600.0)),
            true,
        );
        assert_eq!(narrow.min.x, 18.0);
        assert_eq!(narrow.width(), 654.0);
        assert_eq!(size::DEFAULT_WINDOW, [1280.0, 800.0]);
        assert_eq!(size::MIN_WINDOW, [960.0, 640.0]);
        // The minimum window is below the Fit minimum: such a page scrolls as a whole.
        let min = layout::page_rect(central(size::MIN_WINDOW), true);
        assert!(min.height() < layout::FIT_MIN_HEIGHT);
        assert!(
            layout::page_rect(central([1280.0, 700.0]), true).height() >= layout::FIT_MIN_HEIGHT
        );
    }

    #[test]
    fn tracks_split_fixed_and_fill_sizes() {
        use Track::{Fill, Fixed};
        assert_eq!(
            layout::split(1000.0, &[Fixed(176.0), Fill, Fixed(372.0)], 16.0),
            vec![176.0, 420.0, 372.0]
        );
        assert_eq!(layout::split(100.0, &[Fill, Fill], 0.0), vec![50.0, 50.0]);
        assert_eq!(layout::split(100.0, &[Fixed(30.0)], 8.0), vec![30.0]);
        assert!(layout::split(100.0, &[], 8.0).is_empty());
        // Fixed tracks larger than the room shrink proportionally; fills get nothing.
        let s = layout::split(300.0, &[Fixed(200.0), Fill, Fixed(200.0)], 10.0);
        assert_eq!(s[1], 0.0);
        assert!((s[0] + s[2] - 280.0).abs() < 0.01 && (s[0] - s[2]).abs() < 0.01);
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(1000.0, 600.0));
        let cols = layout::column_rects(rect, &[Fixed(176.0), Fill, Fixed(372.0)], 16.0);
        assert_eq!(
            cols.iter().map(|r| (r.min.x, r.max.x)).collect::<Vec<_>>(),
            vec![(10.0, 186.0), (202.0, 622.0), (638.0, 1010.0)]
        );
        assert!(cols.iter().all(|r| r.min.y == 20.0 && r.height() == 600.0));
        let rows = layout::row_rects(rect, &[Fixed(32.0), Fill, Fixed(40.0)], 0.0);
        assert_eq!(
            rows.iter().map(|r| r.height()).collect::<Vec<_>>(),
            vec![32.0, 528.0, 40.0]
        );
        assert_eq!(rows[2].max.y, 620.0);
    }

    fn frame(theme: Theme, height: f32, add: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let context = egui::Context::default();
        context.set_theme(theme);
        context.set_fonts(fonts::definitions(&FontReport::default()));
        let mut add = add;
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, height))),
            ..egui::RawInput::default()
        };
        context
            .run_ui(input(), &mut add)
            .drop_without_applying_deltas();
        context.run_ui(input(), &mut add)
    }

    #[test]
    fn fit_area_gives_the_exact_rectangle_and_scrolls_only_below_its_minimum() {
        for (height, scrolled) in [(800.0, false), (500.0, true)] {
            let mut seen = None;
            let out = frame(Theme::Dark, height, |ui| {
                let avail = ui.available_rect_before_wrap();
                let fit = layout::fit_area(ui, 600.0, |ui| ui.max_rect());
                seen = Some((fit.scrolled, fit.inner, avail));
            });
            out.drop_without_applying_deltas();
            let (was_scrolled, inner, avail) = seen.unwrap();
            assert_eq!(was_scrolled, scrolled, "height {height}");
            if scrolled {
                assert_eq!(
                    inner.height(),
                    600.0,
                    "the minimum height inside the scroll area"
                );
            } else {
                assert_eq!(inner, avail, "exactly the available rectangle");
            }
        }
    }

    #[test]
    fn pager_counts_ranges_and_buttons() {
        assert_eq!(pager::page_count(0, 11), 1);
        assert_eq!(pager::page_count(340, 11), 31);
        assert_eq!(pager::page_count(11, 11), 1);
        assert_eq!(pager::item_range(0, 11, 340), (1, 11));
        assert_eq!(pager::item_range(30, 11, 340), (331, 340));
        assert_eq!(pager::item_range(99, 11, 340), (331, 340), "clamped");
        assert_eq!(pager::item_range(0, 11, 0), (0, 0));
        assert_eq!(pager::range_label(0, 11, 340), "1–11 / 340");
        assert_eq!(
            pager::page_buttons(0, 5),
            (0..5).map(Some).collect::<Vec<_>>()
        );
        assert_eq!(
            pager::page_buttons(0, 31),
            vec![Some(0), Some(1), Some(2), Some(3), None, Some(30)]
        );
        assert_eq!(
            pager::page_buttons(15, 31),
            vec![Some(0), None, Some(14), Some(15), Some(16), None, Some(30)]
        );
        assert_eq!(
            pager::page_buttons(30, 31),
            vec![Some(0), None, Some(27), Some(28), Some(29), Some(30)]
        );
    }

    #[test]
    fn paged_state_keeps_the_selection_on_screen_and_moves_with_keys() {
        assert_eq!(rows_per_page(678.0 - 28.0 - 40.0, size::ROW_TABLE2), 13);
        assert_eq!(rows_per_page(10.0, 32.0), 1);
        assert_eq!(rows_per_page(100.0, 0.0), 1);
        let total = 340;
        let mut s = PagedState::default();
        s.resize(11, total);
        assert_eq!((s.page, s.visible(total)), (0, 0..11));
        s.select(25, total);
        assert_eq!(s.page, 2);
        // A taller window: the page re-anchors on the selected row.
        s.resize(13, total);
        assert_eq!((s.page, s.selected), (1, Some(25)));
        assert!(s.visible(total).contains(&25));
        assert!(s.key(Key::ArrowDown, total));
        assert_eq!(s.selected, Some(26));
        s.key(Key::PageDown, total);
        assert_eq!((s.selected, s.page), (Some(39), 3));
        s.key(Key::Home, total);
        assert_eq!((s.selected, s.page), (Some(0), 0));
        s.key(Key::ArrowUp, total);
        assert_eq!(s.selected, Some(0), "stays at the top");
        s.key(Key::End, total);
        assert_eq!((s.selected, s.page), (Some(339), 26));
        assert!(!s.key(Key::A, total));
        // A filter shortens the list.
        s.clamp(20);
        assert_eq!((s.selected, s.page), (Some(19), 1));
        s.clamp(0);
        assert_eq!((s.selected, s.page), (None, 0));
        assert!(!s.key(Key::ArrowDown, 0));
    }

    #[test]
    fn speed_control_shows_standard_offered_and_current_tiers() {
        let shown = speed_tiers::shown_tiers(&[SpeedTier::Fast], SpeedTier::Standard);
        assert_eq!(
            shown,
            vec![(SpeedTier::Standard, true), (SpeedTier::Fast, true)]
        );
        // The profile's Ultrafast stays visible (disabled) when the list dropped it.
        let shown = speed_tiers::shown_tiers(&[], SpeedTier::Ultrafast);
        assert_eq!(
            shown,
            vec![(SpeedTier::Standard, true), (SpeedTier::Ultrafast, false)]
        );
        assert_eq!(tile_columns(900.0, 3), 3);
        assert_eq!(tile_columns(400.0, 3), 1);
    }

    fn widgets_frame(theme: Theme) {
        let mut state = PagedState::default();
        let mut page = 3;
        let mut tier = SpeedTier::Standard;
        let mut choice = "krill";
        let mut open = true;
        let mut per_page = 0;
        let out = frame(theme, 1600.0, |ui| {
            ui.set_width(900.0);
            // A 400 px table: (400 - 28 header - 40 footer) / 32 = 10 rows.
            let (rect, _) = ui.allocate_exact_size(vec2(900.0, 400.0), Sense::hover());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            let r = PagedTable::new("smoke-table", Table::new(&[2.0, 1.0]), 340, &mut state)
                .header(vec!["Name".into(), "Value".into()])
                .footer(|ui| {
                    let action = PendingBar::new(
                        3,
                        PendingLabels {
                            count: "3 unsaved changes".into(),
                            diff: "View diff".into(),
                            discard: "Discard".into(),
                            save: "Save & apply".into(),
                        },
                    )
                    .show(ui);
                    assert_eq!(action, PendingAction::None);
                })
                .show(&mut child, |ui, row, col| {
                    ui.label(format!("{row}:{col}"));
                });
            per_page = r.per_page;
            assert!(
                !Pager::new("smoke-pager", &mut page, 340, 11)
                    .show(ui)
                    .changed()
            );
            assert_eq!(
                PendingBar::new(0, PendingLabels::default()).show(ui),
                PendingAction::None
            );
            let items = [
                Choice::new("krill", "Krill"),
                Choice {
                    body: Some("Your own ChatGPT login".into()),
                    badge: Some("Default".into()),
                    ..Choice::new("official", "Official")
                },
                Choice::new("custom", "Compatible site"),
            ];
            assert!(!choice_cards(ui, &mut choice, &items));
            ui.add(ChoiceCard::new("Disabled", false).enabled(false));
            ui.add(
                LineTile::new("Overseas", "api.example.com", 240.0)
                    .selected(true)
                    .latency(superba_gui::design::widgets::Latency::Ms(38))
                    .caps("PAT and API key"),
            );
            ui.add(
                LineTile::new("CDN", "cdn.example.com", 240.0)
                    .pending(Some("Switching in 3 s".into())),
            );
            SpeedTiers::new(
                "smoke-speed",
                &mut tier,
                &[SpeedTier::Fast, SpeedTier::Ultrafast],
            )
            .text(
                SpeedTier::Fast,
                TierText {
                    label: "Fast".into(),
                    tooltip: Some("Billed at the provider's price".into()),
                },
            )
            .not_offered("No listed model offers this tier")
            .small()
            .show(ui);
            let (rect, _) = ui.allocate_exact_size(vec2(900.0, 200.0), Sense::hover());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            layout::SidePane::new("smoke-pane", 372.0)
                .dock_below(800.0)
                .show(
                    &mut child,
                    &mut open,
                    |ui| ui.label("main"),
                    |ui| {
                        ui.label("detail");
                    },
                );
        });
        assert_eq!(per_page, 10);
        assert_eq!(state.per_page, 10);
        assert!(!out.shapes.is_empty());
        out.drop_without_applying_deltas();
        assert!(open);
        assert_eq!(page, 3);
        assert_eq!(tier, SpeedTier::Standard);
    }

    #[test]
    fn v3_widgets_render_in_both_themes() {
        widgets_frame(Theme::Dark);
        widgets_frame(Theme::Light);
    }

    #[test]
    fn side_pane_docks_only_on_wide_content() {
        let pane = layout::SidePane::new("pane", 372.0);
        assert!(pane.docked(1156.0));
        assert!(
            !pane.docked(996.0),
            "1280x800 shows the detail as a sheet by default"
        );
        assert!(pane.clone().dock_below(900.0).docked(996.0));
    }
}

#[test]
fn hovered_links_underline_the_text_even_in_a_wide_justified_layout() {
    for width in [240.0, 800.0] {
        let ctx = egui::Context::default();
        let mut hit = Rect::NOTHING;
        for hovering in [false, true] {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 120.0))),
                ..Default::default()
            };
            if hovering {
                input.events.push(egui::Event::PointerMoved(hit.center()));
            }
            let output = ctx.run_ui(input, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                        ui.set_min_width(width - 16.0);
                        hit = link_button(ui, "Release notes", false).rect;
                    });
                });
            });
            if hovering {
                let underline = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::epaint::Shape::LineSegment { points, .. }
                            if points[0].y == points[1].y =>
                        {
                            Some(points)
                        }
                        _ => None,
                    })
                    .expect("hover underline");
                let text = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::epaint::Shape::Text(t) if t.galley.text() == "Release notes" => {
                            Some(t)
                        }
                        _ => None,
                    })
                    .expect("link text");
                assert!((underline[1].x - underline[0].x - text.galley.size().x).abs() < 1.0);
                assert!(
                    underline[1].x - underline[0].x < hit.width() - 20.0,
                    "underline={underline:?}, hit={hit:?}"
                );
            }
            output.drop_without_applying_deltas();
        }
    }
}
