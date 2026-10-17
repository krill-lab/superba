//! `spb usage`: the request log without a subcommand, `log` and `stats`, rows
//! from Codex's session files marked as such; the router's log and stats
//! subcommands are gone.

mod common;

use common::*;
use superba_core::*;

#[tokio::test]
async fn usage_lists_the_log_without_a_subcommand_and_marks_session_file_rows() {
    let service = FakeService::new();
    {
        let mut v3 = service.v3.lock().unwrap();
        let mut rollout = v3.page.rows[0].clone();
        rollout.id = -7;
        rollout.source = RequestSource::Rollout;
        rollout.line_id = None;
        rollout.line_name = None;
        rollout.first_token_ms = None;
        rollout.tps_milli = None;
        v3.page.rows.push(rollout);
    }
    let (code, stdout, stderr) = invoke(service.clone(), &["usage", "--since", "3h"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Source"), "{stdout}");
    assert!(stdout.contains("router"), "{stdout}");
    assert!(stdout.contains("session file"), "{stdout}");
    let q = service.v3.lock().unwrap().queries[0].clone();
    assert!(q.from.is_some());
    let (code, stdout, _) = invoke(service.clone(), &["--locale", "zh-CN", "usage", "log"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("来源"), "{stdout}");
    assert!(stdout.contains("会话记录"), "{stdout}");
    let (code, _, _) = invoke(service.clone(), &["usage", "stats", "--by", "day"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        service.v3.lock().unwrap().stats_queries[0].by,
        StatsGroupBy::Day
    );
    use clap::Parser;
    for gone in [["spb", "router", "log"], ["spb", "router", "stats"]] {
        assert!(superba_cli::Cli::try_parse_from(gone).is_err(), "{gone:?}");
    }
}
