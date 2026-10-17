//! The session migration scene and dialog: the SVG's keyframes evaluated on the
//! egui clock (values, seams), the split of the artwork into layers and texts,
//! progress in the flow, the reduced-motion frame, and repaints only while the
//! window is visible.

#![allow(missing_docs)]

mod support;

use std::time::Duration;

use egui::{Rect, ViewportId, ViewportInfo, pos2, vec2};
use superba_core::{
    AuthMode, Locale, LocalizedText, PlanDestination, RestartKind, SessionCurrent,
    SessionDirection, SessionProgress, SessionStage, StepStatus, SwitchEvent, SwitchPlan,
    SwitchRequest, SwitchStep, Timestamp,
};
use superba_gui::design::{fonts, style, tokens::Palette};
use superba_gui::flow::Flow;
use superba_gui::i18n::Tr;
use superba_gui::movers::dialog::{self, MoversDialog};
use superba_gui::movers::keyframes::{EASE_IN_OUT, EASE_OUT, Easing, Track};
use superba_gui::movers::pose::{self, CYCLE_SECS, Pose};
use superba_gui::movers::svg::{self, LayerKind};
use superba_gui::movers::{self as movers, render};
use superba_gui::pages::{Cx, Data};
use superba_gui::runtime::Command;

// --- keyframes -------------------------------------------------------------------

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn carry_cycle_positions_at_the_start_the_middle_and_between_keyframes() {
    // t = 0: standing at the left stack, facing right, the crate on the ground.
    let p = Pose::at(0.0);
    assert_eq!(p.travel_x, 0.0);
    assert_eq!(p.facing_sx, 1.0);
    assert_eq!((p.walking, p.standing), (0.0, 1.0));
    assert_eq!((p.cargo_opacity, p.cargo_y), (1.0, 0.0));
    assert_eq!((p.delivered, p.return_arm), (0.0, 0.0));
    assert!(close(p.progress_sx, 0.06));
    // 26 % (4.68 s): walking right, linearly between 8 % (0) and 46 % (370 px).
    let p = Pose::at(0.26 * f64::from(CYCLE_SECS));
    assert!(close(p.travel_x, 370.0 * 0.18 / 0.38), "{}", p.travel_x);
    assert_eq!((p.walking, p.standing), (1.0, 0.0));
    assert!(close(p.cargo_y, -16.0));
    // 50 % (9 s): set down at the shelf, the crate delivered, the arm returning.
    let p = Pose::at(9.0);
    assert!(close(p.travel_x, 370.0));
    assert_eq!(p.facing_sx, 1.0);
    assert_eq!(
        (p.cargo_opacity, p.delivered, p.return_arm),
        (0.0, 1.0, 1.0)
    );
    assert_eq!((p.walking, p.standing), (0.0, 1.0));
    assert!(close(p.progress_sx, 1.0));
    // 59 % turns round (steps(1, end) switches exactly at the keyframe).
    assert_eq!(Pose::at(0.589 * 18.0).facing_sx, 1.0);
    assert_eq!(Pose::at(0.59 * 18.0 + 1e-4).facing_sx, -1.0);
    // 76 %: walking back, linearly from 370 px (59 %) to 0 (92 %).
    let p = Pose::at(0.76 * 18.0);
    assert!(
        close(p.travel_x, 370.0 * (1.0 - 0.17 / 0.33)),
        "{}",
        p.travel_x
    );
    assert_eq!(p.facing_sx, -1.0);
    // 96 %: home again, facing right, a new crate ready.
    let p = Pose::at(0.961 * 18.0);
    assert_eq!(p.facing_sx, 1.0);
    assert_eq!((p.cargo_opacity, p.delivered), (1.0, 0.0));
    // 8 % -> 12 %: the crate is lifted 16 px, linearly.
    assert!(close(Pose::at(0.10 * 18.0).cargo_y, -8.0));
}

