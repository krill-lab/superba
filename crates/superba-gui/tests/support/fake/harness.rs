//! Deterministic tool fixtures and recorded writes; no programs or files are used.
use super::FakeService;
use std::collections::BTreeMap;
use superba_core::{harness::*, *};

#[derive(Debug, Default)]
pub(super) struct State {
    settings: BTreeMap<HarnessId, HarnessSettings>,
    applied: BTreeMap<HarnessId, HarnessApplied>,
    pub saves: usize,
    pub writes: usize,
    pub model_failure: bool,
}
impl FakeService {
    pub fn harness_counts(&self) -> (usize, usize) {
        let state = self.areas.harness.lock().unwrap();
        (state.saves, state.writes)
    }
    pub fn fail_harness_models(&self, fail: bool) {
        self.areas.harness.lock().unwrap().model_failure = fail;
    }
    pub fn tool_fixture(&self, h: HarnessId) -> HarnessView {
        let settings = self
            .areas
            .harness
            .lock()
            .unwrap()
            .settings
            .get(&h)
            .cloned()
            .unwrap_or_default();
        preview(self, h, settings)
    }
}
fn key(h: HarnessId) -> &'static str {
    match h {
        HarnessId::ClaudeCode => "settings.json",
        HarnessId::Dsh => "profiles/web/cordis.patch.yml",
        HarnessId::OpenCode => "opencode.jsonc",
        HarnessId::Pi => "models.json",
        HarnessId::ZCode => "provider_config.json",
        HarnessId::WorkBuddy => "cn/models.json",
    }
}
pub(super) fn list(fake: &FakeService) -> Vec<HarnessSummary> {
    HarnessId::ALL
        .into_iter()
        .map(|h| {
            let v = fake.tool_fixture(h);
            HarnessSummary {
                harness: h,
                settings: v.settings,
                installed: v.install.is_some(),
                install: v.install,
                applied: v.applied.is_some(),
                applied_provider: v.applied.and_then(|a| a.provider),
                drift: 0,
            }
        })
        .collect()
}
pub(super) fn preview(fake: &FakeService, h: HarnessId, settings: HarnessSettings) -> HarnessView {
    let protocol = settings.protocol.unwrap_or(if h == HarnessId::WorkBuddy {
        HarnessProtocol::Chat
    } else {
        HarnessProtocol::Responses
    });
    let endpoint = if settings.route == HarnessRoute::Routed {
        routed_endpoint(
            &url::Url::parse("http://127.0.0.1:18431").unwrap(),
            protocol,
        )
        .unwrap()
    } else {
        ProtocolEndpoint {
            protocol,
            base_url: url::Url::parse("https://api.krill-code.net/v1").unwrap(),
            source: EndpointSource::Krill,
        }
    };
    let applied = fake.areas.harness.lock().unwrap().applied.get(&h).cloned();
    HarnessView {
        harness: h,
        settings,
        install: (h != HarnessId::WorkBuddy).then(|| HarnessInstall {
            binary: Some(format!("/usr/local/bin/{h}").into()),
            version: Some(
                if h == HarnessId::OpenCode {
                    "2.0.32"
                } else {
                    "1.2.3"
                }
                .into(),
            ),
            opencode_major: (h == HarnessId::OpenCode).then_some(OpencodeMajor::V2),
        }),
        files: vec![HarnessFileView {
            key: key(h).into(),
            path: format!("/home/demo/.config/{h}/{}", key(h)).into(),
            status: HarnessFileStatus::Valid,
            holds_key: true,
            backups: 4,
        }],
        protocols: h.protocols().to_vec(),
        endpoint: Some(endpoint),
        applied,
        pending: vec![format!("{} provider", key(h))],
        notes: Vec::new(),
    }
}
pub(super) fn save(fake: &FakeService, h: HarnessId, settings: HarnessSettings) -> HarnessView {
    {
        let mut state = fake.areas.harness.lock().unwrap();
        state.saves += 1;
        state.settings.insert(h, settings.clone());
    }
    preview(fake, h, settings)
}
pub(super) fn models(fake: &FakeService) -> Result<ModelList> {
    if fake.areas.harness.lock().unwrap().model_failure {
        return Err(Error::other("Model server unavailable (fixture)"));
    }
    Ok(ModelList {
        source: ModelListSource::Union {
            members: vec!["Krill".into()],
        },
        models: fake.model_fixture(),
        fallback_reason: None,
        sources: vec![],
    })
}
pub(super) fn plan(h: HarnessId, kind: HarnessPlanKind) -> HarnessPlan {
    HarnessPlan {
        kind,
        harness: h,
        provider: Some("krill".into()),
        protocol: Some(HarnessProtocol::Responses),
        files: vec![HarnessPlanFile {
            key: key(h).into(),
            path: format!("/fake/{h}/{}", key(h)).into(),
            base_sha256: None,
        }],
        changes: vec![HarnessChange {
            file: key(h).into(),
            slot: "provider.superba".into(),
            before: None,
            after: Some("Krill · ••••".into()),
            drift: true,
            secret: true,
        }],
        notes: vec![],
        desired: None,
    }
}
pub(super) fn apply(fake: &FakeService, plan: HarnessPlan, take: bool) -> HarnessApplyReport {
    let mut state = fake.areas.harness.lock().unwrap();
    state.writes += 1;
    state.applied.insert(
        plan.harness,
        HarnessApplied {
            generation: 1,
            at: Timestamp::now(),
            provider: plan.provider.clone(),
            protocol: plan.protocol,
            owned: vec![format!("{} provider.superba", key(plan.harness))],
            drift: vec![],
        },
    );
    HarnessApplyReport {
        kind: plan.kind,
        harness: plan.harness,
        files: vec![],
        changed: if take {
            vec!["provider.superba".into()]
        } else {
            vec![]
        },
        kept: if take {
            vec![]
        } else {
            vec!["provider.superba".into()]
        },
        notes: vec![],
    }
}
pub(super) fn file(fake: &FakeService, h: HarnessId, file: String) -> Result<HarnessFileDoc> {
    let view = fake.tool_fixture(h);
    let file = view
        .files
        .into_iter()
        .find(|f| f.key == file)
        .ok_or_else(|| Error::invalid("unknown file"))?;
    Ok(HarnessFileDoc {
        harness: h,
        file,
        contents: Some(SecretBytes::new(b"{\n  \"theme\": \"dark\"\n}\n".to_vec())),
        sha256: Some("fake-hash".into()),
    })
}
