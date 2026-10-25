#![allow(missing_docs)]
//! The 计时 (Timing) tab of a request (user bug 12): zero-length phases (the
//! first event read together with the response headers) are labelled and never
//! drawn over each other, marks out of order in old rows are raised instead of
//! going negative, WebSocket and error rows explain missing marks, and the speed
//! numbers are the row's own (the table's 首字 / 完成时间 / TPS).

mod router_harness;
mod support;

use std::sync::Arc;

use egui::{Rect, pos2, vec2};
use router_harness::{Frame, Harness};
use superba_core::{
    Locale, RequestDetail, RequestRow, RequestStatus, RequestTiming, RequestTransport, tps_milli,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::router::requests;
use superba_gui::pages::router::timing::{self, Mark, Missing};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

/// A finished desktop request of the fake log with `timing` and 287 output
/// tokens; TPS as the router computes it.
fn request(service: &FakeService, timing: RequestTiming) -> (RequestRow, RequestDetail) {
    let mut row = service
        .request_rows()
        .into_iter()
        .find(|r| r.status == RequestStatus::Ok && r.output_tokens.is_some())
        .unwrap();
    row.output_tokens = Some(287);
    row.ttfb_ms = timing.headers_ms;
    row.first_token_ms = timing.first_token_ms;
    row.duration_ms = timing.end_ms;
    row.tps_milli = tps_milli(
        287,
        timing.first_event_ms.unwrap_or(0),
        timing.end_ms.unwrap(),
    );
    let detail = RequestDetail {
        row: row.clone(),
        timing,
        attempts: Vec::new(),
        log: vec!["krill/overseas: HTTP 200 after 640 ms".into()],
    };
    (row, detail)
}

/// Headers, first event and first token in one read.
fn coalesced() -> RequestTiming {
    RequestTiming {
        sent_ms: Some(3),
        headers_ms: Some(640),
        first_event_ms: Some(640),
        first_token_ms: Some(640),
        last_delta_ms: Some(4210),
        end_ms: Some(4226),
    }
}

fn normal() -> RequestTiming {
    RequestTiming {
        sent_ms: Some(2),
        headers_ms: Some(520),
        first_event_ms: Some(548),
        first_token_ms: Some(1310),
        last_delta_ms: Some(5020),
        end_ms: Some(5041),
    }
}

#[test]
fn zero_length_phases_say_what_they_arrived_with() {
    let service = fake();
    let tr = Tr::new(Locale::ZhCn);
    let (row, d) = request(&service, coalesced());
    let steps = timing::steps(&row, Some(&d));
    assert_eq!(
        steps.iter().map(|s| s.mark).collect::<Vec<_>>(),
        Mark::ALL.to_vec()
    );
    let by = |m: Mark| steps.iter().find(|s| s.mark == m).unwrap();
    assert_eq!(by(Mark::FirstEvent).phase_ms(), Some(0));
    assert_eq!(by(Mark::FirstEvent).same_as(), Some(Mark::Headers));
    assert_eq!(by(Mark::FirstToken).same_as(), Some(Mark::FirstEvent));
    assert_eq!(by(Mark::Headers).phase_ms(), Some(637));
    assert_eq!(by(Mark::LastDelta).phase_ms(), Some(3570));
    assert_eq!(
        timing::phase_text(&tr, by(Mark::FirstEvent)),
        "+0 ms · 与响应头同时到达"
    );
    assert_eq!(
        timing::phase_text(&tr, by(Mark::FirstToken)),
        "+0 ms · 与首个事件同时到达"
    );
    assert_eq!(timing::phase_text(&tr, by(Mark::Headers)), "+637 ms");
    let en = Tr::new(Locale::En);
    assert_eq!(
        timing::phase_text(&en, by(Mark::FirstEvent)),
        "+0 ms · arrived with the headers"
    );
    // Every phase is >= 0 and the marks never go back.
    let at: Vec<u64> = steps.iter().filter_map(|s| s.at_ms).collect();
    assert!(at.windows(2).all(|w| w[0] <= w[1]));
    // A normal request has no such note.
    let (row, d) = request(&service, normal());
    for s in timing::steps(&row, Some(&d)) {
        assert!(s.same_as().is_none(), "{s:?}");
        assert!(!timing::phase_text(&tr, &s).contains("同时"));
    }
}

#[test]
fn old_rows_out_of_order_are_raised_not_negative() {
    let service = fake();
    // As a hand-made or earlier row could have it: no sent / last delta, the
    // first event and first token before the headers.
    let (row, d) = request(
        &service,
        RequestTiming {
            headers_ms: Some(700),
            first_event_ms: Some(660),
            first_token_ms: Some(650),
            end_ms: Some(4200),
            ..RequestTiming::default()
        },
    );
    let steps = timing::steps(&row, Some(&d));
    let by = |m: Mark| steps.iter().find(|s| s.mark == m).unwrap().clone();
    assert_eq!(by(Mark::Sent).at_ms, None);
    assert_eq!(by(Mark::Sent).missing, Some(Missing::Unknown));
    assert_eq!(by(Mark::FirstEvent).at_ms, Some(700));
    assert_eq!(by(Mark::FirstToken).at_ms, Some(700));
    assert_eq!(by(Mark::Headers).from, Some((Mark::Received, 0)));
    assert!(
        steps
            .iter()
            .all(|s| s.phase_ms().is_none_or(|p| p < 10_000))
    );
    // The row's own numbers fill in a detail without timing.
    let mut bare = d.clone();
    bare.timing = RequestTiming::default();
    let t = timing::timing_of(&row, Some(&bare));
    assert_eq!(t.headers_ms, row.ttfb_ms);
    assert_eq!(t.first_token_ms, Some(700));
    assert_eq!(t.end_ms, row.duration_ms);
}

#[test]
fn websocket_and_error_rows_explain_missing_marks() {
    let service = fake();
    let tr = Tr::new(Locale::ZhCn);
    let (mut row, mut d) = request(
        &service,
        RequestTiming {
            sent_ms: Some(0),
            first_event_ms: Some(410),
            first_token_ms: Some(430),
            last_delta_ms: Some(2100),
            end_ms: Some(2110),
            ..RequestTiming::default()
        },
    );
    row.transport = RequestTransport::WebSocket;
    // Earlier routers stored the first frame as the WebSocket TTFB.
    row.ttfb_ms = Some(410);
    d.row = row.clone();
    let steps = timing::steps(&row, Some(&d));
    let headers = steps.iter().find(|s| s.mark == Mark::Headers).unwrap();
    assert_eq!(headers.at_ms, None);
    assert_eq!(headers.missing, Some(Missing::WebSocketHeaders));
    assert_eq!(
        timing::phase_text(&tr, headers),
        "WebSocket 没有单独的响应头"
    );
    let first = steps.iter().find(|s| s.mark == Mark::FirstEvent).unwrap();
    assert_eq!(first.from, Some((Mark::Sent, 0)));
    // A 429 without events.
    let mut err = service
        .request_rows()
        .into_iter()
        .find(|r| matches!(r.status, RequestStatus::Http { .. }))
        .unwrap();
    err.first_token_ms = None;
    let d = RequestDetail {
        row: err.clone(),
        timing: RequestTiming {
            sent_ms: Some(1),
            headers_ms: Some(180),
            end_ms: Some(181),
            ..RequestTiming::default()
        },
        attempts: Vec::new(),
        log: Vec::new(),
    };
    let steps = timing::steps(&err, Some(&d));
    let first = steps.iter().find(|s| s.mark == Mark::FirstEvent).unwrap();
    assert_eq!(first.missing, Some(Missing::NoEvents));
    assert_eq!(timing::phase_text(&tr, first), "没有收到事件");
    let done = steps.iter().find(|s| s.mark == Mark::Done).unwrap();
    assert_eq!(done.from, Some((Mark::Headers, 180)));
    assert_eq!(timing::phase_text(&tr, done), "+1 ms");
    assert_eq!(timing::tps_note(&tr, &err, &d.timing), None);
}

#[test]
fn tps_dash_is_explained_and_the_window_shown() {
    let service = fake();
    let tr = Tr::new(Locale::ZhCn);
    let (row, d) = request(&service, coalesced());
    assert_eq!(row.tps_milli, Some(287 * 1_000_000 / 3586));
    assert_eq!(
        timing::tps_note(&tr, &row, &d.timing).unwrap(),
        "287 个输出 token，用时 3,586 ms（首个事件 → 完成）"
    );
    // The whole answer in one read: no window, TPS "—" with the reason.
    let (row, d) = request(
        &service,
        RequestTiming {
            sent_ms: Some(2),
            headers_ms: Some(640),
            first_event_ms: Some(640),
            first_token_ms: Some(641),
            last_delta_ms: Some(645),
            end_ms: Some(652),
        },
    );
    assert_eq!(row.tps_milli, None);
    assert_eq!(requests::tps(row.tps_milli), "—");
    assert_eq!(
        timing::tps_note(&tr, &row, &d.timing).unwrap(),
        "TPS 为 —：首个事件到完成只有 12 ms（不足 0.2 秒）"
    );
    let en = Tr::new(Locale::En);
    assert_eq!(
        timing::tps_note(&en, &row, &d.timing).unwrap(),
        "TPS is —: only 12 ms from the first event to completed (under 0.2 s)"
    );
}

/// Draw the tab in a rectangle the size of the detail pane body at 1280x800.
fn draw(h: &mut Harness, row: &RequestRow, d: &RequestDetail) -> (Frame, Rect) {
    let mut area = Rect::NOTHING;
    let f = h.frames(3, |ui, cx, _out| {
        let r = Rect::from_min_size(ui.available_rect_before_wrap().min, vec2(830.0, 170.0));
        area = r;
        superba_gui::design::layout::fixed(ui, r, |ui| {
            timing::show(ui, cx, row, Some(d), false);
        });
    });
    (f, area)
}

/// Texts drawn inside `area` that overlap another one.
fn overlaps(f: &Frame, area: Rect) -> Vec<(String, String)> {
    let texts: Vec<&(String, Rect)> = f
        .texts
        .iter()
        .filter(|(t, r)| !t.trim().is_empty() && area.expand(1.0).contains_rect(*r))
        .collect();
    let mut out = Vec::new();
    for (i, a) in texts.iter().enumerate() {
        for b in &texts[i + 1..] {
            let x = a.1.intersect(b.1);
            if x.is_positive() && x.area() > 1.0 {
                out.push((a.0.clone(), b.0.clone()));
            }
        }
    }
    out
}

#[test]
fn the_tab_draws_every_mark_on_its_own_line_without_overlaps() {
    let service = fake();
    for (locale, same, labels) in [
        (
            Locale::ZhCn,
            ["与响应头同时到达", "与首个事件同时到达"],
            [
                "收到请求",
                "发往上游",
                "上游响应头",
                "首个事件",
                "首字",
                "最后输出",
                "完成",
            ],
        ),
        (
            Locale::En,
            ["arrived with the headers", "arrived with the first event"],
            [
                "Received",
                "Sent upstream",
                "Upstream headers",
                "First event",
                "First token",
                "Last output",
                "Completed",
            ],
        ),
    ] {
        for dark in [false, true] {
            let mut h = Harness::new([1280.0, 800.0], locale, service.clone(), true);
            if dark {
                h = h.dark();
            }
            for timing in [coalesced(), normal()] {
                let coalesced = timing.first_event_ms == timing.headers_ms;
                let (row, d) = request(&service, timing);
                let (f, area) = draw(&mut h, &row, &d);
                let o = overlaps(&f, area);
                assert!(
                    o.is_empty(),
                    "{locale:?} dark={dark}: {o:?}\n{}",
                    f.all_text()
                );
                let mut ys = Vec::new();
                for l in labels {
                    let r = f
                        .find(l)
                        .unwrap_or_else(|| panic!("{l} missing:\n{}", f.all_text()));
                    ys.push(r.center().y);
                }
                // One line per mark, top to bottom in order.
                assert!(ys.windows(2).all(|w| w[1] - w[0] >= 14.0), "{ys:?}");
                for s in same {
                    assert_eq!(f.has(s), coalesced, "{locale:?} {s}");
                }
                // The numbers of the table and the stats line.
                assert!(f.has(&requests::seconds(row.first_token_ms)));
                assert!(f.has(&requests::seconds(row.duration_ms)));
                assert!(f.has(&format!("{} tok/s", requests::tps(row.tps_milli))));
                assert!(f.has("640 ms") || !coalesced);
                assert!(area.contains(pos2(area.min.x + 1.0, area.min.y + 1.0)));
            }
        }
    }
}

#[test]
fn attempts_are_localized_from_their_fields_and_the_raw_log_is_folded() {
    // Review ux-visual #9: "1. Overseas · 127.0.0.1:34469 · 200 · answered ·
    // 520 ms", "krill/overseas: HTTP 200 after 520 ms" and "stats line added"
    // (next to its localized note) in a Chinese pane.
    let service = fake();
    let (mut row, mut d) = request(&service, normal());
    row.injected = true;
    row.client = superba_core::RequestClient::Desktop;
    row.request_kind = Some(superba_core::RequestKind::Turn);
    d.row = row.clone();
    d.attempts = vec![
        superba_core::RequestAttempt {
            member_id: row.member_id.clone(),
            line_name: Some("国内极速".into()),
            upstream_host: Some("api-slb.kbsfdxc.net".into()),
            status: None,
            outcome: "connect failed: dns error".into(),
            elapsed_ms: Some(120),
            compression: None,
            upstream_protocol: None,
        },
        superba_core::RequestAttempt {
            member_id: row.member_id.clone(),
            line_name: row.line_name.clone(),
            upstream_host: Some("127.0.0.1:34469".into()),
            status: Some(200),
            outcome: "answered".into(),
            elapsed_ms: Some(520),
            compression: None,
            upstream_protocol: None,
        },
    ];
    d.log = vec![
        "krill/overseas: HTTP 200 after 520 ms".into(),
        "stats line added".into(),
    ];
    let zh = Tr::new(Locale::ZhCn);
    assert_eq!(timing::outcome_text(&zh, "answered"), "已应答");
    assert_eq!(
        timing::outcome_text(&zh, "connect failed: dns error"),
        "连接失败"
    );
    assert_eq!(timing::outcome_text(&zh, "something new"), "something new");
    assert_eq!(
        timing::raw_log(&d),
        vec!["krill/overseas: HTTP 200 after 520 ms"]
    );
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let (f, _) = draw(&mut h, &row, &d);
    let line = format!(
        "2. {} · 127.0.0.1:34469 · HTTP 200 · 已应答 · 520 ms",
        row.line_name.clone().unwrap()
    );
    assert!(f.has(&line), "{line}:\n{}", f.all_text());
    assert!(f.has("1. 国内极速 · api-slb.kbsfdxc.net · 连接失败 · 120 ms"));
    assert!(!f.has("answered"), "{}", f.all_text());
    // The raw log is folded away and never repeats the stats line note.
    assert!(f.has("路由原始日志（英文）"));
    assert!(!f.has("HTTP 200 after"), "{}", f.all_text());
    assert!(!f.has("stats line added"));
    assert!(f.has("这条回复已添加统计行"));
}

#[test]
fn the_speed_tier_has_its_own_line_by_name() {
    // Review ux-visual #9: "客户端 桌面 app · 请求 priority · 实际 priori…".
    let service = fake();
    let (mut row, _) = request(&service, normal());
    let zh = Tr::new(Locale::ZhCn);
    let tier = |row: &RequestRow| requests::tier_text(&zh, row);
    row.service_tier_requested = Some("priority".into());
    row.service_tier_answered = Some("priority".into());
    assert_eq!(tier(&row).as_deref(), Some("Fast"));
    row.service_tier_answered = Some("default".into());
    assert_eq!(tier(&row).as_deref(), Some("Fast · 实际 标准"));
    row.service_tier_requested = None;
    row.service_tier_answered = Some("flex".into());
    assert_eq!(tier(&row).as_deref(), Some("标准 · 实际 Flex"));
    // A plain Standard request has no tier line.
    row.service_tier_answered = Some("default".into());
    assert_eq!(tier(&row), None);
    // The pane: a line of its own in the speed card, never the wire value.
    let rows = service.request_rows();
    let fast = rows
        .iter()
        .find(|r| r.service_tier_requested.as_deref() == Some("priority"))
        .unwrap();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = superba_gui::pages::router::State::default();
    h.until(
        |ui, cx, out| superba_gui::pages::router::show_usage(ui, cx, &mut state, out),
        |f| f.has("Krill"),
    );
    state.recent.selected = Some(fast.id);
    let f = h.until(
        |ui, cx, out| superba_gui::pages::router::show_usage(ui, cx, &mut state, out),
        |f| f.has("速度档位"),
    );
    assert!(f.has("Fast"), "{}", f.all_text());
    assert!(!f.has("priority"), "{}", f.all_text());
}
