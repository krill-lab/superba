//! A router Models page fixture shared by the layout tests and the screenshot
//! example (`examples/models_frames.rs`): two providers, Krill (a Codex
//! catalog) and an intranet relay named 「内网」 (a standard `/v1/models` list
//! only), as in the alpha.7 bug report. Rows follow the router's rules: one
//! row per model id, one source per provider, the mode deciding which rows
//! exist and which aliases are published.

use superba_core::*;

/// One model of the fixture.
struct Model {
    id: &'static str,
    name: &'static str,
    window: u64,
    /// Fast tier offered.
    fast: bool,
    /// Installed Codex (L0) knows it.
    l0: bool,
    /// Providers: (member, layer, readiness).
    providers: &'static [(&'static str, CatalogLayer, CatalogReadiness)],
    /// Reached through a protocol conversion on the intranet relay.
    converted: bool,
}

use CatalogLayer::{Builtin, Codex, StandardKnown, Synthesized};
use CatalogReadiness::{Failed, NeedsTranslation, Pending, Ready};

const MODELS: &[Model] = &[
    Model {
        id: "gpt-5.5",
        name: "GPT-5.5",
        window: 272_000,
        fast: true,
        l0: true,
        providers: &[("krill", Codex, Ready), ("office", StandardKnown, Ready)],
        converted: false,
    },
    Model {
        id: "gpt-5.4",
        name: "GPT-5.4",
        window: 272_000,
        fast: true,
        l0: true,
        providers: &[("krill", Codex, Ready), ("office", Builtin, Ready)],
        converted: false,
    },
    Model {
        id: "gpt-5.4-mini",
        name: "GPT-5.4 Mini",
        window: 272_000,
        fast: true,
        l0: true,
        providers: &[("krill", Builtin, Ready), ("office", Builtin, Ready)],
        converted: false,
    },
    Model {
        id: "gpt-5.4-nano",
        name: "GPT-5.4 Nano",
        window: 128_000,
        fast: true,
        l0: true,
        providers: &[("krill", Builtin, Ready), ("office", StandardKnown, Ready)],
        converted: false,
    },
    Model {
        id: "gpt-5.5-pro",
        name: "GPT-5.5 Pro",
        window: 272_000,
        fast: false,
        l0: true,
        providers: &[("krill", Builtin, Ready), ("office", StandardKnown, Ready)],
        converted: false,
    },
    Model {
        id: "gpt-6-astra",
        name: "GPT-6 Astra",
        window: 400_000,
        fast: true,
        l0: false,
        providers: &[("krill", Codex, Ready), ("office", StandardKnown, Pending)],
        converted: false,
    },
    Model {
        id: "gpt-5.6",
        name: "gpt-5.6",
        window: 272_000,
        fast: false,
        l0: false,
        providers: &[("office", Synthesized, Pending)],
        converted: false,
    },
    Model {
        id: "deepseek-v4.1-flash",
        name: "deepseek-v4.1-flash",
        window: 64_000,
        fast: false,
        l0: false,
        providers: &[("office", Synthesized, Ready)],
        converted: false,
    },
    Model {
        id: "glm-5",
        name: "glm-5",
        window: 64_000,
        fast: false,
        l0: false,
        providers: &[("office", Synthesized, Pending)],
        converted: false,
    },
    Model {
        id: "kimi-k2",
        name: "kimi-k2",
        window: 64_000,
        fast: false,
        l0: false,
        providers: &[("office", Synthesized, Failed)],
        converted: false,
    },
    Model {
        id: "claude-sonnet-4-5",
        name: "claude-sonnet-4-5",
        window: 200_000,
        fast: false,
        l0: false,
        providers: &[("office", Synthesized, Pending)],
        converted: true,
    },
    Model {
        id: "gemini-2.5-pro",
        name: "gemini-2.5-pro",
        window: 128_000,
        fast: false,
        l0: false,
        providers: &[("office", Synthesized, NeedsTranslation)],
        converted: true,
    },
];

/// The members, their names and slugs (the relay has an IP address and a
/// name in Chinese, so its slug is a generated number).
pub const PROVIDERS: [(&str, &str, &str); 2] =
    [("krill", "Krill", "krill"), ("office", "内网", "p2")];

