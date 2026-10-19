#![allow(missing_docs)]
//! View model of the request log (Router & usage, UX §3.5): the 13 columns in
//! the user's order, cell texts, totals, search over session names, CSV, the
//! stats line template / preview, price ratios and calibration.

mod support;

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use superba_core::{
    InflightPhase, InflightRequest, Locale, Multiplier, ProfileId, RequestClient, RequestKind,
    RequestSortKey, RequestStatus, RouterStatsSettings, SessionTitle, StatsLineClient,
    StatsLineSettings, StatsLineStyle, StatsMetric, Timestamp, TitleSource, Usd,
};
use superba_gui::design::widgets::QuotePreview;
use superba_gui::design::widgets::log_table::column_widths;
use superba_gui::i18n::Tr;
use superba_gui::pages::router::prices::{
    calibrated_ratio, cost_lines, parse_ratio, ratio_of, with_ratio,
};
use superba_gui::pages::router::requests::{self, ActiveCol, Col, Range};
use superba_gui::pages::router::stats_line::{self, Sample};
use support::fake::FakeService;

fn rows() -> Vec<superba_core::RequestRow> {
    FakeService::fixture_request_rows(Timestamp::now())
}

fn titles() -> BTreeMap<String, SessionTitle> {
    let mut t = BTreeMap::new();
    t.insert(
        FakeService::THREAD_LOGIN.to_owned(),
        SessionTitle {
            thread_id: Some(FakeService::THREAD_LOGIN.into()),
            title: "登录页样式".into(),
            source: TitleSource::Title,
        },
    );
    t
}

#[test]
fn the_thirteen_columns_are_in_the_users_order_with_the_specified_widths() {
    let zh = Tr::new(Locale::ZhCn);
    let labels: Vec<String> = requests::columns(&zh)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert_eq!(
        labels,
        [
            "缓存标识",
            "日期",
            "会话",
            "状态",
            "供应商",
            "输入",
            "输出",
            "缓存读取",
            "缓存写入",
            "首字延迟",
            "完成时间",
            "速度",
            "费用"
        ]
    );
    let total: f32 = Col::ALL.iter().map(|c| c.width()).sum();
    assert!(total <= 834.0, "UX §3.5 table width at 1280x800: {total}");
    // The status column holds a dot and a short label (ux-visual #3).
    assert!(Col::Status.width() >= 72.0);
    for c in Col::ALL {
        assert_eq!(Col::of_sort_key(c.sort_key()), Some(c));
    }
    assert_eq!(Col::of_sort_key(RequestSortKey::Time), Some(Col::Date));
}

#[test]
fn cells_follow_the_token_and_timing_semantics() {
    let tr = Tr::new(Locale::ZhCn);
    let now = Timestamp::now();
    let rows = rows();
    let ok = &rows[0];
    let cell = |c| requests::cell(&tr, &titles(), ok, c, now);
    // Input = input - cached - cache write; the tooltip has the raw value.
    assert_eq!(cell(Col::Input).text, "668");
    assert!(cell(Col::Input).tooltip.unwrap().contains("12,572"));
    assert_eq!(cell(Col::CacheRead).text, "11,904");
    assert_eq!(cell(Col::Output).text, "321");
    assert_eq!(cell(Col::FirstToken).text, "0.82s");
    assert_eq!(cell(Col::Duration).text, "3.42s");
    assert_eq!(cell(Col::Session).text, "登录页样式");
    assert_eq!(cell(Col::CacheKey).text, "019a7c…e1");
    assert_eq!(
        cell(Col::CacheKey).tooltip.as_deref(),
        Some(FakeService::THREAD_LOGIN)
    );
    assert!(cell(Col::Cost).text.starts_with('≈'));
    assert_eq!(cell(Col::Provider).text, "Krill · 海外线路");
    // Unknown values are dashes, never 0.
    let cancelled = rows
        .iter()
        .find(|r| r.status == RequestStatus::Cancelled)
        .unwrap();
    let c = |col| requests::cell(&tr, &titles(), cancelled, col, now).text;
    assert_eq!(c(Col::Input), "—");
    assert_eq!(c(Col::Cost), "—");
    assert_eq!(c(Col::Status), "取消");
    // A failed-over success says so; an HTTP error shows the code.
    let failover = rows.iter().find(|r| r.failovers > 0).unwrap();
    assert_eq!(requests::status_cell(&tr, failover).1, "自动切换");
    let limited = rows.iter().find(|r| r.http_status == Some(429)).unwrap();
    assert_eq!(requests::status_cell(&tr, limited).1, "429");
    // Unknown sessions show the short key; title generation is marked.
    let unknown = rows
        .iter()
        .find(|r| r.prompt_cache_key.as_deref() == Some(FakeService::THREAD_UNKNOWN))
        .unwrap();
    assert_eq!(requests::session_cell(&tr, &titles(), unknown), "019affff");
    let title = rows
        .iter()
        .find(|r| r.thread_source.as_deref() == Some("thread_title"))
        .unwrap();
    assert!(requests::session_cell(&tr, &titles(), title).ends_with("· 标题生成"));
}