#[test]
fn small_loops_follow_their_easing_and_delays() {
    // bob: 0 -> -3 px over the first quarter of 1.2 s, ease-in-out.
    assert!(close(Pose::at(0.3).bob_y, -3.0));
    assert!(
        close(Pose::at(0.15).bob_y, -1.5),
        "ease-in-out is symmetric"
    );
    assert!(
        Pose::at(0.05).bob_y > -0.5 * 3.0 * (0.05 / 0.3) as f32,
        "eases in"
    );
    // legs swing in opposition.
    for t in [0.0, 0.2, 0.6, 0.9] {
        let p = Pose::at(t);
        assert!(close(p.leg_a, -p.leg_b));
    }
    assert!(close(Pose::at(0.6).leg_a, -14.0));
    // the second and third steam wisps wait 1.3 s and 2.6 s (base opacity 0).
    let p = Pose::at(1.0);
    assert!(p.steam[0].0 > 0.0);
    assert_eq!(p.steam[1], (0.0, 0.0));
    assert_eq!(p.steam[2], (0.0, 0.0));
    assert!(Pose::at(1.3 + 0.3).steam[1].0 > 0.0);
    // steam peaks at 20 % (0.55) and rises 46 px over 3.8 s, ease-out.
    assert!(close(Pose::at(0.76).steam[0].0, 0.55));
    assert!(close(Pose::at(3.8 - 1e-6).steam[0].1, -43.0));
    // the sip is held from 43 % to 60 % of 7 s.
    let p = Pose::at(0.5 * 7.0);
    assert_eq!(p.sip, (-21.0, -28.0, -12.0));
}

#[test]
fn every_loop_is_continuous_at_its_seam() {
    let tracks: Vec<(&str, Track)> = vec![
        ("travel", pose::TRAVEL_X),
        ("facing", pose::FACING_SX),
        ("walking", pose::WALKING),
        ("standing", pose::STANDING),
        ("cargo", pose::CARGO_OPACITY),
        ("cargo y", pose::CARGO_Y),
        ("delivered", pose::DELIVERED),
        ("progress", pose::PROGRESS_SX),
        ("bob", pose::BOB_Y),
        ("leg a", pose::LEG_A_DEG),
        ("leg b", pose::LEG_B_DEG),
        ("breathe y", pose::BREATHE_Y),
        ("breathe", pose::BREATHE_DEG),
        ("foot", pose::FOOT_DEG),
        ("sip x", pose::SIP_X),
        ("sip y", pose::SIP_Y),
        ("sip", pose::SIP_DEG),
        ("steam", pose::STEAM_OPACITY),
        ("pulse", pose::PULSE),
    ];
    for (name, t) in tracks {
        let d = f64::from(t.duration);
        for n in 1..4 {
            let seam = d * f64::from(n);
            let before = t.at(seam - 1e-6);
            let after = t.at(seam);
            assert!(
                (before - after).abs() < 1e-3,
                "{name} jumps at {seam} s: {before} -> {after}"
            );
            assert_eq!(t.at(seam), t.at(0.0), "{name} repeats");
        }
    }
    // The steam's rise restarts at 3 px below, but invisibly (opacity 0).
    assert!(pose::STEAM_OPACITY.at(3.8 - 1e-6) < 1e-3);
    // The whole carry cycle repeats every 18 s.
    for t in [0.0, 1.7, 9.0, 12.5, 17.99] {
        let (a, b) = (Pose::at(t), Pose::at(t + 18.0));
        assert!(close(a.travel_x, b.travel_x) && a.facing_sx == b.facing_sx);
        assert_eq!(
            (a.cargo_opacity, a.delivered, a.walking),
            (b.cargo_opacity, b.delivered, b.walking)
        );
    }
}

#[test]
fn easings_match_css() {
    assert!(
        close(EASE_IN_OUT.apply(0.25), 0.1292),
        "{}",
        EASE_IN_OUT.apply(0.25)
    );
    assert!(close(Easing::Linear.apply(0.3), 0.3));
    // ease-out = cubic-bezier(0, 0, .58, 1): 0.684 at half time.
    assert!(
        (EASE_OUT.apply(0.5) - 0.684).abs() < 2e-3,
        "{}",
        EASE_OUT.apply(0.5)
    );
    for e in [Easing::Linear, EASE_IN_OUT, EASE_OUT, Easing::StepEnd] {
        assert_eq!((e.apply(0.0), e.apply(1.0)), (0.0, 1.0));
    }
    assert_eq!(Easing::StepEnd.apply(0.5), 0.0);
    // Equal offsets (`0%,8%{…}`) hold the value.
    assert_eq!(pose::TRAVEL_X.at_progress(0.04), 0.0);
}

