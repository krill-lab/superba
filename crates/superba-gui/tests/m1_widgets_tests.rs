#![allow(missing_docs)]
//! Interaction and accessibility of the shared section/update components.
use egui::accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use egui::{Context, Event, Id, Key, Modifiers, Pos2, Rect, Theme, Ui, vec2};
use std::collections::{HashMap, HashSet};
use superba_gui::design::{
    a11y, elide::Shape, fonts, icons::Icon, layout, style, tokens::size, widgets::*,
};

fn context(theme: Theme) -> Context {
    let ctx = Context::default();
    ctx.enable_accesskit();
    style::install(&ctx, Some(theme));
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    ctx
}
fn frame<R>(
    ctx: &Context,
    width: f32,
    events: Vec<Event>,
    mut show: impl FnMut(&mut Ui) -> R,
) -> (R, Vec<(NodeId, Node)>) {
    let mut result = None;
    let out = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
            events,
            ..Default::default()
        },
        |ui| result = Some(show(ui)),
    );
    let update = out.platform_output.accesskit_update.as_ref().unwrap();
    let by_id: HashMap<_, _> = update.nodes.iter().cloned().collect();
    let mut stack = vec![update.tree.as_ref().unwrap().root];
    let mut visited = HashSet::new();
    let mut nodes = Vec::new();
    while let Some(id) = stack.pop() {
        assert!(
            visited.insert(id),
            "a node appears twice in the tree: {id:?}"
        );
        let node = by_id.get(&id).expect("child must be in the tree");
        stack.extend(node.children().iter().rev());
        nodes.push((id, node.clone()));
    }
    for (id, node) in &update.nodes {
        if a11y::needs_name(node) {
            assert!(visited.contains(id), "unreachable {:?}", node.role());
        }
    }
    out.drop_without_applying_deltas();
    (result.unwrap(), nodes)
}
fn invoke(id: Id, action: Action) -> Event {
    Event::AccessKitActionRequest(ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: id.accesskit_id(),
        data: None,
    })
}
fn key(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}
fn update<'a>(state: &'a str, label: &'a str) -> UpdateRow<'a> {
    UpdateRow::new("claude", "Claude Code", state)
        .versions("2.1.284 → 2.1.295")
        .action(label)
        .status(Status::Warn)
        .tile(UpdateTile::Monogram("CC"))
}

