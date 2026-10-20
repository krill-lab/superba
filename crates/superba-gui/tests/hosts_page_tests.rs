#![allow(missing_docs)]
//! Remote hosts page (UX §3.8, plan §5 B7): only hosts the desktop app knows
//! (never ssh-config-only ones), the follow toggle reaching `set_host_follow`,
//! the sync dialog rows (write / skip / remove / conflict) and the `HostSync`
//! intent it plans, the empty state, the Fit layout and the sidebar badge.

mod router_harness;
mod support;

use std::sync::Arc;

use pretty_assertions::assert_eq;
use router_harness::Harness;
use superba_core::{HostSyncAction, Locale, SwitchIntent, TargetId};
use superba_gui::pages::hosts::{self, State, sync};
use superba_gui::pages::{Action, NavBadge};
use support::fake::{FakeOptions, FakeService};
use support::fit::SIZES;

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn devbox() -> TargetId {
    TargetId::remote("devbox-7c1e")
}

#[test]
fn only_app_known_hosts_are_listed_and_the_page_fits() {
    for size in SIZES {
        for locale in [Locale::ZhCn, Locale::En] {
            let service = fake();
            let mut h = Harness::new(size, locale, service.clone(), true);
            let mut state = State::default();
            let f = h.until(
                |ui, cx, out| hosts::show(ui, cx, &mut state, out),
                |f| f.has("devbox") && f.has("gpu-01"),
            );
            assert!(
                !f.has(FakeService::SSH_CONFIG_ONLY_HOST),
                "{}",
                f.all_text()
            );
            assert!(!f.scrolled && f.fits(), "{size:?} {locale:?}");
            assert_eq!(f.scroll_areas, 0, "the page itself never scrolls");
        }
    }
}

#[test]
fn the_detail_shows_where_the_host_is_its_codex_and_the_sync_summary() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service, true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("该主机可接收 3 项"),
    );
    for want in [
        "Linux x86_64",
        "0.162.0 · 独立安装",
        "上次同步",
        "本机之后改了 3 项",
        "本机 4 项设置 → 该主机可接收 3 项，1 项需要更新 Codex",
        "可以（app 记录了 hostId）",
        "随本机切换",
        "查看会话 →",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    // The sidebar count is the shell's (one source); the page adds none.
    assert_eq!(hosts::status(&state).badge, NavBadge::None);
    assert_eq!(state.unsynced(), 1);
}

#[test]
fn the_follow_toggle_reaches_the_service() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("随本机切换"),
    );
    // The switch sits right of its caption. The first frames lay the page out
    // (sizing passes, async host data), so click only once the caption holds
    // still, and click again if the press landed on a frame that moved.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut caption = f.find("随本机切换").unwrap();
    let mut reloaded = f.actions.iter().any(|a| matches!(a, Action::LoadHosts));
    while service.follow_calls().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the follow toggle never reached the service"
        );
        let mut stable = 0;
        while stable < 3 {
            let f = h.frame(|ui, cx, out| hosts::show(ui, cx, &mut state, out));
            reloaded |= f.actions.iter().any(|a| matches!(a, Action::LoadHosts));
            let now = f.find("随本机切换").unwrap();
            stable = if now == caption { stable + 1 } else { 0 };
            caption = now;
        }
        h.click_at(egui::pos2(caption.max.x + 26.0, caption.center().y));
        // Hover, press, release, then the service call.
        for _ in 0..6 {
            let f = h.frame(|ui, cx, out| hosts::show(ui, cx, &mut state, out));
            reloaded |= f.actions.iter().any(|a| matches!(a, Action::LoadHosts));
        }
        let waited = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while service.follow_calls().is_empty() && std::time::Instant::now() < waited {
            let f = h.frame(|ui, cx, out| hosts::show(ui, cx, &mut state, out));
            reloaded |= f.actions.iter().any(|a| matches!(a, Action::LoadHosts));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    assert_eq!(
        service.follow_calls().len(),
        1,
        "{:?}",
        service.follow_calls()
    );
    assert_eq!(service.follow_calls(), vec![(devbox(), true)]);
    let shown = state
        .hosts
        .value
        .as_ref()
        .unwrap()
        .iter()
        .find(|h| h.host.target_id == devbox())
        .unwrap()
        .settings
        .follow_switches;
    assert!(shown);
    // The shell's host list (sidebar badge, Overview) is reloaded.
    if !reloaded {
        h.until(
            |ui, cx, out| hosts::show(ui, cx, &mut state, out),
            |f| f.actions.iter().any(|a| matches!(a, Action::LoadHosts)),
        );
    }
}

