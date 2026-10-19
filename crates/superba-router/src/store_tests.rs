use super::*;
use superba_core::RequestProtocol;

fn database() -> (tempfile::TempDir, Reader) {
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::from_dir(dir.path());
    let conn = open_write(&paths.usage_db(), USAGE_SCHEMA).unwrap();
    for (id, harness, protocol, model, requested, session, host) in [
        (
            1,
            "codex",
            "responses",
            "gpt-mini",
            "gpt-mini",
            Some("shared"),
            None,
        ),
        (
            2,
            "claude_code",
            "messages",
            "gpt-mini",
            "haiku",
            Some("shared"),
            None,
        ),
        (
            3,
            "dsh",
            "chat_completions",
            "gpt-mini",
            "gpt-mini",
            Some("shared"),
            Some("wsl:Ubuntu"),
        ),
        (4, "pi", "responses", "gpt-large", "gpt-large", None, None),
    ] {
        conn.execute("INSERT INTO requests (id, started_ms, member_id, member_name, transport, client, status_kind, harness, protocol, model, requested_model, session_id, prompt_cache_key, codex_host, input_tokens, output_tokens, cost_micros) VALUES (?1, ?1 * 1000, 'provider', 'Provider', 'http', 'other', 'ok', ?2, ?3, ?4, ?5, ?6, 'cache-key', ?7, 100, 20, 5)", rusqlite::params![id, harness, protocol, model, requested, session, host]).unwrap();
    }
    conn.execute(
        "UPDATE requests SET upstream_model = model, model = 'claude-alias' WHERE id = 2",
        [],
    )
    .unwrap();
    (dir, Reader::open(&paths).unwrap())
}

#[test]
fn tools_models_hosts_and_protocol_filter_before_paging_and_aggregation() {
    let (_dir, reader) = database();
    let page = reader
        .query(&RequestQuery {
            harnesses: vec![RequestHarness::ClaudeCode, RequestHarness::Dsh],
            model: Some("gpt-mini".into()),
            limit: 1,
            ..RequestQuery::default()
        })
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.truncated);
    assert_eq!(page.totals.requests, 2);
    assert_eq!(page.totals.input_tokens, 200);
    let stats = reader
        .stats(&StatsQuery {
            harnesses: vec![RequestHarness::ClaudeCode, RequestHarness::Dsh],
            model: Some("gpt-mini".into()),
            by: StatsGroupBy::Harness,
            ..StatsQuery::default()
        })
        .unwrap();
    assert_eq!(stats.totals, page.totals);
    assert_eq!(stats.buckets.len(), 2);
    for (model, host, protocol, count) in [
        (
            Some("haiku"),
            Some("local"),
            Some(RequestProtocol::Messages),
            1,
        ),
        (Some("claude-alias"), None, None, 1),
        (None, Some("wsl:Ubuntu"), None, 1),
        (Some("gpt-large"), Some("wsl:Ubuntu"), None, 0),
    ] {
        let q = RequestQuery {
            model: model.map(str::to_owned),
            host: host.map(str::to_owned),
            protocol,
            ..RequestQuery::default()
        };
        assert_eq!(reader.query(&q).unwrap().totals.requests, count);
        assert_eq!(
            reader
                .stats(&StatsQuery {
                    model: q.model,
                    host: q.host,
                    protocol,
                    ..StatsQuery::default()
                })
                .unwrap()
                .totals
                .requests,
            count
        );
    }
}

#[test]
fn sessions_are_scoped_to_tools_and_use_the_tools_own_id() {
    let (_dir, reader) = database();
    let page = reader.query(&RequestQuery::default()).unwrap();
    assert_eq!(
        page.groups.len(),
        3,
        "a missing pi session must not use a Codex cache key"
    );
    for harness in [
        RequestHarness::Codex,
        RequestHarness::ClaudeCode,
        RequestHarness::Dsh,
    ] {
        let key = superba_core::harness_group_key(harness, "shared");
        let group = page.groups.iter().find(|g| g.key == key).unwrap();
        assert_eq!(group.count, 1);
        let selected = reader
            .query(&RequestQuery {
                group: Some(key),
                ..RequestQuery::default()
            })
            .unwrap();
        assert_eq!(selected.rows.len(), 1);
        assert_eq!(selected.rows[0].harness, harness);
    }
    // SQL session sorting must treat the complete expression as one column.
    let sorted = reader
        .query(&RequestQuery {
            sort: superba_core::RequestSort {
                key: RequestSortKey::Session,
                descending: false,
            },
            ..RequestQuery::default()
        })
        .unwrap();
    assert_eq!(sorted.rows.len(), 4);
}

#[test]
fn tool_and_model_groups_keep_distinct_tools_on_the_same_model() {
    let (_dir, reader) = database();
    for (group_by, by) in [
        (RequestGrouping::Harness, StatsGroupBy::Harness),
        (RequestGrouping::HarnessModel, StatsGroupBy::HarnessModel),
    ] {
        let page = reader
            .query(&RequestQuery {
                group_by,
                ..RequestQuery::default()
            })
            .unwrap();
        let stats = reader
            .stats(&StatsQuery {
                by,
                ..StatsQuery::default()
            })
            .unwrap();
        assert_eq!(page.groups.len(), 4);
        assert_eq!(stats.buckets.len(), 4);
        for g in page.groups {
            assert_eq!(
                reader
                    .query(&RequestQuery {
                        group_by,
                        group: Some(g.key.clone()),
                        ..RequestQuery::default()
                    })
                    .unwrap()
                    .totals
                    .requests,
                1
            );
            assert_eq!(
                stats
                    .buckets
                    .iter()
                    .find(|b| b.key == g.key)
                    .unwrap()
                    .totals
                    .requests,
                1
            );
        }
    }
}