#[test]
fn components_are_named_and_states_readable_in_both_languages_and_sizes() {
    for theme in [Theme::Light, Theme::Dark] {
        for zh in [false, true] {
            for width in [520.0, 1000.0] {
                let ctx = context(theme);
                let mut selected = 0;
                let mut expanded = false;
                let (updates, notify, state, label, busy, absent) = if zh {
                    ("更新", "通知", "有新版本", "更新", "下载中", "未安装")
                } else {
                    (
                        "Updates",
                        "Notifications",
                        "Update available",
                        "Update",
                        "Downloading",
                        "Not installed",
                    )
                };
                let mut show = |ui: &mut Ui| {
                    ui.set_max_width(width);
                    SubNav::new(&[
                        SubNavItem::new("updates", updates, Icon::Download).badge("2"),
                        SubNavItem::new("notify", notify, Icon::Bell).dot("Pending"),
                    ])
                    .show(ui, &mut selected);
                    update(state, label).show(ui, &mut expanded, |ui| {
                        ui.label("Details");
                    });
                    ui.add(Badge::new(busy).status(Status::Busy));
                    ui.add(Badge::new(absent).hollow());
                    ui.add(
                        InlineProgress::new(Some(0.5), "Download progress").total("50 / 100 MB"),
                    );
                    ui.add(MonogramTile::new("CC"));
                    ui.add(IconTile::new(Icon::Warning).warning());
                    InfoCard::fact("Version", "1.0")
                        .description("Installed")
                        .link("Details")
                        .show(ui);
                    ui.add(
                        ListRow::new(Icon::Tools, "Tool")
                            .subtitle("Connected")
                            .status(Status::Ok, "Ready")
                            .selected(true),
                    );
                    AttentionRow {
                        id: 0,
                        title: "Install hook",
                        detail: Some("Approval needed"),
                        label: "Install",
                        tip: "Install the hook",
                        icon: Icon::Bell,
                        color: superba_gui::design::Palette::of(ui.ctx()).text_muted,
                    }
                    .show(ui);
                };
                frame(&ctx, width, vec![], &mut show);
                let (_, nodes) = frame(&ctx, width, vec![], show);
                for (_, n) in &nodes {
                    assert!(
                        !a11y::needs_name(n) || a11y::is_named(n),
                        "unnamed {:?}",
                        n.role()
                    );
                }
                assert!(
                    nodes
                        .iter()
                        .any(|(_, n)| n.role() == Role::Tab && n.is_selected() == Some(true))
                );
                assert!(nodes.iter().any(|(_, n)| {
                    n.label()
                        .is_some_and(|s| s.contains(state) && s.contains("2.1.284 → 2.1.295"))
                        && n.is_expanded() == Some(false)
                }));
                assert!(
                    nodes
                        .iter()
                        .any(|(_, n)| n.label() == Some(format!("{label} Claude Code").as_str()))
                );
                for state in [busy, absent] {
                    assert!(
                        nodes
                            .iter()
                            .any(|(_, n)| n.value() == Some(state) || n.label() == Some(state))
                    );
                }
                assert!(
                    nodes
                        .iter()
                        .any(|(_, n)| n.value() == Some(busy) && n.is_busy())
                );
                assert!(nodes.iter().any(|(_, n)| {
                    n.label()
                        .is_some_and(|s| s.contains("Tool, Connected, Ready"))
                        && n.is_selected() == Some(true)
                }));
                assert!(nodes.iter().any(|(_, n)| n.role() == Role::Button
                    && n.label() == Some("Install")
                    && n.description() == Some("Install the hook")));
            }
        }
    }
}

#[test]
fn update_expands_and_collapses_through_uia_without_running_operation() {
    let ctx = context(Theme::Light);
    let mut expanded = false;
    let show = |ui: &mut Ui, expanded: &mut bool| {
        update("Update available", "Update").show(ui, expanded, |ui| {
            ui.label("Expanded details");
        })
    };
    let (r, _) = frame(&ctx, 900.0, vec![], |ui| show(ui, &mut expanded));
    let (r, nodes) = frame(
        &ctx,
        900.0,
        vec![invoke(r.response.id, Action::Expand)],
        |ui| show(ui, &mut expanded),
    );
    assert!(expanded);
    assert!(!r.action_clicked);
    assert!(
        nodes
            .iter()
            .any(|(_, n)| n.value() == Some("Expanded details"))
    );
    let (r, _) = frame(
        &ctx,
        900.0,
        vec![invoke(r.response.id, Action::Collapse)],
        |ui| show(ui, &mut expanded),
    );
    assert!(!expanded);
    assert!(!r.action_clicked);
    let action = r.action.unwrap();
    let (r, _) = frame(&ctx, 900.0, vec![invoke(action.id, Action::Click)], |ui| {
        show(ui, &mut expanded)
    });
    assert!(r.action_clicked);
    assert!(!expanded, "an operation must not also expand the row");
    let (r, _) = frame(
        &ctx,
        900.0,
        // UIA/keyboard invocation is independent of the current pointer position.
        vec![
            Event::PointerMoved(action.rect.center()),
            invoke(r.response.id, Action::Click),
        ],
        |ui| show(ui, &mut expanded),
    );
    assert!(expanded);
    assert!(!r.action_clicked);
}

