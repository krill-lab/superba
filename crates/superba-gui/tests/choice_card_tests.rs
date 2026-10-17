#![allow(missing_docs)]

//! Choice cards (user bug 4): separate option cards, transparent until selected,
//! a neutral fill when selected, all four borders drawn on every card, arrow
//! keys / Space selecting inside the group, accessible radio buttons.

use egui::accesskit::{Role, Toggled};
use egui::{Color32, Event, Key, Rect, Shape, Theme, pos2, vec2};
use pretty_assertions::assert_eq;
use superba_gui::design::tokens::{DARK, LIGHT, contrast_ratio};
use superba_gui::design::widgets::choice_card::{GAP, RADIUS, card_style};
use superba_gui::design::widgets::{Choice, choice_cards};
use superba_gui::design::{fonts, style};

const WIDTH: f32 = 600.0;

fn items() -> Vec<Choice<&'static str>> {
    vec![
        Choice {
            body: Some("内置 3 条线路".into()),
            badge: Some("默认".into()),
            ..Choice::new("krill", "Krill")
        },
        Choice {
            body: Some("使用你自己的 ChatGPT 账号".into()),
            ..Choice::new("official", "官方 ChatGPT 登录")
        },
        Choice {
            enabled: false,
            ..Choice::new("disabled", "Disabled")
        },
        Choice::new("custom", "自定义 OpenAI 兼容站点"),
    ]
}

struct Group {
    ctx: egui::Context,
    value: &'static str,
    items: Vec<Choice<&'static str>>,
    changed: usize,
}

impl Group {
    fn new(theme: Theme) -> Self {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
        style::install(&ctx, Some(theme));
        ctx.enable_accesskit();
        Self {
            ctx,
            value: "krill",
            items: items(),
            changed: 0,
        }
    }

    fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                vec2(WIDTH + 40.0, 600.0),
            )),
            events,
            ..Default::default()
        };
        let (value, items, changed) = (&mut self.value, &self.items, &mut self.changed);
        self.ctx.run_ui(input, |ui| {
            ui.set_width(WIDTH);
            if choice_cards(ui, value, items) {
                *changed += 1;
            }
        })
    }

    fn settle(&mut self) -> egui::FullOutput {
        self.frame(Vec::new()).drop_without_applying_deltas();
        self.frame(Vec::new())
    }

    fn key(&mut self, key: Key) {
        let event = Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        self.frame(vec![event]).drop_without_applying_deltas();
    }

    /// The card rectangles (painted rounded rects as wide as the group).
    fn cards(out: &egui::FullOutput) -> Vec<egui::epaint::RectShape> {
        out.shapes
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Rect(r) if (r.rect.width() - WIDTH).abs() < 0.5 => Some(r.clone()),
                _ => None,
            })
            .collect()
    }

    /// Label, state and disabled flag of every radio button (AccessKit tree,
    /// sorted by label: the update lists nodes in no particular order).
    fn radio_nodes(out: &egui::FullOutput) -> Vec<(String, Toggled, bool)> {
        let update = out
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("accesskit on");
        let mut nodes: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::RadioButton)
            .map(|(_, n)| {
                (
                    n.label().unwrap_or_default().to_owned(),
                    n.toggled().expect("radio state"),
                    n.is_disabled(),
                )
            })
            .collect();
        nodes.sort_by(|a, b| a.0.cmp(&b.0));
        nodes
    }

    /// Label of the focused AccessKit node.
    fn focused_label(out: &egui::FullOutput) -> Option<String> {
        let update = out.platform_output.accesskit_update.as_ref()?;
        update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .and_then(|(_, n)| n.label().map(str::to_owned))
    }
}

#[test]
fn selected_fill_differs_from_the_transparent_unselected_card() {
    for pal in [&DARK, &LIGHT] {
        let (rest, rest_border) = card_style(pal, false, true, 0.0);
        let (hovered, hover_border) = card_style(pal, false, true, 1.0);
        let (selected, selected_border) = card_style(pal, true, true, 0.0);
        let (disabled, _) = card_style(pal, false, false, 1.0);
        assert_eq!(
            rest,
            Color32::TRANSPARENT,
            "unselected cards are transparent"
        );
        assert_eq!(disabled, Color32::TRANSPARENT, "disabled cards never hover");
        assert_eq!(
            selected, pal.hover,
            "the selected card has the neutral gray fill"
        );
        assert_ne!(hovered, rest, "hover shows");
        assert!(
            hovered.a() < selected.a(),
            "hover is fainter than the selected fill"
        );
        // Every state has a visible 1 px border; the selected one is stronger.
        for border in [rest_border, hover_border, selected_border] {
            assert_eq!(border.width, 1.0);
            assert_eq!(border.color.a(), 255);
        }
        assert_eq!(rest_border.color, pal.border);
        assert_eq!(selected_border.color, pal.border_strong);
        assert!(
            contrast_ratio(selected_border.color, pal.surface)
                > contrast_ratio(rest_border.color, pal.surface)
        );
        // The selected fill is a neutral gray.
        assert!(selected.r() == selected.g() && selected.g() == selected.b());
    }
}

