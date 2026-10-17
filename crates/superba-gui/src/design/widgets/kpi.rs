//! Dashboard figures after Surge (user request 24): a big bold number with a
//! smaller unit next to it ("97 ms", "3.25 M"), a small gray caption above, and
//! the formatting that splits values into number and unit (tabular figures come
//! from the bundled fonts).

use egui::{Align2, Color32, Response, Sense, Ui, Widget, pos2, vec2};

use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, text};

/// A value split for display: an optional small prefix ("$"), the number in
/// large type and an optional small unit after it ("ms", "K").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KpiValue {
    /// Small text before the number (currency).
    pub prefix: String,
    /// The number.
    pub number: String,
    /// Small text after the number.
    pub unit: String,
}

impl KpiValue {
    /// A number without prefix or unit.
    pub fn plain(number: impl Into<String>) -> Self {
        Self {
            number: number.into(),
            ..Self::default()
        }
    }

    /// A number with a unit.
    pub fn with_unit(number: impl Into<String>, unit: impl Into<String>) -> Self {
        Self {
            number: number.into(),
            unit: unit.into(),
            ..Self::default()
        }
    }

    /// "—" (no data).
    pub fn none() -> Self {
        Self::plain("—")
    }

    /// The value as one line of text ("97 ms", "$12.84", "1.2 K").
    pub fn text(&self) -> String {
        let mut s = format!("{}{}", self.prefix, self.number);
        if !self.unit.is_empty() {
            s.push(' ');
            s.push_str(&self.unit);
        }
        s
    }
}

/// `n` with three significant figures and a K / M / B suffix as the unit:
/// 870 -> ("870", ""), 12 345 -> ("12.3", "K"), 1 234 567 -> ("1.23", "M").
pub fn si(n: u64) -> KpiValue {
    const STEPS: [(f64, &str); 3] = [(1e9, "B"), (1e6, "M"), (1e3, "K")];
    for (scale, unit) in STEPS {
        if n as f64 >= scale * 0.9995 {
            return KpiValue::with_unit(sig3(n as f64 / scale), unit);
        }
    }
    KpiValue::plain(n.to_string())
}

/// Three significant figures without trailing zeros: 1.234 -> "1.23",
/// 12.34 -> "12.3", 123.4 -> "123", 2.0 -> "2".
pub fn sig3(x: f64) -> String {
    let decimals = if x >= 99.95 {
        0
    } else if x >= 9.995 {
        1
    } else {
        2
    };
    let s = format!("{x:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

/// A duration: below a second in ms ("97", "ms"), else seconds with up to two
/// decimals ("1.25", "s").
pub fn millis(ms: u64) -> KpiValue {
    if ms < 1000 {
        KpiValue::with_unit(ms.to_string(), "ms")
    } else {
        KpiValue::with_unit(sig3(ms as f64 / 1000.0), "s")
    }
}

/// US dollars from micro-dollars: "$" prefix, two decimals below 1,000, whole
/// dollars with separators above ("$12.84", "$1,284").
pub fn usd(micros: i64) -> KpiValue {
    let dollars = micros as f64 / 1e6;
    let number = if dollars.abs() < 1000.0 {
        format!("{dollars:.2}")
    } else {
        group(dollars.round() as i64)
    };
    KpiValue {
        prefix: "$".into(),
        number,
        unit: String::new(),
    }
}

/// A rate with one decimal below 10 ("2.4"), whole numbers below 1,000 ("87"),
/// else [`si`] ("1.2 K").
pub fn rate(x: f64) -> KpiValue {
    if !x.is_finite() || x <= 0.0 {
        return KpiValue::plain("0");
    }
    if x < 10.0 {
        let s = format!("{x:.1}");
        return KpiValue::plain(s.trim_end_matches(".0").to_owned());
    }
    if x < 1000.0 {
        return KpiValue::plain(format!("{}", x.round() as u64));
    }
    si(x.round() as u64)
}

/// `n` with thousands separators ("12,345").
pub fn group(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// A big number with its prefix and unit, left aligned, baselines shared.
#[must_use = "add it with ui.add(..)"]
pub struct Kpi {
    value: KpiValue,
    size: f32,
    color: Option<Color32>,
}

impl Kpi {
    /// A KPI in the dashboard size ([`text::KPI`]).
    pub fn new(value: KpiValue) -> Self {
        Self {
            value,
            size: text::KPI,
            color: None,
        }
    }

    /// The smaller size used for secondary figures ([`text::KPI_SMALL`]).
    pub fn small(mut self) -> Self {
        self.size = text::KPI_SMALL;
        self
    }

    /// Number size in points.
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Number colour (default: primary text).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }
}

impl Widget for Kpi {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let color = self.color.unwrap_or(pal.text);
        let unit_size = if self.size >= text::KPI {
            text::KPI_UNIT
        } else {
            text::SMALL
        };
        let number = ui.painter().layout_no_wrap(
            self.value.number.clone(),
            font(self.size, Weight::Semibold),
            color,
        );
        let small = |s: &str| {
            (!s.is_empty()).then(|| {
                ui.painter()
                    .layout_no_wrap(s.to_owned(), font(unit_size, Weight::Medium), pal.text)
            })
        };
        let prefix = small(&self.value.prefix);
        let unit = small(&self.value.unit);
        let gap = (self.size * 0.1).max(3.5);
        let width = number.size().x
            + prefix.as_ref().map_or(0.0, |g| g.size().x + 1.0)
            + unit.as_ref().map_or(0.0, |g| g.size().x + gap);
        let height = number.size().y;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let label = self.value.text();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
        // Baselines: the number's baseline sits at about 0.8 of its row height
        // (Geist ascent / line height); small text is aligned to it.
        let baseline = rect.min.y + height * 0.8;
        let painter = ui.painter();
        let mut x = rect.min.x;
        if let Some(g) = prefix {
            let y = baseline - g.size().y * 0.8;
            let w = g.size().x;
            painter.galley(pos2(x, y), g, pal.text);
            x += w + 1.0;
        }
        let w = number.size().x;
        painter.galley(pos2(x, rect.min.y), number, color);
        x += w + gap;
        if let Some(g) = unit {
            let y = baseline - g.size().y * 0.8;
            painter.galley(pos2(x, y), g, pal.text);
        }
        response
    }
}

/// A small gray caption (card title, fact label) at `pos`, left-center aligned;
/// returns its width.
pub fn caption_at(ui: &Ui, pos: egui::Pos2, s: &str, color: Color32) -> f32 {
    ui.painter()
        .text(
            pos,
            Align2::LEFT_CENTER,
            s,
            font(text::CAPTION + 1.0, Weight::Medium),
            color,
        )
        .width()
}
