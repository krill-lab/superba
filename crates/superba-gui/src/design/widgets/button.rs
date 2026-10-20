//! Pill buttons and square icon buttons.

use egui::{
    Color32, CursorIcon, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, vec2,
};

use super::{focus_ring, hover_t, show_focus};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, size, text};

/// Visual weight of a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// White fill / black text in dark mode (black / white in light mode).
    Primary,
    /// Surface fill with a 1 px border.
    Secondary,
    /// No fill until hovered.
    Ghost,
    /// Red fill for destructive actions ("force switch").
    Danger,
}

/// Height of a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonSize {
    /// 28 px.
    Small,
    /// 32 px.
    Medium,
    /// 40 px.
    Large,
}

impl ButtonSize {
    fn height(self) -> f32 {
        match self {
            ButtonSize::Small => size::CONTROL_SM,
            ButtonSize::Medium => size::CONTROL,
            ButtonSize::Large => size::CONTROL_LG,
        }
    }

    fn padding(self) -> f32 {
        match self {
            ButtonSize::Small => 12.0,
            ButtonSize::Medium => 14.0,
            ButtonSize::Large => 20.0,
        }
    }

    fn font_size(self) -> f32 {
        match self {
            ButtonSize::Small => text::SMALL,
            ButtonSize::Medium | ButtonSize::Large => text::BODY,
        }
    }
}

/// A pill-shaped text button with optional leading icon, spinner and keyboard hint.
#[must_use = "add it with ui.add(..)"]
pub struct Button {
    text: String,
    kind: ButtonKind,
    size: ButtonSize,
    icon: Option<Icon>,
    trailing: Option<Icon>,
    loading: bool,
    enabled: bool,
    min_width: f32,
    kbd: Option<String>,
}

impl Button {
    /// A secondary medium button.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: ButtonKind::Secondary,
            size: ButtonSize::Medium,
            icon: None,
            trailing: None,
            loading: false,
            enabled: true,
            min_width: 0.0,
            kbd: None,
        }
    }

    /// A primary button.
    pub fn primary(text: impl Into<String>) -> Self {
        Self::new(text).kind(ButtonKind::Primary)
    }

    /// A ghost button.
    pub fn ghost(text: impl Into<String>) -> Self {
        Self::new(text).kind(ButtonKind::Ghost)
    }

    /// A danger button.
    pub fn danger(text: impl Into<String>) -> Self {
        Self::new(text).kind(ButtonKind::Danger)
    }

    /// Set the kind.
    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// Set the size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Small size.
    pub fn small(self) -> Self {
        self.size(ButtonSize::Small)
    }

    /// Large size.
    pub fn large(self) -> Self {
        self.size(ButtonSize::Large)
    }

    /// Leading icon.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Trailing icon (e.g. an arrow or external-link mark).
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.trailing = Some(icon);
        self
    }

    /// Show a spinner instead of the icon and ignore clicks.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// Enable or disable.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Minimum width.
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }

    /// Keyboard hint chip shown at the end (`Enter`, `Esc`).
    pub fn kbd(mut self, key: impl Into<String>) -> Self {
        self.kbd = Some(key.into());
        self
    }

    /// Measured size, including padding, icons and keyboard hints. Containers can
    /// reserve this space before wrapping adjacent text.
    pub fn desired_size(&self, ui: &Ui) -> egui::Vec2 {
        self.layout(ui).size
    }

    fn layout(&self, ui: &Ui) -> ButtonLayout {
        let galley = ui.painter().layout_no_wrap(
            self.text.clone(),
            font(self.size.font_size(), Weight::Medium),
            Color32::PLACEHOLDER,
        );
        let kbd = self.kbd.as_ref().map(|key| {
            ui.painter().layout_no_wrap(
                key.clone(),
                mono_font(text::CAPTION - 1.0, Weight::Medium),
                Color32::PLACEHOLDER,
            )
        });
        let mut width = self.size.padding() * 2.0 + galley.size().x;
        if self.icon.is_some() || self.loading {
            width += size::ICON + 6.0;
        }
        if self.trailing.is_some() {
            width += size::ICON + 6.0;
        }
        if let Some(key) = &kbd {
            width += key.size().x + 16.0;
        }
        ButtonLayout {
            galley,
            kbd,
            size: vec2(
                width.max(self.min_width).max(self.size.height()),
                self.size.height(),
            ),
        }
    }
}

struct ButtonLayout {
    galley: std::sync::Arc<egui::Galley>,
    kbd: Option<std::sync::Arc<egui::Galley>>,
    size: egui::Vec2,
}

struct Colors {
    fill: Color32,
    text: Color32,
    border: Option<Color32>,
}

fn colors(kind: ButtonKind, enabled: bool, t: f32, pressed: bool, pal: &Palette) -> Colors {
    if !enabled {
        return Colors {
            fill: pal.hover,
            text: pal.text_disabled,
            border: Some(pal.border),
        };
    }
    let lerp = |a: Color32, b: Color32| a.lerp_to_gamma(b, t);
    match kind {
        ButtonKind::Primary => Colors {
            fill: if pressed {
                pal.primary.lerp_to_gamma(pal.text_muted, 0.25)
            } else {
                lerp(pal.primary, pal.primary_hover)
            },
            text: pal.on_primary,
            border: None,
        },
        ButtonKind::Secondary => Colors {
            fill: if pressed {
                pal.active
            } else {
                lerp(pal.surface, pal.hover)
            },
            text: pal.text,
            border: Some(lerp(pal.border, pal.border_strong)),
        },
        ButtonKind::Ghost => Colors {
            fill: if pressed {
                pal.active
            } else {
                lerp(Color32::TRANSPARENT, pal.hover)
            },
            text: lerp(pal.text_muted, pal.text),
            border: None,
        },
        ButtonKind::Danger => Colors {
            fill: if pressed {
                pal.red.lerp_to_gamma(Color32::BLACK, 0.15)
            } else {
                lerp(pal.red, pal.red.lerp_to_gamma(Color32::WHITE, 0.12))
            },
            text: pal.on_danger,
            border: None,
        },
    }
}