#[test]
fn the_sync_dialog_groups_rows_unticks_conflicts_and_plans_a_host_sync() {
    let plan = FakeService::fixture_sync_plan(&devbox());
    let (writes, skips, removes) = sync::groups(&plan);
    assert_eq!(writes.len(), 3, "writes and the conflict");
    assert_eq!(skips.len(), 1);
    assert_eq!(removes.len(), 1);
    let ticked = sync::default_ticked(&plan);
    assert_eq!(
        ticked.iter().cloned().collect::<Vec<_>>(),
        ["approval_policy", "model_reasoning_summary", "notify"]
    );
    assert!(
        plan.rows
            .iter()
            .filter(|r| matches!(r.action, HostSyncAction::Skip { .. }))
            .all(|r| !sync::tickable(r))
    );

    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service, true);
    let mut state = State::default();
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("devbox"),
    );
    h.frame(|_, cx, out| hosts::command(&mut state, cx, &format!("sync:{}", devbox()), out));
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("将写入（3）") && f.has("将恢复主机原值（1）") && f.has("tui.theme"),
    );
    assert!(hosts::status(&state).modal);
    for want in [
        "同步设置到 devbox · codex 0.162.0",
        "跳过（1）",
        "将恢复主机原值（1）",
        "上次同步写入、本机已不再设置；恢复主机原来的值",
        "冲突",
        "同步 3 项",
        "同步后会热重启 devbox 上的 Codex 内核",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    // Let the dialog settle (it re-centres once its full size is known), then
    // click like a user would.
    let f = h.frames(3, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
    h.click_text(&f, "同步 3 项");
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::Plan(_))),
    );
    let plan = f
        .actions
        .into_iter()
        .find_map(|a| match a {
            Action::Plan(r) => Some(r),
            _ => None,
        })
        .unwrap();
    match plan.intent {
        SwitchIntent::HostSync {
            target_id,
            selection,
        } => {
            assert_eq!(target_id, devbox());
            assert_eq!(
                selection.paths.unwrap(),
                ["approval_policy", "model_reasoning_summary", "notify"]
            );
        }
        other => panic!("unexpected intent {other:?}"),
    }
    assert!(state.sync.is_none(), "the dialog closes");
}

#[test]
fn unticking_changes_the_selection_and_nothing_ticked_cannot_sync() {
    let mut d = sync::SyncDialog::with_plan(
        devbox(),
        "devbox".into(),
        FakeService::fixture_sync_plan(&devbox()),
    );
    d.set("notify", false);
    d.set("tui.theme", true);
    match d.confirm().unwrap().intent {
        SwitchIntent::HostSync { selection, .. } => assert_eq!(
            selection.paths.unwrap(),
            ["approval_policy", "model_reasoning_summary", "tui.theme"]
        ),
        other => panic!("{other:?}"),
    }
    for p in ["approval_policy", "model_reasoning_summary", "tui.theme"] {
        d.set(p, false);
    }
    assert!(d.confirm().is_none());
}