#[test]
fn numbers_and_times_are_formatted_for_the_table() {
    assert_eq!(requests::tokens(Some(1_234_567)), "1,234,567");
    assert_eq!(requests::tokens(None), "—");
    assert_eq!(requests::seconds(Some(820)), "0.82s");
    assert_eq!(requests::seconds(Some(17_900)), "17.9s");
    assert_eq!(requests::seconds(Some(125_000)), "2m 05s");
    assert_eq!(requests::tps(Some(87_400)), "87");
    assert_eq!(requests::tps(Some(4_250)), "4.2");
    assert_eq!(
        requests::cost_cell(Some(Usd::from_micros(19_000))),
        "≈0.019"
    );
    assert_eq!(
        requests::cost_cell(Some(Usd::from_micros(12_840_000))),
        "≈12.84"
    );
    assert_eq!(requests::cost_cell(Some(Usd::from_micros(120))), "≈<0.001");
    assert_eq!(requests::cost_cell(None), "—");
    assert_eq!(requests::short_cache_key("short"), "short");
    let now = Timestamp::now();
    assert_eq!(requests::date_cell(now, now).len(), 8, "HH:MM:SS today");
    let old = Timestamp::from_millis(now.as_millis() - 3 * 86_400_000);
    assert_eq!(
        requests::date_cell(old, now).len(),
        11,
        "MM-DD HH:MM otherwise"
    );
}

#[test]
fn totals_count_success_tokens_and_unpriced_rows() {
    let rows = rows();
    let t = requests::totals_of(&rows);
    assert_eq!(t.requests, rows.len() as u64);
    assert_eq!(t.errors, 2, "the 429 and the cancelled request");
    assert_eq!(requests::success_rate(&t), Some(75));
    assert!(t.cost.micros() > 0);
    let tr = Tr::new(Locale::En);
    let cells = requests::totals_cells(&tr, &t, &rows);
    assert_eq!(cells.len(), 13);
    assert_eq!(cells[0].text, "Total");
    assert_eq!(cells[1].text, "8 req.");
    assert_eq!(cells[3].text, "75 %");
    assert!(requests::avg_first_token(&rows).is_some());
    assert!(requests::weighted_tps(&rows).is_some());
}

#[test]
fn search_matches_session_names_and_merges_their_rows() {
    let rows = rows();
    let t = titles();
    assert_eq!(
        requests::keys_matching(&t, "登录"),
        vec![FakeService::THREAD_LOGIN.to_owned()]
    );
    assert!(requests::keys_matching(&t, "").is_empty());
    let login = rows.iter().find(|r| r.id == 12).unwrap();
    assert!(requests::row_matches(login, Some("登录页样式"), "登录"));
    assert!(requests::row_matches(login, None, "krill"));
    assert!(!requests::row_matches(login, None, "nothing-like-this"));
    let merged = requests::merge_rows(rows[2..4].to_vec(), rows[..3].to_vec());
    let ids: Vec<i64> = merged.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![12, 11, 10, 9], "no duplicates, newest first");
    assert_eq!(requests::new_rows(&rows[2..], &rows), 2);
}

#[test]
fn csv_has_a_header_full_keys_and_no_content() {
    let tr = Tr::new(Locale::En);
    let rows = rows();
    let csv = requests::csv(&tr, &titles(), &rows);
    let mut lines = csv.lines();
    let header = lines.next().unwrap();
    assert!(header.starts_with("Cache ID,Date,Session,Status,Provider,Input"));
    assert!(
        header.ends_with(",Model,Client,Line,Upstream host,Source"),
        "{header}"
    );
    assert_eq!(lines.count(), rows.len());
    assert!(csv.contains(FakeService::THREAD_LOGIN), "full cache keys");
    assert!(csv.contains("登录页样式"));
    // The provider column holds the provider alone (a spreadsheet groups by it);
    // its line and host are columns of their own.
    let first = csv.lines().nth(1).unwrap();
    let fields: Vec<&str> = first.split(',').collect();
    assert_eq!(fields[4], rows[0].member_name);
    assert_eq!(fields[fields.len() - 3], rows[0].line_label().unwrap());
    assert_eq!(
        fields[fields.len() - 2],
        rows[0].upstream_host.as_deref().unwrap()
    );
    // The last column names where the row comes from.
    assert_eq!(fields[fields.len() - 1], "Local router");
    let zh = requests::csv(&Tr::new(Locale::ZhCn), &titles(), &rows);
    assert!(
        zh.lines()
            .next()
            .unwrap()
            .ends_with(",模型,客户端,线路,上游主机,来源")
    );
}