impl Widget for Button {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let ButtonLayout {
            galley,
            kbd: kbd_galley,
            size: measured,
        } = self.layout(ui);
        let icon_size = size::ICON;
        let gap = 6.0;
        let leading = self.icon.is_some() || self.loading;
        let sense = if self.enabled && !self.loading {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(measured, sense);
        let response = if self.enabled && !self.loading {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        };
        let label = self.text.clone();
        let enabled = self.enabled;
        response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, &label));

        // Before the visibility check: Tab to a button below the fold scrolls it in.
        let focused = show_focus(&response);
        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let pressed = response.is_pointer_button_down_on();
            let c = colors(self.kind, self.enabled, t, pressed, pal);
            let painter = ui.painter();
            painter.rect(
                rect,
                radius::PILL,
                c.fill,
                c.border.map_or(Stroke::NONE, |b| Stroke::new(1.0, b)),
                StrokeKind::Inside,
            );
            let mut content_w = galley.size().x;
            if leading {
                content_w += icon_size + gap;
            }
            if self.trailing.is_some() {
                content_w += icon_size + gap;
            }
            if let Some(k) = &kbd_galley {
                content_w += k.size().x + 10.0 + gap;
            }
            let mut x = rect.center().x - content_w / 2.0;
            let cy = rect.center().y;
            if leading {
                let icon_rect = Rect::from_min_size(
                    egui::pos2(x, cy - icon_size / 2.0),
                    vec2(icon_size, icon_size),
                );
                if self.loading {
                    icons::spinner(painter, icon_rect.shrink(1.0), c.text, ui.input(|i| i.time));
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(80));
                } else if let Some(icon) = self.icon {
                    icons::paint(painter, icon_rect, icon, c.text);
                }
                x += icon_size + gap;
            }
            let text_pos = egui::pos2(x, cy - galley.size().y / 2.0);
            x += galley.size().x;
            painter.galley(text_pos, galley, c.text);
            if let Some(icon) = self.trailing {
                x += gap;
                let icon_rect = Rect::from_min_size(
                    egui::pos2(x, cy - icon_size / 2.0),
                    vec2(icon_size, icon_size),
                );
                icons::paint(painter, icon_rect.shrink(1.0), icon, c.text);
                x += icon_size;
            }
            if let Some(k) = kbd_galley {
                x += gap;
                let chip =
                    Rect::from_min_size(egui::pos2(x, cy - 9.0), vec2(k.size().x + 10.0, 18.0));
                let chip_fill = c.text.gamma_multiply(0.12);
                painter.rect_filled(chip, radius::INNER, chip_fill);
                painter.galley(
                    egui::pos2(chip.min.x + 5.0, cy - k.size().y / 2.0),
                    k,
                    c.text.gamma_multiply(0.8),
                );
            }
            if focused {
                focus_ring(painter, rect, radius::PILL, pal);
            }
        }
        response
    }
}

/// A square ghost button showing only an icon (with a hover tooltip).
#[must_use = "add it with ui.add(..)"]
pub struct IconButton {
    icon: Icon,
    tooltip: String,
    size: f32,
    enabled: bool,
    checked: Option<bool>,
}

impl IconButton {
    /// New icon button; `tooltip` is also the accessible label.
    pub fn new(icon: Icon, tooltip: impl Into<String>) -> Self {
        Self {
            icon,
            tooltip: tooltip.into(),
            size: size::CONTROL,
            enabled: true,
            checked: None,
        }
    }

    /// Side length.
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Enable or disable.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// An on / off toggle drawn as an icon (eye / eye-off): screen readers
    /// get a check box with this state, named by the tooltip.
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }
}

impl Widget for IconButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(vec2(self.size, self.size), sense);
        let tooltip = self.tooltip.clone();
        let enabled = self.enabled;
        let checked = self.checked;
        response.widget_info(|| match checked {
            Some(on) => WidgetInfo::selected(egui::Role::CheckBox, enabled, on, &tooltip),
            None => WidgetInfo::labeled(egui::Role::Button, enabled, &tooltip),
        });
        let focused = show_focus(&response);
        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let fill = Color32::TRANSPARENT.lerp_to_gamma(pal.hover, t);
            ui.painter().rect_filled(rect, radius::CONTROL, fill);
            let color = if self.enabled {
                pal.text_muted.lerp_to_gamma(pal.text, t)
            } else {
                pal.text_disabled
            };
            let icon_rect = Rect::from_center_size(rect.center(), vec2(size::ICON, size::ICON));
            icons::paint(ui.painter(), icon_rect, self.icon, color);
            if focused {
                focus_ring(ui.painter(), rect, radius::CONTROL, pal);
            }
        }
        if self.enabled {
            response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(self.tooltip)
        } else {
            response.on_hover_text(self.tooltip)
        }
    }
}
