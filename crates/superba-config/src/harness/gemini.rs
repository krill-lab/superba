//! Gemini CLI's user settings and environment, using the shared owned-slot transaction.
//! Configuration contract: https://geminicli.com/docs/reference/configuration/
use super::{DesiredFile, Format, HarnessFile, HarnessSpec, ReadFile, SlotPath, file_of, text};
use serde_json::json;
use superba_core::harness::{HarnessId, HarnessOptions, HarnessPlanKind, HarnessTarget};
use superba_core::{LocalContext, Notice, Result};

/// Gemini CLI.
pub struct Gemini;

impl HarnessSpec for Gemini {
    fn id(&self) -> HarnessId {
        HarnessId::Gemini
    }

    fn files(&self, ctx: &LocalContext, _: &HarnessOptions) -> Result<Vec<HarnessFile>> {
        let dir = ctx.home_dir.join(".gemini");
        Ok(vec![
            HarnessFile::new(
                "settings.json",
                dir.join("settings.json"),
                Format::Json(super::jsonc::JsonFlavor::Jsonc),
                false,
            ),
            HarnessFile::new(".env", dir.join(".env"), Format::Dotenv, true),
        ])
    }

    fn desired(&self, t: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>> {
        Ok(vec![
            DesiredFile {
                file: file_of(files, "settings.json")?,
                slots: vec![
                    (
                        SlotPath::keys(&["security", "auth", "selectedType"]),
                        Some(json!("gemini-api-key")),
                    ),
                    (
                        SlotPath::keys(&["model", "name"]),
                        t.default_model_id().map(|m| json!(m)),
                    ),
                ],
            },
            DesiredFile {
                file: file_of(files, ".env")?,
                slots: vec![
                    (SlotPath::key("GEMINI_API_KEY"), Some(json!(t.key.expose()))),
                    (
                        SlotPath::key("GOOGLE_GEMINI_BASE_URL"),
                        Some(json!(t.endpoint.base_text())),
                    ),
                    (
                        SlotPath::key("GOOGLE_GENAI_API_VERSION"),
                        Some(json!("v1beta")),
                    ),
                ],
            },
        ])
    }

    fn secret_slot(&self, path: &SlotPath) -> bool {
        *path == SlotPath::key("GEMINI_API_KEY")
    }

    fn notes(&self, _: HarnessPlanKind, _: Option<&HarnessTarget>) -> Vec<Notice> {
        vec![Notice::info(text(
            "config.harness.gemini.restart",
            "Restart Gemini CLI to load its user settings and .env. Project settings and shell environment variables can override them.",
        ))]
    }
}
