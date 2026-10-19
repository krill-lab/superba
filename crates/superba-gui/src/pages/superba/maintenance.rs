//! Small helpers shared by the maintenance tables.
use std::{future::Future, time::Duration};

use egui::{Response, Ui};

use crate::pages::Cx;
use crate::pages::shared::loading::{self, Latest};

/// One bounded background read. Explicit refresh discards the previous answer.
#[derive(Debug)]
pub(crate) struct Read<T> {
    pub value: Option<T>,
    pub load: Latest<(), T>,
    attempted: bool,
}

impl<T> Default for Read<T> {
    fn default() -> Self {
        Self {
            value: None,
            load: Latest::default(),
            attempted: false,
        }
    }
}

impl<T: Send + 'static> Read<T> {
    pub fn invalidate(&mut self) {
        self.value = None;
        self.load = Latest::default();
        self.attempted = false;
    }

    pub fn poll<F, Fut>(&mut self, ui: &Ui, read: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = superba_core::Result<T>> + Send + 'static,
    {
        if !self.attempted {
            self.attempted = true;
            self.load
                .start((), crate::runtime::task(read()), Duration::from_secs(90));
        }
        if let Some(((), value)) = self.load.poll() {
            self.value = Some(value);
        }
        if self.load.loading() {
            loading::repaint_soon(ui);
        }
    }

    pub fn message(&self, ui: &mut Ui, cx: &Cx<'_>) {
        if self.load.loading() {
            loading::spinner_small(ui, cx, &cx.tr.t("superba.maintenance.loading"));
        } else if let Some(failure) = self.load.failure() {
            ui.add(
                egui::Label::new(crate::design::widgets::Text::small(
                    failure.text(cx),
                    cx.pal,
                ))
                .wrap(),
            );
        }
    }
}

/// Make the entire row an accessible expander, including its unabridged facts.
pub(crate) fn expander(ui: &Ui, response: Response, name: &str, open: &mut bool) {
    let response = response.interact(egui::Sense::click());
    if response.clicked() {
        *open = !*open;
    }
    ui.input_mut(|i| {
        i.consume_accesskit_action_requests(response.id, |r| match r.action {
            egui::accesskit::Action::Expand => {
                *open = true;
                true
            }
            egui::accesskit::Action::Collapse => {
                *open = false;
                true
            }
            _ => false,
        });
    });
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
    ui.ctx().accesskit_node_builder(response.id, |n| {
        n.set_expanded(*open);
        n.add_action(if *open {
            egui::accesskit::Action::Collapse
        } else {
            egui::accesskit::Action::Expand
        });
    });
}

pub(crate) fn bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    }
}
