//! Window chrome: sidebar navigation, the fixed switch banner (countdown, waiting,
//! progress, blocked, questions), the full-restart confirmation and the dialogs for
//! mid-execution questions (plan 7.3 copy).

use std::time::Duration;

use egui::{Align, Color32, Layout, Panel, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use superba_core::{
    ConsumerKind, Decision, DecisionRequest, KeyResolution, LegacySessionChoice, LocalizedText,
    MigrationPlan, Notice, PlanChoice, ProviderRestore, RestartKind, Severity, StepStatus,
    SwitchIntent, SwitchPhase, SwitchPlan, SwitchStep, TargetId,
};

use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, chrome, radius, size, space, text};
use crate::design::widgets::{
    Button, CountdownBar, Dropdown, Status, StatusDot, Text, bounded, dialog, dialog_body,
    link_button,
};
use crate::flow::{Flow, Run, Stage};
use crate::pages::shared::notices::notice_list;
use crate::pages::{Action, Cx, Page, error_text};
use crate::pages::{NavBadge, PageStatus};

/// The Superba mark: the application icon (a coral mark on a pearl tile), drawn
/// from the pre-rendered PNG closest above the physical size so it stays crisp on
/// HiDPI. The tile fills 928 of the icon's 1024 units; the rest is its soft shadow.
pub fn brand_mark(ui: &mut Ui, _pal: &Palette, size: f32) {
    const SMALL: &[u8] = include_bytes!("../../../assets/icon/png/superba-64.png");
    const MEDIUM: &[u8] = include_bytes!("../../../assets/icon/png/superba-128.png");
    const LARGE: &[u8] = include_bytes!("../../../assets/icon/png/superba-256.png");
    let physical = size * ui.ctx().pixels_per_point();
    let (name, bytes) = if physical <= 64.0 {
        ("brand-mark-64", SMALL)
    } else if physical <= 128.0 {
        ("brand-mark-128", MEDIUM)
    } else {
        ("brand-mark-256", LARGE)
    };
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let id = egui::Id::unique(name);
    let cached = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id));
    let texture = match cached {
        Some(texture) => texture,
        None => {
            let Ok(icon) = eframe::icon_data::from_png_bytes(bytes) else {
                return;
            };
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            let texture = ui
                .ctx()
                .load_texture(name, image, egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, texture.clone()));
            texture
        }
    };
    let paint = rect.expand(size * (1024.0 / 928.0 - 1.0) / 2.0);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter().image(texture.id(), paint, uv, Color32::WHITE);
}

/// The sidebar panel's frame (margins around the navigation): room for the
/// traffic lights above the first row on macOS, the rows
/// [`size::NAV_INSET_RIGHT`] clear of the content card, the last row's bottom
/// in line with the card's (user bug 85).
pub fn sidebar_frame() -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin {
        left: size::NAV_INSET_LEFT as i8,
        right: size::NAV_INSET_RIGHT as i8,
        top: chrome::NAV_TOP as i8,
        bottom: chrome::CARD_INSET as i8,
    })
}

/// Left navigation (user request 24, user bug 85), on the window background:
/// pages under small gray section headings with their badges and unsaved-edit
/// dots (`status` per page), and Superba's own pages (Diagnostics,
/// Preferences) pinned at the bottom above a hairline and the version. No
/// brand and no search field: the window's top is empty (the traffic lights,
/// the drag area) and the command palette opens with Cmd/Ctrl+K and from the
/// Overview's menu. The selected row is a soft gray pill with its icon in the
/// brand coral. The current state lives on the overview, not in the sidebar.
///
/// When the window is too short for the comfortable rows (the 960x640
/// minimum) the rows and headings are compacted, and the main list scrolls
/// above the pinned pages rather than running under them (review ux-visual #2).
pub fn sidebar(
    ui: &mut Ui,
    cx: &Cx<'_>,
    page: &mut Page,
    status: &dyn Fn(Page) -> PageStatus,
    _out: &mut Vec<Action>,
) {
    ui.spacing_mut().item_spacing.y = NAV_GAP;

    // Superba's own pages sit at the bottom.
    let pinned = |p: Page| p.is_superba();
    let main: Vec<Page> = Page::ALL.into_iter().filter(|p| !pinned(*p)).collect();
    let footer: Vec<Page> = Page::ALL.into_iter().filter(|p| pinned(*p)).collect();
    let metrics = if ui.available_height() >= NavMetrics::FULL.needed(&main, footer.len()) {
        NavMetrics::FULL
    } else {
        NavMetrics::COMPACT
    };
    Panel::bottom("sidebar-footer")
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = NAV_GAP;
            ui.add_space(FOOTER_ABOVE);
            for p in footer.iter().copied() {
                if nav_item(
                    ui,
                    cx,
                    p,
                    *page == p || (page.is_superba() && p.is_superba()),
                    &status(p),
                    metrics.row,
                ) {
                    *page = p;
                }
            }
            let (r, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), FOOTER_RULE), Sense::hover());
            ui.painter().hline(
                r.x_range().shrink(size::NAV_ICON_X),
                r.center().y,
                Stroke::new(1.0, cx.pal.border.gamma_multiply(0.8)),
            );
            // The version (also in Preferences → About), a label screen
            // readers read.
            ui.horizontal(|ui| {
                ui.set_height(FOOTER_VERSION);
                ui.add_space(size::NAV_ICON_X);
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .font(mono_font(text::CAPTION - 1.0, Weight::Regular))
                        .color(cx.pal.text_muted),
                );
            });
        });

    // Never under the pinned pages: the rest of the height, scrolling if even
    // the compact rows do not fit.
    egui::ScrollArea::vertical()
        .id_salt("sidebar-nav")
        .auto_shrink([false, true])
        .max_height(ui.available_height())
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = NAV_GAP;
            let mut group = None;
            for p in main.iter().copied() {
                if group != Some(p.group()) {
                    if let Some(key) = p.group().caption_key() {
                        ui.add_space(metrics.caption_gap);
                        let (r, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), metrics.caption),
                            Sense::hover(),
                        );
                        ui.painter().text(
                            pos2(r.min.x + size::NAV_ICON_X, r.center().y),
                            egui::Align2::LEFT_CENTER,
                            cx.tr.t(key),
                            font(text::NAV_CAPTION, Weight::Medium),
                            cx.pal.text_muted,
                        );
                    }
                    group = Some(p.group());
                }
                if nav_item(
                    ui,
                    cx,
                    p,
                    *page == p || (page.is_superba() && p.is_superba()),
                    &status(p),
                    metrics.row,
                ) {
                    *page = p;
                }
            }
        });
}

/// Row and heading sizes of the sidebar: comfortable, or compact when the
/// window is short.
#[derive(Clone, Copy)]
struct NavMetrics {
    row: f32,
    caption: f32,
    caption_gap: f32,
}

impl NavMetrics {
    const FULL: Self = Self {
        row: size::NAV_ROW,
        caption: size::NAV_CAPTION[0],
        caption_gap: size::NAV_CAPTION_GAP[0],
    };
    // Thirteen pages in five groups fit the 960x640 minimum with these.
    const COMPACT: Self = Self {
        row: size::NAV_ROW_COMPACT,
        caption: size::NAV_CAPTION[1],
        caption_gap: size::NAV_CAPTION_GAP[1],
    };

    /// Height of the main list plus the pinned footer (rows, headings, the
    /// footer's hairline and version) with these sizes.
    fn needed(self, main: &[Page], footer_rows: usize) -> f32 {
        let mut groups: Vec<_> = main.iter().map(|p| p.group()).collect();
        groups.dedup();
        let captions = groups.iter().filter(|g| g.caption_key().is_some()).count();
        let items = main.len() + captions;
        let list = main.len() as f32 * self.row
            + captions as f32 * (self.caption + self.caption_gap)
            + items as f32 * NAV_GAP;
        // The space above the pinned rows, the rows, the hairline and the
        // version, with the gaps between them.
        let foot = FOOTER_ABOVE
            + footer_rows as f32 * (self.row + NAV_GAP)
            + FOOTER_RULE
            + FOOTER_VERSION
            + 3.0 * NAV_GAP;
        list + foot
    }
}

/// Space between sidebar rows.
const NAV_GAP: f32 = size::NAV_GAP;

/// Space above the pinned pages at the bottom of the sidebar.
const FOOTER_ABOVE: f32 = 6.0;

/// Height of the hairline's row below the pinned pages.
const FOOTER_RULE: f32 = 7.0;

/// Height of the version line at the bottom of the sidebar.
const FOOTER_VERSION: f32 = 16.0;

/// The shortcut label shown next to search affordances.
pub fn palette_shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd K"
    } else {
        "Ctrl K"
    }
}

