//! Claude Code's settings.json through the common harness transaction.
use super::jsonc::JsonFlavor;
use super::{DesiredFile, Format, HarnessFile, HarnessSpec, ReadFile, SlotPath};
use superba_core::harness::{HarnessId, HarnessOptions, HarnessPlanKind, HarnessTarget};
use superba_core::{Error, LocalContext, LocalizedText, Notice, Result};
/// Claude's single configuration file.
pub const SETTINGS: &str = "settings.json";
/// Claude Code's specialized fields.
pub struct ClaudeCode;
impl HarnessSpec for ClaudeCode {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn files(&self, ctx: &LocalContext, _: &HarnessOptions) -> Result<Vec<HarnessFile>> {
        Ok(vec![HarnessFile::new(
            SETTINGS,
            superba_core::ClaudeHome::resolve(ctx.env.as_ref(), &ctx.home_dir).settings_json(),
            Format::Json(JsonFlavor::Strict),
            true,
        )])
    }
    fn desired(&self, t: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>> {
        let f = files
            .first()
            .ok_or_else(|| Error::invalid("Claude settings were not read"))?;
        let value = f.value();
        if !value.is_null() && !value.is_object() {
            return Err(Error::invalid("settings.json is not a JSON object"));
        }
        if value.get("env").is_some_and(|v| !v.is_object()) {
            return Err(Error::invalid("settings.json: env is not an object"));
        }
        let d = t
            .claude
            .as_ref()
            .ok_or_else(|| Error::invalid("Claude settings were not rendered"))?;
        Ok(vec![DesiredFile {
            file: f.file.clone(),
            slots: d
                .iter()
                .map(|(p, v)| {
                    (
                        SlotPath::keys(&p.split('.').collect::<Vec<_>>()),
                        v.cloned(),
                    )
                })
                .collect(),
        }])
    }
    fn check(&self, after: &[(HarnessFile, serde_json::Value)]) -> Result<()> {
        for (_, value) in after {
            if !value.is_object() {
                return Err(Error::invalid("settings.json is not a JSON object"));
            }
            if value.get("env").is_some_and(|v| !v.is_object()) {
                return Err(Error::invalid("settings.json: env is not an object"));
            }
        }
        Ok(())
    }
    fn secret_slot(&self, path: &SlotPath) -> bool {
        superba_core::claude::is_secret_path(&path.display())
    }
    fn notes(&self, kind: HarnessPlanKind, _: Option<&HarnessTarget>) -> Vec<Notice> {
        vec![Notice::info(match kind {
            HarnessPlanKind::Apply => LocalizedText::new(
                "config.claude.new_sessions",
                "New Claude Code sessions use this; running sessions keep what they started with.",
            ),
            HarnessPlanKind::Restore => LocalizedText::new(
                "config.claude.restored",
                "Claude Code's settings are restored; entries changed by someone else were kept.",
            ),
        })]
    }
}