#[test]
fn without_hosts_the_empty_state_names_the_app_and_no_ssh_config() {
    let service = fake();
    service.without_hosts();
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service, true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("没有远程主机"),
    );
    assert!(f.has("ChatGPT 桌面 app 还没有记录任何远程主机"));
    assert!(!f.has(".ssh"));
    let mut h = Harness::new([1280.0, 800.0], Locale::En, fake(), true);
    let service = fake();
    service.without_hosts();
    let mut h2 = Harness::new([1280.0, 800.0], Locale::En, service, true);
    let mut state = State::default();
    h2.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("The ChatGPT desktop app doesn't know any remote host yet."),
    );
    let _ = &mut h;
}

#[test]
fn probe_and_select_commands() {
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service.clone(), true);
    let mut state = State::default();
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("devbox"),
    );
    h.frame(|_, cx, out| hosts::command(&mut state, cx, &format!("probe:{}", devbox()), out));
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |_| service.host_probes().contains(&devbox()),
    );
    h.frame(|_, cx, out| hosts::command(&mut state, cx, "select:ssh:gpu-01-31aa", out));
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Not compared yet") && f.has("gpu-01"),
    );
    assert!(f.has("Not connected"));
}

#[test]
fn an_unreachable_host_shows_only_why_with_retry_and_keeps_provider_and_version() {
    // Review ux-visual #5: the dialog of an unreachable host showed an empty
    // provider, "codex ?", "Unsupported target: ssh error …", an empty column
    // header, "nothing to sync" and the restart note.
    use superba_core::{BlockReason, HostSyncPlan};
    for locale in [Locale::ZhCn, Locale::En] {
        let service = fake();
        service.set_sync_plan(HostSyncPlan {
            target_id: devbox(),
            profile_id: None,
            host_version: None,
            rows: Vec::new(),
            blocked: vec![BlockReason::UnsupportedTarget {
                detail: "ssh error on devbox: ssh: Could not resolve hostname devbox.example.com"
                    .into(),
            }],
            notes: Vec::new(),
        });
        let mut h = Harness::new([1280.0, 800.0], locale, service.clone(), true);
        let provider = h
            .data
            .active_profile()
            .expect("an applied provider")
            .name
            .clone();
        let mut state = State::default();
        h.until(
            |ui, cx, out| hosts::show(ui, cx, &mut state, out),
            |f| f.has("devbox"),
        );
        h.frame(|_, cx, out| hosts::command(&mut state, cx, &format!("sync:{}", devbox()), out));
        let zh = locale == Locale::ZhCn;
        let headline = if zh {
            "无法连接 devbox"
        } else {
            "Can't connect to devbox"
        };
        let f = h.until(
            |ui, cx, out| hosts::show(ui, cx, &mut state, out),
            |f| f.has(headline),
        );
        let (title, body, retry) = if zh {
            (
                "同步设置到 devbox · codex 0.162.0",
                format!("供应商「{provider}」"),
                "重试",
            )
        } else {
            (
                "Sync settings to devbox · codex 0.162.0",
                format!("provider “{provider}”"),
                "Retry",
            )
        };
        for want in [
            title,
            body.as_str(),
            retry,
            "ssh: Could not resolve hostname",
        ] {
            assert!(
                f.has(want),
                "{want} missing ({locale:?}):\n{}",
                f.all_text()
            );
        }
        for absent in [
            "不支持的目标",
            "Unsupported target",
            "ssh error on devbox",
            "codex ?",
            "「」",
            "没有需要同步的设置",
            "Nothing to sync",
            "同步后会热重启",
            "hot-restarts afterwards",
            "devbox 当前",
            "devbox now",
        ] {
            assert!(
                !f.has(absent),
                "{absent} shown ({locale:?}):\n{}",
                f.all_text()
            );
        }
        // Retry loads the preview again; a reachable host now shows its rows.
        service.set_sync_plan(FakeService::fixture_sync_plan(&devbox()));
        let f = h.frames(3, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
        h.click_text(&f, retry);
        let f = h.until(
            |ui, cx, out| hosts::show(ui, cx, &mut state, out),
            |f| f.has("tui.theme"),
        );
        assert!(!f.has(headline), "{}", f.all_text());
    }
}

#[test]
fn a_preview_without_rows_says_so_without_the_column_header_or_restart_note() {
    use superba_core::HostSyncPlan;
    let service = fake();
    service.set_sync_plan(HostSyncPlan {
        target_id: devbox(),
        profile_id: Some("krill".into()),
        host_version: Some(superba_core::CodexVersion::new(0, 162, 0)),
        rows: Vec::new(),
        blocked: Vec::new(),
        notes: Vec::new(),
    });
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service, true);
    let mut state = State::default();
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("devbox"),
    );
    h.frame(|_, cx, out| hosts::command(&mut state, cx, &format!("sync:{}", devbox()), out));
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("没有需要同步的设置"),
    );
    assert!(!f.has("devbox 当前"), "{}", f.all_text());
    assert!(!f.has("同步后会热重启"), "{}", f.all_text());
    assert!(!f.has("同步 0 项"), "{}", f.all_text());
}