#[test]
fn the_still_frame_is_the_svgs_reduced_motion_frame() {
    let p = Pose::still();
    assert_eq!((p.travel_x, p.facing_sx, p.bob_y), (0.0, 1.0, 0.0));
    assert_eq!((p.walking, p.standing), (0.0, 1.0));
    assert_eq!((p.cargo_opacity, p.cargo_y), (1.0, 0.0));
    assert_eq!((p.delivered, p.return_arm), (0.0, 0.0));
    assert_eq!(p.steam, [(0.0, 0.0); 3]);
    assert_eq!(
        (p.breathe_deg, p.foot_deg, p.sip),
        (0.0, 0.0, (0.0, 0.0, 0.0))
    );
    assert_eq!((p.dot, p.progress_sx), (1.0, 1.0));
}

// --- the artwork ---------------------------------------------------------------------

#[test]
fn the_artwork_splits_into_layers_parts_and_texts() {
    let scene = render::scene().expect("the scene parses");
    assert_eq!(scene.size, (1440.0, 900.0));
    let kinds: Vec<&LayerKind> = scene.layers.iter().map(|l| &l.kind).collect();
    assert_eq!(kinds.first(), Some(&&LayerKind::Background));
    let pos = |k: LayerKind| kinds.iter().position(|x| **x == k).expect("layer");
    for part in [
        "dot",
        "delivered",
        "travel",
        "leg-a",
        "leg-b",
        "still-legs",
        "body-bob",
        "working-arm",
        "return-arm",
        "cargo",
        "idle-foot",
        "rest-body",
        "sip",
        "steam",
        "progress",
    ] {
        pos(LayerKind::Part(part));
    }
    // Groups without content of their own are not layers.
    assert!(!kinds.contains(&&LayerKind::Part("facing")));
    assert!(!kinds.contains(&&LayerKind::Part("walk-legs")));
    let steam = kinds
        .iter()
        .filter(|k| ***k == LayerKind::Part("steam"))
        .count();
    assert_eq!(steam, 3);
    // The krill's cup passes behind the side table; the bar is on the status line.
    let fg = pos(LayerKind::Foreground);
    assert!(pos(LayerKind::Part("sip")) < fg);
    assert!(pos(LayerKind::Part("progress")) > fg);
    // No layer keeps a text or the animations.
    for l in &scene.layers {
        assert!(!l.svg.contains("<text"), "{:?}", l.kind);
        assert!(!l.svg.contains("@keyframes"));
    }
    // 15 texts in the document, plus the label of each of the four crates.
    assert_eq!(scene.texts.len(), 19);
    assert_eq!(scene.texts.iter().filter(|t| t.text == "会话").count(), 4);
}

#[test]
fn texts_move_with_their_parts_and_every_one_is_translated() {
    let scene = render::scene().unwrap();
    let crates: Vec<_> = scene.texts.iter().filter(|t| t.text == "会话").collect();
    let carried = crates
        .iter()
        .find(|t| t.place.has("cargo"))
        .expect("the carried crate's label");
    let at = |t: f64| {
        let pose = Pose::at(t);
        let mut p = resvg::tiny_skia::Point::from_xy(carried.x, carried.y);
        carried.place.transform(&pose).map_point(&mut p);
        (p.x, p.y, carried.place.opacity(&pose))
    };
    assert_eq!(at(0.0), (235.0 + 105.0 + 83.0, 582.0 - 27.0 + 83.0, 1.0));
    let (x, y, _) = at(0.26 * 18.0);
    assert!(close(x, 423.0 + 370.0 * 0.18 / 0.38) && close(y, 638.0 - 16.0));
    assert_eq!(at(9.0).2, 0.0, "hidden once delivered");
    let delivered = crates.iter().find(|t| t.place.has("delivered")).unwrap();
    assert_eq!(delivered.place.opacity(&Pose::at(9.0)), 1.0);
    assert_eq!(delivered.place.opacity(&Pose::at(1.0)), 0.0);
    // The note is turned 6 degrees.
    let note = scene.texts.iter().find(|t| t.text == "先喝一口").unwrap();
    let m = note.place.transform(&Pose::at(0.0));
    assert!(close(m.ky.atan2(m.sx).to_degrees(), 6.0));
    let zh = Tr::new(Locale::ZhCn);
    let en = Tr::new(Locale::En);
    for t in &scene.texts {
        let Some(key) = render::text_key(&t.text) else {
            assert_eq!(t.text, "superba", "only the brand is not translated");
            continue;
        };
        assert_eq!(
            zh.t(key),
            t.text
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '…' && c != '。')
                .trim_end(),
            "the Chinese copy is the artwork's"
        );
        assert_ne!(en.t(key), key, "{key} is in the English catalog");
    }
}

