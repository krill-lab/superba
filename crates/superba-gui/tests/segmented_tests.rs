#![allow(missing_docs)]
//! Segmented controls (user bug 16): one bordered track, segments that share
//! their edges (no gaps), exactly one divider per shared edge and none next to
//! the selected segment; the Sessions host switcher (this computer, four hosts,
//! More) is one such control.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Rect, Shape, Theme, pos2, vec2};
use superba_core::{
    HostSource, Locale, RemoteHost, RemoteOverview, SshEndpoint, SwitchService, TargetId, Timestamp,
};
use superba_gui::design::tokens::{DARK, LIGHT, Palette};
use superba_gui::design::widgets::{SegmentLayout, Segmented, Status};
use superba_gui::design::{fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::sessions::{self, State};
use superba_gui::pages::{Cx, Data};
use support::fake::{FakeOptions, FakeService};

fn flatten(shape: Shape, out: &mut Vec<Shape>) {
    match shape {
        Shape::Vec(shapes) => shapes.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// Draw `build` in a headless frame; the shapes and the rect `build` returns.
fn frame(theme: Theme, mut build: impl FnMut(&mut egui::Ui) -> Rect) -> (Vec<Shape>, Rect) {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(theme));
    let mut rect = Rect::NOTHING;
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 400.0))),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        // A fractional origin: the segments' edges must still be whole points.
        ui.add_space(0.4);
        ui.horizontal(|ui| {
            ui.add_space(12.3);
            rect = build(ui);
        });
    });
    let mut shapes = Vec::new();
    for clipped in std::mem::take(&mut out.shapes) {
        flatten(clipped.shape, &mut shapes);
    }
    out.drop_without_applying_deltas();
    (shapes, rect)
}

/// Vertical 1 px lines in the divider colour inside `within`.
fn dividers(shapes: &[Shape], within: Rect, pal: &Palette) -> Vec<f32> {
    let mut xs: Vec<f32> = shapes
        .iter()
        .filter_map(|s| match s {
            Shape::LineSegment { points, stroke }
                if points[0].x == points[1].x
                    && stroke.width == 1.0
                    && stroke.color == pal.border
                    && within.contains(points[0])
                    && within.contains(points[1]) =>
            {
                Some(points[0].x)
            }
            _ => None,
        })
        .collect();
    xs.sort_by(f32::total_cmp);
    xs
}

/// Rectangles with a border inside `within`.
fn bordered(shapes: &[Shape], within: Rect) -> usize {
    shapes
        .iter()
        .filter(|s| matches!(s, Shape::Rect(r) if r.stroke.width > 0.0 && within.expand(0.5).contains_rect(r.rect)))
        .count()
}

