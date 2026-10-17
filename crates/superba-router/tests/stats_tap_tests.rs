//! The SSE splitter and the response tap (spike RS 4.8): event boundaries at
//! every byte position, zero-copy pass mode, the hold / inject state machine on
//! the captured response shapes, and the text invariant Codex relies on.

mod common;

use std::time::{Duration, Instant};

use bytes::Bytes;
use common::responses::*;
use serde_json::json;
use superba_core::{Multiplier, PriceEntry, StatsLineStyle, Usd};
use superba_router::stats::inject::{LineValues, format_usd, render_suffix, render_text};
use superba_router::stats::sse::{Boundary, EventKind, MAX_EVENT_BYTES, event_kind, parse_event};
use superba_router::stats::tap::{
    Decision, LineSetup, Outcome, ResponseTap, SseTap, TurnSoFar, group_digits,
};

const TEMPLATE: &str = "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out";

fn setup(turn: TurnSoFar) -> LineSetup {
    LineSetup {
        template: TEMPLATE.into(),
        style: StatsLineStyle::Quote,
        turn,
        price: None,
        ratio: Multiplier::ONE,
    }
}

/// Run `chunks` through a tap; every event is seen at `t0 + 820 ms`.
fn run(chunks: &[Bytes], line: Option<LineSetup>) -> (Vec<Bytes>, ResponseTap) {
    let t0 = Instant::now();
    let now = t0 + Duration::from_millis(820);
    let mut tap = SseTap::new(ResponseTap::new(t0, line));
    let mut out = Vec::new();
    for c in chunks {
        tap.push(c.clone(), now, &mut out);
    }
    tap.finish(&mut out);
    (out, tap.into_tap())
}

fn joined(out: &[Bytes]) -> Vec<u8> {
    out.iter().flat_map(|b| b.iter().copied()).collect()
}

fn one(s: &str) -> Vec<Bytes> {
    vec![Bytes::copy_from_slice(s.as_bytes())]
}

const LINE: &str = "\n\n> ⚡\u{2060} — tok/s · 首字 0.82s · 本轮 4,002 in / 51 out";

#[test]
fn boundaries_are_found_at_every_split_position() {
    let stream =
        "event: a\ndata: 1\n\nevent: b\r\ndata: 2\r\n\r\ndata: 3\n\n: comment\n\ndata: x\n\r\n";
    let bytes = stream.as_bytes();
    let reference = {
        let mut b = Boundary::default();
        let mut ends = Vec::new();
        let mut pos = 0;
        while let Some(e) = b.find(bytes, pos) {
            ends.push(e);
            pos = e;
        }
        ends
    };
    assert_eq!(reference.len(), 5, "five events");
    for k in 0..=bytes.len() {
        let (a, c) = bytes.split_at(k);
        let mut b = Boundary::default();
        let mut ends = Vec::new();
        let mut pos = 0;
        while let Some(e) = b.find(a, pos) {
            ends.push(e);
            pos = e;
        }
        let mut pos = 0;
        while let Some(e) = b.find(c, pos) {
            ends.push(k + e);
            pos = e;
        }
        assert_eq!(ends, reference, "split at {k}");
    }
}

