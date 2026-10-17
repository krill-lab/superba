//! OS adapters and shared validation and atomic file operations.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use superba_core::{Error, Result};

use super::{CommandOutput, CommandRunner, ServiceHost, ServiceSpec};

/// Executes service commands and manages detached router processes on this OS.
#[derive(Debug)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[OsString]) -> io::Result<CommandOutput> {
        let mut command = superba_core::process::command(program);
        command.args(args).stdin(Stdio::null());
        let output = command.output()?;
        Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn spawn_detached(
        &self,
        exe: &Path,
        args: &[OsString],
        env: &[(String, String)],
        log_file: &Path,
    ) -> io::Result<u32> {
        fs::create_dir_all(parent_dir(log_file))?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let log = options.open(log_file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            log.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        let mut command = superba_core::process::command(exe);
        command.args(args).envs(env.iter().map(|(k, v)| (k, v)));
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        superba_core::process::no_window_new_group(&mut command);
        let mut child = command.spawn()?;
        let pid = child.id();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    }

    fn is_router_process(&self, pid: u32) -> bool {
        pid != 0 && router_process(pid)
    }

    fn terminate(&self, pid: u32) -> io::Result<()> {
        if pid == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "pid is zero"));
        }
        #[cfg(unix)]
        let (program, args) = ("kill", vec!["-TERM".into(), pid.to_string().into()]);
        #[cfg(windows)]
        let (program, args) = (
            "taskkill",
            vec![
                "/PID".into(),
                pid.to_string().into(),
                "/T".into(),
                "/F".into(),
            ],
        );
        let output = self.run(program, &args)?;
        if output.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                process_error(program, &output.stderr).to_string(),
            ))
        }
    }
}

fn router_process(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .is_some_and(|path| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|name| {
                        name.strip_suffix(" (deleted)").unwrap_or(name) == "superba-router"
                    })
            })
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        SystemRunner
            .run(
                "ps",
                &[
                    "-p".into(),
                    pid.to_string().into(),
                    "-o".into(),
                    "comm=".into(),
                ],
            )
            .ok()
            .is_some_and(|output| {
                output.success()
                    && Path::new(output.stdout.trim()).file_name()
                        == Some(std::ffi::OsStr::new("superba-router"))
            })
    }
    #[cfg(windows)]
    {
        SystemRunner
            .run(
                "tasklist",
                &[
                    "/FI".into(),
                    format!("PID eq {pid}").into(),
                    "/FO".into(),
                    "CSV".into(),
                    "/NH".into(),
                ],
            )
            .ok()
            .is_some_and(|output| {
                output.success()
                    && output.stdout.lines().any(|line| {
                        line.split(',').next().is_some_and(|name| {
                            name.trim()
                                .trim_matches('"')
                                .eq_ignore_ascii_case("superba-router.exe")
                        })
                    })
            })
    }
}

pub(super) fn system_host(home: PathBuf) -> ServiceHost {
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    #[cfg(target_os = "macos")]
    // SAFETY: getuid has no preconditions and returns the real uid of this process.
    let uid = Some(unsafe { libc::getuid() });
    #[cfg(not(target_os = "macos"))]
    let uid = None;
    ServiceHost {
        home,
        xdg_config_home,
        uid,
        runner: Arc::new(SystemRunner),
        #[cfg(windows)]
        run_key: Some(Arc::new(WindowsRunKey)),
        #[cfg(not(windows))]
        run_key: None,
    }
}

/// The current user's Windows login Run key; values are stored as REG_SZ.
#[cfg(windows)]
#[derive(Debug)]
pub struct WindowsRunKey;

#[cfg(windows)]
impl super::RunKeyStore for WindowsRunKey {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        let result = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_READ)
            .and_then(|key| key.get_value(name));
        match result {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        let (key, _) =
            winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey(RUN_KEY_PATH)?;
        key.set_value(name, &value)
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        let result = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY_PATH, winreg::enums::KEY_SET_VALUE)
            .and_then(|key| key.delete_value(name));
        match result {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(windows)]
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub(super) fn validate(host: &ServiceHost, spec: &ServiceSpec) -> Result<()> {
    path_text(&host.home)?;
    if let Some(path) = &host.xdg_config_home {
        path_text(path)?;
    }
    validate_spec(spec)
}

pub(super) fn validate_spec(spec: &ServiceSpec) -> Result<()> {
    for path in [&spec.exe, &spec.data_dir, &spec.log_file, &spec.pid_file] {
        path_text(path)?;
    }
    for (key, value) in &spec.env {
        valid_text(key)?;
        valid_text(value)?;
        if key.is_empty() || key.contains('=') {
            return Err(Error::invalid("environment name is empty or contains '='"));
        }
    }
    Ok(())
}

fn valid_text(value: &str) -> Result<()> {
    if value.contains(['\n', '\r', '\0']) {
        Err(Error::invalid(
            "service paths and values must not contain a newline or NUL",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn path_text(path: &Path) -> Result<&str> {
    let value = path
        .to_str()
        .ok_or_else(|| Error::invalid("service path is not Unicode"))?;
    valid_text(value)?;
    Ok(value)
}

pub(super) fn process_error(program: &str, message: &str) -> Error {
    Error::Process {
        message: format!(
            "{program}: {}",
            message.chars().take(300).collect::<String>()
        ),
    }
}

pub(super) fn checked_command(
    host: &ServiceHost,
    program: &str,
    args: &[OsString],
) -> Result<CommandOutput> {
    let output = host
        .runner
        .run(program, args)
        .map_err(|e| process_error(program, &e.to_string()))?;
    if output.success() {
        Ok(output)
    } else {
        Err(process_error(
            program,
            &format!("exit {:?}: {}", output.code, output.stderr.trim()),
        ))
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

pub(super) fn atomic_write(path: &Path, content: &str, mode: u32) -> Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = parent_dir(path);
    fs::create_dir_all(parent).map_err(|e| Error::io(parent, &e))?;
    let (temp, mut file) = loop {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".superba-router-{}-{id}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        match options.open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::io(&temp, &e)),
        }
    };
    let result = (|| -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| Error::io(path, &e))
}

pub(super) fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, &e)),
    }
}