/// Profiles `krill-work` (Krill, renamed) and `my-api` (a custom site whose
/// profile has no name), as profiles.json holds them.
fn provider_names(tr: &Tr) -> requests::ProviderNames {
    let set: superba_core::ProfileSet = serde_json::from_value(serde_json::json!({
        "profiles": [
            {"id": "krill-work", "name": "Krill 工作", "site_id": "krill", "auth_mode": "pat"},
            {"id": "my-api", "name": "", "site_id": "my-api", "auth_mode": "apikey"},
        ]
    }))
    .unwrap();
    let mut custom = superba_core::catalog::compatible_template_site();
    custom.id = superba_core::SiteId::new("my-api");
    custom.name = superba_core::LocalizedText::new("", "My API");
    requests::ProviderNames::new(
        tr,
        Some(&set),
        &[superba_core::catalog::krill_site(), custom],
    )
}

#[test]
fn providers_show_the_current_profile_names_and_their_lines_in_the_ui_language() {
    use superba_core::{RequestGroup, RequestGrouping, StatsBucket, provider_line_key};
    let zh = Tr::new(Locale::ZhCn);
    let names = provider_names(&zh);
    assert_eq!(names.provider("krill-work", "Krill"), "Krill 工作");
    assert_eq!(
        names.provider("my-api", "stored"),
        "My API",
        "the site name"
    );
    assert_eq!(names.provider("gone", "Old name"), "Old name");
    // Lines are matched by id or by the name the router stored.
    assert_eq!(names.line("krill-work", "cn-fast"), "国内极速");
    assert_eq!(names.line("krill-work", "Overseas"), "海外线路");
    assert_eq!(names.line("krill-work", "Elsewhere"), "Elsewhere");
    let mut row = rows()[0].clone();
    row.member_id = ProfileId::new("krill-work");
    row.member_name = "Krill".into();
    row.line_id = Some("overseas".into());
    row.line_name = Some("Overseas".into());
    names.relabel_row(&mut row);
    assert_eq!(requests::provider_cell(&row), "Krill 工作 · 海外线路");
    // Source list by provider: lines as sub-entries only for several lines.
    let group = |key: String, label: &str, count: u64, lines: Vec<RequestGroup>| RequestGroup {
        key,
        label: label.into(),
        count,
        last: Timestamp::from_millis(1_760_000_000_000),
        upstream_host: Some("api.example.com".into()),
        lines,
    };
    let mut groups = vec![
        group(
            "krill-work".into(),
            "Krill",
            30,
            vec![
                group(
                    provider_line_key("krill-work", "China Express"),
                    "China Express",
                    20,
                    vec![],
                ),
                group(
                    provider_line_key("krill-work", "Overseas"),
                    "Overseas",
                    10,
                    vec![],
                ),
            ],
        ),
        group(
            "my-api".into(),
            "my-api",
            5,
            vec![group(
                provider_line_key("my-api", "default"),
                "default",
                5,
                vec![],
            )],
        ),
    ];
    names.relabel_groups(&mut groups);
    let zh = Tr::new(Locale::ZhCn);
    let entries = requests::source_entries(&zh, RequestGrouping::Provider, &groups, &titles());
    let shown: Vec<(&str, u64, bool)> = entries
        .iter()
        .map(|e| (e.label.as_str(), e.count, e.child))
        .collect();
    assert_eq!(
        shown,
        [
            ("Krill 工作", 30, false),
            ("国内极速", 20, true),
            ("海外线路", 10, true),
            ("My API", 5, false),
        ]
    );
    assert_eq!(
        entries[1].key,
        provider_line_key("krill-work", "China Express")
    );
    let tip = entries[1].tooltip.as_deref().unwrap();
    assert!(
        tip.starts_with("Krill 工作 · 国内极速 · api.example.com · "),
        "{tip}"
    );
    assert!(
        entries[3]
            .tooltip
            .as_deref()
            .unwrap()
            .contains("api.example.com")
    );
    // Statistics buckets get the same names.
    let bucket = |key: String, label: &str| StatsBucket {
        key,
        label: label.into(),
        totals: Default::default(),
        lines: Vec::new(),
    };
    let mut buckets = vec![StatsBucket {
        lines: vec![bucket(
            provider_line_key("krill-work", "China Express"),
            "China Express",
        )],
        ..bucket("krill-work".into(), "Krill")
    }];
    names.relabel_buckets(&mut buckets);
    assert_eq!(buckets[0].label, "Krill 工作");
    assert_eq!(buckets[0].lines[0].label, "国内极速");
    let en = provider_names(&Tr::new(Locale::En));
    assert_eq!(en.line("krill-work", "cn-fast"), "China Express");
}