#[test]
fn transform_attributes_parse() {
    let t = svg::parse_transform("translate(102 586) scale(.73)").unwrap();
    let mut p = resvg::tiny_skia::Point::from_xy(10.0, 10.0);
    t.map_point(&mut p);
    assert!(close(p.x, 102.0 + 7.3) && close(p.y, 586.0 + 7.3));
    let t = svg::parse_transform("translate(1195 397) rotate(6)").unwrap();
    assert!(close(t.ky.atan2(t.sx).to_degrees(), 6.0));
    assert!(svg::parse_transform("skewX(4)").is_none());
}

#[test]
fn the_port_composites_the_frames_it_should() {
    // A small CPU composite (the same layers and transforms the dialog paints).
    let a = render::composite(&Pose::at(0.0), 0.25).unwrap();
    let b = render::composite(&Pose::at(9.0), 0.25).unwrap();
    let still = render::composite(&Pose::still(), 0.25).unwrap();
    assert_eq!((a.width(), a.height()), (360, 225));
    assert_ne!(a.data(), b.data(), "the worker moved");
    // The paper is opaque and the corners outside its rounded rectangle clear.
    assert_eq!(a.pixel(180, 2).unwrap().alpha(), 255);
    assert_eq!(a.pixel(0, 0).unwrap().alpha(), 0);
    // The delivered crate (at 710 + 70, 555 + 60) shows only after the set-down.
    let shelf = |p: &resvg::tiny_skia::Pixmap| p.pixel(195, 154).unwrap();
    assert_ne!(shelf(&a), shelf(&b));
    assert_eq!(shelf(&a), shelf(&still));
}

// --- flow and dialog -------------------------------------------------------------------