fn nav_item(
    ui: &mut Ui,
    cx: &Cx<'_>,
    page: Page,
    selected: bool,
    status: &PageStatus,
    height: f32,
) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    // The row shows the short label; its accessible name is the page title
    // ("Claude Code 设置", not a second "设置").
    let label = cx.tr.t(page.nav_key());
    let name = cx.tr.t(page.sidebar_page().title_key());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, selected, &name));
    let t = crate::design::widgets::hover_t(ui, &response);
    // Selected: a soft gray rounded rectangle on the window background.
    let b = crate::design::backdrop::Backdrop::of(ui.ctx());
    let fill = if selected {
        b.selected
    } else {
        Color32::TRANSPARENT.lerp_to_gamma(b.hover, t)
    };
    let p = ui.painter();
    p.rect_filled(rect, radius::NAV, fill);
    let color = cx.pal.text;
    // The selected page's icon in the brand coral (the app icon's mark).
    let icon_color = if selected && !page.is_superba() {
        crate::design::tokens::brand::coral(cx.pal.dark)
    } else {
        cx.pal.text_muted.lerp_to_gamma(cx.pal.text, t)
    };
    let side = size::NAV_ICON;
    let icon_rect = Rect::from_min_size(
        pos2(rect.min.x + size::NAV_ICON_X, rect.center().y - side / 2.0),
        vec2(side, side),
    );
    icons::paint(p, icon_rect, page.icon(), icon_color);
    p.text(
        pos2(rect.min.x + size::NAV_LABEL_X, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font(
            text::NAV,
            if selected {
                Weight::Semibold
            } else {
                Weight::Medium
            },
        ),
        color,
    );
    // Right edge: the page's badge, then (further left) the amber dot of unsaved
    // edits.
    let mut x = rect.max.x - 12.0;
    let mut tip: Option<String> = None;
    match status.badge {
        NavBadge::None => {}
        NavBadge::Dot(s) => {
            p.circle_filled(pos2(x - 3.0, rect.center().y), 3.0, cx.pal.status(s));
            x -= 14.0;
            tip.clone_from(&status.badge_tip);
        }
        NavBadge::Count(n) => {
            let g = p.layout_no_wrap(
                n.to_string(),
                mono_font(text::CAPTION, Weight::Medium),
                cx.pal.text_subtle,
            );
            let w = g.size().x;
            p.galley(
                pos2(x - w, rect.center().y - g.size().y / 2.0),
                g,
                cx.pal.text_subtle,
            );
            x -= w + 10.0;
            tip.clone_from(&status.badge_tip);
        }
    }
    if status.pending > 0 {
        p.circle_filled(
            pos2(x - 3.0, rect.center().y),
            3.0,
            cx.pal.status(Status::Warn),
        );
        tip = Some(match tip {
            Some(t) => format!("{t}\n{}", cx.tr.t("shell.pending_dot")),
            None => cx.tr.t("shell.pending_dot"),
        });
    }
    if crate::design::widgets::show_focus(&response) {
        crate::design::widgets::focus_ring(p, rect, radius::NAV, cx.pal);
    }
    let response = match tip {
        Some(t) => response.on_hover_text(t),
        None => response,
    };
    response.clicked()
}

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

/// Extra UI state of the banner.
#[derive(Debug, Default)]
pub struct BannerState {
    /// Replacement model picked in the banner.
    pub model: Option<String>,
    /// Show the key diff in the banner.
    pub show_changes: bool,
    /// Whether the risk disclosure is open after its first completed switch.
    pub show_risks: bool,
    risk_shown_for: Option<String>,
}

const HOT_RISK_SEEN: &str = "superba-hot-risk-seen-v1";

impl BannerState {
    /// Restore the remembered disclosure state without marking any pending switch.
    pub fn restore_risk_memory(ctx: &egui::Context, seen: bool) {
        ctx.data_mut(|data| data.insert_persisted(egui::Id::unique(HOT_RISK_SEEN), seen));
    }

    /// Whether the full risk list was shown during a previously completed switch.
    pub fn risks_seen(ctx: &egui::Context) -> bool {
        ctx.data_mut(|data| data.get_persisted(egui::Id::unique(HOT_RISK_SEEN)))
            .unwrap_or(false)
    }

    /// Remember a successful switch only if its risk list was actually displayed.
    /// Returns true when the app should persist the newly learned preference.
    pub fn complete_switch(&mut self, ctx: &egui::Context, plan_id: &str) -> bool {
        if self.risk_shown_for.as_deref() != Some(plan_id) || Self::risks_seen(ctx) {
            return false;
        }
        Self::restore_risk_memory(ctx, true);
        self.show_risks = false;
        true
    }
}

enum HeaderAside {
    Buttons(Vec<Button>),
    Text(String),
}

impl HeaderAside {
    fn width(&self, ui: &Ui) -> f32 {
        match self {
            Self::Buttons(buttons) => {
                buttons.iter().map(|b| b.desired_size(ui).x).sum::<f32>()
                    + 8.0 * buttons.len().saturating_sub(1) as f32
            }
            Self::Text(text) => {
                ui.painter()
                    .layout_no_wrap(
                        text.clone(),
                        mono_font(text::SMALL, Weight::Regular),
                        Color32::PLACEHOLDER,
                    )
                    .size()
                    .x
            }
        }
    }

    fn show(self, ui: &mut Ui, pal: &Palette) -> Vec<egui::Response> {
        match self {
            Self::Buttons(buttons) => {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    buttons.into_iter().map(|b| ui.add(b)).collect()
                })
                .inner
            }
            Self::Text(text) => {
                ui.label(Text::mono_muted(text, pal));
                Vec::new()
            }
        }
    }
}

/// Reserve measured action space before laying out the wrapping title. At narrow
/// widths the buttons use their own row; neither child can paint over the other.
fn banner_header(
    ui: &mut Ui,
    pal: &Palette,
    icon: Option<(Icon, Color32)>,
    title: String,
    aside: HeaderAside,
) -> (egui::Response, Vec<egui::Response>) {
    let width = ui.available_width();
    let aside_width = aside.width(ui);
    let inline = width - aside_width >= 260.0
        && (!matches!(aside, HeaderAside::Buttons(_)) || width >= 480.0);
    let title_width = if inline {
        width - aside_width - 16.0
    } else {
        width
    };
    let mut aside = Some(aside);
    let (title, mut buttons) = ui
        .horizontal_top(|ui| {
            let title = ui
                .allocate_ui_with_layout(
                    vec2(title_width, 0.0),
                    Layout::left_to_right(Align::Min),
                    |ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(18.0, 20.0), Sense::hover());
                        let r = Rect::from_center_size(r.center(), vec2(18.0, 18.0));
                        if let Some((icon, color)) = icon {
                            icons::paint(ui.painter(), r, icon, color);
                        } else {
                            icons::spinner(ui.painter(), r, pal.text, ui.input(|i| i.time));
                        }
                        ui.add(egui::Label::new(Text::title(title, pal)).wrap())
                    },
                )
                .inner;
            let buttons = if inline {
                ui.add_space((16.0 - ui.spacing().item_spacing.x).max(0.0));
                aside.take().map(|a| a.show(ui, pal)).unwrap_or_default()
            } else {
                Vec::new()
            };
            (title, buttons)
        })
        .inner;
    if let Some(aside) = aside {
        buttons = aside.show(ui, pal);
    }
    (title, buttons)
}

/// What the banner shows this frame (owned, so the flow can be mutated while
/// drawing).
enum BannerView {
    Planning,
    Armed {
        plan: Box<SwitchPlan>,
        secs: u64,
        progress: f32,
    },
    TurnsAtPlan {
        plan: Box<SwitchPlan>,
        busy: Vec<superba_core::ThreadActivity>,
        progress: f32,
    },
    Blocked(Box<SwitchPlan>),
    Choose(Box<SwitchPlan>),
    WaitingRun,
    Running {
        label: String,
        steps: Vec<(String, StepStatus)>,
        elapsed: Duration,
        failed: bool,
        /// Re-applying the provider in use (user bug 25): "applying the
        /// changes", not "switching".
        reapply: bool,
    },
}

/// Whether the flow stage has a switch overlay (planning, armed, waiting, blocked,
/// questions, running).
pub fn overlay_visible(flow: &Flow) -> bool {
    !matches!(flow.stage(), Stage::Idle | Stage::ConfirmFull { .. })
}

