//! Anthropic endpoints of known providers, each checked against the vendor's
//! own documentation (2026-10-18; the page is [`AnthropicPreset::doc_url`]).
//! A provider whose endpoint is not listed here is filled in by the user.
//!
//! Krill serves the Anthropic Messages API on every line: `<line>/v1/messages`
//! routes every model of the key (GPT included), `<line>/codex/v1/messages`
//! only the Codex models.

use url::Url;

use super::{AnthropicAuth, AnthropicEndpoint};
use crate::catalog::is_krill_host;
use crate::i18n::LocalizedText;
use crate::profile::Profile;
use crate::site::{Site, SiteKind};

/// Whether the endpoint lists its models at `<base>/v1/models`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelsListing {
    /// Documented.
    Yes,
    /// Documented as missing (the list is filled by hand).
    No,
    /// Not documented: tried, a failure is expected.
    Unknown,
}

/// A known Anthropic endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnthropicPreset {
    /// Stable id (`krill`, `deepseek`, `kimi-cn`, ...).
    pub id: &'static str,
    /// Vendor name (not translated).
    pub vendor: &'static str,
    /// Variant (region, plan), translated: "China", "International", ...
    pub variant: Option<LocalizedText>,
    /// Base URL; `None` for Krill (the line's origin, plus [`Self::path`]).
    pub base_url: Option<&'static str>,
    /// Path appended to a line origin (Krill only).
    pub path: &'static str,
    /// How the vendor documents the key (all use `ANTHROPIC_AUTH_TOKEN`).
    pub auth: AnthropicAuth,
    /// The vendor's documentation of its Claude Code setup.
    pub doc_url: &'static str,
    /// Hosts of the vendor's OpenAI-compatible API: a provider on one of them
    /// is offered this preset first.
    pub hosts: &'static [&'static str],
    /// `<base>/v1/models`.
    pub models: ModelsListing,
    /// The endpoint also serves every model of the provider's Codex list
    /// (Krill): those ids are usable even when its own list leaves them out.
    pub serves_codex_list: bool,
    /// Settings the vendor's documentation recommends (`env` name, value),
    /// offered as hints, never written by themselves.
    pub env_hints: &'static [(&'static str, &'static str)],
}

impl AnthropicPreset {
    /// The endpoint of this preset for `origin` (Krill's line; ignored by
    /// presets with a fixed base URL).
    pub fn endpoint(&self, origin: Option<&Url>) -> Option<AnthropicEndpoint> {
        let base = match self.base_url {
            Some(b) => Url::parse(b).ok()?,
            None => {
                let o = origin?;
                let text = format!(
                    "{}{}",
                    o.as_str().trim_end_matches('/'),
                    self.path.trim_end_matches('/')
                );
                Url::parse(&text).ok()?
            }
        };
        Some(AnthropicEndpoint {
            base_url: base,
            auth: self.auth,
            preset: Some(self.id.to_owned()),
        })
    }

    /// "DeepSeek", "Kimi · China".
    pub fn label(&self) -> LocalizedText {
        match &self.variant {
            Some(v) => LocalizedText::new("claude.preset.with_variant", "{vendor} · {variant}")
                .arg("vendor", self.vendor)
                .arg_text("variant", v.clone()),
            None => {
                LocalizedText::new("claude.preset.plain", "{vendor}").arg("vendor", self.vendor)
            }
        }
    }
}

fn variant(key: &str, fallback: &str) -> Option<LocalizedText> {
    Some(LocalizedText::new(
        format!("claude.preset.variant.{key}"),
        fallback,
    ))
}

/// Preset id of Krill's all-models endpoint (`<line>/v1/messages`).
pub const PRESET_KRILL: &str = "krill";
/// Preset id of Krill's Codex endpoint (`<line>/codex/v1/messages`).
pub const PRESET_KRILL_CODEX: &str = "krill-codex";