fn texts(shapes: &[Shape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Text(t) => Some(t.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn segments_share_their_edges_on_whole_points_with_one_divider_per_edge() {
    let widths = [56.0, 75.0, 66.4, 90.2, 160.0, 71.7];
    let track = Rect::from_min_size(
        pos2(252.3, 120.6),
        vec2(widths.iter().sum::<f32>() + 6.0, 28.0),
    );
    let layout = SegmentLayout::new(track, &widths, 3.0);
    let n = widths.len();
    assert_eq!(layout.segments.len(), n);
    for pair in layout.segments.windows(2) {
        assert_eq!(pair[0].max.x, pair[1].min.x, "segments abut: {pair:?}");
        assert_eq!(
            (pair[0].min.y, pair[0].max.y),
            (pair[1].min.y, pair[1].max.y),
            "segments have one height"
        );
    }
    for seg in &layout.segments {
        assert_eq!(seg.min.x.fract(), 0.0, "edges on whole points: {seg:?}");
        assert_eq!(seg.max.x.fract(), 0.0, "edges on whole points: {seg:?}");
        assert!(seg.width() > 0.0);
        assert!(track.contains_rect(*seg));
    }
    // Rounding never accumulates: the last edge is where the widths end.
    let end = track.min.x + 3.0 + widths.iter().sum::<f32>();
    assert!((layout.segments[n - 1].max.x - end).abs() <= 0.5);

    let dividers = layout.dividers();
    assert_eq!(dividers.len(), n - 1, "one divider per shared edge");
    for (i, x) in dividers.iter().enumerate() {
        assert_eq!(*x, layout.segments[i].max.x);
        assert_eq!(*x, layout.segments[i + 1].min.x);
    }
    // None next to the selected segment.
    assert_eq!(layout.visible_dividers(None), dividers);
    assert_eq!(layout.visible_dividers(Some(0)), dividers[1..].to_vec());
    assert_eq!(
        layout.visible_dividers(Some(n - 1)),
        dividers[..n - 2].to_vec()
    );
    let mid = layout.visible_dividers(Some(2));
    assert_eq!(mid.len(), n - 3);
    assert!(!mid.contains(&dividers[1]) && !mid.contains(&dividers[2]));
}

#[test]
fn a_segmented_control_draws_one_border_and_one_divider_between_segments() {
    for (theme, pal) in [(Theme::Light, &LIGHT), (Theme::Dark, &DARK)] {
        let labels = ["24h", "7 days", "30 days", "All"];
        let n = labels.len();
        // (selected value, dividers drawn): none selected, the first, a middle one.
        for (selected, drawn) in [(99, n - 1), (0, n - 2), (1, n - 3), (3, n - 2)] {
            let mut value = selected;
            let (shapes, rect) = frame(theme, |ui| {
                let mut seg = Segmented::new(("range", selected), &mut value).small();
                for (i, l) in labels.iter().enumerate() {
                    seg = seg.option(i, *l);
                }
                seg.show(ui).rect
            });
            assert_eq!(rect.height(), 28.0);
            let xs = dividers(&shapes, rect, pal);
            assert_eq!(xs.len(), drawn, "{theme:?}, selected {selected}: {xs:?}");
            for x in &xs {
                assert_eq!(x.fract(), 0.0, "dividers on shared whole-point edges");
            }
            // The track's border, plus the indicator's when one is selected;
            // segments have no border of their own.
            let borders = bordered(&shapes, rect);
            assert_eq!(borders, if selected < n { 2 } else { 1 }, "{theme:?}");
        }
    }
}

#[test]
fn the_overflow_segment_joins_the_control_and_long_labels_are_elided() {
    let hosts = [
        "devbox",
        "gpu-01",
        "staging-eu",
        "build-farm-shanghai-03.internal",
    ];
    let rest = [("h5", "mac-mini"), ("h6", "lab-7")];
    for (value, drawn, shown) in [
        ("local", 4, "More"),
        ("gpu-01", 3, "More"),
        ("h5", 4, "mac-mini"),
    ] {
        let mut v = value.to_owned();
        let (shapes, rect) = frame(Theme::Light, |ui| {
            let mut seg = Segmented::new(("hosts", value), &mut v)
                .small()
                .max_segment(140.0)
                .option("local".to_owned(), "This computer");
            for h in hosts {
                seg = seg.option(h.to_owned(), h).dot(Status::Ok);
            }
            seg.overflow(
                "More",
                rest.iter()
                    .map(|(id, l)| ((*id).to_owned(), (*l).to_owned())),
            )
            .show(ui)
            .rect
        });
        // Six segments: this computer, four hosts and the overflow one.
        let xs = dividers(&shapes, rect, &LIGHT);
        assert_eq!(xs.len(), drawn, "{value}: {xs:?}");
        assert_eq!(bordered(&shapes, rect), 2, "{value}: track and indicator");
        let t = texts(&shapes);
        assert!(t.iter().any(|s| s == shown), "{value}: {t:?}");
        assert!(!t.iter().any(|s| s == "lab-7"), "menu closed: {t:?}");
        // The long name is elided to the longest segment (140 px less the
        // padding and the dot).
        let long = shapes
            .iter()
            .find_map(|s| match s {
                Shape::Text(t) if t.galley.text() == hosts[3] => Some(t.galley.clone()),
                _ => None,
            })
            .expect("long host");
        assert!(long.elided, "{value}");
        assert!(long.size().x <= 140.0 - 28.0 - 12.0, "{}", long.size().x);
    }
}

fn host(id: &str) -> RemoteOverview {
    RemoteOverview {
        host: RemoteHost {
            target_id: TargetId::remote(id),
            label: id.into(),
            host_id: None,
            kind: superba_core::HostKind::Ssh,
            ssh: SshEndpoint {
                alias: Some(id.into()),
                ..SshEndpoint::default()
            },
            sources: vec![HostSource::GlobalState],
            connected: Some(true),
        },
        state: None,
        error: None,
    }
}

/// The Sessions page with this computer, devbox, gpu-01 and `extra` hosts.
async fn sessions_shapes(extra: &[&str], locale: Locale) -> Vec<Shape> {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data.sessions.finish(service.sessions().await);
    for id in extra {
        data.overview.value.as_mut().unwrap().remotes.push(host(id));
    }
    let service: Arc<dyn SwitchService> = service;
    let ctx = egui::Context::default();
    let report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&report));
    style::install(&ctx, Some(Theme::Light));
    let tr = Tr::new(locale);
    let mut state = State::default();
    let mut actions = Vec::new();
    let mut shapes = Vec::new();
    let start = Instant::now();
    // A few frames so the page settles (fonts, first layout).
    while start.elapsed() < Duration::from_millis(300) || shapes.is_empty() {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1060.0, 760.0))),
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data: &data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &report,
                    service: &service,
                    log: &[],
                };
                sessions::show(ui, &cx, &mut state, &mut actions);
            },
        );
        shapes.clear();
        for clipped in std::mem::take(&mut out.shapes) {
            flatten(clipped.shape, &mut shapes);
        }
        out.drop_without_applying_deltas();
        std::thread::sleep(Duration::from_millis(20));
    }
    shapes
}

#[tokio::test]
async fn the_sessions_host_switcher_is_one_segmented_control() {
    for (extra, segments, local) in [
        (&[][..], 3, "本机"),
        (&["h3", "h4", "h5"][..], 6, "This computer"),
    ] {
        let locale = if local == "本机" {
            Locale::ZhCn
        } else {
            Locale::En
        };
        let shapes = sessions_shapes(extra, locale).await;
        // The switcher's band: the row of the "this computer" label.
        let label = shapes
            .iter()
            .find_map(|s| match s {
                Shape::Text(t) if t.galley.text() == local => Some(t.visual_bounding_rect()),
                _ => None,
            })
            .expect("this computer");
        // The track: the small (28 px) bordered rectangle around the label.
        let track = shapes
            .iter()
            .find_map(|s| match s {
                Shape::Rect(r)
                    if r.stroke.width > 0.0
                        && r.rect.height() == 28.0
                        && r.rect.contains_rect(label) =>
                {
                    Some(r.rect)
                }
                _ => None,
            })
            .expect("the switcher's track");
        // One track and the indicator under this computer; no host has a border
        // of its own.
        assert_eq!(bordered(&shapes, track), 2, "{local}");
        // One divider per shared edge, except the one next to this computer.
        let xs = dividers(&shapes, track, &LIGHT);
        assert_eq!(xs.len(), segments - 2, "{local}: {xs:?}");
        let t = texts(&shapes);
        if segments == 6 {
            assert!(t.iter().any(|s| s == "More"), "{t:?}");
            assert!(!t.iter().any(|s| s == "h5"), "{t:?}");
        }
    }
}