#[test]
fn provider_names_are_resolved_again_when_profiles_language_or_sites_change() {
    use requests::ProviderNames;
    let (zh, en) = (Tr::new(Locale::ZhCn), Tr::new(Locale::En));
    let mut set: superba_core::ProfileSet = serde_json::from_value(serde_json::json!({
        "profiles": [{"id": "krill", "name": "Krill", "site_id": "krill", "auth_mode": "pat"}]
    }))
    .unwrap();
    let sites = [superba_core::catalog::krill_site()];
    let base = ProviderNames::signature(&zh, Some(&set), &sites);
    assert_eq!(base, ProviderNames::signature(&zh, Some(&set), &sites));
    // Rows read before the profiles arrived carry the stored names.
    assert_ne!(base, ProviderNames::signature(&zh, None, &sites));
    assert_ne!(base, ProviderNames::signature(&en, Some(&set), &sites));
    assert_ne!(base, ProviderNames::signature(&zh, Some(&set), &[]));
    set.profiles[0].name = "Krill 工作".into();
    assert_ne!(base, ProviderNames::signature(&zh, Some(&set), &sites));
}

#[test]
fn ranges_and_active_requests() {
    let now = Timestamp::now();
    assert_eq!(
        now.as_millis() - Range::Week.from(now).as_millis(),
        7 * 86_400_000
    );
    let tr = Tr::new(Locale::ZhCn);
    let r = InflightRequest {
        id: 1,
        started_at: now,
        member_id: ProfileId::new("krill"),
        member_name: "Krill".into(),
        line_name: Some("海外线路".into()),
        model: Some("gpt-5.5".into()),
        prompt_cache_key: Some(FakeService::THREAD_LOGIN.into()),
        client: RequestClient::Desktop,
        transport: Default::default(),
        phase: InflightPhase::Streaming,
        output_deltas: 120,
        first_token_ms: Some(640),
        elapsed_ms: 2_000,
    };
    let cell = |c| requests::active_cell(&tr, &titles(), &r, c).text;
    assert_eq!(cell(ActiveCol::Session), "登录页样式");
    assert_eq!(cell(ActiveCol::Status), "输出中");
    assert_eq!(cell(ActiveCol::Rate), "≈60");
    assert_eq!(cell(ActiveCol::FirstToken), "0.64s");
    assert_eq!(cell(ActiveCol::Provider), "Krill · 海外线路");
}

#[test]
fn the_stats_line_is_never_offered_for_codex_exec() {
    let rows = rows();
    let exec = rows
        .iter()
        .find(|r| r.client == RequestClient::Exec)
        .unwrap();
    assert!(exec.injected, "the fixture sets the flag on purpose");
    assert!(!requests::line_applicable(exec));
    let mut prewarm = rows[0].clone();
    prewarm.request_kind = Some(RequestKind::Prewarm);
    assert!(!requests::line_applicable(&prewarm));
    assert!(requests::line_applicable(&rows[0]));
}

#[test]
fn default_stats_line_templates_in_both_languages() {
    let metrics = StatsLineSettings::default().metrics;
    assert_eq!(
        stats_line::default_template(&Tr::new(Locale::ZhCn), &metrics),
        "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out"
    );
    assert_eq!(
        stats_line::default_template(&Tr::new(Locale::En), &metrics),
        "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out"
    );
}

#[test]
fn a_custom_template_disables_the_metric_checkboxes_until_reset() {
    let tr = Tr::new(Locale::ZhCn);
    let line = StatsLineSettings::default();
    assert!(!stats_line::is_custom(&line));
    assert_eq!(
        stats_line::template_note(&tr, false),
        "默认（跟随界面语言）"
    );
    assert_eq!(
        stats_line::template_note(&tr, true),
        "已自定义格式 · 恢复默认后可用勾选"
    );
    // Typing the default back is not a custom template.
    let same = stats_line::with_template(
        &tr,
        &line,
        &stats_line::default_template(&tr, &line.metrics),
    );
    assert_eq!(same.template, None);
    // An emptied field or `default` is the default marker too.
    for text in ["⚡ ", "⚡ default", ""] {
        assert_eq!(
            stats_line::with_template(&tr, &line, text).template,
            None,
            "{text:?}"
        );
    }
    let custom = stats_line::with_template(&tr, &line, "⚡ {tps} tok/s");
    assert!(stats_line::is_custom(&custom));
    // An edited template is kept as written in every UI language.
    for locale in Locale::ALL {
        assert_eq!(
            stats_line::effective_template(&Tr::new(locale), &custom),
            "⚡ {tps} tok/s"
        );
    }
    // 恢复默认: back to the marker, which follows the UI language.
    let reset = stats_line::with_default_template(&custom);
    assert_eq!(reset.template, None);
    assert!(!stats_line::is_custom(&reset));
    assert_eq!(
        stats_line::effective_template(&Tr::new(Locale::En), &reset),
        "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out"
    );
    assert_eq!(
        stats_line::unknown_placeholders("{tps} {foo} {bar} {foo}"),
        vec!["foo", "bar"]
    );
    let more = stats_line::with_metric(&line, StatsMetric::Cost, true);
    assert_eq!(
        more.metrics,
        vec![
            StatsMetric::Tps,
            StatsMetric::FirstToken,
            StatsMetric::TurnTokens,
            StatsMetric::Cost
        ]
    );
    let tui = stats_line::with_client(&line, StatsLineClient::Tui, true);
    assert_eq!(
        tui.clients,
        vec![StatsLineClient::Desktop, StatsLineClient::Tui]
    );
    // Claude Code has its own switch (on by default).
    let claude = superba_core::RequestHarness::ClaudeCode;
    assert!(line.harnesses.contains(&claude));
    let off = stats_line::with_harness(&line, claude, false);
    assert_eq!(off.harnesses, vec![superba_core::RequestHarness::Codex]);
    let on = stats_line::with_harness(&off, claude, true);
    assert_eq!(on.harnesses, line.harnesses);
}