/// Draw the floating switch overlay for the current flow stage (nothing when idle).
/// Its body scrolls beyond `max_h`. Returns whether it was drawn.
pub fn banner(
    ui: &mut Ui,
    cx: &Cx<'_>,
    flow: &mut Flow,
    st: &mut BannerState,
    now: Duration,
    max_h: f32,
) -> bool {
    let tr = cx.tr;
    let pal = cx.pal;
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::unique("superba-overlay-max-h"), max_h));
    let view = match flow.stage() {
        Stage::Idle | Stage::ConfirmFull { .. } => return false,
        Stage::Planning => BannerView::Planning,
        Stage::Armed { plan, countdown } => BannerView::Armed {
            plan: Box::new(plan.clone()),
            secs: countdown.remaining(now).as_millis().div_ceil(1000) as u64,
            progress: countdown.progress(now),
        },
        Stage::WaitingBeforeSwitch {
            plan,
            countdown,
            busy,
        } => BannerView::TurnsAtPlan {
            plan: Box::new(plan.clone()),
            busy: busy.to_vec(),
            progress: countdown.progress(now),
        },
        Stage::Blocked { plan } => BannerView::Blocked(Box::new(plan.clone())),
        Stage::Choose { plan } => BannerView::Choose(Box::new(plan.clone())),
        Stage::Running(run) if run.waiting_for_turns() && run.phase != SwitchPhase::Done => {
            BannerView::WaitingRun
        }
        Stage::Running(run) => BannerView::Running {
            label: cx.dest(&run.plan.to),
            steps: progress_steps(run)
                .into_iter()
                .map(|(s, st)| {
                    let st = if run.recovering() && st == StepStatus::Running {
                        StepStatus::Pending
                    } else {
                        st
                    };
                    (step_label(tr, &s), st)
                })
                .collect(),
            elapsed: now.saturating_sub(run.started),
            failed: run.recovering(),
            reapply: is_reapply(&run.plan),
        },
    };
    match view {
        BannerView::Planning => {
            let mut cancel = false;
            banner_card(ui, pal, None, |ui| {
                let (_, buttons) = banner_header(
                    ui,
                    pal,
                    None,
                    tr.t("banner.planning"),
                    HeaderAside::Buttons(vec![Button::ghost(tr.t("action.cancel")).small()]),
                );
                cancel = buttons.iter().any(egui::Response::clicked);
                if !flow.remote_progress().is_empty() {
                    dialog_body(ui, |ui| {
                        for (_, message, _) in flow.remote_progress() {
                            ui.label(tr.text(message));
                        }
                    });
                }
                ui.ctx().request_repaint();
            });
            if cancel {
                flow.cancel(now);
            }
        }
        BannerView::Armed {
            plan,
            secs,
            progress,
        } => {
            let mut clicked_now = false;
            let mut clicked_cancel = false;
            banner_card(ui, pal, Some((progress, false)), |ui| {
                // Re-applying the provider in use (its connection or settings
                // changed) says so, and why Codex restarts, instead of "Switching".
                let reapply = reapply_reason(cx, &plan);
                let title_key = if reapply.is_some() {
                    "banner.reapply.title"
                } else {
                    "banner.armed.title"
                };
                let (_, buttons) = banner_header(
                    ui,
                    pal,
                    Some((Icon::Bolt, pal.text)),
                    tr.f(title_key, &[("secs", &secs), ("dest", &cx.dest(&plan.to))]),
                    HeaderAside::Buttons(vec![
                        Button::primary(tr.t(if reapply.is_some() {
                            "banner.reapply.now"
                        } else {
                            "banner.switch_now"
                        }))
                        .small()
                        .kbd("Enter"),
                        Button::new(tr.t("action.cancel")).small().kbd("Esc"),
                    ]),
                );
                clicked_now = buttons[0].clicked();
                clicked_cancel = buttons[1].clicked();
                dialog_body(ui, |ui| {
                    if let Some(reason) = &reapply {
                        ui.add(egui::Label::new(Text::small(reason, pal)).wrap());
                    }
                    ui.add_space(2.0);
                    if BannerState::risks_seen(ui.ctx()) {
                        let icon = if st.show_risks {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        };
                        if ui
                            .add(
                                Button::ghost(tr.t("shell.what_restarts"))
                                    .small()
                                    .icon(icon),
                            )
                            .clicked()
                        {
                            st.show_risks = !st.show_risks;
                        }
                        if st.show_risks {
                            hot_risk_copy(ui, cx, &plan);
                            plan_notes(ui, cx, &plan, st);
                        } else {
                            // Routine restart details live in the disclosure. New
                            // issues (cross-site changes, drift, unsupported versions,
                            // etc.) must still be visible on every affected switch.
                            let notices: Vec<Notice> = plan
                                .warnings
                                .iter()
                                .filter(|w| !routine_restart_note(&w.text.key))
                                .cloned()
                                .collect();
                            notice_list(ui, cx, plan_notices_id(&plan), &notices);
                        }
                    } else {
                        hot_risk_copy(ui, cx, &plan);
                        plan_notes(ui, cx, &plan, st);
                        st.risk_shown_for = Some(plan.id.clone());
                    }
                });
            });
            if clicked_now {
                flow.switch_now(now);
            } else if clicked_cancel {
                flow.cancel(now);
            }
            ui.ctx().request_repaint();
        }
        BannerView::TurnsAtPlan {
            plan,
            busy,
            progress,
        } => {
            let mut action = None;
            banner_card(ui, pal, Some((progress, true)), |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    icons::paint(ui.painter(), r, Icon::Pause, pal.amber);
                    ui.add(
                        egui::Label::new(Text::title(
                            tr.f("banner.waiting.title", &[("n", &busy.len())]),
                            pal,
                        ))
                        .wrap(),
                    );
                });
                ui.label(Text::small(
                    tr.f("banner.destination", &[("dest", &cx.dest(&plan.to))]),
                    pal,
                ));
                dialog_body(ui, |ui| {
                    for t in &busy {
                        busy_line(ui, cx, t);
                    }
                    ui.label(Text::muted(tr.t("banner.waiting.body"), pal));
                });
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add(Button::primary(tr.t("banner.waiting.wait")).small())
                        .clicked()
                    {
                        action = Some(0);
                    }
                    if ui
                        .add(Button::danger(tr.t("banner.waiting.force")).small())
                        .clicked()
                    {
                        action = Some(1);
                    }
                    if ui
                        .add(Button::new(tr.t("action.cancel")).small().kbd("Esc"))
                        .clicked()
                    {
                        action = Some(2);
                    }
                });
            });
            match action {
                Some(0) => flow.acknowledge_wait(now),
                Some(1) => flow.switch_now(now),
                Some(2) => {
                    flow.cancel(now);
                }
                _ => {}
            }
        }
        BannerView::Blocked(plan) => {
            let mut close = false;
            let mut alternative = None;
            banner_card(ui, pal, None, |ui| {
                let mut actions: Vec<Button> = plan
                    .alternatives
                    .iter()
                    .map(|alt| Button::primary(tr.text(&alt.label)).small())
                    .collect();
                actions.push(Button::new(tr.t("action.close")).small().kbd("Esc"));
                let (_, buttons) = banner_header(
                    ui,
                    pal,
                    Some((Icon::Close, pal.red)),
                    tr.t("banner.blocked.title"),
                    HeaderAside::Buttons(actions),
                );
                for (i, button) in buttons.iter().enumerate() {
                    if button.clicked() {
                        if i == plan.alternatives.len() {
                            close = true;
                        } else {
                            alternative = Some(i);
                        }
                    }
                }
                ui.add_space(2.0);
                ui.label(Text::small(
                    tr.f("banner.destination", &[("dest", &cx.dest(&plan.to))]),
                    pal,
                ));
                dialog_body(ui, |ui| {
                    // What blocks the switch first, then the plan's notices.
                    if let RestartKind::Blocked { reasons } = &plan.primary {
                        for r in reasons {
                            bullet(ui, pal, &block_text(cx, r));
                        }
                    }
                    notice_list(ui, cx, plan_notices_id(&plan), &plan.warnings);
                });
            });
            if let Some(i) = alternative {
                flow.take_alternative(i, now);
            } else if close {
                flow.cancel(now);
            }
        }
        BannerView::Choose(plan) => {
            banner_card(ui, pal, None, |ui| choices(ui, cx, flow, &plan, st, now));
        }
        BannerView::WaitingRun => waiting_banner(ui, cx, flow, now),
        BannerView::Running {
            label,
            steps,
            elapsed,
            failed,
            reapply,
        } => {
            banner_card(ui, pal, None, |ui| {
                banner_header(
                    ui,
                    pal,
                    failed.then_some((Icon::Close, pal.red)),
                    tr.f(
                        match (failed, reapply) {
                            (true, false) => "shell.failed_title",
                            (true, true) => "banner.reapply.failed",
                            (false, false) => "banner.running.title",
                            (false, true) => "banner.reapply.running",
                        },
                        &[("dest", &label)],
                    ),
                    HeaderAside::Text(if failed {
                        String::new()
                    } else {
                        format!("{:.1} s", elapsed.as_secs_f32())
                    }),
                );
                ui.add_space(4.0);
                dialog_body(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (i, (label, status)) in steps.iter().enumerate() {
                            if i > 0 {
                                ui.label(Text::small("·", pal));
                            }
                            step_chip(ui, pal, label, status);
                        }
                    });
                    for (_, message, _) in flow.remote_progress() {
                        ui.label(tr.text(message));
                    }
                });
                if flow
                    .remote_progress()
                    .iter()
                    .any(|(_, _, can_stop)| *can_stop)
                    && ui.add(egui::Button::new(tr.t("action.cancel"))).clicked()
                {
                    flow.stop_remotes();
                }
            });
            if !failed {
                ui.ctx().request_repaint();
            }
        }
    }
    true
}

