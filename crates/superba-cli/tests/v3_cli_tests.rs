//! v0.3 commands over the fake service: models, speed, router request log and
//! stats settings, Codex versions and the app pin, app updates, remote hosts
//! (probe, follow, sync, Codex), remote sessions and feeds; human output in
//! both languages, one JSON document each.

mod common;

use common::*;
use superba_core::*;

async fn run(service: &std::sync::Arc<FakeService>, args: &[&str]) -> (u8, String, String) {
    invoke(service.clone(), args).await
}

/// The fake service with a second, not applied profile `other`.
fn service() -> std::sync::Arc<FakeService> {
    let s = FakeService::new();
    s.profiles
        .lock()
        .unwrap()
        .profiles
        .push(profile("other", "krill"));
    s
}

async fn run_zh(service: &std::sync::Arc<FakeService>, args: &[&str]) -> (u8, String, String) {
    let mut all = vec!["--locale", "zh-CN"];
    all.extend_from_slice(args);
    invoke(service.clone(), &all).await
}

fn transcript(command: &str, code: u8, stdout: &str, stderr: &str) {
    println!("$ {command}");
    for (stream, contents) in [("stdout", stdout), ("stderr", stderr)] {
        if !contents.is_empty() {
            println!("[{stream}]\n{}", contents.trim_end());
        }
    }
    println!("[exit {code}]\n");
}

// --- models -------------------------------------------------------------------

#[tokio::test]
async fn models_list_shows_source_speed_and_models_in_both_languages() {
    let service = service();
    let (code, stdout, stderr) = run(&service, &["models"]).await;
    transcript("spb models", code, &stdout, &stderr);
    assert_eq!(code, 0);
    for value in [
        "The provider (/backend-api/codex/models?client_version=0.162.0)",
        "gpt-6-astra",
        "272,000",
        "Standard, Fast, Ultrafast",
        "The desktop app runs Standard in API-key mode",
        "No model catalog file",
    ] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
    let (code, stdout, stderr) = run_zh(&service, &["models"]).await;
    transcript("spb --locale zh-CN models", code, &stdout, &stderr);
    for value in ["模型列表", "供应商", "速度档", "上下文"] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
    let (_, stdout, _) = run(&service, &["models", "--json", "--profile", "work"]).await;
    let view = service.v3.lock().unwrap().models.clone();
    assert_eq!(one_json(&stdout), serde_json::to_value(&view).unwrap());
}

#[tokio::test]
async fn models_set_parses_tokens_and_percent_and_switches_the_applied_profile() {
    let service = service();
    let (code, stdout, stderr) = run(
        &service,
        &[
            "models",
            "set",
            "gpt-6-astra",
            "--context",
            "400k",
            "--compact",
            "80%",
            "--yes",
        ],
    )
    .await;
    transcript(
        "spb models set gpt-6-astra --context 400k --compact 80% --yes",
        code,
        &stdout,
        &stderr,
    );
    assert_eq!(code, 0, "{stderr}");
    let edit = service.v3.lock().unwrap().model_edits[0].clone();
    let o = &edit.set["gpt-6-astra"];
    assert_eq!(o.context_window, Some(400_000));
    assert_eq!(o.auto_compact, Some(320_000));
    // `work` is applied: the switch runs right away.
    let calls = service.calls();
    assert!(calls.names.contains(&"plan"), "{:?}", calls.names);
    assert!(matches!(
        calls.plans[0].intent,
        SwitchIntent::Profile { ref profile_id, .. } if profile_id.as_str() == "work"
    ));
    assert!(stdout.contains("switching now"));
}

