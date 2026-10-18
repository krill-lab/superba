//! Accessibility names (user bug 56): every control a screen reader or UI
//! Automation reaches has a name — a text field, dropdown or segmented control
//! the visible label next to it (AccessKit *labelled-by*), a control without a
//! visible label a name of its own — and rows, cells, tabs and banners say
//! what they show.
//!
//! egui names its own buttons and check boxes from their text. Superba's
//! widgets name themselves where they know their text (buttons, tabs, rows,
//! table cells). A text field, a dropdown or a segmented control does not know
//! the label drawn next to it: [`label_unnamed_by`] names every control a
//! closure added that has no name yet by such a label ([`super::widgets::FormRow`]
//! does it for its controls), [`name_unnamed`] gives them a name of their own.
//!
//! Unlike egui's `Ui::label_inputs_by`, a placeholder does not count as a
//! name: UI Automation shows it as help text, so a field named only by its
//! placeholder reads as an unlabeled `Edit ''`. Focusable groups (a segmented
//! control's track, a list or table the arrow keys move in) count as controls.
//!
//! All of this costs nothing while AccessKit is off (no screen reader, no UI
//! Automation client): egui then keeps no accessibility nodes.

use egui::accesskit::{Action, Node, Role};
use egui::{Id, Response, Ui};

/// AccessKit is on for `ui`'s context: egui builds an accessibility tree this
/// pass (every visible `Ui` has a node in it).
pub fn enabled(ui: &Ui) -> bool {
    ui.ctx().has_accesskit_node(ui.unique_id())
}

/// The node is something a user operates or moves the keyboard focus to: a
/// control (egui's input roles), or anything that takes clicks or the focus.
/// Text labels name themselves (by their value).
pub fn needs_name(node: &Node) -> bool {
    let role = node.role();
    if matches!(role, Role::Label | Role::ScrollBar | Role::Window) {
        return false;
    }
    egui::accessibility::is_input(role)
        || node.supports_action(Action::Click)
        || node.supports_action(Action::Focus)
}

/// The node has a name: a non-blank label, or nodes it is labelled by.
pub fn is_named(node: &Node) -> bool {
    node.label().is_some_and(|l| !l.trim().is_empty()) || !node.labelled_by().is_empty()
}

/// How many widgets this pass has registered on `ui`'s layer so far.
fn widget_count(ui: &Ui) -> usize {
    let layer = ui.layer_id();
    ui.ctx()
        .viewport(|v| v.this_pass.widgets.get_layer(layer).count())
}

/// Controls registered on `ui`'s layer from index `first` on that have no
/// name.
fn unnamed_since(ui: &Ui, first: usize) -> Vec<Id> {
    let layer = ui.layer_id();
    ui.ctx().viewport(|v| {
        let Some(state) = &v.this_pass.accesskit_state else {
            return Vec::new();
        };
        let mut ids: Vec<Id> = v
            .this_pass
            .widgets
            .get_layer(layer)
            .skip(first)
            .filter(|w| {
                state
                    .nodes
                    .get(&w.id)
                    .is_some_and(|n| needs_name(n) && !is_named(n))
            })
            .map(|w| w.id)
            .collect();
        ids.dedup();
        ids
    })
}

/// Run `add`, then mark every control it added without a name as labelled by
/// the node `label` (the visible label next to them).
pub fn label_unnamed_by<R>(ui: &mut Ui, label: Id, add: impl FnOnce(&mut Ui) -> R) -> R {
    if !enabled(ui) {
        return add(ui);
    }
    let first = widget_count(ui);
    let inner = add(ui);
    for id in unnamed_since(ui, first) {
        ui.ctx().accesskit_node_builder(id, |node| {
            node.clear_label();
            node.push_labelled_by(label.accesskit_id());
        });
    }
    inner
}