#[test]
fn navigation_click_has_exactly_one_selected_tab_and_full_pending_description() {
    let ctx = context(Theme::Dark);
    let mut selected = 0;
    let items = [
        SubNavItem::new("home", "Superba", Icon::Superba).identity(),
        SubNavItem::new("notify", "Notifications", Icon::Bell)
            .badge("2")
            .dot("Hooks missing"),
    ];
    let (_, nodes) = frame(&ctx, size::RAIL_NARROW, vec![], |ui| {
        SubNav::new(&items).show(ui, &mut selected)
    });
    let target = nodes
        .iter()
        .find(|(_, n)| n.label() == Some("Notifications"))
        .unwrap()
        .0;
    let event = Event::AccessKitActionRequest(ActionRequest {
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: target,
        data: None,
    });
    let (r, nodes) = frame(&ctx, size::RAIL_NARROW, vec![event], |ui| {
        SubNav::new(&items).show(ui, &mut selected).unwrap()
    });
    assert_eq!(selected, 1);
    assert_eq!(r.id.accesskit_id(), target);
    let tabs: Vec<_> = nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::Tab)
        .collect();
    assert_eq!(
        tabs.iter()
            .filter(|(_, n)| n.is_selected() == Some(true))
            .count(),
        1
    );
    assert_eq!(tabs[1].1.label(), Some("Notifications"));
    assert_eq!(tabs[1].1.description(), Some("2, Hooks missing"));
    // No settling frame: arrows must work immediately after a focus request.
    ctx.memory_mut(|m| m.request_focus(r.id));
    let (r, _) = frame(&ctx, size::RAIL_NARROW, vec![key(Key::ArrowDown)], |ui| {
        SubNav::new(&items).show(ui, &mut selected).unwrap()
    });
    assert_eq!(selected, 0);
    assert!(ctx.memory(|m| m.has_focus(r.id)));
}

#[test]
fn disabled_update_cannot_expand_or_run_an_operation() {
    let ctx = context(Theme::Light);
    let mut expanded = false;
    let show = |ui: &mut Ui, expanded: &mut bool| {
        ui.add_enabled_ui(false, |ui| {
            update("Checking", "Update").show(ui, expanded, |_| {})
        })
        .inner
    };
    let (r, _) = frame(&ctx, 700.0, vec![], |ui| show(ui, &mut expanded));
    let operation = r.action.unwrap();
    let (r, nodes) = frame(
        &ctx,
        700.0,
        vec![
            invoke(r.response.id, Action::Expand),
            invoke(operation.id, Action::Click),
        ],
        |ui| show(ui, &mut expanded),
    );
    assert!(!expanded);
    assert!(!r.action_clicked);
    for id in [r.response.id, operation.id] {
        assert!(
            nodes
                .iter()
                .find(|(n, _)| *n == id.accesskit_id())
                .unwrap()
                .1
                .is_disabled()
        );
    }
}

#[test]
fn named_error_and_busy_rows_expose_state_and_operation_without_truncation() {
    let ctx = context(Theme::Light);
    let mut expanded = false;
    for width in [480.0, 699.0, 700.0, 1000.0] {
        let (r, nodes) = frame(&ctx, width, vec![], |ui| {
            UpdateRow::new("cc", "Claude Code", "更新失败")
                .status(Status::Error)
                .versions("2.1.284 → 2.1.295")
                .description("下载文件校验失败，请重试")
                .action("重试")
                .action_name("重试更新 Claude Code")
                .action_enabled(false)
                .show(ui, &mut expanded, |_| {})
        });
        let row = &nodes
            .iter()
            .find(|(id, _)| *id == r.response.id.accesskit_id())
            .unwrap()
            .1;
        assert_eq!(
            row.label(),
            Some("Claude Code, 更新失败, 2.1.284 → 2.1.295, 下载文件校验失败，请重试")
        );
        let operation = r.action.unwrap();
        let button = &nodes
            .iter()
            .find(|(id, _)| *id == operation.id.accesskit_id())
            .unwrap()
            .1;
        assert_eq!(button.label(), Some("重试更新 Claude Code"));
        assert!(button.is_disabled());
        assert!(r.response.rect.contains_rect(operation.rect));
    }
    let (r, nodes) = frame(&ctx, 500.0, vec![], |ui| {
        UpdateRow::new("cc", "Claude Code", "Downloading")
            .status(Status::Busy)
            .progress(None, "Downloading Claude Code")
            .show(ui, &mut expanded, |_| {})
    });
    assert!(
        nodes
            .iter()
            .find(|(id, _)| *id == r.response.id.accesskit_id())
            .unwrap()
            .1
            .is_busy()
    );
    assert!(
        nodes
            .iter()
            .any(|(_, n)| n.role() == Role::ProgressIndicator && n.numeric_value().is_none())
    );
}