/// One busy thread line ("This computer · “title” is generating a reply").
pub(crate) fn busy_line(ui: &mut Ui, cx: &Cx<'_>, t: &superba_core::ThreadActivity) {
    if t.signal == superba_core::ActivitySignal::AppSetup {
        // The app's own first-start setup, not a conversation (user bug 59).
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.add(StatusDot::new(Status::Warn).pulse(true).size(7.0));
            ui.label(Text::body(cx.tr.t("banner.waiting.app_setup"), cx.pal));
        });
        return;
    }
    let local = t.target_id.is_local();
    let host = if local {
        cx.tr.t("banner.waiting.this_computer")
    } else {
        // Never the raw app host id (a remote-control device's thread, user bug 36).
        crate::pages::shared::hosts::target_name(cx, &t.target_id)
    };
    let title = t
        .title
        .clone()
        .or(t.thread_id.clone())
        .unwrap_or_else(|| cx.tr.t("banner.waiting.untitled"));
    let key = if local {
        "banner.waiting.thread_local"
    } else {
        "banner.waiting.thread_remote"
    };
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.add(StatusDot::new(Status::Warn).pulse(true).size(7.0));
        ui.label(Text::body(
            cx.tr.f(key, &[("host", &host), ("title", &title)]),
            cx.pal,
        ));
    });
}

/// A block reason with line ids replaced by their display names.
pub fn block_text(cx: &Cx<'_>, reason: &superba_core::BlockReason) -> String {
    let mut text = reason.text();
    if let Some((_, id)) = text.args.iter().find(|(n, _)| n == "line") {
        let name = cx.line_name(&superba_core::LineId::new(id.clone()));
        text = text.arg("line", name);
    }
    cx.tr.text(&text)
}

/// The risk copy of plan 7.3 for a hot restart.
fn hot_risk_copy(ui: &mut Ui, cx: &Cx<'_>, plan: &SwitchPlan) {
    let pal = cx.pal;
    let tr = cx.tr;
    ui.label(Text::muted(tr.t("banner.armed.body"), pal));
    if plan.primary.interrupts_turns() {
        bullet(ui, pal, &tr.t("banner.risk.replies"));
        bullet(ui, pal, &tr.t("banner.risk.mcp"));
        bullet(ui, pal, &tr.t("banner.risk.terminal"));
    }
}

fn remote_labels(cx: &Cx<'_>, plan: &SwitchPlan) -> Vec<String> {
    let remotes: Vec<&TargetId> = plan
        .restarts
        .iter()
        .filter_map(|r| match &r.consumer {
            ConsumerKind::Remote { target_id } => Some(target_id),
            _ => None,
        })
        .collect();
    remotes
        .into_iter()
        .map(|id| crate::pages::shared::hosts::target_name(cx, id))
        .collect()
}

fn cli_restart_note(key: &str) -> bool {
    key == "switch.warn.cli_processes"
}

fn remote_restart_note(key: &str) -> bool {
    key == "switch.warn.remotes"
}

fn routine_restart_note(key: &str) -> bool {
    cli_restart_note(key) || remote_restart_note(key) || key == "switch.warn.authapi_line"
}

/// Situational notes: CLI processes, remote hosts, warnings, key changes. The
/// notices are grouped and sorted, information collapsed (user bug 103).
fn plan_notes(ui: &mut Ui, cx: &Cx<'_>, plan: &SwitchPlan, st: &mut BannerState) {
    let tr = cx.tr;
    let cli = plan
        .restarts
        .iter()
        .find(|r| r.consumer == ConsumerKind::CliProcesses && !r.pids.is_empty());
    let remotes = remote_labels(cx, plan);
    let mut notices = Vec::new();
    if let Some(c) = cli {
        notices.push(Notice::info(
            LocalizedText::new("banner.note.cli", "").arg("n", c.pids.len()),
        ));
    }
    if !remotes.is_empty() {
        notices.push(Notice::warning(
            LocalizedText::new("banner.note.remotes", "").arg("hosts", remotes.join(", ")),
        ));
    }
    notices.extend(
        plan.warnings
            .iter()
            .filter(|w| {
                !((cli.is_some() && cli_restart_note(&w.text.key))
                    || (!remotes.is_empty() && remote_restart_note(&w.text.key)))
            })
            .cloned(),
    );
    if !notices.is_empty() {
        ui.add_space(space::XS);
        notice_list(ui, cx, plan_notices_id(plan), &notices);
    }
    let count = change_count(cx, plan);
    if count > 0 {
        let label = if st.show_changes {
            tr.t("banner.hide_changes")
        } else {
            tr.f("banner.show_changes", &[("n", &count)])
        };
        if link_button(ui, &label, false).clicked() {
            st.show_changes = !st.show_changes;
        }
        if st.show_changes {
            key_changes(ui, cx, plan);
        }
    }
}

/// Where the switch preview keeps its open notice disclosures.
pub fn plan_notices_id(plan: &SwitchPlan) -> egui::Id {
    egui::Id::unique(("plan-notices", &plan.id))
}

/// Number of rows [`key_changes`] shows.
pub fn change_count(cx: &Cx<'_>, plan: &SwitchPlan) -> usize {
    match cx.data.plan_settings_for(&plan.id) {
        Some(ps) if !ps.changes.is_empty() => {
            ps.changes.len()
                + usize::from(
                    plan.config
                        .as_ref()
                        .is_some_and(|c| c.auth_change.is_some()),
                )
        }
        _ => plan.config.as_ref().map_or(0, |c| {
            c.key_changes.len() + usize::from(c.auth_change.is_some())
        }),
    }
}

/// Keys named in the reason line of a re-apply countdown (the rest are counted).
const REAPPLY_KEYS: usize = 4;

/// The reason line of a countdown that re-applies the provider Codex already
/// uses (its connection or settings changed): "Why: “My relay” is in use and its
/// settings changed: model, service_tier". `None` when the plan is not a provider switch
/// or switches to another provider (its title names the destination).
pub fn reapply_reason(cx: &Cx<'_>, plan: &SwitchPlan) -> Option<String> {
    if !matches!(plan.request.intent, SwitchIntent::Profile { .. }) {
        return None;
    }
    let from = plan.from.as_ref()?.profile_id.as_ref()?;
    if plan.to.profile_id.as_ref() != Some(from) {
        return None;
    }
    let dest = cx.dest(&plan.to);
    let mut keys: Vec<String> = match cx
        .data
        .plan_settings_for(&plan.id)
        .filter(|ps| !ps.changes.is_empty())
    {
        Some(ps) => ps
            .changes
            .iter()
            .filter(|c| c.target.is_local())
            .map(|c| c.path.clone())
            .collect(),
        None => plan
            .config
            .as_ref()
            .map(|c| c.key_changes.iter().map(|k| k.path.clone()).collect())
            .unwrap_or_default(),
    };
    if plan
        .config
        .as_ref()
        .is_some_and(|c| c.auth_change.is_some())
    {
        keys.push("auth.json".into());
    }
    keys.dedup();
    if keys.is_empty() {
        return Some(cx.tr.f("banner.reapply.reason_other", &[("dest", &dest)]));
    }
    let shown = keys[..keys.len().min(REAPPLY_KEYS)].join(", ");
    let keys = if keys.len() > REAPPLY_KEYS {
        cx.tr.f(
            "banner.reapply.more",
            &[("keys", &shown), ("n", &(keys.len() - REAPPLY_KEYS))],
        )
    } else {
        shown
    };
    Some(
        cx.tr
            .f("banner.reapply.reason", &[("dest", &dest), ("keys", &keys)]),
    )
}

/// The diff preview: `path  before → after`, in monospace. With the switch
/// service's settings preview (`plan_settings`) every row also names its Codex
/// home (this computer or a remote host) and why it changes; otherwise the owned
/// keys of the local `config.toml`.
pub fn key_changes(ui: &mut Ui, cx: &Cx<'_>, plan: &SwitchPlan) {
    let pal = cx.pal;
    let preview = cx
        .data
        .plan_settings_for(&plan.id)
        .filter(|ps| !ps.changes.is_empty());
    let config = plan.config.as_ref();
    egui::Frame::new()
        .fill(pal.bg_subtle)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(radius::CONTROL)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            // An unset value shows as an em dash (Geist has no empty-set sign).
            // Rows in fixed columns: long values are cut (middle for paths and
            // URLs) and open into the full change below their row (user bug 15).
            let unset = "—";
            match preview {
                Some(ps) => {
                    let multi = ps.changes.iter().any(|c| c.target != ps.changes[0].target);
                    for c in &ps.changes {
                        let reason = cx.tr.text(&c.reason.text());
                        crate::pages::shared::values::change_row(
                            ui,
                            cx,
                            egui::Id::unique(("key-change", &plan.id, &c.target, &c.path)),
                            multi.then_some(c.target_label.as_str()),
                            &c.path,
                            c.from.as_deref(),
                            c.to.as_deref(),
                            Some(&reason),
                            unset,
                        );
                    }
                }
                None => {
                    for c in config.map(|c| c.key_changes.as_slice()).unwrap_or_default() {
                        crate::pages::shared::values::change_row(
                            ui,
                            cx,
                            egui::Id::unique(("key-change", &plan.id, &c.path)),
                            None,
                            &c.path,
                            c.before.as_deref(),
                            c.after.as_deref(),
                            None,
                            unset,
                        );
                    }
                }
            }
            if let Some(a) = config.and_then(|c| c.auth_change.as_ref()) {
                // The router's local token is not one of the user's keys: name it.
                let router_after = plan.to.label.key == "switch.destination.router";
                let router_before = crate::pages::router::mode(cx).is_router();
                let name = |shape: &superba_core::AuthShape, router: bool| {
                    if router && matches!(shape, superba_core::AuthShape::ApiKey { .. }) {
                        cx.tr.t("banner.router_token")
                    } else {
                        shape_name(shape)
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(Text::mono("auth.json", pal));
                    ui.label(Text::mono_muted(name(&a.before, router_before), pal));
                    ui.label(Text::mono_muted("→", pal));
                    ui.label(Text::mono(name(&a.after, router_after), pal));
                });
            }
        });
}

