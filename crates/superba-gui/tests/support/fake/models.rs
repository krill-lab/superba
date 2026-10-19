//! Fake service, area "models" (owner B6).
//! Models page: model lists, overrides, speed support.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here, so the area's
//! owner edits only this file.
//!
//! The fake list mirrors the shape of a Krill line's `/backend-api/codex/models`
//! (every listed model offers Fast, only `gpt-6-astra` Ultrafast); compatible
//! sites get an OpenAI-style list without tiers; the official login is read-only.

use std::collections::BTreeMap;

use superba_core::*;

use super::FakeService;

/// Fake state of the area (kept for the life of the fake world).
#[derive(Debug, Default)]
pub(super) struct State {
    /// A view a test set for a profile (returned instead of the generated one).
    views: BTreeMap<ProfileId, ModelsView>,
    /// Every edit `save_models` received.
    edits: Vec<ModelsEdit>,
    /// Fail the next `models_view` with this error.
    fail: Option<Error>,
    /// What Codex reads for a profile (set by [`FakeService::apply_models`]).
    applied: BTreeMap<ProfileId, AppliedModels>,
    /// `models_view` answers after this long (a provider list fetch and the
    /// local Codex's bundled list take seconds in the real service).
    delay: Option<std::time::Duration>,
}

impl FakeService {
    fn models_area(&self) -> std::sync::MutexGuard<'_, State> {
        self.areas.models.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Return `view` for its profile from now on.
    pub fn set_models_view(&self, view: ModelsView) {
        self.models_area()
            .views
            .insert(view.profile_id.clone(), view);
    }

    /// Fail the next `models_view` with `error`.
    pub fn fail_models_view(&self, error: Error) {
        self.models_area().fail = Some(error);
    }

    /// Answer `models_view` only after `delay` (`None` = at once).
    pub fn delay_models_view(&self, delay: Option<std::time::Duration>) {
        self.models_area().delay = delay;
    }

    /// The models of the fake Krill list.
    pub fn model_fixture(&self) -> Vec<ModelSpec> {
        krill_models()
    }

    /// Every `save_models` edit so far.
    pub fn models_edits(&self) -> Vec<ModelsEdit> {
        self.models_area().edits.clone()
    }

    /// Simulate a finished switch to `profile`: Codex now reads its saved
    /// window / compaction values (a catalog file with every listed model when it
    /// has per-model values, else the all-models keys), as the real switch writes
    /// them.
    pub fn apply_models(&self, profile: &ProfileId) {
        let p = self
            .state()
            .profiles
            .profile(profile)
            .cloned()
            .expect("profile");
        let per_model = p.auth_mode.uses_key();
        let catalog = per_model
            && p.model_overrides
                .values()
                .any(|o| o.context_window.is_some() || o.auto_compact.is_some());
        let applied = if catalog {
            AppliedModels {
                catalog: Some("/fake/.codex/superba/models-0a1b2c3d.json".into()),
                models: krill_models()
                    .iter()
                    .map(|m| {
                        let l = model_limits(m, p.model_overrides.get(&m.slug), &p.model);
                        (
                            m.slug.clone(),
                            AppliedModel {
                                context_window: l.window,
                                max_context_window: l.max.max(l.window),
                                auto_compact: l.compact_limit,
                            },
                        )
                    })
                    .collect(),
                ..AppliedModels::default()
            }
        } else {
            AppliedModels {
                context_window: p.model.context_window,
                auto_compact: p.model.auto_compact,
                ..AppliedModels::default()
            }
        };
        self.models_area().applied.insert(profile.clone(), applied);
    }
}

/// A Codex-native style entry.
fn spec(slug: &str, name: &str, window: u64, max: u64, tiers: &[&str]) -> ModelSpec {
    ModelSpec {
        display_name: Some(name.into()),
        description: Some(format!("{name} (fake list)")),
        context_window: Some(window),
        max_context_window: Some(max),
        effective_context_window_percent: Some(95),
        reasoning_levels: ["low", "medium", "high", "xhigh"]
            .into_iter()
            .map(|e| ReasoningLevel {
                effort: e.into(),
                description: None,
            })
            .collect(),
        default_reasoning_level: Some("medium".into()),
        service_tiers: tiers
            .iter()
            .map(|id| ServiceTierSpec {
                id: (*id).into(),
                name: if *id == "priority" {
                    "Fast".into()
                } else {
                    "Ultrafast".into()
                },
                description: if *id == "priority" {
                    "2x speed, increased usage".into()
                } else {
                    "Fastest, highest usage".into()
                },
            })
            .collect(),
        input_modalities: vec!["text".into(), "image".into()],
        ..ModelSpec::new(slug)
    }
}