#[test]
fn clicking_operation_with_mouse_does_not_expand_row() {
    let ctx = context(Theme::Light);
    let mut expanded = false;
    let (r, _) = frame(&ctx, 900.0, vec![], |ui| {
        update("Ready", "Install").show(ui, &mut expanded, |_| {})
    });
    let pos = r.action.unwrap().rect.center();
    for pressed in [true, false] {
        let (r, _) = frame(
            &ctx,
            900.0,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ],
            |ui| update("Ready", "Install").show(ui, &mut expanded, |_| {}),
        );
        if !pressed {
            assert!(r.action_clicked);
        }
    }
    assert!(!expanded);
}

#[test]
fn navigation_arrows_move_focus_with_selection_and_wrap() {
    let ctx = context(Theme::Light);
    let mut selected = 0;
    let items = [
        SubNavItem::new("home", "Superba", Icon::Superba).identity(),
        SubNavItem::new("updates", "Updates", Icon::Download),
        SubNavItem::new("notify", "Notifications", Icon::Bell).group("Preferences"),
    ];
    let (r, _) = frame(&ctx, 200.0, vec![], |ui| {
        SubNav::new(&items).show(ui, &mut selected).unwrap()
    });
    ctx.memory_mut(|m| m.request_focus(r.id));
    frame(&ctx, 200.0, vec![], |ui| {
        SubNav::new(&items).show(ui, &mut selected)
    });
    for (key, expected) in [
        (Key::ArrowDown, 1),
        (Key::ArrowDown, 2),
        (Key::ArrowDown, 0),
        (Key::ArrowUp, 2),
    ] {
        let (r, nodes) = frame(&ctx, 200.0, vec![self::key(key)], |ui| {
            SubNav::new(&items).show(ui, &mut selected).unwrap()
        });
        assert_eq!(selected, expected);
        assert!(
            ctx.memory(|m| m.has_focus(r.id)),
            "key={key:?}, selected={selected}, returned={:?}, focused={:?}",
            r.id,
            ctx.memory(|m| m.focused())
        );
        assert_eq!(
            nodes
                .iter()
                .filter(|(_, n)| n.role() == Role::Tab && n.is_selected() == Some(true))
                .count(),
            1
        );
    }
}

#[test]
fn progress_clamps_values_and_has_no_numeric_value_when_indeterminate() {
    let ctx = context(Theme::Dark);
    for (value, expected) in [
        (Some(-1.0), Some(0.0)),
        (Some(2.0), Some(1.0)),
        (Some(f32::NAN), None),
        (None, None),
    ] {
        let (r, nodes) = frame(&ctx, 300.0, vec![], |ui| {
            ui.add(InlineProgress::new(value, "Transfer"))
        });
        let n = &nodes
            .iter()
            .find(|(id, _)| *id == r.id.accesskit_id())
            .unwrap()
            .1;
        assert_eq!(n.numeric_value(), expected);
        assert_eq!(r.rect.height(), 3.0);
    }
}