#[test]
fn events_are_parsed_and_classified() {
    let e = parse_event(b"event: response.output_text.delta\ndata: {\"type\":\"x\"}\n\n");
    assert_eq!(e.name, Some("response.output_text.delta"));
    assert_eq!(event_kind(&e), EventKind::TextDelta);
    let e = parse_event(b"data: {\"type\" : \"response.completed\",\"a\":1}\r\n\r\n");
    assert_eq!(event_kind(&e), EventKind::Completed);
    let e = parse_event(b"data: a\ndata: b\n\n");
    assert_eq!(&e.data[..], b"a\nb");
    assert_eq!(
        event_kind(&parse_event(b"data: [DONE]\n\n")),
        EventKind::Other
    );
    assert_eq!(
        EventKind::of("response.reasoning_summary_text.delta"),
        EventKind::OutputDelta
    );
    // The top-level type, wherever it is (a WebSocket frame has no event line).
    use superba_router::stats::sse::sniff_type;
    assert_eq!(
        sniff_type(
            br#"{"item":{"type":"message","x":["type"]},"type":"response.output_item.added"}"#
        ),
        Some("response.output_item.added")
    );
    assert_eq!(sniff_type(br#" {"type" : "error"}"#), Some("error"));
    assert_eq!(sniff_type(br#"{"item":{"type":"message"}}"#), None);
    assert_eq!(sniff_type(br#"{"a":"type","b":1}"#), None);
    assert_eq!(sniff_type(b"[DONE]"), None);
}

#[test]
fn a_final_answer_gets_the_line_with_identical_text_everywhere() {
    let input = sse(&text_answer(), false);
    let (out, tap) = run(&one(&input), Some(setup(TurnSoFar::default())));
    let events = parse_sse(&joined(&out));
    let text = consistent_text(&events, "msg_1");
    assert_eq!(text, format!("Hello world.{LINE}"));
    let s = tap.summary();
    assert!(s.injected);
    assert_eq!(s.outcome, Some(Outcome::Completed));
    let u = s.usage.unwrap();
    assert_eq!((u.input, u.cached, u.output), (4002, Some(3328), 51));
    assert_eq!(s.response_model.as_deref(), Some("gpt-x-2026"));
    assert_eq!(s.first_token, Some(Duration::from_millis(820)));
    // One synthetic delta, every other event still there in order.
    assert_eq!(events.len(), text_answer().len() + 1);
}

#[test]
fn the_line_carries_the_turn_totals() {
    let input = sse(&text_answer(), false);
    let turn = TurnSoFar {
        input: 10_000,
        output: 200,
        cached: 9_000,
        requests: 2,
        cost: None,
    };
    let (out, _) = run(&one(&input), Some(setup(turn)));
    let text = consistent_text(&parse_sse(&joined(&out)), "msg_1");
    assert!(
        text.ends_with("本轮 14,002 in / 251 out"),
        "turn totals: {text:?}"
    );
}

#[test]
fn every_split_position_and_crlf_give_the_same_output() {
    for crlf in [false, true] {
        let input = sse(&text_answer(), crlf);
        let bytes = Bytes::from(input.clone());
        let (reference, _) = run(
            std::slice::from_ref(&bytes),
            Some(setup(TurnSoFar::default())),
        );
        let reference = joined(&reference);
        let text = consistent_text(&parse_sse(&reference), "msg_1");
        assert!(text.ends_with(LINE.trim_start_matches('\n')));
        for k in 0..=bytes.len() {
            let chunks = [bytes.slice(..k), bytes.slice(k..)];
            let (out, tap) = run(&chunks, Some(setup(TurnSoFar::default())));
            assert_eq!(joined(&out), reference, "split at {k} (crlf {crlf})");
            assert!(tap.summary().injected);
        }
        // One byte per chunk.
        let chunks: Vec<Bytes> = (0..bytes.len()).map(|i| bytes.slice(i..i + 1)).collect();
        let (out, _) = run(&chunks, Some(setup(TurnSoFar::default())));
        assert_eq!(joined(&out), reference, "byte by byte (crlf {crlf})");
    }
}

#[test]
fn reasoning_then_text_is_injected_after_the_text() {
    let events = response(
        &[
            Item::Reasoning { id: "rs_1" },
            Item::Message {
                id: "msg_1",
                phase: Some("final_answer"),
                deltas: vec!["Done."],
            },
        ],
        Ending::Completed,
        usage(100, 0, 40),
    );
    let (out, tap) = run(
        &one(&sse(&events, false)),
        Some(setup(TurnSoFar::default())),
    );
    let parsed = parse_sse(&joined(&out));
    assert!(consistent_text(&parsed, "msg_1").starts_with("Done.\n\n> ⚡\u{2060}"));
    // The reasoning item arrives whole: it is the first output.
    assert!(tap.summary().first_token.is_some());
    // The encrypted reasoning item is untouched.
    let rs: Vec<_> = parsed
        .iter()
        .filter(|e| e["item"]["id"] == "rs_1")
        .collect();
    assert_eq!(rs.len(), 2);
    assert_eq!(rs[1]["item"]["encrypted_content"], "gAAAA");
}

fn untouched(events: &[Ev], line: Option<LineSetup>) -> ResponseTap {
    let input = sse(events, false);
    let (out, tap) = run(&one(&input), line);
    assert_eq!(String::from_utf8(joined(&out)).unwrap(), input);
    assert!(!tap.summary().injected);
    tap
}

#[test]
fn commentary_tool_calls_failures_and_cut_streams_pass_unchanged() {
    let line = || Some(setup(TurnSoFar::default()));
    // A preamble followed by a tool call: not the final answer.
    untouched(
        &response(
            &[
                Item::Message {
                    id: "msg_c",
                    phase: Some("commentary"),
                    deltas: vec!["Let me look."],
                },
                Item::Tool { id: "fc_1" },
            ],
            Ending::Completed,
            usage(500, 0, 30),
        ),
        line(),
    );
    // A message without phase followed by a tool call: released when the tool starts.
    untouched(
        &response(
            &[
                Item::Message {
                    id: "msg_n",
                    phase: None,
                    deltas: vec!["Checking."],
                },
                Item::Tool { id: "fc_1" },
            ],
            Ending::Completed,
            usage(500, 0, 30),
        ),
        line(),
    );
    // Tool only.
    let tap = untouched(
        &response(
            &[Item::Tool { id: "fc_1" }],
            Ending::Completed,
            usage(500, 0, 30),
        ),
        line(),
    );
    assert_eq!(tap.summary().outcome, Some(Outcome::Completed));
    // Failed.
    let tap = untouched(
        &response(
            &[Item::Message {
                id: "msg_1",
                phase: Some("final_answer"),
                deltas: vec!["partial"],
            }],
            Ending::Failed,
            json!(null),
        ),
        line(),
    );
    assert_eq!(
        tap.summary().outcome,
        Some(Outcome::Failed {
            code: Some("server_error".into())
        })
    );
    assert_eq!(tap.summary().usage, None);
    // Incomplete.
    let tap = untouched(
        &response(
            &[Item::Message {
                id: "msg_1",
                phase: Some("final_answer"),
                deltas: vec!["partial"],
            }],
            Ending::Incomplete,
            usage(10, 0, 5),
        ),
        line(),
    );
    assert_eq!(
        tap.summary().outcome,
        Some(Outcome::Incomplete {
            reason: Some("max_output_tokens".into())
        })
    );
    // Cut before response.completed (Codex cancelled, upstream broke): the held
    // tail goes out unchanged at the end.
    let tap = untouched(
        &response(
            &[Item::Message {
                id: "msg_1",
                phase: Some("final_answer"),
                deltas: vec!["almost"],
            }],
            Ending::Cut,
            json!(null),
        ),
        line(),
    );
    assert_eq!(tap.summary().outcome, None);
    // No line configured for the request: never touched.
    untouched(&text_answer(), None);
}

#[test]
fn with_two_messages_only_the_last_one_gets_the_line() {
    let events = response(
        &[
            Item::Message {
                id: "msg_a",
                phase: None,
                deltas: vec!["First."],
            },
            Item::Message {
                id: "msg_b",
                phase: Some("final_answer"),
                deltas: vec!["Second."],
            },
        ],
        Ending::Completed,
        usage(100, 0, 20),
    );
    let (out, tap) = run(
        &one(&sse(&events, false)),
        Some(setup(TurnSoFar::default())),
    );
    let parsed = parse_sse(&joined(&out));
    assert_eq!(consistent_text(&parsed, "msg_a"), "First.");
    assert!(consistent_text(&parsed, "msg_b").starts_with("Second.\n\n> ⚡\u{2060}"));
    assert!(tap.summary().injected);
}

#[test]
fn pass_mode_forwards_the_same_bytes_without_copying() {
    let events = text_answer();
    let chunks: Vec<Bytes> = events
        .iter()
        .map(|e| Bytes::from(sse(std::slice::from_ref(e), false)))
        .collect();
    // Observer (no line): every chunk goes out as the very same allocation.
    let t0 = Instant::now();
    let mut tap = SseTap::new(ResponseTap::new(t0, None));
    for c in &chunks {
        let mut out = Vec::new();
        tap.push(c.clone(), t0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ptr(), c.as_ptr(), "no copy");
        assert_eq!(out[0].len(), c.len());
    }
    // With a line: every chunk before the candidate also passes without a copy.
    let mut tap = SseTap::new(ResponseTap::new(t0, Some(setup(TurnSoFar::default()))));
    for c in &chunks {
        if parse_event(c).name == Some("response.output_text.done") {
            break;
        }
        let mut out = Vec::new();
        tap.push(c.clone(), t0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ptr(), c.as_ptr(), "no copy before the candidate");
    }
}

#[test]
fn an_event_larger_than_the_inspection_cap_passes_uninspected() {
    let mut events = text_answer();
    let big = "x".repeat(MAX_EVENT_BYTES + 200_000);
    events.insert(
        3,
        (
            "response.metadata".into(),
            json!({"type": "response.metadata", "blob": big}),
        ),
    );
    let input = Bytes::from(sse(&events, false));
    let chunks: Vec<Bytes> = (0..input.len())
        .step_by(64 * 1024)
        .map(|i| input.slice(i..(i + 64 * 1024).min(input.len())))
        .collect();
    for line in [None, Some(setup(TurnSoFar::default()))] {
        let injects = line.is_some();
        let (out, tap) = run(&chunks, line);
        let s = tap.summary();
        assert!(s.uninspected, "line {injects}: {s:?}");
        assert_eq!(s.outcome, Some(Outcome::Completed));
        assert_eq!(s.injected, injects);
        let out = joined(&out);
        if injects {
            let text = consistent_text(&parse_sse(&out), "msg_1");
            assert!(text.ends_with(LINE.trim_start_matches('\n')));
        } else {
            assert_eq!(out, input.to_vec());
        }
    }
}

#[test]
fn holding_more_than_the_cap_releases_unchanged() {
    let t0 = Instant::now();
    let mut tap = ResponseTap::new(t0, Some(setup(TurnSoFar::default())));
    let added =
        br#"{"item":{"id":"m","type":"message","role":"assistant","phase":"final_answer"}}"#;
    assert_eq!(
        tap.on_event(EventKind::ItemAdded, added, t0),
        Decision::Pass
    );
    let done = br#"{"item_id":"m","output_index":0,"content_index":0,"text":"x"}"#;
    assert_eq!(tap.on_event(EventKind::TextDone, done, t0), Decision::Hold);
    assert!(tap.note_held(1024));
    assert!(!tap.note_held(5 * 1024 * 1024), "past the 4 MiB cap");
    assert!(!tap.holding());
    let completed = br#"{"response":{"usage":{"input_tokens":1,"output_tokens":1}}}"#;
    assert_eq!(
        tap.on_event(EventKind::Completed, completed, t0),
        Decision::Pass,
        "no line after an abandoned hold"
    );
}

#[test]
fn the_line_renders_from_the_template() {
    let v = LineValues {
        tps: Some(group_digits(1234)),
        ttft: Some("0.82s".into()),
        input: 12_345,
        output: 321,
        cached: 11_904,
        cost: Some(Usd::from_micros(19_400)),
        requests: 2,
    };
    assert_eq!(
        render_suffix(TEMPLATE, StatsLineStyle::Quote, &v),
        "\n\n> ⚡\u{2060} 1,234 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out"
    );
    assert_eq!(
        render_suffix(
            "⚡ {cost} · {cached} · {requests}",
            StatsLineStyle::QuoteItalic,
            &v
        ),
        "\n\n> *⚡\u{2060} $0.019 · 11,904 · 2*"
    );
    // A custom template without the bolt still gets the marker; newlines are
    // flattened so the line stays one line.
    assert_eq!(
        render_text("speed {tps}\nnext", &LineValues::default()),
        "speed — next"
    );
    assert_eq!(format_usd(Usd::from_micros(1_234_567)), "$1.23");
    assert_eq!(format_usd(Usd::from_micros(10)), "<$0.001");
}

#[test]
fn the_cost_placeholder_uses_the_turn_and_this_requests_price() {
    let input = sse(&text_answer(), false);
    let mut s = setup(TurnSoFar {
        cost: Some(Usd::from_micros(1_000)),
        ..TurnSoFar::default()
    });
    s.template = "⚡ {cost}".into();
    s.price = Some(PriceEntry {
        input: Usd::from_micros(1_250_000),
        cached_input: Some(Usd::from_micros(125_000)),
        output: Usd::from_micros(10_000_000),
        ..PriceEntry::default()
    });
    let (out, _) = run(&one(&input), Some(s));
    let text = consistent_text(&parse_sse(&joined(&out)), "msg_1");
    // 674 fresh + 3,328 cached + 51 out = 1,769 micros, plus 1,000 earlier.
    assert!(text.ends_with("⚡\u{2060} $0.003"), "{text:?}");
}