/// The fake upstream list of a Krill-kind line (14 models, two hidden).
pub fn krill_models() -> Vec<ModelSpec> {
    let mut list = vec![
        spec(
            "gpt-6-astra",
            "GPT-6 Astra",
            272_000,
            872_000,
            &["priority", "ultrafast"],
        ),
        spec("gpt-6-sol", "GPT-6 Sol", 272_000, 872_000, &["priority"]),
        spec("gpt-6-luna", "GPT-6 Luna", 272_000, 872_000, &["priority"]),
        spec(
            "gpt-5.6-sol",
            "GPT-5.6 Sol",
            272_000,
            872_000,
            &["priority"],
        ),
        spec(
            "gpt-5.6-luna",
            "GPT-5.6 Luna",
            272_000,
            872_000,
            &["priority"],
        ),
        spec("gpt-5.5", "GPT-5.5", 272_000, 272_000, &["priority"]),
        spec(
            "gpt-5.5-mini",
            "GPT-5.5 mini",
            272_000,
            272_000,
            &["priority"],
        ),
        spec("gpt-5.4", "GPT-5.4", 272_000, 400_000, &["priority"]),
        spec(
            "gpt-5.4-mini",
            "GPT-5.4 mini",
            272_000,
            400_000,
            &["priority"],
        ),
        spec(
            "gpt-5.3-codex",
            "GPT-5.3 Codex",
            272_000,
            400_000,
            &["priority"],
        ),
        spec("gpt-5.2", "GPT-5.2", 272_000, 400_000, &["priority"]),
        spec(
            "gpt-5.1-codex-max",
            "GPT-5.1 Codex Max",
            272_000,
            400_000,
            &[],
        ),
        spec(
            "gpt-5-codex-mini",
            "GPT-5 Codex mini",
            200_000,
            272_000,
            &[],
        ),
        spec("codex-auto-review", "Auto review", 272_000, 272_000, &[]),
    ];
    for m in list.iter_mut().skip(12) {
        m.visibility = ModelVisibility::Hide;
    }
    list
}

/// The generated view of a profile.
pub fn view_for(profile: &Profile, site: &Site, now: Timestamp) -> ModelsView {
    let official = profile.builtin.is_some() || site.kind == SiteKind::Official;
    let (list, picker_available) = match site.kind {
        _ if official => (
            ModelList {
                source: ModelListSource::OfficialReadOnly,
                models: krill_models(),
                fallback_reason: None,
                sources: Vec::new(),
            },
            false,
        ),
        SiteKind::Compatible => (
            ModelList {
                source: ModelListSource::Upstream {
                    path: "/v1/models".into(),
                    fetched_at: now,
                    etag: None,
                },
                models: ["deepseek-chat", "deepseek-reasoner", "qwen-max", "gpt-5.5"]
                    .into_iter()
                    .map(ModelSpec::new)
                    .collect(),
                fallback_reason: None,
                sources: Vec::new(),
            },
            true,
        ),
        _ => (
            ModelList {
                source: ModelListSource::Upstream {
                    path: "/backend-api/codex/models?client_version=0.162.0".into(),
                    fetched_at: now,
                    etag: Some("\"fake\"".into()),
                },
                models: krill_models(),
                fallback_reason: None,
                sources: Vec::new(),
            },
            false,
        ),
    };
    let pat = profile.auth_mode == AuthMode::Pat;
    let offered = list.offered_tiers();
    let speed = SpeedSupport {
        offered,
        cli: SpeedEffect::Applies,
        app: if official || pat {
            SpeedEffect::AppMenu
        } else {
            SpeedEffect::AppForcesStandard
        },
        // Each tier's effect as the real service computes it.
        tiers: superba_switch::models::tier_effects(
            &list,
            |_| None,
            profile.model.model.as_deref(),
            official,
            pat,
            site.capabilities.tier_model_suffix,
        ),
    };
    let mut notes = Vec::new();
    if !profile.model_overrides.is_empty() && site.kind == SiteKind::Krill {
        notes.push(Notice::warning(LocalizedText::new(
            "switch.models.fingerprint",
            "With a model catalog file, requests differ from stock Codex.",
        )));
    }
    ModelsView {
        router_catalog: None,
        profile_id: profile.id.clone(),
        site_id: site.id.clone(),
        read_only: official,
        list,
        defaults: profile.model.clone(),
        overrides: if official {
            BTreeMap::new()
        } else {
            profile.model_overrides.clone()
        },
        picker: profile.model_picker,
        picker_available: picker_available && !official,
        speed,
        catalog_file: CatalogFileStatus::NotNeeded,
        notes,
        per_model: !official,
        applied: None,
    }
}

