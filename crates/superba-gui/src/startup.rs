//! Opening the window, and telling the user when it cannot open (user bug 43).
//!
//! **Renderer** (chosen per platform in this crate's `Cargo.toml`): wgpu with
//! Direct3D 12 on Windows, glow (OpenGL 2 or newer) on macOS and Linux. Windows
//! machines without a 3D driver (VMs, some remote desktops) only have Windows' own
//! OpenGL 1.1, which glow cannot use; Direct3D 12 runs there on WARP, the software
//! rasterizer of every Windows 10/11. A GPU that is listed but cannot open the
//! window gets one more try on WARP, and `SUPERBA_GPU=software` ([`ENV_SUPERBA_GPU`])
//! uses WARP from the start (a way around a GPU driver that hangs or crashes).
//!
//! **Reports** ([`Reporter`]): release builds on Windows have no console, so what
//! goes to stderr is lost there. Each start rewrites `superba-gui.log` in the logs
//! directory (version, renderer and adapter, why the window could not open);
//! panics are appended to `superba-crash.log`. A failed start and a panic of the
//! main thread (which ends the GUI) are shown in a native message box on Windows,
//! and printed to stderr everywhere.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use superba_core::paths::display_path;
use superba_core::{Locale, Timestamp};

use crate::i18n::Tr;

/// This run's GUI log, in the logs directory: rewritten at every start.
pub const GUI_LOG: &str = "superba-gui.log";
/// The crash log, in the logs directory: one entry per panic, appended.
pub const CRASH_LOG: &str = "superba-crash.log";
/// The crash log starts over once it is larger than this.
const CRASH_LOG_LIMIT: u64 = 1 << 20;
/// Environment variable: `software` makes the window draw with the software
/// rasterizer from the start (Windows: Direct3D 12 WARP).
pub const ENV_SUPERBA_GPU: &str = "SUPERBA_GPU";

/// Which adapters the window may draw with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gpu {
    /// A GPU first, the software rasterizer when no GPU can be used.
    Any,
    /// The software rasterizer only.
    Software,
}

impl Gpu {
    /// The choice of a [`ENV_SUPERBA_GPU`] value: `software` (any case), else [`Gpu::Any`].
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value {
            Some(v) if v.trim().eq_ignore_ascii_case("software") => Self::Software,
            _ => Self::Any,
        }
    }
}

/// eframe options for the renderer this platform uses (see the module docs).
pub fn native_options(gpu: Gpu) -> eframe::NativeOptions {
    #[cfg(windows)]
    {
        wgpu_options(gpu)
    }
    #[cfg(not(windows))]
    {
        // glow is the only renderer built in.
        let _ = gpu;
        eframe::NativeOptions::default()
    }
}

/// Whether a window that failed with `error` on `gpu` should be tried once more
/// on the software rasterizer: on Windows, a wgpu error (no adapter, a device or
/// surface the GPU driver refused) when a GPU was allowed.
pub fn retry_in_software(gpu: Gpu, error: &eframe::Error) -> bool {
    #[cfg(windows)]
    {
        gpu == Gpu::Any && matches!(error, eframe::Error::Wgpu(_))
    }
    #[cfg(not(windows))]
    {
        let _ = (gpu, error);
        false
    }
}

#[cfg(windows)]
fn wgpu_options(gpu: Gpu) -> eframe::NativeOptions {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetupCreateNew};

    let mut setup = WgpuSetupCreateNew::without_display_handle();
    // The only backend built in (wgpu has no Direct3D 11 backend): hardware
    // adapters first, then WARP ("Microsoft Basic Render Driver").
    setup.instance_descriptor.backends = wgpu::Backends::DX12;
    // FXC (d3dcompiler_47.dll, part of Windows): never a dxcompiler.dll found on PATH.
    setup
        .instance_descriptor
        .backend_options
        .dx12
        .shader_compiler = wgpu::Dx12Compiler::Fxc;
    // An integrated GPU before a discrete one: Superba draws little and should not
    // wake a laptop's discrete GPU.
    setup.power_preference = wgpu::PowerPreference::LowPower;
    if gpu == Gpu::Software {
        setup.native_adapter_selector = Some(Arc::new(software_adapter));
    }
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: WgpuConfiguration {
            wgpu_setup: setup.into(),
            ..WgpuConfiguration::default()
        },
        ..eframe::NativeOptions::default()
    }
}

