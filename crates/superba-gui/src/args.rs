//! GUI-only command line flags (subcommands are routed to `superba_cli` before
//! these are parsed).
//!
//! ```text
//! superba [--theme dark|light] [--lang en|zh-CN] [--size WxH] [--page NAME]
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
}

/// Usage text for `--gui-help`.
pub const USAGE: &str = "\
superba [--theme dark|light] [--lang en|zh-CN] [--size WxH] [--page NAME]

  --theme T     dark or light (default: preferences / system)
  --lang L      en or zh-CN (default: preferences / system)
  --size WxH    initial window size in points, e.g. 1280x800
  --page NAME   open a page first: overview providers models router usage
                settings sessions hosts updates diagnostics preferences

Subcommands (status, switch, restore, ...) behave like `spb`; see `superba --help`.";

/// Flag names of the pages, in navigation order.
pub const PAGE_NAMES: [(Page, &str); 11] = [
    (Page::Overview, "overview"),
    (Page::Providers, "providers"),
    (Page::Models, "models"),
    (Page::Router, "router"),
    (Page::Usage, "usage"),
    (Page::Settings, "settings"),
    (Page::Sessions, "sessions"),
    (Page::Hosts, "hosts"),
    (Page::Updates, "updates"),
    (Page::Diagnostics, "diagnostics"),
    (Page::Preferences, "preferences"),
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
                other => return Err(format!("unknown argument {other:?} (see --gui-help)")),
            }
        }
        Ok(out)
    }
}