/// Body of `SwitchService::models_view`.
pub(super) async fn models_view(
    fake: &FakeService,
    request: ModelsViewRequest,
) -> Result<ModelsView> {
    if let Some(e) = fake.models_area().fail.take() {
        return Err(e);
    }
    let delay = fake.models_area().delay;
    if let Some(d) = delay {
        tokio::time::sleep(d).await;
    }
    let (profile, site, active) = {
        let s = fake.state();
        let id = request
            .profile_id
            .clone()
            .or_else(|| s.profiles.last_selected.clone())
            .or_else(|| s.profiles.profiles.first().map(|p| p.id.clone()))
            .ok_or_else(|| Error::NotFound {
                what: "a profile".into(),
            })?;
        let profile = s
            .profiles
            .profile(&id)
            .cloned()
            .or_else(|| catalog::builtin_profiles().into_iter().find(|p| p.id == id))
            .ok_or_else(|| Error::NotFound {
                what: format!("profile {id}"),
            })?;
        let site = catalog::builtin_sites()
            .into_iter()
            .chain(s.profiles.custom_sites.iter().cloned())
            .find(|x| x.id == profile.site_id)
            .ok_or_else(|| Error::NotFound {
                what: format!("site {}", profile.site_id),
            })?;
        let active = s
            .config
            .active
            .as_ref()
            .is_some_and(|a| a.profile_id.as_ref() == Some(&profile.id));
        (profile, site, active)
    };
    if let Some(v) = fake.models_area().views.get(&profile.id) {
        return Ok(v.clone());
    }
    let mut view = view_for(&profile, &site, Timestamp::now());
    // The applied profile: what the last (simulated) switch wrote, else the
    // listed values (no catalog file, no all-models keys).
    if active && !view.read_only {
        view.applied = Some(
            fake.models_area()
                .applied
                .get(&profile.id)
                .cloned()
                .unwrap_or_default(),
        );
    }
    Ok(view)
}

/// Body of `SwitchService::save_models`: stores defaults, overrides and picker
/// mode in the profile (upstream names in the custom site's model map).
pub(super) async fn save_models(fake: &FakeService, edit: ModelsEdit) -> Result<ModelsEditReport> {
    fake.models_area().edits.push(edit.clone());
    let mut s = fake.state();
    let id = edit
        .profile_id
        .clone()
        .or_else(|| s.profiles.last_selected.clone())
        .ok_or_else(|| Error::invalid("no profile"))?;
    let site_id = s
        .profiles
        .profile(&id)
        .map(|p| p.site_id.clone())
        .ok_or_else(|| Error::NotFound {
            what: format!("profile {id}"),
        })?;
    let mut map_edits = Vec::new();
    let profile = s
        .profiles
        .profiles
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| Error::invalid("built-in profiles cannot be changed"))?;
    if let Some(d) = &edit.defaults {
        profile.model = d.clone();
    }
    if edit.reset_all {
        profile.model_overrides.clear();
    }
    for slug in &edit.reset {
        profile.model_overrides.remove(slug);
    }
    for (slug, o) in &edit.set {
        if let Some(u) = &o.upstream_name {
            map_edits.push((slug.clone(), u.clone()));
        }
        let mut stored = o.clone();
        stored.upstream_name = None;
        if stored.is_empty() {
            profile.model_overrides.remove(slug);
        } else {
            profile.model_overrides.insert(slug.clone(), stored);
        }
    }
    if let Some(p) = edit.picker {
        profile.model_picker = p;
    }
    let saved = profile.clone();
    if let Some(site) = s.profiles.custom_sites.iter_mut().find(|x| x.id == site_id) {
        for (slug, upstream) in map_edits {
            if upstream.trim().is_empty() || upstream == slug {
                site.model_map.remove(&slug);
            } else {
                site.model_map.insert(slug, upstream);
            }
        }
    }
    Ok(ModelsEditReport {
        profile: saved,
        notes: Vec::new(),
    })
}