fn shape_name(s: &superba_core::AuthShape) -> String {
    match s {
        superba_core::AuthShape::Missing => "none".into(),
        superba_core::AuthShape::ChatGpt { .. } => "chatgpt".into(),
        superba_core::AuthShape::ApiKey { key_last4 } => format!("apikey ••••{key_last4}"),
        superba_core::AuthShape::Pat { key_last4 } => format!("pat ••••{key_last4}"),
        _ => "unknown".into(),
    }
}

fn bullet(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.horizontal_top(|ui| {
        ui.add_space(4.0);
        let (r, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
        ui.painter()
            .circle_filled(pos2(r.center().x, r.min.y + 9.0), 2.0, pal.text_subtle);
        ui.add(egui::Label::new(Text::muted(text, pal)).wrap());
    });
}

fn note(ui: &mut Ui, pal: &Palette, severity: Severity, text: &str) {
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
        let (icon, color) = match severity {
            Severity::Info => (Icon::Info, pal.text_subtle),
            Severity::Warning => (Icon::Warning, pal.amber),
            Severity::Danger => (Icon::Warning, pal.red),
        };
        icons::paint(
            ui.painter(),
            Rect::from_center_size(r.center(), vec2(14.0, 14.0)),
            icon,
            color,
        );
        ui.add(egui::Label::new(Text::small(text, pal)).wrap());
    });
}

/// The floating overlay surface, at most `max_h` high inside its padding (the
/// part a view draws in [`dialog_body`] scrolls); `bar` adds the countdown bar
/// (elapsed fraction, paused) as a hairline along the top edge.
fn banner_card(
    ui: &mut Ui,
    pal: &Palette,
    bar: Option<(f32, bool)>,
    add: impl FnOnce(&mut Ui),
) -> egui::Response {
    let max_h: f32 = ui
        .ctx()
        .data(|d| d.get_temp(egui::Id::unique("superba-overlay-max-h")))
        .unwrap_or(f32::INFINITY);
    let r = egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.border_strong))
        .corner_radius(radius::CARD)
        .shadow(egui::Shadow {
            offset: [0, 12],
            blur: 36,
            spread: 0,
            color: pal.shadow,
        })
        .inner_margin(egui::Margin {
            left: 18,
            right: 18,
            top: 14,
            bottom: 14,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            // Each view draws its long part in a `dialog_body`: the title and
            // the buttons stay in view while it scrolls (user bug 103).
            bounded(ui, egui::Id::unique("switch-overlay"), max_h, add);
        })
        .response;
    if let Some((progress, paused)) = bar {
        // A 2 px progress line inside the top border.
        let line = Rect::from_min_max(
            pos2(r.rect.min.x + radius::CARD as f32, r.rect.min.y),
            pos2(r.rect.max.x - radius::CARD as f32, r.rect.min.y + 2.0),
        );
        let mut fill = line;
        fill.set_width(line.width() * progress.clamp(0.0, 1.0));
        ui.painter().rect_filled(line, 1, pal.active);
        ui.painter()
            .rect_filled(fill, 1, if paused { pal.amber } else { pal.text });
    }
    ui.interact(r.rect, ui.scope_id().with("banner-hover"), Sense::hover())
}

fn waiting_banner(ui: &mut Ui, cx: &Cx<'_>, flow: &mut Flow, now: Duration) {
    let pal = cx.pal;
    let tr = cx.tr;
    let Some(run) = flow.run() else {
        return;
    };
    let busy: Vec<superba_core::ThreadActivity> = run
        .turns
        .iter()
        .flat_map(|t| t.busy.iter().cloned())
        .collect();
    let count = busy.len();
    let waited = Duration::from_millis(run.waited_ms);
    let can_decide = run.decision.is_some();
    let acknowledged = run.wait_acknowledged;
    let mut decision = None;
    let mut ack = false;
    banner_card(ui, pal, Some((0.55, true)), |ui| {
        banner_header(
            ui,
            pal,
            Some((Icon::Pause, pal.amber)),
            tr.f("banner.waiting.title", &[("n", &count)]),
            HeaderAside::Text(tr.f(
                "banner.waiting.waited",
                &[(
                    "t",
                    &format!("{}:{:02}", waited.as_secs() / 60, waited.as_secs() % 60),
                )],
            )),
        );
        dialog_body(ui, |ui| {
            for t in &busy {
                busy_line(ui, cx, t);
            }
            ui.label(Text::muted(tr.t("banner.waiting.body"), pal));
        });
        ui.horizontal_wrapped(|ui| {
            let primary = if acknowledged {
                Button::primary(tr.t("banner.waiting.will_switch"))
                    .small()
                    .enabled(false)
            } else {
                Button::primary(tr.t("banner.waiting.wait"))
                    .small()
                    .enabled(can_decide)
            };
            if ui.add(primary).clicked() {
                ack = true;
            }
            if ui
                .add(
                    Button::danger(tr.t("banner.waiting.force"))
                        .small()
                        .enabled(can_decide),
                )
                .clicked()
            {
                decision = Some(Decision::Force);
            }
            if ui
                .add(
                    Button::new(tr.t("action.cancel"))
                        .small()
                        .kbd("Esc")
                        .enabled(can_decide),
                )
                .clicked()
            {
                decision = Some(Decision::Cancel);
            }
        });
    });
    if ack {
        flow.acknowledge_wait(now);
    }
    if let Some(d) = decision {
        flow.decide(d);
    }
    ui.ctx().request_repaint_after(Duration::from_millis(250));
}

/// The label of one step in the switch banner's chain.
pub fn step_label(cx_tr: &crate::i18n::Tr, step: &SwitchStep) -> String {
    let key = match step {
        SwitchStep::WriteConfig => "step.write_config",
        SwitchStep::WriteEnv => "step.write_env",
        SwitchStep::MigrateSessions => "step.migrate",
        SwitchStep::QuitApp => "step.quit",
        SwitchStep::RestartCore => "step.restart_core",
        SwitchStep::StopProcesses => "step.stop_processes",
        SwitchStep::LaunchApp => "step.launch",
        SwitchStep::VerifyProcess => "step.verify_process",
        SwitchStep::VerifyFiles => "step.verify_files",
        SwitchStep::VerifyAuth => "step.verify_auth",
        SwitchStep::VerifyLine => "step.verify_line",
        SwitchStep::VerifyRenderer => "step.verify_renderer",
        SwitchStep::Remote { target_id } => {
            // "WSL: Ubuntu", an SSH alias; never the app's raw host id.
            let host = cx_tr.text(&superba_core::target_title(target_id));
            return cx_tr.f("step.remote", &[("host", &host)]);
        }
        _ => "step.other",
    };
    cx_tr.t(key)
}

/// The compact chain shown while executing: config, core, auth, line (+ remotes).
fn progress_steps(run: &Run) -> Vec<(SwitchStep, StepStatus)> {
    let full = matches!(run.plan.primary, RestartKind::FullAppRestart { .. });
    let mut chain: Vec<SwitchStep> = Vec::new();
    if full {
        chain.push(SwitchStep::QuitApp);
    }
    chain.push(SwitchStep::WriteConfig);
    if run.plan.migration.is_some() {
        chain.push(SwitchStep::MigrateSessions);
    }
    chain.push(if full {
        SwitchStep::LaunchApp
    } else {
        SwitchStep::RestartCore
    });
    chain.push(SwitchStep::VerifyAuth);
    chain.push(SwitchStep::VerifyLine);
    for (s, _) in &run.steps {
        if matches!(s, SwitchStep::Remote { .. }) && !chain.contains(s) {
            chain.push(s.clone());
        }
    }
    chain
        .into_iter()
        .map(|s| {
            let st = run.step(&s).cloned().unwrap_or(StepStatus::Pending);
            (s, st)
        })
        .collect()
}

fn step_chip(ui: &mut Ui, pal: &Palette, label: &str, status: &StepStatus) {
    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    let color = match status {
        StepStatus::Done => {
            icons::paint(ui.painter(), r, Icon::Check, pal.text);
            pal.text
        }
        StepStatus::Running => {
            icons::spinner(ui.painter(), r, pal.text, ui.input(|i| i.time));
            pal.text
        }
        StepStatus::Failed { .. } => {
            icons::paint(ui.painter(), r.shrink(1.0), Icon::Close, pal.red);
            pal.text
        }
        StepStatus::Skipped => {
            ui.painter().hline(
                r.x_range().shrink(3.0),
                r.center().y,
                Stroke::new(1.5, pal.text_subtle),
            );
            pal.text_subtle
        }
        StepStatus::Pending => {
            ui.painter()
                .circle_stroke(r.center(), 3.0, Stroke::new(1.0, pal.text_subtle));
            pal.text_subtle
        }
    };
    ui.label(
        RichText::new(label)
            .font(font(text::SMALL, Weight::Medium))
            .color(color),
    );
}

