//! Hand over a complete font family between frames, including diagnostics.

use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Context, FontDefinitions};

use super::FontReport;

type Loaded = (FontReport, FontDefinitions);

/// Installed faces and an optional background replacement.
pub struct InstalledFonts {
    report: FontReport,
    pending: Option<Receiver<Loaded>>,
}

impl InstalledFonts {
    pub(super) fn new(ctx: &Context, report: FontReport, definitions: FontDefinitions) -> Self {
        ctx.set_fonts(definitions);
        Self {
            report,
            pending: None,
        }
    }

    /// Faces currently supplied to egui (also used by diagnostics).
    pub fn report(&self) -> &FontReport {
        &self.report
    }

    /// Apply all three weights together before laying out any text this frame.
    pub fn poll(&mut self, ctx: &Context) {
        let Some(pending) = &self.pending else { return };
        match pending.try_recv() {
            Ok((report, definitions)) => {
                ctx.set_fonts(definitions);
                self.report = report;
                self.pending = None;
                ctx.request_repaint();
                tracing::debug!("Background CJK fonts installed");
            }
            Err(TryRecvError::Disconnected) => self.pending = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub(super) fn defer(
        &mut self,
        ctx: &Context,
        load: impl FnOnce() -> Option<Loaded> + Send + 'static,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        match std::thread::Builder::new()
            .name("cjk-fonts".into())
            .spawn(move || {
                if let Some(loaded) = load()
                    && tx.send(loaded).is_ok()
                {
                    ctx.request_repaint();
                }
            }) {
            Ok(_) => self.pending = Some(rx),
            Err(error) => tracing::warn!(%error, "Could not start CJK font conversion"),
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/font_loading.rs"]
mod tests;