#[test]
fn the_preview_renders_the_last_turn_or_sample_data() {
    let sample = Sample::example();
    let tr = Tr::new(Locale::ZhCn);
    let line = stats_line::render(
        &stats_line::default_template(&tr, &StatsLineSettings::default().metrics),
        &sample,
    );
    assert_eq!(line, "⚡ 87 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out");
    assert_eq!(QuotePreview::markdown(&line, false), format!("> {line}"));
    assert_eq!(QuotePreview::markdown("x", true), "> *x*");
    // The editor shows the template after the fixed bolt and adds it back.
    assert_eq!(stats_line::editable("⚡ {tps} tok/s"), "{tps} tok/s");
    assert_eq!(stats_line::editable("⚡\u{2060} {tps}"), "{tps}");
    assert_eq!(stats_line::with_bolt("{tps} tok/s"), "⚡ {tps} tok/s");
    assert_eq!(stats_line::with_bolt("⚡ {tps}"), "⚡ {tps}");
    // The last finished user turn of the log (turn totals of its requests).
    let rows = rows();
    let last = Sample::last_turn(&rows).expect("a turn");
    assert!(last.at.is_some());
    assert_eq!(last.tps_milli, rows[0].tps_milli);
    assert!(Sample::last_turn(&[]).is_none());
    let mut s = RouterStatsSettings::default();
    s = stats_line::with_style(&s, StatsLineStyle::QuoteItalic);
    assert_eq!(s.line.style, StatsLineStyle::QuoteItalic);
}

/// The preview is the line the router writes (user bug 23): the same template,
/// resolved in the UI language, through the router's renderer, with the router's
/// number format, for the default and an edited template.
#[test]
fn the_preview_equals_the_line_the_router_writes_in_each_language() {
    use superba_router::StatsConfig;
    use superba_router::stats::inject::{LineValues, render_suffix};
    use superba_router::stats::tap::group_digits;
    let sample = Sample::example();
    // The values as the router's tap formats them.
    let values = LineValues {
        tps: sample.tps_milli.map(|m| group_digits((m + 500) / 1000)),
        ttft: sample
            .first_token_ms
            .map(|ms| format!("{:.2}s", ms as f64 / 1000.0)),
        input: sample.input,
        output: sample.output,
        cached: sample.cached,
        cost: sample.cost,
        requests: sample.requests as u32,
    };
    let mut stats = RouterStatsSettings::default();
    stats.line.enabled = true;
    let mut edited = stats.clone();
    edited
        .line
        .set_template("⚡ {tps} tok/s · in {input} · {cost}");
    for (locale, expected) in [
        (
            Locale::ZhCn,
            "> ⚡ 87 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out",
        ),
        (
            Locale::En,
            "> ⚡ 87 tok/s · first token 0.82s · turn 12,345 in / 321 out",
        ),
    ] {
        let tr = Tr::new(locale);
        for (stats, expected) in [
            (&stats, expected),
            (&edited, "> ⚡ 87 tok/s · in 12,345 · $0.019"),
        ] {
            // What the editor shows and previews.
            let field = stats_line::editable(&stats_line::effective_template(&tr, &stats.line));
            let preview = stats_line::render(&stats_line::with_bolt(&field), &sample);
            let shown = QuotePreview::markdown(&preview, false);
            // What the router writes for the same settings and UI language.
            let cfg = StatsConfig::from_settings(stats, Default::default(), locale);
            let line = cfg.line.expect("line on");
            let written = render_suffix(&line.template, line.style, &values);
            let written = written.trim_start_matches('\n').replace('\u{2060}', "");
            assert_eq!(shown, written, "{locale:?}");
            assert_eq!(shown, expected, "{locale:?}");
        }
    }
}