fn choices(
    ui: &mut Ui,
    cx: &Cx<'_>,
    flow: &mut Flow,
    plan: &SwitchPlan,
    st: &mut BannerState,
    now: Duration,
) {
    let pal = cx.pal;
    let tr = cx.tr;
    let (_, buttons) = banner_header(
        ui,
        pal,
        Some((Icon::Warning, pal.amber)),
        tr.f("banner.choose.title", &[("dest", &cx.dest(&plan.to))]),
        HeaderAside::Buttons(vec![Button::new(tr.t("action.cancel")).small().kbd("Esc")]),
    );
    if buttons.iter().any(egui::Response::clicked) {
        flow.cancel(now);
    }
    let mut accept = Vec::new();
    dialog_body(ui, |ui| {
        for choice in &plan.choices {
            ui.add_space(6.0);
            match choice {
                PlanChoice::LegacySessions { providers } => {
                    let (provider, count) = providers
                        .iter()
                        .max_by_key(|(_, c)| **c)
                        .map(|(p, c)| (p.clone(), *c))
                        .unwrap_or_default();
                    ui.label(Text::strong(
                        tr.f(
                            "wizard.legacy.title",
                            &[("count", &count), ("provider", &provider)],
                        ),
                        pal,
                    ));
                    ui.label(Text::small(tr.t("wizard.legacy.migrate_body"), pal));
                    ui.label(Text::small(
                        tr.f("wizard.legacy.skip_body", &[("count", &count)]),
                        pal,
                    ));
                    ui.horizontal(|ui| {
                        if ui
                            .add(Button::primary(tr.t("wizard.legacy.migrate")).small())
                            .clicked()
                        {
                            flow.answer_legacy_sessions(LegacySessionChoice::Migrate, now);
                        }
                        if ui
                            .add(Button::new(tr.t("wizard.legacy.skip")).small())
                            .clicked()
                        {
                            flow.answer_legacy_sessions(LegacySessionChoice::Skip, now);
                        }
                    });
                }
                PlanChoice::RestoreProvider {
                    baseline_provider,
                    created_during,
                    migrated_in,
                } => {
                    ui.label(Text::body(
                        tr.f(
                            "banner.restore.body",
                            &[
                                ("provider", baseline_provider),
                                ("created", created_during),
                                ("migrated", migrated_in),
                            ],
                        ),
                        pal,
                    ));
                    ui.label(Text::body(tr.t("banner.restore.reverse_note"), pal));
                    ui.label(Text::muted(tr.t("banner.restore.keep_note"), pal));
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add(Button::primary(tr.t("banner.restore.reverse")).small())
                            .clicked()
                        {
                            flow.answer_restore_provider(
                                ProviderRestore::Baseline {
                                    reverse_migrate: true,
                                },
                                now,
                            );
                        }
                        if ui
                            .add(Button::new(tr.t("banner.restore.keep")).small())
                            .clicked()
                        {
                            flow.answer_restore_provider(ProviderRestore::KeepOpenai, now);
                        }
                        if ui
                            .add(Button::new(tr.t("banner.restore.exact")).small())
                            .clicked()
                        {
                            flow.answer_restore_provider(
                                ProviderRestore::Baseline {
                                    reverse_migrate: false,
                                },
                                now,
                            );
                        }
                    });
                }
                PlanChoice::ReplacementModel {
                    current,
                    suggestions,
                } => {
                    if st.model.is_none() {
                        st.model = suggestions.first().cloned();
                    }
                    let site = plan
                        .to
                        .site_id
                        .as_ref()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(Text::body(
                            tr.f("banner.model.body", &[("site", &site), ("model", current)]),
                            pal,
                        ));
                        let selected = st.model.clone().unwrap_or_default();
                        Dropdown::new("replacement-model")
                            .selected_text(
                                RichText::new(&selected)
                                    .font(mono_font(text::SMALL, Weight::Regular)),
                            )
                            .show_ui(ui, |ui| {
                                for s in suggestions {
                                    ui.selectable_value(&mut st.model, Some(s.clone()), s);
                                }
                            });
                        if ui
                            .add(Button::primary(tr.t("wizard.continue")).small())
                            .clicked()
                            && let Some(m) = st.model.clone()
                        {
                            flow.answer_model(m, now);
                        }
                    });
                }
                PlanChoice::OwnedKeyConflict {
                    conflicts,
                    auth,
                    source,
                } => {
                    // GH#5: who changed what Superba manages, and take over / let
                    // that tool manage it / keep its values (not for a credential:
                    // a switch cannot route Codex with another tool's key) / Esc.
                    ui.label(Text::body(
                        tr.f(
                            "banner.conflict.body_source",
                            &[("source", &cx.localize(&source.text()))],
                        ),
                        pal,
                    ));
                    for c in conflicts {
                        crate::pages::shared::values::change_row(
                            ui,
                            cx,
                            egui::Id::unique(("key-conflict", &plan.id, &c.path)),
                            None,
                            &c.path,
                            c.before.as_deref(),
                            c.after.as_deref(),
                            None,
                            "—",
                        );
                    }
                    if let Some(a) = auth {
                        crate::external::auth_row(ui, cx, &plan.id, a);
                    }
                    ui.horizontal_wrapped(|ui| {
                        let mut answer = None;
                        if ui
                            .add(Button::primary(tr.t("banner.conflict.take_over")).small())
                            .clicked()
                        {
                            answer = Some(KeyResolution::Overwrite);
                        }
                        let hand_off =
                            crate::external::hand_off_label(cx, source, "banner.conflict.hand_off");
                        if ui.add(Button::new(hand_off).small()).clicked() {
                            flow.request(crate::external::hand_off_request(source), now);
                        }
                        if auth.is_none()
                            && ui
                                .add(Button::new(tr.t("banner.conflict.keep")).small())
                                .clicked()
                        {
                            answer = Some(KeyResolution::KeepCurrent);
                        }
                        if let Some(a) = answer {
                            let mut request = plan.request.clone();
                            for c in conflicts {
                                request.key_resolutions.insert(c.path.clone(), a);
                            }
                            if auth.is_some() {
                                request
                                    .key_resolutions
                                    .insert(superba_core::AUTH_JSON_PATH.to_owned(), a);
                            }
                            flow.request(request, now);
                        }
                    });
                }
                PlanChoice::ForeignEnv { lines } => {
                    ui.label(Text::body(
                        tr.f("banner.foreign_env", &[("n", &lines.len())]),
                        pal,
                    ));
                    accept.push(choice.clone());
                }
                PlanChoice::UnknownVersion { consumer } => {
                    ui.label(Text::body(
                        tr.f("banner.unknown_version", &[("consumer", consumer)]),
                        pal,
                    ));
                    accept.push(choice.clone());
                }
                PlanChoice::SharedHome { targets } => {
                    let names: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
                    ui.label(Text::body(
                        tr.f("banner.shared_home", &[("hosts", &names.join(", "))]),
                        pal,
                    ));
                    accept.push(choice.clone());
                }
                _ => {}
            }
        }
    });
    if !accept.is_empty() {
        ui.add_space(4.0);
        if ui
            .add(Button::primary(tr.t("banner.accept")).small())
            .clicked()
        {
            flow.accept_risks(&accept, now);
        }
    }
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------

/// Number of sessions in a migration, whose thread count may include several
/// database rows for each session.
pub fn migration_sessions(m: &MigrationPlan) -> u32 {
    if m.items.is_empty() {
        m.thread_count.max(m.rollout_count)
    } else {
        u32::try_from(m.items.len()).unwrap_or(u32::MAX)
    }
}

/// Where the full-restart confirmation keeps its open notice disclosures.
pub fn confirm_notices_id(plan: &SwitchPlan) -> egui::Id {
    egui::Id::unique(("confirm-full-notices", &plan.id))
}

