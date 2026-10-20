//! Request side and arithmetic of the request log: the stats line strip, the
//! request peek and the injection gating matrix, usage normalisation and the
//! price formula.

mod common;

use std::collections::BTreeMap;

use common::responses::*;
use http::HeaderMap;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use superba_core::{
    LongContextPrice, Multiplier, PriceEntry, PriceTable, RequestClient, RequestKind,
    RouterStatsSettings, StatsLineClient, StatsLineStyle, StatsMetric, Usd,
};
use superba_router::stats::meta::{self, TURN_METADATA_HEADER};
use superba_router::stats::price::{cost, estimate, lookup};
use superba_router::stats::strip::{decode_zstd, may_contain_line, strip, strip_text, strip_zstd};
use superba_router::stats::tap::line_allowed;
use superba_router::stats::usage::Usage;
use superba_router::{PriceConfig, StatsConfig, StatsLineConfig};

const ANSWER: &str = "Here it is.\n\n> ⚡\u{2060} 87 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out";

fn items(body: &[u8]) -> Vec<String> {
    let v: BTreeMap<String, Box<RawValue>> = serde_json::from_slice(body).unwrap();
    let list: Vec<Box<RawValue>> = serde_json::from_str(v["input"].get()).unwrap();
    list.iter().map(|r| r.get().to_owned()).collect()
}

#[test]
fn only_the_trailing_marked_line_of_assistant_text_is_removed() {
    let body = serde_json::to_vec(&request_body("gpt-x", ANSWER, json!({}))).unwrap();
    let (out, n) = strip(&body).expect("a line to strip");
    assert_eq!(n, 1);
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["input"][1]["content"][0]["text"], "Here it is.");
    // User text and tool output with the marker are never touched.
    assert_eq!(
        v["input"][0]["content"][0]["text"],
        "hi ⚡\u{2060} user text stays"
    );
    assert_eq!(v["input"][2]["output"], "tool ⚡\u{2060} output stays");
    // Untouched items keep their exact bytes, other members too.
    let (a, b) = (items(&body), items(&out));
    for i in [0, 2, 3] {
        assert_eq!(a[i], b[i], "item {i}");
    }
    assert_eq!(
        v["prompt_cache_key"],
        "019a7c00-0000-7000-8000-0000000000e1"
    );
    assert_eq!(v["model"], "gpt-x");
}

#[test]
fn styles_escapes_and_several_messages_are_stripped() {
    assert_eq!(strip_text("a\n\n> *⚡\u{2060} 1 tok/s*"), Some("a"));
    assert_eq!(strip_text("a\n\n> just a quote"), None);
    assert_eq!(
        strip_text("a\n\n> ⚡\u{2060} x\nmore"),
        None,
        "not the last line"
    );
    assert_eq!(strip_text("⚡\u{2060} no blank line"), None);
    // JSON-escaped marker (ASCII-only serializers).
    let escaped = r#"{"model":"m","input":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok\n\n> MARK 1 tok/s"}]}]}"#
        .replace("MARK", "\\u26a1\\u2060");
    assert!(may_contain_line(escaped.as_bytes()));
    let (out, n) = strip(escaped.as_bytes()).unwrap();
    assert_eq!(n, 1);
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["input"][0]["content"][0]["text"], "ok");
    // Two assistant messages (an HTTP turn re-sends the whole history).
    let mut b = request_body("m", ANSWER, json!({}));
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "assistant", "content": [{"type": "output_text", "text": ANSWER}]}));
    let (_, n) = strip(&serde_json::to_vec(&b).unwrap()).unwrap();
    assert_eq!(n, 2);
}

