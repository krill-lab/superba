//! The one way a tool is labelled outside the Tools and Updates pages: its
//! icon tile (the official logo where one is licensed, else the monogram;
//! [`crate::design::tool_icons`]) before its name. Usage (requests, the tool
//! filter, stats, session groups) and the Router's entry table use it, so a
//! new view picks the icons up by calling these helpers.

use egui::{Pos2, Response, RichText, Ui, vec2};
use superba_core::RequestHarness;

use crate::design::widgets::MonogramTile;

/// Side of the small tile next to a tool's name in tables and lists.
pub const TILE: f32 = 16.0;
/// Room the tile takes before the text (tile and gap).
pub const ROOM: f32 = TILE + 6.0;

/// The tile identity of a request's tool (`None`: an unidentified client).
pub fn monogram(h: RequestHarness) -> Option<&'static str> {
    Some(match h {
        RequestHarness::Codex => "cx",
        RequestHarness::ClaudeCode => "cc",
        RequestHarness::Dsh => "ds",
        RequestHarness::OpenCode => "oc",
        RequestHarness::Pi => "pi",
        RequestHarness::ZCode => "zc",
        RequestHarness::WorkBuddy => "wb",
        RequestHarness::Gemini => "gm",
        _ => return None,
    })
}

/// The tool of an aggregate key: a tool id (`claude`) or a tool-scoped key
/// (tool × model, a session).
pub fn monogram_of_key(key: &str) -> Option<&'static str> {
    RequestHarness::from_id(key)
        .or_else(|| superba_core::split_harness_key(key).map(|(h, _)| h))
        .and_then(monogram)
}

/// Paint the small tile centred on `center` (the caller names the tool in
/// text next to it, so the tile adds no accessible name of its own).
pub fn paint(ui: &Ui, monogram: &str, center: Pos2) {
    MonogramTile::new(monogram).size(TILE).paint_at(ui, center);
}

/// The tile, then `text`, in a row; the response is the text's.
pub fn tool_label(ui: &mut Ui, monogram: Option<&str>, text: RichText) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if let Some(m) = monogram {
            let (r, _) = ui.allocate_exact_size(vec2(TILE, TILE), egui::Sense::hover());
            paint(ui, m, r.center());
        }
        ui.label(text)
    })
    .inner
}