/// The router's catalog for `mode`.
pub fn catalog(mode: CatalogMode) -> RouterCatalogView {
    let mut view = RouterCatalogView::default();
    view.settings.mode = mode;
    view.settings.provider_slugs = PROVIDERS
        .iter()
        .map(|(id, _, slug)| ((*id).to_owned(), (*slug).to_owned()))
        .collect();
    for (priority, m) in MODELS.iter().enumerate() {
        if mode == CatalogMode::Codex && !m.l0 {
            // Withheld by the mode; the page says how to get them.
            view.withheld.push(m.id.to_owned());
            continue;
        }
        let tiers = if m.fast {
            serde_json::json!([{"id":"priority","name":"Fast","description":"2x speed, increased usage"}])
        } else {
            serde_json::json!([])
        };
        let mut entry = serde_json::json!({
            "slug": m.id, "display_name": m.name, "context_window": m.window,
            "visibility": "list", "supported_in_api": true, "priority": priority,
            "service_tiers": tiers,
            "supported_reasoning_levels": [{"effort":"low"},{"effort":"medium"},{"effort":"high"}],
        });
        if m.providers.iter().all(|(_, l, _)| *l != Synthesized) {
            entry["max_context_window"] = serde_json::json!(m.window);
        }
        let sources: Vec<CatalogProvenance> = m
            .providers
            .iter()
            .map(|(member, layer, readiness)| {
                let (_, name, slug) = PROVIDERS.iter().find(|p| p.0 == *member).unwrap();
                CatalogProvenance {
                    provider: (*member).into(),
                    provider_name: (*name).into(),
                    provider_slug: (*slug).into(),
                    key_scope: "fixture-scope".into(),
                    upstream_id: m.id.into(),
                    endpoint: if *member == "krill" {
                        "https://api.krill.example/backend-api/codex/models".into()
                    } else {
                        "http://10.20.0.8:8000/v1/models".into()
                    },
                    protocol: if m.converted {
                        "anthropic_messages"
                    } else {
                        "responses"
                    }
                    .into(),
                    layer: *layer,
                    evidence: "fixture".into(),
                    observed_at: 1,
                    readiness: Some(*readiness),
                    alias: Some(format!("{slug}/{}", m.id)),
                }
            })
            .collect();
        let readiness = [Ready, Pending, Failed, NeedsTranslation]
            .into_iter()
            .find(|r| m.providers.iter().any(|(_, _, x)| x == r))
            .unwrap();
        if mode == CatalogMode::Prefixed {
            for s in sources.iter().filter(|s| s.readiness == Some(Ready)) {
                view.aliases
                    .insert(s.alias.clone().unwrap(), m.id.to_owned());
            }
        }
        view.models.push(RouterCatalogModel {
            entry,
            sources,
            readiness,
            detail: (readiness == Failed)
                .then(|| "probe did not call the requested tool".to_owned()),
        });
    }
    view.sources = vec![
        CatalogSourceStatus {
            provider: "krill".into(),
            layer: Codex,
            endpoint: "https://api.krill.example/backend-api/codex/models".into(),
            observed_at: Some(1),
            stale: false,
            error: None,
        },
        CatalogSourceStatus {
            provider: "office".into(),
            layer: Codex,
            endpoint: "http://10.20.0.8:8000/v1/models?client_version=0.163.0".into(),
            observed_at: None,
            stale: false,
            // A relay without a Codex catalog: normal, not a warning.
            error: Some("expected a Codex models[] catalog".into()),
        },
        CatalogSourceStatus {
            provider: "office".into(),
            layer: CatalogLayer::Standard,
            endpoint: "http://10.20.0.8:8000/v1/models".into(),
            observed_at: Some(1),
            stale: false,
            error: None,
        },
    ];
    view
}

/// `base` (the fake Krill view) turned into the router's merged view for `mode`.
pub fn view(mut base: ModelsView, mode: CatalogMode) -> ModelsView {
    let catalog = catalog(mode);
    base.list.models = catalog
        .models
        .iter()
        .filter_map(|r| {
            let mut spec = superba_switch::models::spec_from_entry(&r.entry)?;
            spec.estimated_context_window = r.estimated();
            Some(spec)
        })
        .collect();
    base.list.source = ModelListSource::Union {
        members: PROVIDERS.iter().map(|p| p.1.to_owned()).collect(),
    };
    base.defaults = ModelDefaults::default();
    base.overrides.clear();
    base.applied = None;
    base.per_model = true;
    base.read_only = false;
    base.picker_available = false;
    base.notes.clear();
    base.router_catalog = Some(catalog);
    base
}