fn plan() -> SwitchPlan {
    SwitchPlan {
        id: "movers-plan".into(),
        created_at: Timestamp::EPOCH,
        request: SwitchRequest::profile("test".into()),
        from: None,
        to: PlanDestination {
            profile_id: Some("test".into()),
            site_id: Some("test".into()),
            line_id: Some("test-line".into()),
            auth_mode: Some(AuthMode::Pat),
            label: LocalizedText::new("test.destination", "Test destination"),
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

/// A flow executing a plan.
fn running() -> Flow {
    let mut flow = Flow::new(0);
    let plan = plan();
    flow.request(plan.request.clone(), Duration::ZERO);
    let commands = flow.take_commands();
    let [Command::Plan { seq, .. }] = commands.as_slice() else {
        panic!("plan command");
    };
    flow.on_planned(*seq, Ok(plan), Duration::ZERO);
    flow.switch_now(Duration::ZERO);
    assert!(flow.is_running());
    flow.take_commands();
    flow
}

fn report(direction: SessionDirection, stage: SessionStage, done: u32) -> SwitchEvent {
    SwitchEvent::Sessions {
        progress: SessionProgress {
            direction,
            stage,
            done,
            total: 600,
            step_done: done,
            step_total: 600,
            current: Some(SessionCurrent {
                title: Some("Fix the login flow".into()),
                file: Some("rollout-2026-10-17T09-12-33-0199.jsonl".into()),
            }),
            can_stop: direction == SessionDirection::Migrate
                && matches!(
                    stage,
                    SessionStage::Reading | SessionStage::BackingUp | SessionStage::Moving
                ),
        },
    }
}

#[test]
fn progress_reports_open_and_close_the_dialog_and_stop_only_when_safe() {
    let mut flow = running();
    let now = Duration::from_secs(3);
    flow.on_event(
        &SwitchEvent::Step {
            step: SwitchStep::MigrateSessions,
            status: StepStatus::Running,
            at: Timestamp::EPOCH,
        },
        now,
    );
    assert!(flow.run().unwrap().sessions_active().is_none());
    flow.on_event(
        &report(SessionDirection::Migrate, SessionStage::Moving, 120),
        now,
    );
    let s = flow.run().unwrap().sessions_active().expect("dialog open");
    assert_eq!(
        (s.progress.done, s.progress.total, s.started),
        (120, 600, now)
    );
    // Stop: once, and only while the migration says it can.
    assert!(flow.stop_sessions());
    assert!(matches!(
        flow.take_commands().as_slice(),
        [Command::StopSessions]
    ));
    assert!(!flow.stop_sessions(), "already asked");
    // The undo keeps the dialog (and its clock) and cannot be stopped.
    let later = Duration::from_secs(9);
    flow.on_event(
        &report(SessionDirection::Restore, SessionStage::Moving, 10),
        later,
    );
    let s = flow.run().unwrap().sessions_active().unwrap();
    assert_eq!((s.started, s.stop_requested), (now, true));
    assert!(!s.can_stop());
    flow.on_event(
        &report(SessionDirection::Restore, SessionStage::Done, 600),
        later,
    );
    assert!(flow.run().unwrap().sessions_active().is_none());

    // A migration past its rewrite cannot be stopped.
    let mut flow = running();
    flow.on_event(
        &report(SessionDirection::Migrate, SessionStage::Databases, 600),
        now,
    );
    assert!(!flow.stop_sessions());
    assert!(flow.take_commands().is_empty());
    // The step ending closes the dialog even without a last report.
    flow.on_event(
        &SwitchEvent::Step {
            step: SwitchStep::MigrateSessions,
            status: StepStatus::Done,
            at: Timestamp::EPOCH,
        },
        now,
    );
    assert!(flow.run().unwrap().sessions_active().is_none());
}

#[test]
fn repaints_only_while_the_window_is_visible() {
    assert_eq!(movers::repaint_after(Some(false), false), None);
    assert_eq!(movers::repaint_after(Some(false), true), None);
    assert_eq!(
        movers::repaint_after(Some(true), false),
        Some(movers::FRAME)
    );
    assert_eq!(movers::repaint_after(None, false), Some(movers::FRAME));
    assert_eq!(
        movers::repaint_after(Some(true), true),
        Some(movers::CLOCK_TICK)
    );
}

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(egui::Theme::Light));
    ctx
}

/// One frame of the dialog at egui time `time`; returns the repaint delay the
/// frame asked for and the scene's mesh vertex positions.
fn dialog_frame(
    ctx: &egui::Context,
    flow: &mut Flow,
    state: &mut MoversDialog,
    time: f64,
    visible: bool,
    reduce_motion: bool,
) -> (Duration, Vec<egui::Pos2>, bool) {
    let (delay, points, shown, _) =
        dialog_frame_texts(ctx, flow, state, time, visible, reduce_motion);
    (delay, points, shown)
}

/// [`dialog_frame`] plus the texts the frame drew.
fn dialog_frame_texts(
    ctx: &egui::Context,
    flow: &mut Flow,
    state: &mut MoversDialog,
    time: f64,
    visible: bool,
    reduce_motion: bool,
) -> (Duration, Vec<egui::Pos2>, bool, Vec<String>) {
    let mut viewports = egui::ViewportIdMap::default();
    viewports.insert(
        ViewportId::ROOT,
        ViewportInfo {
            minimized: Some(!visible),
            occluded: Some(false),
            focused: Some(visible),
            ..ViewportInfo::default()
        },
    );
    let mut shown = false;
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
            time: Some(time),
            viewports,
            ..Default::default()
        },
        |ui| {
            let cx = Cx {
                tr: &Tr::new(Locale::En),
                pal: Palette::of(ui.ctx()),
                data: &Data::default(),
                busy: true,
                now: Timestamp::EPOCH,
                fonts: &fonts::FontReport::default(),
                service: &support::idle_service(),
                log: &[],
            };
            shown = dialog::show(
                ui.ctx(),
                &cx,
                flow,
                state,
                Duration::from_secs(5),
                reduce_motion,
            );
        },
    );
    let delay = output.viewport_output[&ViewportId::ROOT].repaint_delay;
    let mut points = Vec::new();
    let mut texts = Vec::new();
    for s in &output.shapes {
        match &s.shape {
            egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default() => {
                points.extend(m.vertices.iter().map(|v| v.pos));
            }
            egui::Shape::Text(t) => texts.push(t.galley.text().to_owned()),
            _ => {}
        }
    }
    output.drop_without_applying_deltas();
    (delay, points, shown, texts)
}