/// Every known endpoint, Krill first.
pub fn anthropic_presets() -> Vec<AnthropicPreset> {
    const BEARER: AnthropicAuth = AnthropicAuth::Bearer;
    vec![
        AnthropicPreset {
            id: PRESET_KRILL,
            vendor: "Krill",
            variant: variant("krill_all", "All models"),
            base_url: None,
            path: "",
            auth: BEARER,
            doc_url: "https://www.krill-code.com",
            hosts: &[],
            models: ModelsListing::Yes,
            serves_codex_list: true,
            env_hints: &[],
        },
        AnthropicPreset {
            id: PRESET_KRILL_CODEX,
            vendor: "Krill",
            variant: variant("krill_codex", "Codex models"),
            base_url: None,
            path: "/codex",
            auth: BEARER,
            doc_url: "https://www.krill-code.com",
            hosts: &[],
            models: ModelsListing::Yes,
            serves_codex_list: true,
            env_hints: &[],
        },
        AnthropicPreset {
            id: "deepseek",
            vendor: "DeepSeek",
            variant: None,
            base_url: Some("https://api.deepseek.com/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://api-docs.deepseek.com/guides/anthropic_api",
            hosts: &["api.deepseek.com"],
            models: ModelsListing::Unknown,
            serves_codex_list: false,
            env_hints: &[
                ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
                ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "786432"),
            ],
        },
        AnthropicPreset {
            id: "kimi",
            vendor: "Kimi",
            variant: variant("international", "International"),
            base_url: Some("https://api.moonshot.ai/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://platform.kimi.ai/docs/guide/claude-code-kimi",
            hosts: &["api.moonshot.ai"],
            models: ModelsListing::Unknown,
            serves_codex_list: false,
            env_hints: &[
                ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1000000"),
                ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
            ],
        },
        AnthropicPreset {
            id: "kimi-cn",
            vendor: "Kimi",
            variant: variant("china", "China"),
            base_url: Some("https://api.moonshot.cn/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://platform.kimi.com/docs/guide/claude-code-kimi",
            hosts: &["api.moonshot.cn"],
            models: ModelsListing::Unknown,
            serves_codex_list: false,
            env_hints: &[
                ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1000000"),
                ("CLAUDE_CODE_EFFORT_LEVEL", "max"),
            ],
        },
        AnthropicPreset {
            id: "glm",
            vendor: "GLM (Z.ai)",
            variant: variant("international", "International"),
            base_url: Some("https://api.z.ai/api/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://docs.z.ai/devpack/tool/claude",
            hosts: &["api.z.ai"],
            models: ModelsListing::Unknown,
            serves_codex_list: false,
            env_hints: &[
                ("API_TIMEOUT_MS", "3000000"),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
                ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1000000"),
            ],
        },
        AnthropicPreset {
            id: "glm-cn",
            vendor: "GLM (智谱)",
            variant: variant("china", "China"),
            base_url: Some("https://open.bigmodel.cn/api/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://docs.bigmodel.cn/cn/guide/develop/claude",
            hosts: &["open.bigmodel.cn"],
            models: ModelsListing::Unknown,
            serves_codex_list: false,
            env_hints: &[
                ("API_TIMEOUT_MS", "3000000"),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
                ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "1000000"),
            ],
        },
        AnthropicPreset {
            id: "qwen-coding",
            vendor: "Qwen",
            variant: variant("qwen_coding_cn", "Coding Plan, China"),
            base_url: Some("https://coding.dashscope.aliyuncs.com/apps/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://help.aliyun.com/zh/model-studio/claude-code",
            hosts: &["coding.dashscope.aliyuncs.com"],
            models: ModelsListing::No,
            serves_codex_list: false,
            env_hints: &[],
        },
        AnthropicPreset {
            id: "qwen-coding-intl",
            vendor: "Qwen",
            variant: variant("qwen_coding_intl", "Coding Plan, International"),
            base_url: Some("https://coding-intl.dashscope.aliyuncs.com/apps/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://www.alibabacloud.com/help/en/model-studio/claude-code",
            hosts: &["coding-intl.dashscope.aliyuncs.com"],
            models: ModelsListing::No,
            serves_codex_list: false,
            env_hints: &[],
        },
        AnthropicPreset {
            id: "qwen-token-plan",
            vendor: "Qwen",
            variant: variant("qwen_token_cn", "Token Plan, China"),
            base_url: Some("https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://help.aliyun.com/zh/model-studio/claude-code",
            hosts: &["token-plan.cn-beijing.maas.aliyuncs.com"],
            models: ModelsListing::No,
            serves_codex_list: false,
            env_hints: &[],
        },
        AnthropicPreset {
            id: "qwen-token-plan-intl",
            vendor: "Qwen",
            variant: variant("qwen_token_intl", "Token Plan, International"),
            base_url: Some("https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://www.alibabacloud.com/help/en/model-studio/claude-code",
            hosts: &["token-plan.ap-southeast-1.maas.aliyuncs.com"],
            models: ModelsListing::No,
            serves_codex_list: false,
            env_hints: &[],
        },
        AnthropicPreset {
            id: "minimax",
            vendor: "MiniMax",
            variant: variant("international", "International"),
            base_url: Some("https://api.minimax.io/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://platform.minimax.io/docs/m-plan/claude-code.md",
            hosts: &["api.minimax.io"],
            models: ModelsListing::Yes,
            serves_codex_list: false,
            env_hints: &[("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "524288")],
        },
        AnthropicPreset {
            id: "minimax-cn",
            vendor: "MiniMax",
            variant: variant("china", "China"),
            base_url: Some("https://api.minimax.cn/anthropic"),
            path: "",
            auth: BEARER,
            doc_url: "https://platform.minimax.cn/docs/m-plan/claude-code.md",
            hosts: &["api.minimax.cn", "api.minimaxi.com"],
            models: ModelsListing::Yes,
            serves_codex_list: false,
            env_hints: &[("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "524288")],
        },
    ]
}

/// The preset with `id`.
pub fn anthropic_preset(id: &str) -> Option<AnthropicPreset> {
    anthropic_presets().into_iter().find(|p| p.id == id)
}

/// The origin a profile's key is used on: its line (the primary, else the
/// site's default) or, for a site without lines, its base URL.
pub fn profile_origin(site: &Site, profile: &Profile) -> Option<Url> {
    let line = profile
        .line_id
        .as_ref()
        .and_then(|id| site.line(id))
        .or_else(|| site.default_line());
    match line {
        Some(l) => Some(l.origin.url().clone()),
        None => profile.base_url_override.as_ref().map(|u| {
            let mut o = u.clone();
            o.set_path("");
            o.set_query(None);
            o.set_fragment(None);
            o
        }),
    }
}

/// The preset Superba suggests for a provider: Krill (all models) for Krill
/// lines, else the preset whose OpenAI host is the provider's.
pub fn suggested_preset(site: &Site, profile: &Profile) -> Option<AnthropicPreset> {
    let origin = profile_origin(site, profile)?;
    let host = origin.host_str()?.to_ascii_lowercase();
    if site.kind == SiteKind::Krill || is_krill_host(&host) {
        return anthropic_preset(PRESET_KRILL);
    }
    let base_host = profile
        .base_url_override
        .as_ref()
        .and_then(|u| u.host_str())
        .map(str::to_ascii_lowercase);
    anthropic_presets().into_iter().find(|p| {
        p.hosts
            .iter()
            .any(|h| *h == host || base_host.as_deref() == Some(h))
    })
}

/// The endpoint Claude Code uses for a provider: the one stored with it,
/// else the suggested preset's.
pub fn effective_endpoint(site: &Site, profile: &Profile) -> Option<AnthropicEndpoint> {
    if let Some(e) = &profile.anthropic {
        return Some(e.clone());
    }
    let preset = suggested_preset(site, profile)?;
    preset.endpoint(profile_origin(site, profile).as_ref())
}