/// The full-restart confirmation (plan 7.3 ConfirmFull).
pub fn confirm_full(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    flow: &mut Flow,
    st: &mut BannerState,
    now: Duration,
) {
    let Stage::ConfirmFull { plan } = flow.stage() else {
        return;
    };
    let plan = plan.clone();
    let pal = cx.pal;
    let tr = cx.tr;
    let mut action: Option<u8> = None;
    // "Reopen ChatGPT with Superba" (GH#7, user bug 61): no switch, nothing to
    // write; the app is closed and opened again through Superba.
    let reopen = matches!(plan.request.intent, SwitchIntent::RestartApp { .. });
    let resp = dialog(ctx, egui::Id::unique("confirm-full"), 560.0, |ui| {
        ui.label(Text::h2(
            tr.t(if reopen {
                "confirm.title_reopen"
            } else {
                "confirm.title"
            }),
            pal,
        ));
        ui.label(Text::small(
            tr.f("banner.destination", &[("dest", &cx.dest(&plan.to))]),
            pal,
        ));
        if let Some(reason) = reapply_reason(cx, &plan) {
            ui.add(egui::Label::new(Text::small(reason, pal)).wrap());
        }
        dialog_body(ui, |ui| {
            ui.add_space(space::XS);
            if let RestartKind::FullAppRestart { reason } = &plan.primary {
                ui.horizontal_top(|ui| {
                    ui.label(Text::strong(tr.t("confirm.reason"), pal));
                    ui.add(egui::Label::new(Text::body(tr.text(&reason.text()), pal)).wrap());
                });
            }
            if let Some(m) = &plan.migration {
                note(
                    ui,
                    pal,
                    Severity::Warning,
                    &tr.f("confirm.migration", &[("n", &migration_sessions(m))]),
                );
            }
            ui.add(egui::Label::new(Text::muted(tr.t("confirm.body"), pal)).wrap());
            // Running turns (GH#7): the restart waits for them, and says so now.
            if !plan.active_threads.is_empty() && plan.primary.interrupts_turns() {
                ui.add_space(space::XS);
                note(
                    ui,
                    pal,
                    Severity::Warning,
                    &tr.f("confirm.turns", &[("n", &plan.active_threads.len())]),
                );
                for t in &plan.active_threads {
                    busy_line(ui, cx, t);
                }
            }
            notice_list(ui, cx, confirm_notices_id(&plan), &plan.warnings);
            if !plan.cleanup.is_empty() {
                ui.add_space(space::XS);
                ui.label(Text::strong(tr.t("confirm.cleanup"), pal));
                for c in &plan.cleanup {
                    bullet(ui, pal, &tr.text(c));
                }
            }
            if change_count(cx, &plan) > 0 {
                ui.add_space(space::XS);
                key_changes(ui, cx, &plan);
            }
        });
        ui.add_space(space::XS);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::primary(tr.t(if reopen {
                    "confirm.reopen"
                } else {
                    "confirm.full_restart"
                })))
                .clicked()
            {
                action = Some(0);
            }
            if plan.migration.is_none()
                && !reopen
                && ui.add(Button::new(tr.t("confirm.write_only"))).clicked()
            {
                action = Some(1);
            }
            for (i, alt) in plan.alternatives.iter().enumerate() {
                if ui.add(Button::new(tr.text(&alt.label))).clicked() {
                    action = Some(10 + i as u8);
                }
            }
            if ui
                .add(Button::ghost(tr.t("action.cancel")).kbd("Esc"))
                .clicked()
            {
                action = Some(2);
            }
        });
    });
    if resp.should_close() && action.is_none() {
        action = Some(2);
    }
    let _ = st;
    match action {
        Some(0) => flow.confirm_full(false, now),
        Some(1) => flow.confirm_full(true, now),
        Some(2) => {
            flow.cancel(now);
        }
        Some(n) if n >= 10 => flow.take_alternative(usize::from(n - 10), now),
        _ => {}
    }
}

/// The processes of the app still running after a quit (pid, image,
/// architecture, parent, start) and what was tried (user bug 53).
fn quit_report(ui: &mut egui::Ui, cx: &Cx<'_>, report: &superba_core::QuitReport) {
    let pal = cx.pal;
    for p in &report.left {
        ui.add(egui::Label::new(Text::mono_muted(p.describe(), pal)).wrap());
    }
    let tried = cx
        .tr
        .text(&superba_switch::messages::quit_steps(&report.steps));
    ui.label(Text::small(
        cx.tr.f("decide.quit_tried", &[("steps", &tried)]),
        pal,
    ));
    if superba_switch::messages::quit_access_denied(report) {
        ui.add(
            egui::Label::new(Text::small(
                cx.tr
                    .text(&superba_switch::messages::text("switch.quit.access_denied")),
                pal,
            ))
            .wrap(),
        );
    }
}

/// Dialogs for questions other than "wait for turns" (that one lives in the banner).
pub fn decision_dialog(ctx: &egui::Context, cx: &Cx<'_>, flow: &mut Flow, now: Duration) {
    let Some(run) = flow.run_mut() else {
        return;
    };
    let Some(pending) = run.decision.as_ref() else {
        return;
    };
    if matches!(pending.request, DecisionRequest::WaitForTurns { .. }) {
        return;
    }
    let request = pending.request.clone();
    let recovery = matches!(request, DecisionRequest::VerificationFailed { .. });
    let stop_question = matches!(
        request,
        DecisionRequest::CloseConsumers { .. } | DecisionRequest::ForceStop { .. }
    );
    let stop_offered = request.allowed().contains(&Decision::StopProcesses);
    let remaining = run.rollback_remaining(now);
    let pal = cx.pal;
    let tr = cx.tr;
    let mut answer = None;
    dialog(ctx, egui::Id::unique("decision"), 520.0, |ui| {
        let (title, body) = match &request {
            // Windows: closing the window leaves the app running in the
            // notification area, so without CDP there is nothing to wait for.
            DecisionRequest::ForceQuit { .. } if cfg!(windows) => (
                "decide.force_quit.title_windows",
                "decide.force_quit.body_windows",
            ),
            DecisionRequest::ForceQuit { .. } => {
                ("decide.force_quit.title", "decide.force_quit.body")
            }
            DecisionRequest::AppStillRunning { .. } => {
                ("decide.still_running.title", "decide.still_running.body")
            }
            DecisionRequest::IpcFailed { .. } => ("decide.ipc.title", "decide.ipc.body"),
            DecisionRequest::VerificationFailed { .. } => {
                ("decide.verify.title", "decide.verify.body")
            }
            DecisionRequest::UnknownRemoteActivity { .. } => {
                ("decide.remote.title", "decide.remote.body")
            }
            DecisionRequest::CloseConsumers { .. } => {
                ("decide.consumers.title", "decide.consumers.body")
            }
            // Windows cannot ask terminal programs or pipe app-servers to exit.
            DecisionRequest::ForceStop { .. } if cfg!(windows) => {
                ("decide.force_stop.title", "decide.force_stop.body_windows")
            }
            DecisionRequest::ForceStop { .. } => {
                ("decide.force_stop.title", "decide.force_stop.body")
            }
            _ => ("decide.other.title", "decide.other.body"),
        };
        ui.label(Text::h2(tr.t(title), pal));
        ui.add(egui::Label::new(Text::muted(tr.t(body), pal)).wrap());
        // The details (process lists, a quit report) scroll; the rollback
        // countdown and the answers stay in view.
        dialog_body(ui, |ui| {
            match &request {
                DecisionRequest::IpcFailed { error } => {
                    ui.label(Text::mono_muted(error_text(tr, error), pal));
                }
                DecisionRequest::VerificationFailed { verification } => {
                    let rows = [
                        ("verify.process", &verification.process),
                        ("verify.files", &verification.files),
                        ("verify.auth", &verification.auth),
                        ("verify.line", &verification.line),
                        ("verify.renderer", &verification.renderer),
                    ];
                    for (key, r) in rows {
                        ui.horizontal_wrapped(|ui| {
                            let (st, detail) = match r {
                                superba_core::CheckResult::Passed => (Status::Ok, String::new()),
                                superba_core::CheckResult::Failed { detail } => {
                                    (Status::Error, detail.clone())
                                }
                                superba_core::CheckResult::Skipped { reason } => {
                                    (Status::Idle, reason.clone())
                                }
                                superba_core::CheckResult::Pending => (Status::Idle, String::new()),
                            };
                            ui.add(StatusDot::new(st));
                            ui.label(Text::body(tr.t(key), pal));
                            if !detail.is_empty() {
                                ui.label(Text::small(detail, pal));
                            }
                        });
                    }
                }
                DecisionRequest::ForceQuit { pids, report } if report.left.is_empty() => {
                    ui.label(Text::mono_muted(
                        pids.iter()
                            .map(|p| p.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        pal,
                    ));
                }
                // User bug 53: which of the app's processes still run, what was tried.
                DecisionRequest::ForceQuit { report, .. }
                | DecisionRequest::AppStillRunning { report } => {
                    quit_report(ui, cx, report);
                }
                DecisionRequest::CloseConsumers { processes, busy }
                | DecisionRequest::ForceStop { processes, busy } => {
                    crate::in_effect::process_list(ui, cx, processes, busy);
                }
                DecisionRequest::UnknownRemoteActivity { targets } => {
                    let names: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
                    ui.label(Text::mono_muted(names.join(", "), pal));
                }
                _ => {}
            }
        });
        if recovery {
            ui.add_space(4.0);
            if let Some(remaining) = remaining {
                let secs = remaining.as_millis().div_ceil(1000);
                ui.label(Text::strong(
                    tr.f("shell.rollback_in", &[("secs", &secs)]),
                    pal,
                ));
                ui.add(CountdownBar::new(1.0 - remaining.as_secs_f32() / 10.0));
                ctx.request_repaint_after(Duration::from_millis(100));
            } else {
                ui.label(Text::strong(tr.t("shell.rollback_stopped"), pal));
            }
            ui.label(Text::small(tr.t("shell.rollback_shortcuts"), pal));
        }
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            let still_running = matches!(request, DecisionRequest::AppStillRunning { .. });
            for d in request.allowed() {
                let (label, kind) = match d {
                    Decision::Force if still_running => (tr.t("decide.still_running.end"), 2),
                    Decision::Continue if still_running => (tr.t("decide.still_running.retry"), 1),
                    Decision::Force => (tr.t("decide.force"), 2),
                    Decision::Cancel => (tr.t("action.cancel"), 0),
                    Decision::FullRestart => (tr.t("decide.full_restart"), 1),
                    Decision::Undo => (tr.t("decide.undo"), 1),
                    Decision::KeepWritten => (
                        tr.t(if recovery {
                            "shell.keep_new"
                        } else {
                            "decide.keep"
                        }),
                        0,
                    ),
                    Decision::Rollback => (tr.t("decide.rollback"), 1),
                    Decision::Continue if stop_question => (tr.t("decide.check_again"), 1),
                    Decision::Continue => (tr.t("decide.continue"), 1),
                    Decision::Wait => (tr.t("banner.waiting.wait"), 1),
                    Decision::StopProcesses => (tr.t("decide.stop_processes"), 3),
                };
                // "Stop them for me" leads (it only asks the processes to
                // exit; ending one by force is a second, danger question).
                let safe = *d == request.safe_default() && !stop_offered;
                let mut b = match kind {
                    2 if stop_question => Button::danger(tr.t("decide.end_by_force")),
                    2 => Button::danger(label),
                    3 => Button::primary(label),
                    _ if safe => Button::primary(label),
                    _ => Button::new(label),
                };
                if recovery && *d == Decision::Rollback {
                    b = b.kbd("Enter");
                }
                if ui.add(b).clicked() {
                    answer = Some(*d);
                }
            }
        });
    });
    if let Some(d) = answer {
        flow.decide(d);
    }
}