#[test]
fn the_dialogs_counter_names_the_stage_and_matches_the_percentage() {
    let ctx = context();
    let mut state = MoversDialog::default();
    for (stage, done, counter, percent) in [
        (
            SessionStage::Reading,
            200,
            "Reading 200 of 600 sessions",
            "· 11%",
        ),
        (
            SessionStage::BackingUp,
            40,
            "Backed up 40 of 600 files",
            "· 36%",
        ),
        (
            SessionStage::Moving,
            300,
            "Migrated 300 of 600 sessions",
            "· 75%",
        ),
    ] {
        let mut flow = running();
        flow.on_event(
            &report(SessionDirection::Migrate, stage, done),
            Duration::from_secs(1),
        );
        dialog_frame(&ctx, &mut flow, &mut state, 1.0, true, true);
        let (_, _, _, texts) = dialog_frame_texts(&ctx, &mut flow, &mut state, 2.0, true, true);
        assert!(texts.iter().any(|t| t == counter), "{counter} in {texts:?}");
        assert!(texts.iter().any(|t| t == percent), "{percent} in {texts:?}");
        // The stage is not repeated on the line below.
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("Now: Fix the login flow")),
            "{texts:?}"
        );
    }
}

#[test]
fn the_dialog_animates_while_visible_and_stands_still_with_reduced_motion() {
    let ctx = context();
    let mut flow = running();
    flow.on_event(
        &report(SessionDirection::Migrate, SessionStage::BackingUp, 40),
        Duration::from_secs(1),
    );
    let mut state = MoversDialog::default();
    // The modal's first frame only measures it.
    dialog_frame(&ctx, &mut flow, &mut state, 9.0, true, false);
    // Animated: the parts move between frames, a frame is asked for.
    let (delay, a, shown) = dialog_frame(&ctx, &mut flow, &mut state, 10.0, true, false);
    assert!(shown);
    assert!(delay <= movers::FRAME, "{delay:?}");
    let (_, b, _) = dialog_frame(&ctx, &mut flow, &mut state, 14.5, true, false);
    assert!(!a.is_empty());
    assert_ne!(a, b, "the scene moves");
    // Textures are rendered once, not per frame.
    assert_eq!(state.renders(), 1);
    // Reduced motion: the same still frame whatever the time, one repaint a
    // second for the elapsed time.
    let (delay, c, _) = dialog_frame(&ctx, &mut flow, &mut state, 20.0, true, true);
    let (_, d, _) = dialog_frame(&ctx, &mut flow, &mut state, 27.3, true, true);
    assert_eq!(c, d, "a still frame");
    assert!(
        delay > movers::FRAME && delay <= movers::CLOCK_TICK,
        "{delay:?}"
    );
    // Minimised: nothing is asked for.
    let (delay, _, _) = dialog_frame(&ctx, &mut flow, &mut state, 30.0, false, false);
    assert_eq!(delay, Duration::MAX, "no repaint while hidden");
    // Without sessions moving there is no dialog and no repaint either.
    let mut idle = running();
    let mut state = MoversDialog::default();
    let (delay, points, shown) = dialog_frame(&ctx, &mut idle, &mut state, 31.0, true, false);
    assert!(!shown && points.is_empty());
    assert_eq!(delay, Duration::MAX);
}