/// Run `add`, then name every control it added without a name `name` (a
/// control without a visible label, e.g. a search field or a filter).
pub fn name_unnamed<R>(ui: &mut Ui, name: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    if !enabled(ui) {
        return add(ui);
    }
    let first = widget_count(ui);
    let inner = add(ui);
    for id in unnamed_since(ui, first) {
        ui.ctx()
            .accesskit_node_builder(id, |node| node.set_label(name.to_owned()));
    }
    inner
}

/// Run `add`, then return the first node it added that names something (a
/// text label, or a control with a name such as a radio button): the label a
/// form row draws with its own widgets.
pub fn first_named<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> (R, Option<Id>) {
    first_added(ui, add, |n| {
        is_named(n) || (n.role() == Role::Label && n.value().is_some())
    })
}

/// Run `add`, then return the first text label it added (a dialog's title).
pub fn first_text<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> (R, Option<Id>) {
    first_added(ui, add, |n| {
        n.role() == Role::Label && n.value().is_some_and(|v| !v.trim().is_empty())
    })
}

fn first_added<R>(
    ui: &mut Ui,
    add: impl FnOnce(&mut Ui) -> R,
    pick: impl Fn(&Node) -> bool,
) -> (R, Option<Id>) {
    if !enabled(ui) {
        return (add(ui), None);
    }
    let first = widget_count(ui);
    let inner = add(ui);
    let layer = ui.layer_id();
    let id = ui.ctx().viewport(|v| {
        let state = v.this_pass.accesskit_state.as_ref()?;
        v.this_pass
            .widgets
            .get_layer(layer)
            .skip(first)
            .find(|w| state.nodes.get(&w.id).is_some_and(&pick))
            .map(|w| w.id)
    });
    (inner, id)
}

/// Name the dialog (modal) `ui` is drawn in by the node `title`.
pub fn name_dialog(ui: &Ui, title: Id) {
    let mut stack = Some(ui.stack().clone());
    while let Some(s) = stack {
        if s.kind() == Some(egui::UiKind::Modal) {
            ui.ctx().accesskit_node_builder(s.unique_id, |node| {
                node.clear_label();
                node.push_labelled_by(title.accesskit_id());
            });
            return;
        }
        stack = s.parent.clone();
    }
}

/// Run `add`, then return the texts of the text labels it added (their full
/// values, in drawing order): the name of a row drawn from labels.
pub fn texts_added<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> (R, Vec<String>) {
    if !enabled(ui) {
        return (add(ui), Vec::new());
    }
    let first = widget_count(ui);
    let inner = add(ui);
    let layer = ui.layer_id();
    let texts = ui.ctx().viewport(|v| {
        let Some(state) = &v.this_pass.accesskit_state else {
            return Vec::new();
        };
        v.this_pass
            .widgets
            .get_layer(layer)
            .skip(first)
            .filter_map(|w| {
                let n = state.nodes.get(&w.id)?;
                let text = match n.role() {
                    Role::Label => n.value(),
                    _ => None,
                }?;
                (!text.trim().is_empty()).then(|| text.to_owned())
            })
            .collect()
    });
    (inner, texts)
}

/// Give the node of `response` a role and a name (a focusable area egui made a
/// role-less node for, e.g. a list the arrow keys move in).
pub fn describe(response: &Response, role: Role, name: &str) {
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(role);
        if !name.is_empty() {
            node.set_label(name.to_owned());
        }
    });
}

/// A selectable row or entry: `role` (a table `Row`, a `ListItem`, a `Tab`),
/// its name, whether it is the selected one, and a click (egui's node of a
/// clicked area has the click action already).
pub fn selectable(response: &Response, role: Role, name: &str, selected: bool) {
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(role);
        node.set_label(name.to_owned());
        node.set_selected(selected);
    });
}

/// A text a widget paints itself (a banner line, a pane title), as a text node
/// of `response` (its area) with the full, unshortened text.
pub fn text(response: &Response, text: &str) {
    response.widget_info(|| egui::WidgetInfo::labeled(Role::Label, true, text));
}