/// The software (WARP) adapter that can draw into the window.
#[cfg(windows)]
fn software_adapter(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, String> {
    adapters
        .iter()
        .find(|a| {
            a.get_info().device_type == wgpu::DeviceType::Cpu
                && surface.is_none_or(|s| a.is_surface_supported(s))
        })
        .cloned()
        .ok_or_else(|| "no software (WARP) Direct3D 12 adapter".to_owned())
}

/// The renderer and adapter the window got, for the GUI log.
pub fn describe_renderer(cc: &eframe::CreationContext<'_>) -> String {
    #[cfg(windows)]
    if let Some(state) = &cc.wgpu_render_state {
        let i = state.adapter.get_info();
        return format!(
            "wgpu {:?}: {} ({:?}; driver {} {})",
            i.backend, i.name, i.device_type, i.driver, i.driver_info
        );
    }
    #[cfg(not(windows))]
    if let Some(gl) = &cc.gl {
        use eframe::glow::HasContext as _;
        let v = gl.version();
        let es = if v.is_embedded { " ES" } else { "" };
        return format!("glow: OpenGL{es} {}.{} {}", v.major, v.minor, v.vendor_info);
    }
    "no renderer state".to_owned()
}

/// Why the window could not open: what eframe reported, as text (eframe's error
/// is not `Send` with every renderer).
#[derive(Debug)]
pub struct WindowError {
    /// eframe's error message.
    pub reason: String,
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the window could not open: {}", self.reason)
    }
}

impl std::error::Error for WindowError {}

/// Where the GUI keeps its startup and crash logs, and how it tells the user
/// that it could not start or stopped (see the module docs).
#[derive(Clone, Debug)]
pub struct Reporter {
    logs: PathBuf,
    tr: Arc<Tr>,
}

impl Reporter {
    /// Logs in Superba's data directory (`<data>/logs`; the temp directory when
    /// the data directory is unknown), messages in `locale`.
    pub fn new(locale: Locale) -> Self {
        let logs = superba_switch::detect_context()
            .map(|ctx| ctx.data_dir.logs_dir())
            .unwrap_or_else(|_| std::env::temp_dir().join("Superba"));
        Self::with_logs_dir(logs, locale)
    }

    /// Logs in `logs`.
    pub fn with_logs_dir(logs: impl Into<PathBuf>, locale: Locale) -> Self {
        Self {
            logs: logs.into(),
            tr: Arc::new(Tr::new(locale)),
        }
    }

    /// This run's GUI log.
    pub fn gui_log(&self) -> PathBuf {
        self.logs.join(GUI_LOG)
    }

    /// The crash log.
    pub fn crash_log(&self) -> PathBuf {
        self.logs.join(CRASH_LOG)
    }

    /// Start this run's GUI log (the previous run's is replaced). Best effort.
    pub fn begin(&self) {
        let header = format!(
            "Superba {} ({} {}) started {}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            Timestamp::now().to_rfc3339()
        );
        let _ = fs::create_dir_all(&self.logs).and_then(|()| fs::write(self.gui_log(), header));
    }

    /// Add a line to this run's GUI log. Best effort.
    pub fn note(&self, line: &str) {
        let _ = append(&self.gui_log(), &format!("{line}\n"));
    }

    /// What the user is told when the GUI could not start with `error`.
    pub fn failure_message(&self, error: &anyhow::Error) -> String {
        let reason = match error.downcast_ref::<WindowError>() {
            Some(w) => self.tr.f("startup.window_failed", &[("reason", &w.reason)]),
            None => format!("{error:#}"),
        };
        self.tr.f(
            "startup.failed_body",
            &[
                ("reason", &reason),
                ("path", &display_path(&self.gui_log())),
            ],
        )
    }