#[test]
fn every_card_is_a_separate_rounded_rect_with_all_four_borders() {
    for (theme, pal) in [(Theme::Light, &LIGHT), (Theme::Dark, &DARK)] {
        let mut g = Group::new(theme);
        let out = g.settle();
        let cards = Group::cards(&out);
        assert_eq!(cards.len(), 4, "{theme:?}: one shape per card");
        for (i, card) in cards.iter().enumerate() {
            assert_eq!(
                card.corner_radius,
                egui::CornerRadius::same(RADIUS),
                "{theme:?}: card {i} rounds all four corners"
            );
            assert_eq!(card.stroke.width, 1.0, "{theme:?}: card {i} border");
            assert_ne!(card.stroke.color, Color32::TRANSPARENT);
            assert_eq!(card.stroke_kind, egui::StrokeKind::Inside);
        }
        // No shared edges: a gap between neighbours, so no card paints over the
        // selected card's bottom border.
        for pair in cards.windows(2) {
            let gap = pair[1].rect.min.y - pair[0].rect.max.y;
            assert!(gap >= GAP - 0.01, "{theme:?}: gap {gap}");
            assert!(!pair[0].rect.intersects(pair[1].rect.shrink(0.01)));
        }
        let fills: Vec<Color32> = cards.iter().map(|c| c.fill).collect();
        assert_eq!(
            fills,
            vec![
                pal.hover,
                Color32::TRANSPARENT,
                Color32::TRANSPARENT,
                Color32::TRANSPARENT
            ],
            "{theme:?}: only the selected card is filled"
        );
        assert_eq!(cards[0].stroke.color, pal.border_strong);
        assert_eq!(cards[1].stroke.color, pal.border);
        // Nothing wider than a card (e.g. the next card's fill) overlaps the
        // selected card's border.
        let sel = cards[0].rect;
        for s in &out.shapes {
            if let Shape::Rect(r) = &s.shape
                && r.rect != sel
                && r.rect.intersects(sel)
            {
                assert!(
                    sel.shrink(1.0).contains_rect(r.rect),
                    "{theme:?}: {:?} overlaps the selected card's border",
                    r.rect
                );
            }
        }
    }
}

#[test]
fn cards_are_accessible_radio_buttons_with_their_titles() {
    let mut g = Group::new(Theme::Light);
    let out = g.settle();
    assert_eq!(
        Group::radio_nodes(&out),
        vec![
            ("Disabled".to_owned(), Toggled::False, true),
            ("Krill".to_owned(), Toggled::True, false),
            ("官方 ChatGPT 登录".to_owned(), Toggled::False, false),
            ("自定义 OpenAI 兼容站点".to_owned(), Toggled::False, false),
        ]
    );
}

#[test]
fn arrow_keys_move_the_selection_and_the_focus_skipping_disabled_cards() {
    let mut g = Group::new(Theme::Dark);
    g.settle().drop_without_applying_deltas();
    // Tab focuses the first card; the selection does not change.
    g.key(Key::Tab);
    let out = g.settle();
    assert_eq!(Group::focused_label(&out).as_deref(), Some("Krill"));
    assert_eq!((g.value, g.changed), ("krill", 0));
    // Down selects the next card and keeps the focus on it.
    g.key(Key::ArrowDown);
    assert_eq!((g.value, g.changed), ("official", 1));
    let out = g.settle();
    assert_eq!(
        Group::focused_label(&out).as_deref(),
        Some("官方 ChatGPT 登录")
    );
    // Two presses in consecutive frames move by exactly two (egui's own arrow
    // navigation does not move the focus a second time).
    g.key(Key::ArrowDown);
    assert_eq!(g.value, "custom", "the disabled card is skipped");
    g.key(Key::ArrowRight);
    assert_eq!(g.value, "krill", "wraps to the first card");
    g.key(Key::ArrowUp);
    assert_eq!(g.value, "custom", "wraps to the last card");
    g.key(Key::ArrowLeft);
    assert_eq!(g.value, "official");
    let out = g.settle();
    assert_eq!(
        Group::focused_label(&out).as_deref(),
        Some("官方 ChatGPT 登录")
    );
    assert_eq!(g.changed, 5);
    // The selected card is drawn filled, the others transparent.
    let cards = Group::cards(&out);
    assert_eq!(cards[1].fill, DARK.hover);
    assert_eq!(cards[0].fill, Color32::TRANSPARENT);
}