#[test]
fn bodies_without_a_line_are_left_alone() {
    let body = serde_json::to_vec(&request_body("m", "plain answer", json!({}))).unwrap();
    // The user's own marker text is no stats line.
    assert!(may_contain_line(&body));
    assert_eq!(strip(&body), None);
    assert_eq!(strip(br#"{"input":"no marker"}"#), None);
    assert_eq!(strip(b"not json \xe2\x81\xa0"), None);
}

#[test]
fn zstd_bodies_round_trip() {
    let plain = serde_json::to_vec(&request_body("m", ANSWER, json!({}))).unwrap();
    let packed = zstd::stream::encode_all(plain.as_slice(), 3).unwrap();
    let (encoded, stripped, n) = strip_zstd(&packed, 1 << 20).unwrap();
    assert_eq!(n, 1);
    assert_eq!(decode_zstd(&encoded, 1 << 20).unwrap(), stripped);
    assert_eq!(strip(&plain).unwrap().0, stripped);
    assert_eq!(decode_zstd(&packed, 10), None, "decode is bounded");
}

fn headers(originator: Option<&str>, turn: Option<String>) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Some(o) = originator {
        h.insert("originator", o.parse().unwrap());
    }
    if let Some(t) = turn {
        h.insert(TURN_METADATA_HEADER, t.parse().unwrap());
    }
    h.insert("session-id", "header-session".parse().unwrap());
    h
}

#[test]
fn the_request_peek_reads_headers_body_and_client_metadata() {
    let body = serde_json::to_vec(&request_body(
        "gpt-x",
        "a",
        json!({"service_tier": "priority", "previous_response_id": "resp_0"}),
    ))
    .unwrap();
    let m = meta::peek(
        &headers(
            Some("Codex Desktop"),
            Some(turn_metadata("t1", "turn", "user")),
        ),
        &body,
    );
    assert_eq!(m.client, RequestClient::Desktop);
    assert_eq!(m.request_kind, Some(RequestKind::Turn));
    assert_eq!(m.thread_source.as_deref(), Some("user"));
    assert_eq!(m.turn_id.as_deref(), Some("t1"));
    assert_eq!(
        m.thread_id.as_deref(),
        Some("019a7c00-0000-7000-8000-0000000000e1")
    );
    assert_eq!(m.model.as_deref(), Some("gpt-x"));
    assert_eq!(m.service_tier.as_deref(), Some("priority"));
    assert!(m.continuation);
    assert!(!m.structured_output);
    // A WebSocket frame carries the metadata in client_metadata.
    let frame = json!({
        "type": "response.create",
        "model": "gpt-y",
        "generate": false,
        "client_metadata": {"x-codex-turn-metadata": turn_metadata("t2", "prewarm", "user")},
        "text": {"format": {"type": "json_schema"}},
        "input": []
    });
    let m = meta::peek_frame(
        &headers(Some("codex_exec"), None),
        &serde_json::to_vec(&frame).unwrap(),
    );
    assert_eq!(m.client, RequestClient::Exec);
    assert_eq!(m.request_kind, Some(RequestKind::Prewarm));
    assert_eq!(m.turn_id.as_deref(), Some("t2"));
    assert!(m.prewarm);
    assert!(m.structured_output);
    // Without turn metadata the session header names the thread.
    let m = meta::peek(&headers(None, None), b"{}");
    assert_eq!(m.thread_id.as_deref(), Some("header-session"));
    assert_eq!(m.request_kind, None);
    assert_eq!(m.client, RequestClient::Other);
}

#[test]
fn the_gating_matrix_follows_the_spike_rules() {
    let desktop_only = StatsLineConfig {
        template: "⚡ {tps}".into(),
        style: StatsLineStyle::Quote,
        originators: vec!["Codex Desktop".into()],
        harnesses: superba_router::stats::default_line_harnesses(),
    };
    let with_tui = StatsLineConfig {
        originators: vec![
            "Codex Desktop".into(),
            "codex_cli_rs".into(),
            "codex_exec".into(),
        ],
        ..desktop_only.clone()
    };
    let originators = [
        Some("Codex Desktop"),
        Some("codex_cli_rs"),
        Some("codex_exec"),
        Some("something_else"),
        None,
    ];
    let kinds = [
        Some("turn"),
        Some("prewarm"),
        Some("compaction"),
        Some("memory"),
        None,
    ];
    let sources = [
        Some("user"),
        Some("thread_title"),
        Some("subagent"),
        Some("guardian_review"),
        None,
    ];
    let formats = [None, Some("json_schema"), Some("json_object"), Some("text")];
    let mut allowed = 0;
    for line in [&desktop_only, &with_tui] {
        for o in originators {
            for k in kinds {
                for s in sources {
                    for f in formats {
                        let turn = match (k, s) {
                            (None, None) => None,
                            _ => {
                                let mut t = json!({"turn_id": "t"});
                                if let Some(k) = k {
                                    t["request_kind"] = json!(k);
                                }
                                if let Some(s) = s {
                                    t["thread_source"] = json!(s);
                                }
                                Some(t.to_string())
                            }
                        };
                        let mut body = json!({"model": "m", "input": []});
                        if let Some(f) = f {
                            body["text"] = json!({"format": {"type": f}});
                        }
                        let m = meta::peek(&headers(o, turn), &serde_json::to_vec(&body).unwrap());
                        let expected = o.is_some_and(|o| {
                            o != "codex_exec" && line.originators.iter().any(|a| a == o)
                        }) && k == Some("turn")
                            && s == Some("user")
                            && !matches!(f, Some("json_schema" | "json_object"));
                        assert_eq!(
                            line_allowed(line, &m),
                            expected,
                            "{o:?} {k:?} {s:?} {f:?} {:?}",
                            line.originators
                        );
                        allowed += usize::from(expected);
                    }
                }
            }
        }
    }
    // Desktop with the default list; desktop and TUI with the extended one
    // (codex_exec never, even when listed); two plain-text formats each.
    assert_eq!(allowed, (1 + 2) * 2);
}

#[test]
fn usage_is_normalised_to_openai_semantics() {
    let u = Usage::from_json(
        br#"{"input_tokens":4002,"input_tokens_details":{"cached_tokens":3328,"cache_write_tokens":10},"output_tokens":51,"output_tokens_details":{"reasoning_tokens":7}}"#,
    )
    .unwrap();
    assert_eq!(
        (u.input, u.cached, u.cache_write, u.output, u.reasoning),
        (4002, Some(3328), Some(10), 51, Some(7))
    );
    assert_eq!(u.fresh_input(), 664);
    // Translators that call it cache_creation_tokens.
    let u = Usage::from_json(
        br#"{"input_tokens":100,"input_tokens_details":{"cache_creation_tokens":60},"output_tokens":1}"#,
    )
    .unwrap();
    assert_eq!((u.input, u.cache_write), (100, Some(60)));
    // Anthropic-style additive counts are added to the input.
    let u = Usage::from_json(
        br#"{"input_tokens":100,"cache_read_input_tokens":300,"cache_creation_input_tokens":50,"output_tokens":20}"#,
    )
    .unwrap();
    assert_eq!(
        (u.input, u.cached, u.cache_write, u.output),
        (450, Some(300), Some(50), 20)
    );
    assert_eq!(u.fresh_input(), 100);
    // Chat Completions fallback.
    let u = Usage::from_json(
        br#"{"prompt_tokens":9,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":4}}"#,
    )
    .unwrap();
    assert_eq!((u.input, u.cached, u.output), (9, Some(4), 3));
    // Unknown stays unknown.
    let u = Usage::from_json(br#"{"input_tokens":5,"output_tokens":1}"#).unwrap();
    assert_eq!((u.cached, u.cache_write, u.reasoning), (None, None, None));
    assert_eq!(Usage::from_json(b"{}"), None);
    assert_eq!(Usage::from_json(b"null"), None);
}

fn usd(dollars: f64) -> Usd {
    Usd::from_dollars(dollars).unwrap()
}

fn entry() -> PriceEntry {
    PriceEntry {
        cache_write_1h: None,
        windows: Vec::new(),
        input: usd(1.25),
        cached_input: Some(usd(0.125)),
        cache_write: None,
        output: usd(10.0),
        long_context: vec![LongContextPrice {
            cache_write_1h: None,
            above_input_tokens: 272_000,
            input: usd(2.5),
            cached_input: Some(usd(0.25)),
            cache_write: None,
            output: usd(15.0),
        }],
        tiers: [("priority".to_owned(), Multiplier::from_ppm(2_000_000))].into(),
        tier_prices: BTreeMap::new(),
        source: Some("https://example.com/pricing".into()),
        rev: Some("2026-10".into()),
    }
}

#[test]
fn the_price_formula_uses_micro_dollars() {
    let c = |u: &Usage, tier: Option<&str>, ratio| cost(&entry(), u, tier, ratio, None).unwrap();
    let u = Usage {
        cache_write_1h: None,
        input: 4002,
        cached: Some(3328),
        cache_write: Some(0),
        output: 51,
        reasoning: Some(0),
    };
    // 674 x 1.25 + 3,328 x 0.125 + 51 x 10 = 1,768.5 µ$ (rounded).
    assert_eq!(c(&u, None, Multiplier::ONE), Usd::from_micros(1_769));
    assert_eq!(
        c(&u, Some("default"), Multiplier::ONE),
        Usd::from_micros(1_769)
    );
    assert_eq!(
        c(&u, Some("priority"), Multiplier::ONE),
        Usd::from_micros(3_538)
    );
    // No multiplier for flex in this entry; the member pays half.
    assert_eq!(
        c(&u, Some("flex"), Multiplier::from_ppm(500_000)),
        Usd::from_micros(884)
    );
    // Cache writes default to the input price.
    let w = Usage {
        cache_write_1h: None,
        input: 1000,
        cached: None,
        cache_write: Some(1000),
        output: 0,
        reasoning: None,
    };
    assert_eq!(c(&w, None, Multiplier::ONE), Usd::from_micros(1_250));
    // Long-context prices above the threshold.
    let long = Usage {
        cache_write_1h: None,
        input: 300_000,
        cached: Some(0),
        cache_write: None,
        output: 1000,
        reasoning: None,
    };
    assert_eq!(
        c(&long, None, Multiplier::ONE),
        Usd::from_micros(300_000 * 25 / 10 + 15_000)
    );
}

#[test]
fn prices_come_from_overrides_then_the_table() {
    let mut table = PriceTable {
        rev: "2026-10-01".into(),
        entries: BTreeMap::new(),
    };
    table.entries.insert("gpt-x".into(), entry());
    let mut prices = PriceConfig {
        table,
        ..PriceConfig::default()
    };
    prices
        .ratio_by_member
        .insert("krill".into(), Multiplier::from_ppm(250_000));
    let mut cheap = entry();
    cheap.input = usd(0.0);
    cheap.cached_input = Some(usd(0.0));
    cheap.output = usd(1.0);
    cheap.rev = None;
    prices
        .overrides
        .insert("site".into(), [("gpt-x".to_owned(), cheap)].into());
    let u = Usage {
        cache_write_1h: None,
        input: 4002,
        cached: Some(3328),
        cache_write: Some(0),
        output: 51,
        reasoning: None,
    };
    // The stored basis: the list price's revision and source page; a price set
    // on the Models page is `override`, whatever the table's revision.
    assert_eq!(
        estimate(&prices, "krill", &["gpt-x"], &u, None, None),
        Some((
            Usd::from_micros(442),
            Some("2026-10 https://example.com/pricing".into())
        ))
    );
    assert_eq!(
        estimate(&prices, "site", &["gpt-x"], &u, None, None),
        Some((Usd::from_micros(51), Some("override".into())))
    );
    let mut bare = entry();
    bare.rev = None;
    bare.source = None;
    prices.table.entries.insert("gpt-y".into(), bare);
    assert_eq!(
        estimate(&prices, "krill", &["gpt-y"], &u, None, None).map(|(_, r)| r),
        Some(Some("2026-10-01".into())),
        "an entry without its own revision takes the table's"
    );
    assert_eq!(
        estimate(&prices, "krill", &["unknown"], &u, None, None),
        None
    );
    // The upstream name, then response.model, then a tier-suffix base.
    assert!(lookup(&prices, "x", &["alias", "gpt-x"]).is_some());
    assert!(lookup(&prices, "x", &["gpt-x-ultrafast"]).is_some());
    assert!(lookup(&prices, "x", &["gpt-x-mini"]).is_none());
}

/// User bug 23: with the defaults the line is written in the UI language
/// (stored as the default marker, resolved when the settings reach the router),
/// with the same number format in both languages; an edited template is kept.
#[test]
fn the_default_stats_line_is_written_in_the_ui_language() {
    use superba_router::stats::inject::{LineValues, render_suffix};
    let mut s = RouterStatsSettings::default();
    s.line.enabled = true;
    assert_eq!(s.line.template, None, "the default is a marker, not a text");
    let values = LineValues {
        tps: Some("87".into()),
        ttft: Some("0.82s".into()),
        input: 12_345,
        output: 321,
        cached: 11_904,
        cost: None,
        requests: 3,
    };
    let written = |s: &RouterStatsSettings, locale| {
        let cfg = StatsConfig::from_settings(s, PriceTable::default(), locale);
        let line = cfg.line.unwrap();
        render_suffix(&line.template, line.style, &values)
    };
    assert_eq!(
        written(&s, superba_core::Locale::ZhCn),
        "\n\n> ⚡\u{2060} 87 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out"
    );
    assert_eq!(
        written(&s, superba_core::Locale::En),
        "\n\n> ⚡\u{2060} 87 tok/s · first token 0.82s · turn 12,345 in / 321 out"
    );
    // `default` typed as the template is the marker too.
    s.line.set_template("default");
    assert_eq!(s.line.template, None);
    // An edited template stays as written, whatever the language.
    s.line.set_template("⚡ {tps} tok/s · in {input}");
    for locale in superba_core::Locale::ALL {
        assert_eq!(written(&s, locale), "\n\n> ⚡\u{2060} 87 tok/s · in 12,345");
    }
}

#[test]
fn router_stats_come_from_superba_settings() {
    let mut s = RouterStatsSettings::default();
    s.line.enabled = true;
    s.line.clients = vec![StatsLineClient::Desktop, StatsLineClient::Tui];
    s.prices
        .ratio_by_profile
        .insert("Krill Main".into(), Multiplier::from_ppm(800_000));
    let zh = StatsConfig::from_settings(&s, PriceTable::default(), superba_core::Locale::ZhCn);
    let line = zh.line.as_ref().unwrap();
    assert_eq!(
        line.template,
        "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out"
    );
    assert_eq!(line.originators, ["Codex Desktop", "codex_cli_rs"]);
    assert_eq!(
        zh.prices.ratio_by_member["krill-main"],
        Multiplier::from_ppm(800_000)
    );
    assert!(zh.record);
    let en = StatsConfig::from_settings(&s, PriceTable::default(), superba_core::Locale::En);
    assert_eq!(
        en.line.unwrap().template,
        "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out"
    );
    s.line.metrics = vec![StatsMetric::Cost, StatsMetric::Requests, StatsMetric::Tps];
    let t = StatsConfig::from_settings(&s, PriceTable::default(), superba_core::Locale::ZhCn)
        .line
        .unwrap()
        .template;
    assert_eq!(t, "⚡ {tps} tok/s · ≈ {cost} · {requests} 次请求");
    s.line.template = Some("my {tps}".into());
    let t = StatsConfig::from_settings(&s, PriceTable::default(), superba_core::Locale::En)
        .line
        .unwrap()
        .template;
    assert_eq!(t, "my {tps}");
    s.line.enabled = false;
    assert!(
        StatsConfig::from_settings(&s, PriceTable::default(), superba_core::Locale::En)
            .line
            .is_none()
    );
    // Limits.
    let mut bad = StatsConfig::default();
    assert!(bad.validate().is_ok());
    bad.usage_days = 0;
    assert!(bad.validate().is_err());
}

/// Anthropic reports one-hour cache writes apart
/// (`cache_creation.ephemeral_1h_input_tokens`); they are priced at their own
/// rate (user requirement 115).
#[test]
fn anthropic_one_hour_cache_writes_are_read_and_priced() {
    let u = Usage::from_json(
        br#"{"input_tokens":100,"cache_read_input_tokens":300,"cache_creation_input_tokens":600,
            "cache_creation":{"ephemeral_5m_input_tokens":400,"ephemeral_1h_input_tokens":200},
            "output_tokens":10}"#,
    )
    .unwrap();
    assert_eq!(u.input, 1000);
    assert_eq!(u.cached, Some(300));
    assert_eq!(u.cache_write, Some(600));
    assert_eq!(u.cache_write_1h, Some(200));
    let mut e = entry();
    e.input = Usd::from_micros(1_000_000);
    e.cached_input = Some(Usd::from_micros(100_000));
    e.cache_write = Some(Usd::from_micros(1_250_000));
    e.cache_write_1h = Some(Usd::from_micros(2_000_000));
    e.output = Usd::from_micros(5_000_000);
    e.long_context.clear();
    e.tiers.clear();
    // 100*1 + 300*0.1 + 400*1.25 + 200*2 + 10*5 = 1080 micro-dollars per 1M → 0.00108 USD
    assert_eq!(
        cost(&e, &u, None, superba_core::Multiplier::ONE, None),
        Some(Usd::from_micros(1080))
    );
}