    /// Report that the GUI could not start: the GUI log, stderr and, on Windows,
    /// a message box (never a silent exit).
    pub fn report_failure(&self, error: &anyhow::Error) {
        self.note(&format!("error: {error:#}"));
        let title = self.tr.t("startup.failed_title");
        let body = self.failure_message(error);
        eprintln!("{title}: {body}");
        show_message(&title, &body);
    }

    /// The text of a panic for the crash log.
    pub fn crash_entry(thread: &str, info: &std::panic::PanicHookInfo<'_>) -> String {
        format!(
            "--- {} Superba {} ({} {}): thread '{thread}' {info}\n{}\n",
            Timestamp::now().to_rfc3339(),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::backtrace::Backtrace::force_capture()
        )
    }

    /// Append `entry` to the crash log, starting it over when it is too large.
    pub fn write_crash(&self, entry: &str) {
        let path = self.crash_log();
        if fs::metadata(&path).is_ok_and(|m| m.len() > CRASH_LOG_LIMIT) {
            let _ = fs::remove_file(&path);
        }
        let _ = fs::create_dir_all(&self.logs).and_then(|()| append(&path, entry));
    }

    /// What the user is told when the GUI stopped on a panic with `reason`.
    pub fn crash_message(&self, reason: &str) -> String {
        self.tr.f(
            "startup.crash_body",
            &[
                ("reason", &reason),
                ("path", &display_path(&self.crash_log())),
            ],
        )
    }

    /// Log every panic to the crash log (then the previous hook prints it to
    /// stderr). A panic of the main thread ends the GUI: it is also shown in a
    /// message box on Windows. Panics of other threads (background tasks) only
    /// go to the log.
    pub fn install_panic_hook(&self) {
        let reporter = self.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("unnamed");
            reporter.write_crash(&Self::crash_entry(name, info));
            previous(info);
            if name == "main" {
                let reason = panic_reason(info);
                show_message(
                    &reporter.tr.t("startup.crash_title"),
                    &reporter.crash_message(&reason),
                );
            }
        }));
    }
}

/// The panic message and where it happened.
fn panic_reason(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_owned());
    match info.location() {
        Some(l) => format!("{message} ({}:{})", l.file(), l.line()),
        None => message,
    }
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// Show `body` in a native message box and wait until it is closed. Windows
/// only, and only in a session with a visible desktop: from an SSH or service
/// session nobody could close it.
fn show_message(title: &str, body: &str) {
    #[cfg(windows)]
    win::message_box(title, body);
    #[cfg(not(windows))]
    let _ = (title, body);
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::System::StationsAndDesktops::{
        GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW, WSF_VISIBLE,
    };

    /// Whether this process's window station is the visible, interactive one.
    fn interactive() -> bool {
        let mut flags = USEROBJECTFLAGS {
            fInherit: 0,
            fReserved: 0,
            dwFlags: 0,
        };
        let mut needed = 0u32;
        // SAFETY: the window station handle belongs to the process and is not
        // closed; `flags` is a USEROBJECTFLAGS of the size passed.
        let ok = unsafe {
            let station = GetProcessWindowStation();
            !station.is_null()
                && GetUserObjectInformationW(
                    station,
                    UOI_FLAGS,
                    (&raw mut flags).cast(),
                    size_of::<USEROBJECTFLAGS>() as u32,
                    &mut needed,
                ) != 0
        };
        ok && (flags.dwFlags & WSF_VISIBLE as u32) != 0
    }

    pub(super) fn message_box(title: &str, body: &str) {
        if !interactive() {
            return;
        }
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (title, body) = (wide(title), wide(body));
        // On a thread of its own: the box runs a message loop, which on the GUI
        // thread would dispatch messages to the window that just failed or panicked.
        let shown = std::thread::spawn(move || {
            // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive
            // the call; a null owner window is allowed.
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    body.as_ptr(),
                    title.as_ptr(),
                    MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
                );
            }
        });
        let _ = shown.join();
    }
}