#[test]
fn space_and_enter_select_the_focused_card() {
    for key in [Key::Space, Key::Enter] {
        let mut g = Group::new(Theme::Light);
        g.settle().drop_without_applying_deltas();
        // Tab twice: focus on the second card, selection still on the first.
        g.key(Key::Tab);
        g.settle().drop_without_applying_deltas();
        g.key(Key::Tab);
        let out = g.settle();
        assert_eq!(
            Group::focused_label(&out).as_deref(),
            Some("官方 ChatGPT 登录")
        );
        assert_eq!(g.value, "krill");
        g.key(key);
        g.settle().drop_without_applying_deltas();
        assert_eq!((g.value, g.changed), ("official", 1), "{key:?}");
    }
}

#[test]
fn no_keys_are_taken_without_focus() {
    let mut g = Group::new(Theme::Light);
    g.settle().drop_without_applying_deltas();
    g.key(Key::ArrowDown);
    g.key(Key::ArrowUp);
    assert_eq!((g.value, g.changed), ("krill", 0));
}

#[test]
fn a_card_around_custom_content_looks_like_the_option_cards() {
    // Review ux-visual #16 / user bug 4: the wizard's line tiles had a dark
    // border, a radio dot and a white fill instead of the option-card look.
    use superba_gui::design::widgets::choice_card::choice_card_ui;
    for (theme, pal) in [(Theme::Light, &LIGHT), (Theme::Dark, &DARK)] {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
        style::install(&ctx, Some(theme));
        ctx.enable_accesskit();
        let selected = std::cell::Cell::new(0usize);
        let run = |events: Vec<Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        pos2(0.0, 0.0),
                        vec2(WIDTH + 40.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_width(WIDTH);
                    ui.spacing_mut().item_spacing.y = GAP;
                    for (i, name) in ["海外线路", "国内极速"].into_iter().enumerate() {
                        let id = egui::Id::unique(("line", i));
                        let r = choice_card_ui(ui, id, name, selected.get() == i, true, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(name);
                                ui.label("98 ms");
                            });
                            ui.label("https://127.0.0.1");
                        });
                        if r.clicked() {
                            selected.set(i);
                        }
                    }
                },
            )
        };
        run(Vec::new()).drop_without_applying_deltas();
        let out = run(Vec::new());
        // The cards (an inner layout frame paints nothing: no fill, no border).
        let cards: Vec<_> = Group::cards(&out)
            .into_iter()
            .filter(|c| c.stroke.width > 0.0 || c.fill != Color32::TRANSPARENT)
            .collect();
        assert_eq!(cards.len(), 2, "{theme:?}: one shape per card");
        assert_eq!(cards[0].fill, pal.hover, "{theme:?}: selected = gray fill");
        assert_eq!(cards[1].fill, Color32::TRANSPARENT);
        assert_eq!(cards[0].stroke.color, pal.border_strong);
        assert_eq!(cards[1].stroke.color, pal.border);
        for c in &cards {
            assert_eq!(c.corner_radius, egui::CornerRadius::same(RADIUS));
            assert_eq!(c.stroke_kind, egui::StrokeKind::Inside);
        }
        // The card is drawn under its content: the text lies inside it.
        let texts: Vec<Rect> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Text(t) if t.galley.text() == "98 ms" => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                _ => None,
            })
            .collect();
        assert!(cards[0].rect.contains_rect(texts[0]), "{theme:?}");
        assert!(Group::radio_nodes(&out).contains(&("海外线路".into(), Toggled::True, false)));
        // A click on the second card selects it.
        let at = cards[1].rect.center();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run(vec![Event::PointerMoved(at), button(true)]).drop_without_applying_deltas();
        run(vec![button(false)]).drop_without_applying_deltas();
        assert_eq!(selected.get(), 1, "{theme:?}");
    }
}
