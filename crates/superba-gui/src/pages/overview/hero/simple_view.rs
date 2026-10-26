//! Simple mode's route: no inactive router, client status and a speed popup.

use super::*;
use crate::design::widgets::Badge;

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, hero: &Hero, rect: Rect, out: &mut Vec<Action>) {
    let b = Backdrop::of(ui.ctx());
    crate::design::backdrop::panel(ui.painter(), rect, &b);
    let inner = rect.shrink2(vec2(20.0, 14.0));
    ui.painter().text(
        pos2(inner.min.x, inner.min.y + 10.0),
        Align2::LEFT_CENTER,
        cx.tr.t("overview.hero.caption"),
        font(text::SMALL + 1.0, Weight::Medium),
        cx.pal.text_muted,
    );
    let launch = launch_button(ui, cx, hero, pos2(inner.max.x, inner.min.y + 10.0));

    let mut client = hero.this.clone();
    client.title = "Codex".into();
    client.detail.clear();
    client.tip = cx.tr.t("overview.simple.check_codex");
    let mut provider = hero.provider.clone();
    provider.menu = false;
    provider.detail.clear();
    let mut model = hero.model.clone();
    model.menu = false;
    model.tip = cx.tr.t("overview.hero.speed_menu");
    let mut router = hero.router.clone();
    router.tip = cx.tr.t("overview.hero.router_on_tip");
    let nodes = if hero.router_on {
        vec![&client, &router, &provider, &model]
    } else {
        vec![&client, &provider, &model]
    };
    let col = inner.width() / nodes.len() as f32;
    let x = |i: usize| inner.min.x + col * (i as f32 + 0.5);
    let tile_y = inner.min.y + 22.0 + TILE / 2.0;
    let label_y = tile_y + TILE / 2.0 + 14.0;
    let provider_index = nodes.len() - 2;
    for i in 0..nodes.len() - 1 {
        link(
            ui,
            x(i) + TILE / 2.0 + LINK_GAP,
            x(i + 1) - TILE / 2.0 - LINK_GAP,
            tile_y,
            cx.pal.border_strong,
        );
    }
    let mut speed_response = None;
    for (i, node) in nodes.into_iter().enumerate() {
        let drawn = draw_node(
            ui,
            cx,
            node,
            x(i),
            tile_y,
            label_y,
            col,
            b.chart[i % 2],
            0,
            matches!(node.spot, Spot::ThisComputer | Spot::Provider | Spot::Model),
        );
        if drawn.clicked {
            match node.spot {
                Spot::ThisComputer if !cx.data.versions.loading => {
                    out.push(Action::CheckCodexUpdates)
                }
                Spot::Provider => out.push(Action::Go(Page::Providers)),
                _ => {}
            }
        }
        if node.spot == Spot::Model {
            speed_response = Some(drawn.response);
        }
    }
    let versions = cx.data.versions.value.as_ref();
    let (key, status) = match versions {
        _ if cx.data.versions.loading => ("overview.simple.client_unknown", Status::Busy),
        _ if cx.data.versions.error.is_some() => ("overview.simple.client_unknown", Status::Idle),
        Some(v)
            if v.app.update_available
                || v.cli
                    .iter()
                    .any(|r| !matches!(r.action, superba_core::CliUpdateAction::None)) =>
        {
            ("overview.simple.client_update", Status::Warn)
        }
        Some(v)
            if v.app.latest.is_some()
                && v.app
                    .install
                    .as_ref()
                    .is_some_and(|i| i.app_version.is_some())
                && v.cli
                    .iter()
                    .filter(|r| r.kind != superba_core::CodexInstallKind::AppBundled)
                    .all(|r| r.binary.version.is_some() && r.latest.is_some()) =>
        {
            ("overview.simple.client_latest", Status::Ok)
        }
        _ => ("overview.simple.client_unknown", Status::Idle),
    };
    let label = cx.tr.t(key);
    let width = ui
        .painter()
        .layout_no_wrap(
            label.clone(),
            font(text::CAPTION, Weight::Medium),
            cx.pal.text,
        )
        .size()
        .x
        + 18.0;
    let client_status = ui.put(
        Rect::from_center_size(pos2(x(0), label_y + 21.0), vec2(width.min(col - 8.0), 20.0)),
        Badge::new(label).status(status),
    );
    client_status.on_hover_text(cx.tr.t("overview.simple.check_codex"));
    if !hero.provider.ghost && !cx.data.official_active() {
        latency_pill(
            ui,
            cx,
            pos2(x(provider_index), label_y + 21.0),
            hero.latency,
        );
    }
    if let Some(response) = speed_response {
        super::super::simple_speed::popover(&response, ui, cx, &model.title, out);
    }
    if let Some((rect, button, action, tip)) = launch {
        if ui.put(rect, button).on_hover_text(tip).clicked() {
            out.push(action);
        }
    }
}
