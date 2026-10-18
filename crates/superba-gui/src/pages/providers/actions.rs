//! Per-provider actions of the Providers page (UX §3.3): the right-click menu of
//! a list entry (rename, duplicate, test, delete; only the ones the entry
//! offers), the inline rename in the list, and the rules that refuse a delete
//! (the provider in use, the local router's only member, the official login),
//! with the reason shown where the action is offered.
//!
//! Renaming and duplicating only save a profile ([`Action::SaveProfile`]) and
//! deleting removes it on the page's own task ([`super::ops::remove_provider`]
//! behind the confirmation dialog); none of them plans or runs a switch, so Codex
//! is not restarted for them.

use egui::text::CCursorRange;
use egui::text_edit::TextEditState;
use egui::{Id, Key, Response, RichText, TextEdit, Ui, vec2};
use superba_core::ProfileId;

use super::{RemoveDialog, State, Tab, detail, list};
use crate::design::fonts::{Weight, font};
use crate::design::tokens::text;
use crate::design::widgets::Text;
use crate::pages::shared::providers::{Provider, ProviderId, ProviderKind};
use crate::pages::{Action, Cx};

/// The inline rename of one list entry.
#[derive(Clone, Debug)]
pub(crate) struct Rename {
    /// Whose name.
    pub provider: ProviderId,
    /// The typed name.
    pub text: String,
    /// First frame: focus the field and select its text.
    pub fresh: bool,
}

/// What the inline name field asks for after a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RenameOutcome {
    /// Enter with a valid name: save it (trimmed).
    Save(String),
    /// Escape, or the field lost focus (a click elsewhere): drop the edit.
    Cancel,
}

/// The entry has a name of its own to edit (a provider with a profile; the
/// official login and a custom site without a key have none).
pub(super) fn can_rename(p: &Provider) -> bool {
    p.kind != ProviderKind::Official && p.profile.is_some()
}

/// The entry has a connection to test (not the official login).
pub(super) fn can_test(p: &Provider) -> bool {
    p.kind != ProviderKind::Official
}

/// Why `p` cannot be deleted now (`None` = it can).
pub(super) fn delete_refusal(cx: &Cx<'_>, p: &Provider) -> Option<String> {
    if p.kind == ProviderKind::Official {
        return Some(cx.tr.t("providers.delete.official"));
    }
    let site_in_use =
        p.kind == ProviderKind::Custom && cx.data.active_site_id() == Some(&p.site.id);
    if p.applied || site_in_use {
        return Some(cx.tr.t("providers.delete.in_use"));
    }
    // The profiles the delete removes: the provider's own, or every profile of
    // a custom site. The router must keep at least one member.
    let removed = |id: &ProfileId| match p.kind {
        ProviderKind::Custom => cx.data.profile(id).is_some_and(|q| q.site_id == p.site.id),
        _ => p.profile_id() == Some(id),
    };
    let router = &cx.data.profiles.value.as_ref()?.settings.router;
    if router.enabled && !router.members.is_empty() && router.members.iter().all(removed) {
        return Some(cx.tr.t("providers.delete.router_only"));
    }
    None
}

/// Start the inline rename of `p` in the list (selects it).
pub(super) fn start_rename(state: &mut State, p: &Provider) {
    let Some(profile) = p.profile.as_ref().filter(|_| can_rename(p)) else {
        return;
    };
    if state.is_modal_open() {
        return;
    }
    state.select(p.id.clone());
    state.rename_armed = None;
    state.rename = Some(Rename {
        provider: p.id.clone(),
        text: profile.name.clone(),
        fresh: true,
    });
}

/// Open the delete confirmation of `p` (nothing is deleted before it is
/// confirmed there).
pub(super) fn open_remove(state: &mut State, p: &Provider) {
    if state.is_modal_open() {
        return;
    }
    state.rename = None;
    state.rename_armed = None;
    state.remove = Some(RemoveDialog {
        provider: p.clone(),
        error: None,
        task: None,
    });
}

/// Test the connection of `p` and show the results on its Connection tab.
pub(super) fn test(state: &mut State, cx: &Cx<'_>, p: &Provider, out: &mut Vec<Action>) {
    state.select(p.id.clone());
    state.tab = Tab::Connection;
    detail::test_connection(state, cx, p, out);
}

/// A test of `p` is running.
pub(super) fn testing(state: &State, cx: &Cx<'_>, p: &Provider) -> bool {
    state.check_task.as_ref().is_some_and(|(id, _)| id == &p.id)
        || state.tests.contains_key(&p.site.id)
        || cx.data.caps.get(&p.site.id).is_some_and(|c| c.loading)
}