#[test]
fn price_ratios_parse_save_and_calibrate() {
    assert_eq!(parse_ratio("0.3"), Multiplier::from_f64(0.3));
    assert_eq!(parse_ratio("×0.3"), Multiplier::from_f64(0.3));
    assert_eq!(parse_ratio("30%"), Multiplier::from_f64(0.3));
    for bad in ["", "0", "-1", "abc", "1000"] {
        assert_eq!(parse_ratio(bad), None, "{bad}");
    }
    let id = ProfileId::new("krill");
    let s = with_ratio(
        &RouterStatsSettings::default(),
        &id,
        Multiplier::from_f64(0.3).unwrap(),
    );
    assert_eq!(ratio_of(&s, &id), Multiplier::from_f64(0.3).unwrap());
    assert!(
        with_ratio(&s, &id, Multiplier::ONE)
            .prices
            .ratio_by_profile
            .is_empty()
    );
    // Billed $3, estimated $10 at x0.5 -> list estimate $20 -> ratio 0.15.
    assert_eq!(
        calibrated_ratio(
            Usd::from_micros(3_000_000),
            Usd::from_micros(10_000_000),
            Multiplier::from_f64(0.5).unwrap()
        ),
        Multiplier::from_f64(0.15)
    );
    assert_eq!(
        calibrated_ratio(Usd::ZERO, Usd::from_micros(1), Multiplier::ONE),
        None
    );
}

#[test]
fn fake_titles_follow_the_state_db_rules() {
    let fake = FakeService::new(Default::default());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let names = rt
        .block_on(superba_core::SwitchService::session_titles(
            &fake,
            vec![
                FakeService::THREAD_LOGIN.into(),
                FakeService::THREAD_REPORT.into(),
                format!("thread_title:{}", FakeService::THREAD_ROUTER),
                FakeService::THREAD_UNKNOWN.into(),
            ],
        ))
        .unwrap();
    assert_eq!(names[FakeService::THREAD_LOGIN].title, "登录页样式");
    assert_eq!(
        names[FakeService::THREAD_REPORT].source,
        TitleSource::FirstUserMessage
    );
    assert_eq!(
        names[&format!("thread_title:{}", FakeService::THREAD_ROUTER)].title,
        "router 测试"
    );
    assert_eq!(
        names[FakeService::THREAD_UNKNOWN].source,
        TitleSource::ShortKey
    );
}

#[test]
fn database_sizes_read_in_megabytes() {
    use superba_gui::pages::shared::format::megabytes;
    assert_eq!(megabytes(420_000), "0.4 MB");
    assert_eq!(megabytes(12_345_678), "12.3 MB");
    assert_eq!(megabytes(1_024_000_000), "1,024 MB");
}

#[test]
fn the_preview_uses_the_router_s_own_formatting() {
    // Under 10 tok/s and above $1 the router rounds; the preview matches it.
    let mut sample = Sample::example();
    sample.tps_milli = Some(9_400);
    sample.cost = Some(superba_core::Usd::from_micros(1_234_567));
    let line = stats_line::render("⚡ {tps} tok/s · {cost}", &sample);
    assert_eq!(line, "⚡ 9 tok/s · $1.23");
}

#[test]
fn the_cost_card_names_the_official_price_its_date_and_source_or_why_there_is_none() {
    let mut row = rows().into_iter().find(|r| r.cost.is_some()).unwrap();
    let zh = Tr::new(Locale::ZhCn);
    let en = Tr::new(Locale::En);
    let values = |tr: &Tr, row: &superba_core::RequestRow| -> Vec<String> {
        cost_lines(tr, row).into_iter().map(|l| l.value).collect()
    };
    // A list price: the estimate, OpenAI list prices with the date, the source;
    // the hover texts say it is an estimate at list price x the ratio.
    row.price_rev = Some("2026-10-17 https://developers.openai.com/api/docs/pricing".into());
    let lines = cost_lines(&zh, &row);
    assert!(lines[0].value.starts_with("≈ $"), "{:?}", lines[0].value);
    assert_eq!(lines[1].value, "按 OpenAI 官方价估算 · 2026-10-17");
    assert_eq!(
        lines[1].tooltip.as_deref(),
        Some("按 OpenAI 官方价 × 供应商比例估算")
    );
    assert_eq!(lines[2].label.as_deref(), Some("来源"));
    assert_eq!(lines[2].value, "developers.openai.com/api/docs/pricing");
    assert_eq!(
        lines[2].tooltip.as_deref(),
        Some("https://developers.openai.com/api/docs/pricing")
    );
    let lines = cost_lines(&en, &row);
    assert_eq!(lines[1].value, "At OpenAI list prices · 2026-10-17");
    assert_eq!(
        lines[1].tooltip.as_deref(),
        Some("Estimate at OpenAI list prices × the provider's ratio")
    );
    // A row recorded with the revision only (before sources were stored).
    row.price_rev = Some("2026-10".into());
    assert_eq!(values(&zh, &row).len(), 2);
    // A price set on the Models page.
    row.price_rev = Some("override".into());
    assert_eq!(values(&zh, &row)[1], "按「模型」页的价格估算");
    // Usage but no price: "—" and where to set one.
    row.cost = None;
    row.price_rev = None;
    assert_eq!(values(&zh, &row), ["—", "无官方价格，可在「模型」页设置"]);
    assert_eq!(
        cost_lines(&zh, &row)[1].tooltip.as_deref(),
        Some("没有 OpenAI 官方价格；可在「模型」页设置价格")
    );
    assert_eq!(
        values(&en, &row),
        ["—", "No official price; set one in Models"]
    );
    // No usage at all.
    row.input_tokens = None;
    row.output_tokens = None;
    row.status = RequestStatus::Ok;
    assert_eq!(values(&en, &row), ["—", "No usage was reported"]);
}