#[test]
fn fact_explanation_wraps_inside_card_and_above_footer() {
    let explanation =
        "Codex configuration file backups, kept locally so you can restore your settings.";
    for theme in [Theme::Light, Theme::Dark] {
        let ctx = context(theme);
        for width in [160.0, 220.0, 360.0] {
            for footer in [false, true] {
                let mut card_rect = Rect::NOTHING;
                let mut footer_rect = Rect::NOTHING;
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 600.0))),
                        ..Default::default()
                    },
                    |ui| {
                        let start = ui.next_widget_position();
                        let mut card = InfoCard::fact("Backups", "12").description(explanation);
                        if footer {
                            card = card.link("Manage backups");
                        }
                        footer_rect = card.show(ui).rect;
                        card_rect = Rect::from_min_max(start, ui.min_rect().max);
                    },
                );
                let painted = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(t) if t.galley.text() == explanation => Some(t),
                        _ => None,
                    })
                    .expect("complete explanation is painted");
                assert!(!painted.galley.elided);
                assert!(painted.galley.rows.len() > 1);
                let bounds = painted.galley.rect.translate(painted.pos.to_vec2());
                assert!(
                    card_rect.shrink(11.0).contains_rect(bounds),
                    "{card_rect:?}: {bounds:?}"
                );
                if footer {
                    assert!(bounds.bottom() + 4.0 <= footer_rect.top());
                }
                output.drop_without_applying_deltas();
            }
        }
    }
}

#[test]
fn row_height_and_fact_card_geometry_match_advertised_dimensions() {
    let ctx = context(Theme::Light);
    let mut expanded = false;
    for width in [520.0, 1000.0] {
        let ((r, delta), nodes) = frame(&ctx, width, vec![], |ui| {
            let r =
                update("Ready", "Update")
                    .description("One line")
                    .show(ui, &mut expanded, |_| {});
            let start = ui.next_widget_position().y;
            InfoCard::fact(
                "Path",
                "/Users/test/Library/Application Support/Superba/cache",
            )
            .shape(Shape::Path)
            .description("Local data")
            .link("Open folder")
            .show(ui);
            (r, ui.min_rect().bottom() - start)
        });
        assert_eq!(r.response.rect.height(), UpdateRow::height(width));
        assert!((delta - 124.0).abs() < 1.0, "{delta}");
        assert!(nodes.iter().any(|(_, n)| {
            n.value()
                .is_some_and(|s| s.contains("Application Support/Superba/cache"))
        }));
    }
    assert_eq!(size::RAIL, 164.0);
    assert_eq!(size::RAIL_NARROW, 148.0);
    assert_eq!(Icon::ALL.len(), 53);
}

#[test]
fn rail_stays_fixed_while_only_detail_creates_a_scroll_area() {
    let ctx = context(Theme::Light);
    let ((left, right), _) = frame(&ctx, 900.0, vec![], |ui| {
        layout::rail_split(
            ui,
            size::RAIL,
            |ui| {
                ui.label("Fixed rail");
                ui.max_rect()
            },
            |ui| {
                let r = ui.max_rect();
                for _ in 0..100 {
                    ui.label("Scrollable row");
                }
                r
            },
        )
    });
    assert_eq!(left.width(), size::RAIL);
    assert!(right.left() > left.right());
    assert_eq!(
        ctx.data(|d| d.count::<egui::containers::scroll_area::State>()),
        1
    );
}

#[test]
fn detail_scrolls_without_moving_the_rail() {
    let ctx = context(Theme::Light);
    let show = |ui: &mut Ui| {
        layout::rail_split(
            ui,
            size::RAIL_NARROW,
            |ui| ui.label("Fixed rail").rect,
            |ui| {
                let first = ui.label("First detail").rect;
                for _ in 0..100 {
                    ui.label("Scrollable row");
                }
                first
            },
        )
    };
    let ((rail, first), _) = frame(&ctx, 700.0, vec![], show);
    frame(
        &ctx,
        700.0,
        vec![
            Event::PointerMoved(egui::pos2(500.0, 300.0)),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, -200.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Modifiers::NONE,
            },
        ],
        show,
    );
    let ((rail_after, first_after), _) = frame(&ctx, 700.0, vec![], show);
    assert_eq!(rail_after, rail);
    assert!(first_after.top() < first.top(), "detail did not scroll");
}