/// What a finished switch applied, for its toast and log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// Codex now uses another provider or line: its name.
    Switched(String),
    /// The provider (and line) Codex already used was applied again because its
    /// settings, key or models changed: "Provider · Line · PAT".
    Reapplied(String),
    /// Router mode turned on or off.
    Router(bool),
    /// Settings pushed to a remote host: its name.
    HostSynced(String),
}

/// Re-applying the provider Codex already uses (same profile and line): its
/// settings, key, models or authentication changed, nothing was switched.
pub fn is_reapply(plan: &SwitchPlan) -> bool {
    matches!(plan.request.intent, SwitchIntent::Profile { .. })
        && plan.from.as_ref().is_some_and(|from| {
            from.profile_id.is_some()
                && from.profile_id == plan.to.profile_id
                && (plan.to.line_id.is_none() || from.line_id == plan.to.line_id)
        })
}

/// What `plan` applied (user bug 25: a re-apply said "Switched to <line>").
/// `host` names a remote host.
pub fn applied(
    tr: &crate::i18n::Tr,
    data: &crate::pages::Data,
    plan: &SwitchPlan,
    host: &dyn Fn(&TargetId) -> String,
) -> Applied {
    match &plan.request.intent {
        SwitchIntent::Router { enable } => return Applied::Router(*enable),
        SwitchIntent::HostSync { target_id, .. } => return Applied::HostSynced(host(target_id)),
        _ => {}
    }
    if is_reapply(plan) {
        return Applied::Reapplied(crate::pages::dest_text(tr, data, &plan.to));
    }
    // A line switch on a provider with several lines names the line; anything
    // else names the destination.
    let line = plan.to.line_id.as_ref().filter(|_| {
        plan.to
            .site_id
            .as_ref()
            .and_then(|s| data.site(s))
            .is_some_and(|s| s.lines.len() > 1)
            && plan.to.site_id.as_ref() == data.active_site_id()
    });
    Applied::Switched(match line {
        Some(l) => data.line_label(tr, plan.to.site_id.as_ref(), l),
        None => crate::pages::dest_text(tr, data, &plan.to),
    })
}

impl Applied {
    /// The toast title: "已切换到 线路", "已把改动应用到「供应商 · 线路 · PAT」".
    pub fn title(&self, tr: &crate::i18n::Tr) -> String {
        match self {
            Applied::Switched(line) => tr.f("shell.switched", &[("line", line)]),
            Applied::Reapplied(dest) => tr.f("shell.reapplied", &[("dest", dest)]),
            Applied::Router(true) => tr.t("shell.router_on"),
            Applied::Router(false) => tr.t("shell.router_off"),
            Applied::HostSynced(host) => tr.f("shell.host_synced", &[("host", host)]),
        }
    }

    /// The activity-log line, with the time it took.
    pub fn log_text(&self, tr: &crate::i18n::Tr, elapsed_ms: u64) -> String {
        match self {
            Applied::Switched(line) => switched_text(tr, line, elapsed_ms),
            other => tr.f(
                "toast.timed",
                &[
                    ("title", &other.title(tr)),
                    ("secs", &format!("{:.1}", elapsed_ms as f64 / 1000.0)),
                ],
            ),
        }
    }
}

/// Title for the toast after a hot switch: "Switched to Overseas · 1.2 s".
pub fn switched_text(tr: &crate::i18n::Tr, line: &str, elapsed_ms: u64) -> String {
    tr.f(
        "toast.switched",
        &[
            ("line", &line),
            ("secs", &format!("{:.1}", elapsed_ms as f64 / 1000.0)),
        ],
    )
}

/// Draws a thin divider.
pub fn divider(ui: &mut Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(1.0, pal.border));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{fonts, style, tokens::size};
    use crate::i18n::Tr;
    use superba_core::Locale;

    #[test]
    fn banner_titles_and_controls_never_overlap_at_minimum_or_default_width() {
        for width in [size::MIN_WINDOW[0], 1180.0] {
            for locale in [Locale::En, Locale::ZhCn] {
                for kind in ["armed", "running", "blocked"] {
                    let ctx = egui::Context::default();
                    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
                    style::install(&ctx, Some(egui::Theme::Dark));
                    let tr = Tr::new(locale);
                    let destination = "Destination ".repeat(5);
                    assert_eq!(destination.chars().count(), 60);
                    let title = match kind {
                        "armed" => tr.f(
                            "banner.armed.title",
                            &[("dest", &destination), ("secs", &2)],
                        ),
                        "running" => tr.f("banner.running.title", &[("dest", &destination)]),
                        _ => format!("{}: {destination}", tr.t("banner.blocked.title")),
                    };
                    let mut button_rects = Vec::new();
                    let mut title_rect = Rect::NOTHING;
                    let input = egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            pos2(0.0, 0.0),
                            vec2(width, size::MIN_WINDOW[1]),
                        )),
                        ..Default::default()
                    };
                    let output = ctx.run_ui(input, |ui| {
                        // Same sidebar, page margins and card padding as the app.
                        ui.set_width(width - size::SIDEBAR - 64.0 - 42.0);
                        let available = ui.available_rect_before_wrap();
                        let aside = match kind {
                            "armed" => HeaderAside::Buttons(vec![
                                Button::primary(tr.t("banner.switch_now"))
                                    .small()
                                    .kbd("Enter"),
                                Button::new(tr.t("action.cancel")).small().kbd("Esc"),
                            ]),
                            "running" => HeaderAside::Text("3.3 s".into()),
                            _ => HeaderAside::Buttons(vec![
                                Button::primary(tr.t("action.retry")).small(),
                                Button::new(tr.t("action.close")).small().kbd("Esc"),
                            ]),
                        };
                        let (label, buttons) = banner_header(
                            ui,
                            Palette::of(ui.ctx()),
                            Some((Icon::Bolt, Color32::WHITE)),
                            title.clone(),
                            aside,
                        );
                        title_rect = label.rect;
                        button_rects = buttons.into_iter().map(|b| b.rect).collect();
                        assert!(
                            ui.min_rect().right() <= available.right() + 1.0,
                            "{kind}: header overflow at {width}"
                        );
                    });
                    let mut title_galley = None;
                    let mut elapsed = None;
                    for shape in &output.shapes {
                        if let egui::Shape::Text(text) = &shape.shape {
                            let rect = text.galley.rect.translate(text.pos.to_vec2());
                            if text.galley.text() == title {
                                title_galley = Some(rect);
                            }
                            if text.galley.text() == "3.3 s" {
                                elapsed = Some(rect);
                            }
                        }
                    }
                    let galley = title_galley.expect("the title must be painted");
                    assert!(title_rect.expand(1.0).contains_rect(galley));
                    for button in button_rects {
                        assert!(
                            !galley.intersects(button),
                            "{kind} title overlaps button at {width}: {galley:?}, {button:?}"
                        );
                    }
                    if let Some(elapsed) = elapsed {
                        assert!(
                            !galley.intersects(elapsed),
                            "running title overlaps elapsed time"
                        );
                    }
                    output.drop_without_applying_deltas();
                }
            }
        }
    }
}