#[tokio::test]
async fn models_dry_run_stores_nothing_and_names_the_catalog_reasons() {
    let service = service();
    let (code, stdout, _) = run(
        &service,
        &[
            "models",
            "set",
            "gpt-6-astra",
            "--context",
            "800k",
            "--dry-run",
            "--json",
        ],
    )
    .await;
    assert_eq!(code, 0);
    let doc = one_json(&stdout);
    assert_eq!(doc["outcome"], "dry_run");
    assert_eq!(doc["service_tier"], "default");
    assert!(service.v3.lock().unwrap().model_edits.is_empty());
    assert!(!service.calls().names.contains(&"plan"));
    // PAT applies the per-model override through a catalog.
    assert_eq!(doc["catalog_reasons"], serde_json::json!(["overrides"]));
    let (_, stdout, _) = run(
        &service,
        &["speed", "ultrafast", "--profile", "other", "--dry-run"],
    )
    .await;
    assert!(
        stdout.contains("A model catalog file is written at the switch (Ultrafast)."),
        "{stdout}"
    );
    assert!(stdout.contains("service_tier: ultrafast"), "{stdout}");
}

#[tokio::test]
async fn models_default_reset_and_picker_build_edits() {
    let service = service();
    for args in [
        vec![
            "models",
            "default",
            "gpt-6-luna",
            "--effort",
            "high",
            "--context",
            "300k",
            "--profile",
            "other",
        ],
        vec!["models", "reset", "--all", "--profile", "other"],
        vec!["models", "reset", "gpt-6-astra", "--profile", "other"],
        vec!["models", "picker", "on", "--profile", "other"],
    ] {
        let (code, _, stderr) = run(&service, &args).await;
        assert_eq!(code, 0, "{args:?}: {stderr}");
    }
    let edits = service.v3.lock().unwrap().model_edits.clone();
    let d = edits[0].defaults.clone().unwrap();
    assert_eq!(d.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(d.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(d.context_window, Some(300_000));
    assert!(edits[1].reset_all);
    assert_eq!(edits[2].reset, vec!["gpt-6-astra".to_owned()]);
    assert_eq!(edits[3].picker, Some(ModelPicker::Provider));
    // Not applied: nothing is switched.
    assert!(!service.calls().names.contains(&"plan"));
    // A bad token count is a usage error that never echoes other input.
    let (code, _, stderr) = run(&service, &["models", "set", "x", "--context", "lots"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("Not a token count: lots"), "{stderr}");
}

#[tokio::test]
async fn speed_shows_the_per_consumer_effect_and_sets_the_tier() {
    let service = service();
    let (code, stdout, stderr) = run(&service, &["speed"]).await;
    transcript("spb speed", code, &stdout, &stderr);
    assert!(stdout.contains("Terminal Codex: Applies"), "{stdout}");
    let (_, stdout, _) = run(&service, &["speed", "--json"]).await;
    let doc = one_json(&stdout);
    assert_eq!(doc["service_tier"], "default");
    assert_eq!(doc["support"]["app"]["kind"], "app_forces_standard");
    let (code, stdout, stderr) = run_zh(&service, &["speed", "fast", "--profile", "other"]).await;
    transcript(
        "spb --locale zh-CN speed fast --profile other",
        code,
        &stdout,
        &stderr,
    );
    assert_eq!(code, 0);
    let edit = service.v3.lock().unwrap().model_edits[0].clone();
    assert_eq!(edit.defaults.unwrap().speed, Some(SpeedTier::Fast));
    assert!(stdout.contains("下次切换"), "{stdout}");
}

// --- router request log -------------------------------------------------------------

#[tokio::test]
async fn router_log_lists_rows_with_session_names_and_totals() {
    let service = service();
    let (code, stdout, stderr) = run(
        &service,
        &[
            "usage", "log", "--since", "2h", "--errors", "--client", "app",
        ],
    )
    .await;
    transcript(
        "spb usage log --since 2h --errors --client app",
        code,
        &stdout,
        &stderr,
    );
    assert_eq!(code, 0);
    for value in [
        "Refactor the parser",
        "12,345",
        "Work · Overseas",
        "0.82s",
        "87.0",
        "SSH hosts' session files stay on the host",
    ] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
    let q = service.v3.lock().unwrap().queries[0].clone();
    assert_eq!(q.status, RequestStatusFilter::Errors);
    assert_eq!(q.client, Some(RequestClient::Desktop));
    assert!(q.from.is_some());
    let (_, stdout, _) = run(&service, &["usage", "log", "--json"]).await;
    let page = service.v3.lock().unwrap().page.clone();
    assert_eq!(one_json(&stdout), serde_json::to_value(&page).unwrap());
    let (_, stdout, _) = run_zh(&service, &["usage", "log"]).await;
    for value in ["缓存读取", "首字", "完成时间", "费用"] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
    let (code, _, _) = run(&service, &["usage", "log", "--failover"]).await;
    assert_eq!(code, 0);
    let q = service.v3.lock().unwrap().queries.last().cloned().unwrap();
    assert_eq!(q.status, RequestStatusFilter::Failover);
}

#[tokio::test]
async fn router_stats_by_provider_lists_each_providers_lines_and_log_filters_by_provider() {
    let service = service();
    let bucket = |key: &str, label: &str, requests: u64, lines: Vec<StatsBucket>| StatsBucket {
        key: key.into(),
        label: label.into(),
        totals: RequestTotals {
            requests,
            ..RequestTotals::default()
        },
        lines,
    };
    service.v3.lock().unwrap().stats = RequestStats {
        buckets: vec![
            bucket(
                "krill",
                "Krill",
                30,
                vec![
                    bucket(
                        &provider_line_key("krill", "China Express"),
                        "China Express",
                        20,
                        vec![],
                    ),
                    bucket(
                        &provider_line_key("krill", "Overseas"),
                        "Overseas",
                        10,
                        vec![],
                    ),
                ],
            ),
            bucket(
                "my-api",
                "My API",
                5,
                vec![bucket(
                    &provider_line_key("my-api", "default"),
                    "default",
                    5,
                    vec![],
                )],
            ),
        ],
        ..RequestStats::default()
    };
    let (code, stdout, stderr) = run(&service, &["usage", "stats", "--by", "provider"]).await;
    transcript("spb usage stats --by provider", code, &stdout, &stderr);
    assert_eq!(code, 0);
    assert_eq!(
        service.v3.lock().unwrap().stats_queries[0].by,
        StatsGroupBy::Provider
    );
    let lines: Vec<&str> = stdout.lines().collect();
    let at = |label: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(label))
            .unwrap_or_else(|| panic!("{label:?} missing: {stdout}"))
    };
    assert!(lines.iter().any(|l| l.starts_with("Provider ")), "{stdout}");
    let krill = at("Krill ");
    assert_eq!(at("  China Express "), krill + 1, "{stdout}");
    assert_eq!(at("  Overseas "), krill + 2, "{stdout}");
    assert!(at("My API ") > krill);
    assert!(
        !stdout.contains("  default"),
        "a single line is not repeated: {stdout}"
    );
    let (_, stdout, _) = run_zh(&service, &["usage", "stats", "--by", "provider"]).await;
    assert!(stdout.lines().any(|l| l.starts_with("供应商")), "{stdout}");
    use clap::Parser;
    for gone in ["member", "host"] {
        assert!(
            superba_cli::Cli::try_parse_from(["spb", "usage", "stats", "--by", gone]).is_err(),
            "--by {gone}"
        );
    }
    assert!(superba_cli::Cli::try_parse_from(["spb", "usage", "log", "--member", "work"]).is_err());
    let (code, _, _) = run(&service, &["usage", "log", "--provider", "work"]).await;
    assert_eq!(code, 0);
    let q = service.v3.lock().unwrap().queries.last().cloned().unwrap();
    assert_eq!(q.member, Some(ProfileId::new("work")));
}

#[tokio::test]
async fn router_stats_line_record_and_clear_change_the_stats_settings() {
    let service = service();
    let (code, _, stderr) = run(
        &service,
        &[
            "router",
            "stats-line",
            "on",
            "--clients",
            "app,tui",
            "--template",
            "⚡ {tps} tok/s",
            "--italic",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let s = service.v3.lock().unwrap().stats_settings[0].clone();
    assert!(s.line.enabled);
    assert_eq!(
        s.line.clients,
        vec![StatsLineClient::Desktop, StatsLineClient::Tui]
    );
    assert_eq!(s.line.template.as_deref(), Some("⚡ {tps} tok/s"));
    assert_eq!(s.line.style, StatsLineStyle::QuoteItalic);
    let (code, stdout, _) = run(&service, &["router", "record", "on"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("numbers only"), "{stdout}");
    let s = service.v3.lock().unwrap().stats_settings[1].clone();
    assert!(s.record);
    // Request content is never recorded: no flag for it.
    use clap::Parser;
    for gone in [
        &["spb", "router", "record", "on", "--content", "on"][..],
        &["spb", "router", "clear", "--content-only", "--yes"][..],
    ] {
        assert!(superba_cli::Cli::try_parse_from(gone).is_err(), "{gone:?}");
    }
    let (code, _, stderr) = run(&service, &["router", "clear"]).await;
    assert_eq!(code, 2);
    assert!(stderr.contains("--yes"), "{stderr}");
    let (code, stdout, _) = run(&service, &["router", "clear", "--older-than", "30d"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Deleted 3 requests."), "{stdout}");
    let c = service.v3.lock().unwrap().clears[0].clone();
    assert!(c.before.is_some());
    let (code, _, _) = run(
        &service,
        &["usage", "stats", "--by", "day", "--since", "7d", "--json"],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(
        service.v3.lock().unwrap().stats_queries[0].by,
        StatsGroupBy::Day
    );
    assert!(
        superba_cli::Cli::try_parse_from(["spb", "router", "clear", "--older-than", "soon"])
            .is_err()
    );
}

// --- versions, pin, app ---------------------------------------------------------------

#[tokio::test]
async fn codex_list_install_and_pin() {
    let service = service();
    let (code, stdout, stderr) = run(&service, &["codex", "list"]).await;
    transcript("spb codex list", code, &stdout, &stderr);
    for value in [
        "0.163.0",
        "Compatible",
        "npm install -g @openai/codex@0.163.0",
        "0.141.0",
    ] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
    let (code, stdout, _) = run(&service, &["codex", "install", "0.162.0"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Codex 0.162.0 is installed"));
    let (code, _, _) = run(&service, &["codex", "install", "0.162.0", "--local"]).await;
    assert_eq!(code, 0);
    let installs = service.v3.lock().unwrap().installs.clone();
    assert_eq!(installs[0].target, InstallTarget::Store);
    assert_eq!(installs[1].target, InstallTarget::Local);

    // `/opt/..` is rooted but not absolute on Windows (the CLI would add a drive).
    let pinned = std::env::temp_dir()
        .join("opt")
        .join("codex")
        .join("bin")
        .join("codex");
    let pinned_arg = pinned.to_str().unwrap().to_owned();
    for (args, expected) in [
        (
            vec!["codex", "pin", "0.162.0", "--dry-run"],
            AppCodexPin::Version {
                version: CodexVersion::new(0, 162, 0),
            },
        ),
        (
            vec!["codex", "pin", "bundled", "--dry-run"],
            AppCodexPin::Bundled,
        ),
        (vec!["codex", "unpin", "--dry-run"], AppCodexPin::Bundled),
        (
            vec!["codex", "pin", pinned_arg.as_str(), "--dry-run"],
            AppCodexPin::Path { path: pinned },
        ),
    ] {
        let (code, _, stderr) = run(&service, &args).await;
        assert_eq!(code, 0, "{args:?}: {stderr}");
        let plan = service.calls().plans.last().cloned().unwrap();
        assert_eq!(plan.intent, SwitchIntent::AppCodexPin { pin: expected });
    }
    let (code, _, _) = run(&service, &["codex", "install", "not-a-version"]).await;
    assert_eq!(code, 2);
}

#[tokio::test]
async fn app_check_and_download_show_the_command_and_never_run_it() {
    let service = service();
    let (code, stdout, stderr) = run(&service, &["app", "check"]).await;
    transcript("spb app check", code, &stdout, &stderr);
    assert_eq!(code, 0);
    assert!(stdout.contains("The app is up to date."));
    let (code, stdout, stderr) = run(&service, &["app", "download"]).await;
    transcript("spb app download", code, &stdout, &stderr);
    assert_eq!(code, 0);
    assert!(stdout.contains("sudo apt install /fake/downloads/chatgpt_26.1013_amd64.deb"));
    let (_, stdout, _) = run(&service, &["app", "check", "--json"]).await;
    let view = service.v3.lock().unwrap().versions.app.clone();
    assert_eq!(one_json(&stdout), serde_json::to_value(&view).unwrap());
}

// --- remote hosts -------------------------------------------------------------------------

#[tokio::test]
async fn remote_probe_follow_and_sync() {
    let service = service();
    let (code, stdout, _) = run(&service, &["remote", "probe", "dev"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("ssh:dev"));
    let (code, stdout, _) = run(&service, &["remote", "follow", "dev", "on"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("follows this computer"));
    assert_eq!(
        service.v3.lock().unwrap().follows,
        vec![(TargetId::remote("dev"), true)]
    );

    let (code, stdout, stderr) = run(&service, &["remote", "sync", "dev", "--dry-run"]).await;
    transcript("spb remote sync dev --dry-run", code, &stdout, &stderr);
    assert_eq!(code, 0);
    assert!(stdout.contains("model_verbosity"));
    assert!(stdout.contains("features.new_thing"));
    assert!(stdout.contains("Skip"));
    assert!(!service.calls().names.contains(&"plan"));

    let (code, _, stderr) = run(
        &service,
        &[
            "remote",
            "sync",
            "ssh:dev",
            "--only",
            "model_verbosity",
            "--yes",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let plan = service.calls().plans.last().cloned().unwrap();
    assert!(!plan.targets.local);
    assert_eq!(
        plan.intent,
        SwitchIntent::HostSync {
            target_id: TargetId::remote("dev"),
            selection: HostSyncSelection {
                paths: Some(vec!["model_verbosity".into()])
            }
        }
    );
    let (_, stdout, _) = run_zh(&service, &["remote", "sync", "dev", "--dry-run"]).await;
    for value in ["设置同步", "本机", "主机", "应用"] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
}

#[tokio::test]
async fn remote_codex_install_and_rollback() {
    let service = service();
    let (code, stdout, _) = run(&service, &["remote", "codex", "dev", "install", "0.162.0"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Replaced Codex 0.150.1"));
    let (code, _, _) = run(
        &service,
        &["remote", "codex", "dev", "rollback", "--to", "0.150.1"],
    )
    .await;
    assert_eq!(code, 0);
    let installs = service.v3.lock().unwrap().installs.clone();
    assert_eq!(
        installs[1],
        InstallRequest {
            version: CodexVersion::new(0, 150, 1),
            target: InstallTarget::Remote {
                target_id: TargetId::remote("dev")
            }
        }
    );
    let (code, stdout, _) = run(&service, &["remote", "codex", "dev", "rollback"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("0.150.1 is active again"), "{stdout}");
    let (code, stdout, _) = run(&service, &["codex", "rollback"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("0.150.1 is active again"), "{stdout}");
    assert_eq!(
        service.v3.lock().unwrap().rollbacks,
        vec![
            InstallTarget::Remote {
                target_id: TargetId::remote("dev")
            },
            InstallTarget::Local
        ]
    );
}

#[tokio::test]
async fn sessions_of_a_remote_host_are_read_only_with_the_command() {
    let service = service();
    let (code, stdout, stderr) = run(&service, &["sessions", "--host", "dev"]).await;
    transcript("spb sessions --host dev", code, &stdout, &stderr);
    assert_eq!(code, 0);
    assert!(stdout.contains("krill"));
    assert!(stdout.contains("spb sessions migrate --from krill"));
    let (code, _, _) = run(
        &service,
        &["sessions", "--host", "dev", "migrate", "--from", "krill"],
    )
    .await;
    assert_eq!(code, 2);
    let (_, stdout, _) = run(&service, &["sessions", "--host", "dev", "--json"]).await;
    let s = service.v3.lock().unwrap().host_sessions.clone();
    assert_eq!(one_json(&stdout), serde_json::to_value(&s).unwrap());
}

#[tokio::test]
async fn feeds_status_and_refresh() {
    let service = service();
    let (code, stdout, _) = run(&service, &["feeds", "status"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Built into Superba"));
    let (code, _, _) = run(&service, &["feeds", "refresh", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(service.v3.lock().unwrap().feed_refreshes, 1);
    let (_, stdout, _) = run_zh(&service, &["feeds", "status"]).await;
    assert!(stdout.contains("Superba 内置"), "{stdout}");
}

#[test]
fn ages_parse() {
    assert_eq!(superba_cli::parse_age("90s"), Ok(90));
    assert_eq!(superba_cli::parse_age("30m"), Ok(1_800));
    assert_eq!(superba_cli::parse_age("1h"), Ok(3_600));
    assert_eq!(superba_cli::parse_age("7d"), Ok(604_800));
    assert_eq!(superba_cli::parse_age("2w"), Ok(1_209_600));
    assert!(superba_cli::parse_age("0h").is_err());
    assert!(superba_cli::parse_age("soon").is_err());
}

#[tokio::test]
async fn every_new_command_runs_in_both_languages() {
    let commands: &[&[&str]] = &[
        &["models"],
        &[
            "models",
            "set",
            "gpt-6-astra",
            "--context",
            "400k",
            "--profile",
            "other",
        ],
        &[
            "models",
            "default",
            "--compact",
            "250k",
            "--profile",
            "other",
        ],
        &["models", "reset", "--all", "--profile", "other"],
        &["models", "picker", "off", "--profile", "other"],
        &["speed"],
        &["speed", "standard", "--profile", "other"],
        &["usage", "log"],
        &["usage", "stats"],
        &["router", "stats-line", "off"],
        &["router", "record", "off"],
        &["router", "clear", "--yes"],
        &["codex", "list", "--all"],
        &["codex", "install", "0.162.0"],
        &["codex", "pin", "0.162.0", "--dry-run"],
        &["codex", "unpin", "--dry-run"],
        &["app", "check"],
        &["app", "download"],
        &["remote", "list"],
        &["remote", "probe", "dev"],
        &["remote", "follow", "dev", "off"],
        &["remote", "sync", "dev", "--dry-run"],
        &["remote", "codex", "dev", "install", "0.162.0"],
        &["sessions", "--host", "dev"],
        &["feeds", "status"],
        &["feeds", "refresh"],
    ];
    for locale in ["en", "zh-CN"] {
        for args in commands {
            let service = service();
            let mut all = vec!["--locale", locale];
            all.extend_from_slice(args);
            let (code, stdout, stderr) = invoke(service.clone(), &all).await;
            transcript(&format!("spb {}", all.join(" ")), code, &stdout, &stderr);
            assert_eq!(code, 0, "{all:?}: {stderr}");
            assert!(!stdout.contains("cli."), "raw key in {all:?}: {stdout}");
            if locale == "zh-CN" && !stdout.is_empty() {
                assert!(
                    stdout
                        .chars()
                        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                    "no Chinese in {all:?}: {stdout}"
                );
            }
            // JSON: exactly one document.
            let mut json = all.clone();
            json.push("--json");
            let (code, stdout, _) = invoke(service.clone(), &json).await;
            assert_eq!(code, 0, "{json:?}");
            one_json(&stdout);
        }
    }
}
