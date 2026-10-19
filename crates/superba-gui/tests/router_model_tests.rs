#![allow(missing_docs)]

use pretty_assertions::assert_eq;
use superba_gui::pages::router::model::*;

fn member(healths: &[LineHealth]) -> MemberView {
    MemberView {
        protocols: Vec::new(),
        last_route: None,
        id: "relay".into(),
        name: "Example Relay".into(),
        kind: MemberKind::Compatible,
        lines: healths
            .iter()
            .enumerate()
            .map(|(index, &health)| LineView {
                id: format!("line-{index}"),
                name: format!("Line {index}"),
                base_url: "https://api.example.com/v1".into(),
                health,
                metrics: None,
            })
            .collect(),
        websocket: false,
        model_mappings: 0,
        has_key: true,
    }
}

#[test]
fn loopback_upstreams_are_direct_local_services_without_recorded_routing() {
    assert_eq!(Mode::direct(None), Mode::Direct);
    for (url, address) in [
        ("http://127.0.0.1:11434/v1", "127.0.0.1:11434"),
        ("http://localhost:1234/backend-api/codex", "localhost:1234"),
        ("http://[::1]:8123/openai/v1", "[::1]:8123"),
        ("https://localhost/v1", "localhost:443"),
        ("http://localhost/v1", "localhost:80"),
    ] {
        assert_eq!(
            Mode::direct(Some(url)),
            Mode::Local {
                address: address.into()
            }
        );
    }
}

#[test]
fn non_local_and_invalid_upstreams_are_direct() {
    for url in [
        "https://api.example.com/v1",
        "http://localhost.example.com:8123/v1",
        "http://203.0.113.2:8123/v1",
        "not a URL",
        "http://[",
        "/relative/path",
    ] {
        assert_eq!(Mode::direct(Some(url)), Mode::Direct);
    }
}

#[test]
fn missing_configuration_means_unknown_mode() {
    assert_eq!(Mode::from_config(None, false, None), Mode::Unknown);
}

#[test]
fn is_router_is_true_only_for_router_mode() {
    assert!(Mode::Router { port: 8123 }.is_router());
    assert!(!Mode::Direct.is_router());
    assert!(!Mode::Unknown.is_router());
}

#[test]
fn reorder_up_swaps_only_the_previous_neighbour() {
    let items = ["a", "b", "c", "d"];
    assert_eq!(reorder(&items, 2, true), Some(vec!["a", "c", "b", "d"]));
    assert_eq!(items, ["a", "b", "c", "d"]);
}

#[test]
fn reorder_down_swaps_only_the_next_neighbour() {
    let items = ["a", "b", "c", "d"];
    assert_eq!(reorder(&items, 1, false), Some(vec!["a", "c", "b", "d"]));
    assert_eq!(items, ["a", "b", "c", "d"]);
}

#[test]
fn reorder_cannot_move_past_either_end() {
    let items = ["a", "b", "c"];
    assert_eq!(reorder(&items, 0, true), None);
    assert_eq!(reorder(&items, 2, false), None);
    assert_eq!(reorder(&items, 0, false), Some(vec!["b", "a", "c"]));
    assert_eq!(reorder(&items, 2, true), Some(vec!["a", "c", "b"]));
}

#[test]
fn reorder_rejects_out_of_range_indices_without_overflow() {
    let items = ["a", "b", "c"];
    for from in [items.len(), items.len() + 1, usize::MAX] {
        for up in [false, true] {
            assert_eq!(reorder(&items, from, up), None, "from={from}, up={up}");
        }
    }
}

#[test]
fn reorder_cannot_move_empty_or_single_element_lists() {
    for up in [false, true] {
        assert_eq!(reorder::<&str>(&[], 0, up), None);
        assert_eq!(reorder(&["only"], 0, up), None);
    }
}

#[test]
fn duration_text_formats_truncated_seconds_minutes_and_hours() {
    for (milliseconds, expected) in [
        (0, "0s"),
        (999, "0s"),
        (1_000, "1s"),
        (59_999, "59s"),
        (60_000, "1m 00s"),
        (61_999, "1m 01s"),
        (3_599_999, "59m 59s"),
        (3_600_000, "1h 00m"),
        (3_725_000, "1h 02m"),
    ] {
        assert_eq!(duration_text(milliseconds), expected, "{milliseconds} ms");
    }
}

#[test]
fn count_text_groups_thousands_through_u64_max() {
    for (count, expected) in [
        (0, "0"),
        (999, "999"),
        (1_000, "1,000"),
        (1_234_567, "1,234,567"),
        (u64::MAX, "18,446,744,073,709,551,615"),
    ] {
        assert_eq!(count_text(count), expected);
    }
}

#[test]
fn healthy_lines_counts_up_and_probing_but_not_down_or_unknown() {
    for (health, expected) in [
        (LineHealth::Up, 1),
        (LineHealth::Probing, 1),
        (LineHealth::Down, 0),
        (LineHealth::Unknown, 0),
    ] {
        assert_eq!(member(&[health]).healthy_lines(), expected, "{health:?}");
    }
    assert_eq!(
        member(&[
            LineHealth::Up,
            LineHealth::Down,
            LineHealth::Probing,
            LineHealth::Unknown,
            LineHealth::Up
        ])
        .healthy_lines(),
        3
    );
    assert_eq!(member(&[]).healthy_lines(), 0);
}
