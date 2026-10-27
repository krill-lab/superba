//! GUI-only command line flags (subcommands are routed to `superba_cli` before
//! these are parsed).
//!
//! ```text
//! superba [--theme dark|light] [--lang en|zh-CN] [--size WxH] [--page NAME]
//!         [--launch-chatgpt] [--start-in-tray] [--autostart]
//! ```

use std::ffi::OsString;

use superba_core::Locale;

use crate::pages::Page;

/// Parsed GUI flags.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GuiArgs {
    /// Force a theme.
    pub theme: Option<egui::Theme>,
    /// Force a language.
    pub locale: Option<Locale>,
    /// Initial window size.
    pub size: Option<[f32; 2]>,
    /// Page to open first.
    pub page: Option<Page>,
    /// `--launch-chatgpt`: open ChatGPT through Superba (GH#7; the
    /// "ChatGPT (Superba)" shortcut). A running Superba window takes the
    /// request; otherwise no window opens unless it has to ask something.
    pub launch_chatgpt: bool,
    /// `--start-in-tray`: open with the window hidden in the tray (the menu
    /// bar on macOS) when there is one (item 70; Superba starts itself so
    /// when a Wayland window has to be reopened through XWayland to hide).
    pub start_in_tray: bool,
    /// Login startup: reuse an existing instance without raising its window.
    pub autostart: bool,
    /// `--command CMD`: run a command of the `--page` page once loaded.
    pub command: Option<String>,
}

/// Usage text for `--gui-help`.
pub const USAGE: &str = "\
superba [--theme dark|light] [--lang en|zh-CN] [--size WxH] [--page NAME]
        [--command CMD] [--launch-chatgpt] [--start-in-tray] [--autostart]

  --theme T     dark or light (default: preferences / system)
  --lang L      en or zh-CN (default: preferences / system)
  --size WxH    initial window size in points, e.g. 1280x800
  --page NAME   open a page first: overview tools providers models router usage
                settings sessions hosts claude-code claude-settings dsh opencode
                pi zcode workbuddy gemini troubleshooting superba
                updates diagnostics data general appearance network notify
  --command CMD run a command of that page once it has loaded (tooling):
                e.g. overview wizard, providers import, router doctor
  --launch-chatgpt
                open ChatGPT through Superba (so it can be hot-switched); a
                running Superba window does it, else no window opens unless
                a reopen has to be confirmed (the \"ChatGPT (Superba)\" shortcut)
  --autostart
                login startup; leave an already running window as it is
  --start-in-tray
                start with the window hidden in the tray (menu bar on macOS);
                without a tray the window opens as usual

Subcommands (status, switch, restore, ...) behave like `spb`; see `superba --help`.";

/// Flag names of the pages, in navigation order.
pub const PAGE_NAMES: [(Page, &str); 26] = [
    (Page::Overview, "overview"),
    (Page::Tools, "tools"),
    (Page::Providers, "providers"),
    (Page::Models, "models"),
    (Page::Router, "router"),
    (Page::Usage, "usage"),
    (Page::Settings, "settings"),
    (Page::Sessions, "sessions"),
    (Page::Hosts, "hosts"),
    (Page::ClaudeCode, "claude-code"),
    (Page::ClaudeSettings, "claude-settings"),
    (Page::Tool(superba_core::harness::HarnessId::Dsh), "dsh"),
    (
        Page::Tool(superba_core::harness::HarnessId::OpenCode),
        "opencode",
    ),
    (Page::Tool(superba_core::harness::HarnessId::Pi), "pi"),
    (Page::Tool(superba_core::harness::HarnessId::ZCode), "zcode"),
    (
        Page::Tool(superba_core::harness::HarnessId::WorkBuddy),
        "workbuddy",
    ),
    (
        Page::Tool(superba_core::harness::HarnessId::Gemini),
        "gemini",
    ),
    (Page::Troubleshooting, "troubleshooting"),
    (Page::Superba(crate::pages::SuperbaSection::Home), "superba"),
    (
        Page::Superba(crate::pages::SuperbaSection::Updates),
        "updates",
    ),
    (
        Page::Superba(crate::pages::SuperbaSection::Diagnostics),
        "diagnostics",
    ),
    (Page::Superba(crate::pages::SuperbaSection::Data), "data"),
    (
        Page::Superba(crate::pages::SuperbaSection::General),
        "general",
    ),
    (
        Page::Superba(crate::pages::SuperbaSection::Appearance),
        "appearance",
    ),
    (
        Page::Superba(crate::pages::SuperbaSection::Network),
        "network",
    ),
    (
        Page::Superba(crate::pages::SuperbaSection::Notify),
        "notify",
    ),
];

/// Parse a page name.
pub fn parse_page(name: &str) -> Option<Page> {
    PAGE_NAMES
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(name))
        .map(|(p, _)| *p)
}

impl GuiArgs {
    /// Parse `args` (including argv[0]).
    pub fn parse(args: &[OsString]) -> Result<GuiArgs, String> {
        let mut out = GuiArgs::default();
        let mut it = args
            .iter()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned());
        while let Some(arg) = it.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
                _ => (arg.clone(), None),
            };
            let mut value = |name: &str| -> Result<String, String> {
                inline
                    .clone()
                    .or_else(|| it.next())
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match flag.as_str() {
                "--theme" => {
                    out.theme = Some(match value("--theme")?.to_ascii_lowercase().as_str() {
                        "dark" => egui::Theme::Dark,
                        "light" => egui::Theme::Light,
                        other => return Err(format!("unknown theme {other:?}")),
                    });
                }
                "--lang" | "--locale" => {
                    let v = value("--lang")?;
                    out.locale =
                        Some(Locale::parse(&v).ok_or_else(|| format!("unknown language {v:?}"))?);
                }
                "--page" => {
                    let v = value("--page")?;
                    out.page = Some(parse_page(&v).ok_or_else(|| format!("unknown page {v:?}"))?);
                }
                "--command" => out.command = Some(value("--command")?),
                "--size" => {
                    let v = value("--size")?;
                    let (w, h) = v
                        .split_once(['x', 'X'])
                        .ok_or_else(|| format!("--size expects WxH, got {v:?}"))?;
                    let w: f32 = w
                        .trim()
                        .parse()
                        .map_err(|_| format!("bad width in {v:?}"))?;
                    let h: f32 = h
                        .trim()
                        .parse()
                        .map_err(|_| format!("bad height in {v:?}"))?;
                    out.size = Some([w.max(320.0), h.max(240.0)]);
                }
                "--launch-chatgpt" if inline.is_none() => out.launch_chatgpt = true,
                "--start-in-tray" if inline.is_none() => out.start_in_tray = true,
                "--autostart" if inline.is_none() => out.autostart = true,
                other => return Err(format!("unknown argument {other:?} (see --gui-help)")),
            }
        }
        Ok(out)
    }
}