#[test]
fn status_cells_are_localized_labels_with_the_code_only_in_the_tooltip() {
    // Review ux-visual #3: "failed:response_stream_disconnected" in every locale.
    let now = Timestamp::now();
    let mut row = rows()[0].clone();
    row.failovers = 0;
    row.status = RequestStatus::Failed {
        code: Some("response_stream_disconnected".into()),
    };
    row.error = None;
    for (locale, label, reason) in [
        (Locale::ZhCn, "失败", "断流"),
        (Locale::En, "Failed", "Disconnected"),
    ] {
        let tr = Tr::new(locale);
        assert_eq!(requests::status_cell(&tr, &row).1, label);
        let cell = requests::cell(&tr, &titles(), &row, Col::Status, now);
        assert_eq!(cell.text, format!("{label} · {reason}"));
        assert_eq!(
            cell.short.as_deref(),
            Some(label),
            "falls back to the label"
        );
        assert!(!cell.text.contains("response_stream"), "{}", cell.text);
        assert_eq!(
            cell.tooltip.as_deref(),
            Some(format!("{label} · {reason} · response_stream_disconnected").as_str())
        );
        assert_eq!(
            requests::status_detail(&tr, &row),
            format!("{label} · {reason} · response_stream_disconnected")
        );
    }
    let zh = Tr::new(Locale::ZhCn);
    // An unknown code: the label alone in the cell, the code in the tooltip.
    row.status = RequestStatus::Failed {
        code: Some("weird_thing".into()),
    };
    let cell = requests::cell(&zh, &titles(), &row, Col::Status, now);
    assert_eq!(cell.text, "失败");
    assert_eq!(cell.tooltip.as_deref(), Some("失败 · weird_thing"));
    // HTTP errors: the status code, a reason from the status, "HTTP n" in full.
    row.status = RequestStatus::Http {
        status: 429,
        code: None,
    };
    assert_eq!(
        requests::cell(&zh, &titles(), &row, Col::Status, now).text,
        "429 · 限流"
    );
    assert_eq!(requests::status_detail(&zh, &row), "HTTP 429 · 限流");
    row.status = RequestStatus::Http {
        status: 503,
        code: Some("router_unavailable".into()),
    };
    assert_eq!(
        requests::status_detail(&zh, &row),
        "HTTP 503 · 无可用供应商 · router_unavailable"
    );
    // Short English labels fit the 72 px column.
    let en = Tr::new(Locale::En);
    for (status, label) in [
        (RequestStatus::Cancelled, "Stopped"),
        (RequestStatus::Broken, "Cut off"),
        (RequestStatus::Incomplete { reason: None }, "Partial"),
    ] {
        row.status = status;
        assert_eq!(requests::status_cell(&en, &row).1, label);
    }
}

#[test]
fn the_session_column_grows_and_narrow_tables_hide_low_value_columns() {
    // Review ux-visual #13: at 1440 the extra width went to gaps and 供应商;
    // at 960 numbers were elided.
    let tr = Tr::new(Locale::ZhCn);
    let cols = requests::columns(&tr);
    let index = |c: Col| Col::ALL.iter().position(|x| *x == c).unwrap();
    let sum = |w: &[f32]| w.iter().sum::<f32>();
    // 1280x800: every column at about its preferred width.
    let w = column_widths(&cols, 834.0);
    assert!((sum(&w) - 834.0).abs() < 0.01);
    assert!(w.iter().all(|w| *w > 0.0));
    for c in Col::ALL {
        assert!(w[index(c)] >= c.width() - 0.01, "{c:?}");
    }
    // 1440x900: the session name (then the status) gets the extra width.
    let wide = column_widths(&cols, 964.0);
    assert!((sum(&wide) - 964.0).abs() < 0.01);
    let gained = |c: Col| wide[index(c)] - w[index(c)];
    assert!(gained(Col::Session) > 80.0, "{}", gained(Col::Session));
    assert!(gained(Col::Status) > 20.0);
    assert!(gained(Col::Provider).abs() < 0.01);
    assert!(gained(Col::Input).abs() < 0.01);
    // 960x640: cache write, cache key, TPS and cache read are hidden; the rest
    // keep at least their minimum (no elided digits).
    let narrow = column_widths(&cols, 530.0);
    assert!((sum(&narrow) - 530.0).abs() < 0.01);
    for c in [Col::CacheWrite, Col::CacheKey, Col::Tps, Col::CacheRead] {
        assert_eq!(narrow[index(c)], 0.0, "{c:?} hidden");
    }
    for c in Col::ALL.into_iter().filter(|c| c.drop_rank().is_none()) {
        assert!(narrow[index(c)] >= c.min_width() - 0.01, "{c:?}");
    }
    // 合计 moves to the first shown column, with the count alone as its
    // fallback.
    let mut totals = requests::totals_cells(&tr, &Default::default(), &[]);
    requests::fold_hidden_totals(&mut totals, &narrow);
    assert_eq!(totals[index(Col::CacheKey)].text, "");
    assert_eq!(totals[index(Col::Date)].text, "合计 0 条");
    assert_eq!(totals[index(Col::Date)].short.as_deref(), Some("0 条"));
    // In between only what does not fit is hidden, cache write first.
    let mid = column_widths(&cols, 720.0);
    assert_eq!(mid[index(Col::CacheWrite)], 0.0);
    assert!(mid[index(Col::CacheKey)] > 0.0);
}