#[test]
fn host_rows_show_their_whole_status_line_next_to_long_versions() {
    // Review ux-visual #12: "Not connect…" / "未连接 · 从未…" next to
    // "0.165.0-alpha.3". A host row (the list is 260 px wide) has the version on
    // the name's line and the status line across the row under it.
    use superba_gui::design::icons::Icon;
    use superba_gui::design::widgets::{RowContent, paint_row};
    for hint in ["Not connected · Never synced", "未连接 · 从未同步"] {
        let ctx = egui::Context::default();
        let fonts = superba_gui::design::fonts::FontReport::default();
        ctx.set_fonts(superba_gui::design::fonts::definitions(&fonts));
        superba_gui::design::style::install(&ctx, Some(egui::Theme::Light));
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let mut row = RowContent::new(Icon::Server, "gpu-01");
                row.subtitle = Some(hint.into());
                row.value = Some("0.165.0-alpha.3".into());
                let rect = egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(260.0, superba_gui::design::tokens::size::ROW_LIST2),
                );
                paint_row(ui, rect, &row);
            });
            shapes = out.shapes.clone();
            out.drop_without_applying_deltas();
        }
        let text = |t: &str| {
            shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(g) if g.galley.text() == t => {
                        Some((g.galley.rect.translate(g.pos.to_vec2()), g.galley.elided))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{t} not drawn"))
        };
        let (status, cut) = text(hint);
        assert!(!cut, "{hint} is cut");
        let (version, cut) = text("0.165.0-alpha.3");
        assert!(!cut);
        assert!(
            !version.intersects(status),
            "{hint}: {version:?} {status:?}"
        );
        let (name, _) = text("gpu-01");
        assert!(
            (version.center().y - name.center().y).abs() < 3.0,
            "the version is on the name's line"
        );
    }
}

#[test]
fn the_connection_names_the_line_with_the_same_origin_not_only_the_same_host() {
    // Lines on one host differ by port (the e2e test lines all live on
    // 127.0.0.1): the host's line is the one with its whole origin.
    let service = fake();
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service, true);
    let krill = h
        .data
        .sites
        .iter()
        .find(|s| s.id.as_str() == "krill")
        .cloned()
        .expect("the Krill site");
    let mut other = krill.clone();
    other.id = superba_core::SiteId::new("lab");
    other.name = superba_core::LocalizedText::new("lab.name", "Lab");
    for l in &mut other.lines {
        l.origin = superba_core::Origin::parse(&format!("{}:8443", l.origin.as_str())).unwrap();
        l.name = superba_core::LocalizedText::new("lab.line", "Lab line");
    }
    h.data.sites.insert(0, other);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Krill · Overseas · PAT") || f.has("Lab · Lab line · PAT"),
    );
    assert!(f.has("Krill · Overseas · PAT"), "{}", f.all_text());
}
