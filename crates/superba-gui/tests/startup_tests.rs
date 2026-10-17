//! Opening the window and reporting a failed start (user bug 43): the renderer
//! each platform uses, the GUI and crash logs, and the messages the user gets
//! (shown in a message box on Windows; never a silent exit).

#![allow(missing_docs)]

use std::fs;

use superba_core::Locale;
use superba_gui::startup::{CRASH_LOG, GUI_LOG, Gpu, Reporter, WindowError};

#[test]
fn the_software_renderer_is_chosen_only_by_superba_gpu_software() {
    assert_eq!(Gpu::from_env_value(None), Gpu::Any);
    assert_eq!(Gpu::from_env_value(Some("")), Gpu::Any);
    assert_eq!(Gpu::from_env_value(Some("hardware")), Gpu::Any);
    assert_eq!(Gpu::from_env_value(Some("software")), Gpu::Software);
    assert_eq!(Gpu::from_env_value(Some(" Software ")), Gpu::Software);
}

#[test]
fn each_platform_uses_its_renderer() {
    let options = superba_gui::startup::native_options(Gpu::Any);
    // Windows: Direct3D 12 through wgpu (runs on WARP without a GPU driver);
    // elsewhere OpenGL through glow (Linux CI and screenshots: Xvfb + llvmpipe).
    let expected = if cfg!(windows) { "wgpu" } else { "glow" };
    assert_eq!(options.renderer.to_string(), expected);
    assert_eq!(
        superba_gui::startup::native_options(Gpu::Software)
            .renderer
            .to_string(),
        expected
    );
}

#[test]
fn the_gui_log_is_started_over_at_every_start() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let report = Reporter::with_logs_dir(&logs, Locale::En);
    assert_eq!(report.gui_log(), logs.join(GUI_LOG));
    report.begin();
    report.note("renderer: test");
    let first = fs::read_to_string(report.gui_log()).unwrap();
    assert!(first.starts_with("Superba "), "{first}");
    assert!(first.ends_with("renderer: test\n"), "{first}");
    report.begin();
    let second = fs::read_to_string(report.gui_log()).unwrap();
    assert!(!second.contains("renderer: test"), "{second}");
}

#[test]
fn a_window_that_cannot_open_is_explained_with_the_log_path() {
    let dir = tempfile::tempdir().unwrap();
    let error = anyhow::Error::new(WindowError {
        reason: "egui_glow requires opengl 2.0+".into(),
    });
    let en = Reporter::with_logs_dir(dir.path(), Locale::En);
    let message = en.failure_message(&error);
    assert!(
        message.starts_with(
            "Superba could not open its window (egui_glow requires opengl 2.0+). Updating the \
             graphics driver may help."
        ),
        "{message}"
    );
    assert!(
        message.ends_with(&format!("Details are in {}.", en.gui_log().display())),
        "{message}"
    );
    let zh = Reporter::with_logs_dir(dir.path(), Locale::ZhCn);
    let message = zh.failure_message(&error);
    assert!(
        message.starts_with("Superba 无法打开窗口（egui_glow requires opengl 2.0+）"),
        "{message}"
    );
    // Any other error is shown as it is.
    let other = en.failure_message(&anyhow::anyhow!("cannot determine the home directory"));
    assert!(
        other.starts_with("cannot determine the home directory\n\nDetails are in "),
        "{other}"
    );
}

#[test]
fn crashes_are_appended_and_the_log_starts_over_when_it_is_large() {
    let dir = tempfile::tempdir().unwrap();
    let report = Reporter::with_logs_dir(dir.path().join("logs"), Locale::En);
    assert_eq!(report.crash_log(), dir.path().join("logs").join(CRASH_LOG));
    report.write_crash("first\n");
    report.write_crash("second\n");
    assert_eq!(
        fs::read_to_string(report.crash_log()).unwrap(),
        "first\nsecond\n"
    );
    fs::write(report.crash_log(), vec![b'x'; (1 << 20) + 1]).unwrap();
    report.write_crash("third\n");
    assert_eq!(fs::read_to_string(report.crash_log()).unwrap(), "third\n");
    let message = report.crash_message("index out of bounds (src/app.rs:1)");
    assert!(
        message.starts_with(
            "Superba stopped because of an internal error: index out of bounds (src/app.rs:1)"
        ),
        "{message}"
    );
    assert!(
        message.ends_with(&format!(
            "A crash report was written to {}.",
            report.crash_log().display()
        )),
        "{message}"
    );
}