#[test]
fn hourly_buckets_cover_the_last_24_hours_in_local_time() {
    use superba_gui::pages::router::requests::hourly_buckets;
    let now = Timestamp::from_millis(1_760_700_000_000); // 11:20 UTC
    let mut a = rows()[0].clone();
    a.started_at = now;
    let mut b = a.clone();
    b.started_at = Timestamp::from_millis(now.as_millis() - 3 * 3_600_000);
    let mut old = a.clone();
    old.started_at = Timestamp::from_millis(now.as_millis() - 30 * 3_600_000);
    let buckets = hourly_buckets(&[a, b, old], now, 8 * 3600);
    assert_eq!(buckets.len(), 24);
    // UTC+8: 19:20 local; the last bucket is 19:00, three hours back 16:00.
    assert_eq!(buckets[23].key, "19:00");
    assert_eq!(buckets[23].totals.requests, 1);
    assert_eq!(buckets[20].key, "16:00");
    assert_eq!(buckets[20].totals.requests, 1);
    assert_eq!(buckets[0].key, "20:00");
    assert_eq!(
        buckets.iter().map(|b| b.totals.requests).sum::<u64>(),
        2,
        "the row from 30 hours ago is outside"
    );
}

#[test]
fn provider_sub_entries_add_up_with_a_line_unknown_entry() {
    use superba_core::{RequestGroup, RequestGrouping, provider_line_key};
    // Review ux-visual #17: Krill 172 over 海外线路 91 + 国内极速 71; the 10 rows
    // without a line (rollouts, failures before any line) could not be chosen.
    let group = |key: String, label: &str, count: u64, lines: Vec<RequestGroup>| RequestGroup {
        key,
        label: label.into(),
        count,
        last: Timestamp::from_millis(1_760_000_000_000),
        upstream_host: None,
        lines,
    };
    let groups = vec![
        group(
            "krill".into(),
            "Krill",
            172,
            vec![
                group(
                    provider_line_key("krill", "海外线路"),
                    "海外线路",
                    91,
                    vec![],
                ),
                group(
                    provider_line_key("krill", "国内极速"),
                    "国内极速",
                    71,
                    vec![],
                ),
            ],
        ),
        // One line plus rollout rows: the line and the unknown part.
        group(
            "my-api".into(),
            "My API",
            8,
            vec![group(
                provider_line_key("my-api", "default"),
                "default",
                5,
                vec![],
            )],
        ),
        // Rollout rows only: no sub-entries.
        group("other".into(), "Other", 4, vec![]),
    ];
    for (locale, unknown) in [(Locale::ZhCn, "线路未知"), (Locale::En, "Line unknown")] {
        let tr = Tr::new(locale);
        let entries = requests::source_entries(&tr, RequestGrouping::Provider, &groups, &titles());
        let shown: Vec<(&str, u64, bool)> = entries
            .iter()
            .map(|e| (e.label.as_str(), e.count, e.child))
            .collect();
        assert_eq!(
            shown,
            [
                ("Krill", 172, false),
                ("海外线路", 91, true),
                ("国内极速", 71, true),
                (unknown, 10, true),
                ("My API", 8, false),
                ("default", 5, true),
                (unknown, 3, true),
                ("Other", 4, false),
            ]
        );
        // The entry filters to the provider's rows without a line.
        assert_eq!(entries[3].key, provider_line_key("krill", ""));
    }
    let mut row = rows()[0].clone();
    row.member_id = ProfileId::new("krill");
    row.line_id = None;
    row.line_name = None;
    assert!(row.in_provider_group(&provider_line_key("krill", "")));
}
