//! Isolated maintenance data; never inspects installed tools or real files.
use super::FakeService;
use std::collections::BTreeMap;
use superba_core::{harness::*, *};

#[derive(Debug, Default)]
pub(super) struct State {
    pub harnesses: Vec<HarnessSummary>,
    pub views: BTreeMap<HarnessId, Result<HarnessView>>,
    pub backups: BTreeMap<HarnessId, Result<Vec<HarnessBackup>>>,
    pub restores: Vec<(HarnessId, String)>,
}
impl FakeService {
    pub fn maintenance_fixture(&self) {
        let mut state = self.areas.maintenance.lock().unwrap();
        for h in HarnessId::ALL {
            let install = Some(HarnessInstall {
                binary: Some(format!("/home/u/.local/bin/{h}").into()),
                version: Some("2.0.32".into()),
                opencode_major: None,
            });
            state.harnesses.push(HarnessSummary {
                harness: h,
                installed: h != HarnessId::Pi,
                install: install.clone(),
                applied_provider: None,
                applied: true,
                drift: if h == HarnessId::Dsh { 2 } else { 0 },
            });
            state.views.insert(
                h,
                Ok(HarnessView {
                    harness: h,
                    settings: HarnessSettings::default(),
                    install,
                    files: vec![HarnessFileView {
                        key: "models.json".into(),
                        path: format!("/home/u/.{h}/models.json").into(),
                        status: if h == HarnessId::ZCode {
                            HarnessFileStatus::Invalid {
                                line: 7,
                                column: 3,
                                message: "secret-must-not-appear".into(),
                            }
                        } else {
                            HarnessFileStatus::Valid
                        },
                        holds_key: true,
                    }],
                    protocols: vec![],
                    endpoint: None,
                    applied: None,
                    pending: vec![],
                    notes: vec![],
                }),
            );
            state.backups.insert(
                h,
                Ok(vec![HarnessBackup {
                    id: format!("backup-{h}"),
                    harness: h,
                    file: "models.json".into(),
                    path: format!("/home/u/.{h}/models.json").into(),
                    at: Timestamp::from_millis(1_800_000_000_000),
                    source: BackupSource::Original,
                    size: 2048,
                    sha256: None,
                }]),
            );
        }
        drop(state);
        for (target, kind, text) in [
            (
                TargetId::local(),
                CodexFileKind::ConfigToml,
                "model = 'old'",
            ),
            (TargetId::new("remote-demo"), CodexFileKind::AuthJson, "{}"),
            (TargetId::local(), CodexFileKind::ClaudeSettings, "{}"),
        ] {
            let backup = CodexFileBackup {
                id: format!("{target}-{}", kind.file_name()),
                target,
                target_label: "Home".into(),
                kind,
                at: Timestamp::from_millis(1_800_000_000_000),
                source: BackupSource::Original,
                size: text.len() as u64,
                sha256: None,
            };
            self.areas
                .files
                .lock()
                .unwrap()
                .backups
                .push((backup, text.into()));
        }
    }
    pub fn fail_harness_backups(&self, h: HarnessId) {
        self.areas
            .maintenance
            .lock()
            .unwrap()
            .backups
            .insert(h, Err(Error::other("backup index unreadable")));
    }
    pub fn harness_restores(&self) -> Vec<(HarnessId, String)> {
        self.areas.maintenance.lock().unwrap().restores.clone()
    }
}