/// Why `text` cannot become the name of `id` (`None` = it can): empty, or the
/// name of another entry of the list (ignoring case and surrounding spaces).
pub(super) fn rename_problem(
    cx: &Cx<'_>,
    all: &[Provider],
    id: &ProviderId,
    text: &str,
) -> Option<String> {
    let name = text.trim();
    if name.is_empty() {
        return Some(cx.tr.t("providers.rename.empty"));
    }
    let lower = name.to_lowercase();
    all.iter()
        .filter(|q| &q.id != id)
        .any(|q| list::name(cx, q).trim().to_lowercase() == lower)
        .then(|| cx.tr.f("providers.rename.taken", &[("name", &name)]))
}

/// The right-click menu of a list entry: rename, duplicate, test and delete,
/// as far as the entry offers them; a refused delete says why.
pub(super) fn context_menu(
    resp: &Response,
    cx: &Cx<'_>,
    state: &mut State,
    p: &Provider,
    out: &mut Vec<Action>,
) {
    resp.context_menu(|ui| {
        ui.set_min_width(200.0);
        ui.set_max_width(260.0);
        let mut above = false;
        if can_rename(p) {
            above = true;
            if ui.button(cx.tr.t("providers.menu.rename")).clicked() {
                start_rename(state, p);
                ui.close();
            }
        }
        // Duplicating copies the profile (no key), like renaming it needs one.
        if let Some(profile) = p.profile.as_ref().filter(|_| can_rename(p))
            && ui.button(cx.tr.t("providers.menu.duplicate")).clicked()
        {
            let copy = detail::duplicate(cx, profile);
            let id = copy.id.clone();
            out.push(Action::SaveProfile {
                profile: Box::new(copy),
                key: None,
            });
            state.select(ProviderId::Profile(id));
            ui.close();
        }
        if can_test(p) {
            above = true;
            if ui
                .add_enabled(
                    !testing(state, cx, p),
                    egui::Button::new(cx.tr.t("providers.menu.test")),
                )
                .clicked()
            {
                test(state, cx, p, out);
                ui.close();
            }
        }
        if above {
            ui.separator();
        }
        let refusal = delete_refusal(cx, p);
        let delete = ui.add_enabled(
            refusal.is_none() && !cx.busy,
            egui::Button::new(RichText::new(cx.tr.t("providers.menu.delete")).color(cx.pal.red)),
        );
        if delete.clicked() {
            open_remove(state, p);
            ui.close();
        }
        if let Some(why) = refusal {
            ui.add(egui::Label::new(Text::caption(why, cx.pal)).wrap());
        }
    });
}

/// The name field of a row being renamed (in place of the name) and the line
/// under it: the keys, or why the typed name is refused. `check` tells why a
/// name cannot be saved ([`rename_problem`]).
pub(super) fn rename_field(
    ui: &mut Ui,
    cx: &Cx<'_>,
    edit: &mut Rename,
    check: impl Fn(&str) -> Option<String>,
) -> Option<RenameOutcome> {
    let id = Id::unique(("providers-rename", &edit.provider));
    let fresh = std::mem::take(&mut edit.fresh);
    if fresh {
        // Focus the field with its whole text selected, ready to be typed over.
        let mut st = TextEditState::load(ui.ctx(), id).unwrap_or_default();
        st.cursor
            .set_char_range(Some(CCursorRange::select_all_str(&edit.text)));
        st.store(ui.ctx(), id);
        ui.memory_mut(|m| m.request_focus(id));
    }
    let mut outcome = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        // Keep the field's border off the tile's top edge.
        ui.add_space(3.0);
        let r = ui
            .add(
                TextEdit::singleline(&mut edit.text)
                    .id(id)
                    .font(font(text::BODY, Weight::Medium))
                    .desired_width(ui.available_width())
                    .margin(vec2(6.0, 1.0)),
            )
            .accessible_name(cx.tr.t("sites.form.name"));
        let problem = check(&edit.text);
        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        let had_focus = r.has_focus() || r.lost_focus();
        if had_focus && escape {
            outcome = Some(RenameOutcome::Cancel);
        } else if had_focus && enter {
            match &problem {
                None => outcome = Some(RenameOutcome::Save(edit.text.trim().to_owned())),
                // Refused: the field stays, with the reason below it.
                Some(_) => r.request_focus(),
            }
        } else if !fresh && !r.has_focus() {
            // A click elsewhere (or the page was left): nothing is saved.
            outcome = Some(RenameOutcome::Cancel);
        }
        let line = match problem {
            Some(why) => Text::caption(why, cx.pal).color(cx.pal.red),
            None => Text::caption(cx.tr.t("providers.rename.keys"), cx.pal),
        };
        ui.add(egui::Label::new(line).truncate());
    });
    outcome
}
