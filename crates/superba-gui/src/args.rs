//! GUI-only command line flags (subcommands are routed to `superba_cli` before
//! these are parsed).
//!
//! ```text
//! superba [--theme dark|light] [--lang en|zh-CN] [--size WxH] [--page NAME]
//!         [--launch-chatgpt] [--start-in-tray] [--autostart] [superba://LINK]
//! ```
//!
//! A `superba://import/v1` link (feature 140, the registered URL handler)
//! holds an API key. On Unix a start with a link on its command line runs
//! itself again at once with the link moved into [`ENV_PENDING_LINK`]
//! ([`reexec_without_link`]), which only this user can read
//! (`/proc/<pid>/environ` is private, `cmdline` is not), and the variable is
//! cleared before anything else runs ([`take_pending_link`]), so children
//! never inherit it.

use std::ffi::OsString;

use superba_core::deeplink;
use superba_core::{ImportLink, LinkError, Locale};

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
    /// A `superba://` link to import (feature 140), or why it was refused.
    /// `Debug` never shows the key.
    pub link: Option<Result<ImportLink, LinkError>>,
}

/// The environment variable a start with a link on its command line hands
/// the link over in when it runs itself again (Unix): the canonical link, or
/// `refused:<reason>`.
pub const ENV_PENDING_LINK: &str = "SUPERBA_PENDING_LINK";

/// The value of [`ENV_PENDING_LINK`] for `link` (Unix only hands links over
/// this way).
#[cfg_attr(not(unix), allow(dead_code))]
fn pending_value(link: &Result<ImportLink, LinkError>) -> String {
    match link {
        Ok(l) => l.to_url(),
        Err(e) => format!("refused:{}", e.to_wire()),
    }
}

/// Read [`ENV_PENDING_LINK`].
pub fn parse_pending(value: &str) -> Result<ImportLink, LinkError> {
    match value.strip_prefix("refused:") {
        Some(why) => Err(LinkError::from_wire(why)),
        None => ImportLink::parse(value),
    }
}

/// Take [`ENV_PENDING_LINK`] out of this process's environment. Call first
/// thing in `main`, before any thread starts.
pub fn take_pending_link() -> Option<Result<ImportLink, LinkError>> {
    let value = std::env::var_os(ENV_PENDING_LINK)?;
    // SAFETY: called at the start of `main`, before any other thread exists
    // (nothing else reads or writes the environment concurrently).
    unsafe { std::env::remove_var(ENV_PENDING_LINK) };
    Some(parse_pending(&value.to_string_lossy()))
}

/// The arguments without `superba:` links.
pub fn without_links(args: &[OsString]) -> Vec<OsString> {
    args.iter()
        .filter(|a| !deeplink::is_link(&a.to_string_lossy()))
        .cloned()
        .collect()
}

/// True when the command line holds a `superba:` link.
pub fn has_link_arg(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .any(|a| deeplink::is_link(&a.to_string_lossy()))
}

/// Unix: replace this process with a new start of the same executable, the
/// link moved from the command line (readable by every local user) into
/// [`ENV_PENDING_LINK`]. Returns only when that failed (the caller carries
/// on with the link in its command line). Elsewhere a no-op.
pub fn reexec_without_link(args: &[OsString], link: &Result<ImportLink, LinkError>) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => return e.to_string(),
        };
        let rest = without_links(args);
        let mut command = superba_core::process::command(exe);
        if let Some(argv0) = rest.first() {
            command.arg0(argv0);
        }
        let error = command
            .args(rest.iter().skip(1))
            .env(ENV_PENDING_LINK, pending_value(link))
            .exec();
        error.to_string()
    }
    #[cfg(not(unix))]
    {
        let _ = (args, link);
        String::new()
    }
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
                pi zcode workbuddy gemini superba
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
  superba://import/v1?key=KEY[&name=NAME]
                import a Krill API key from the Krill website (asks first);
                a running Superba window takes the link

Subcommands (status, switch, restore, ...) behave like `spb`; see `superba --help`.";

/// Flag names of the pages, in navigation order.
pub const PAGE_NAMES: [(Page, &str); 25] = [
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
            if deeplink::is_link(&arg) {
                if out.link.is_some() {
                    return Err("more than one superba: link".into());
                }
                out.link = Some(ImportLink::parse(&arg));
                continue;
            }
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
