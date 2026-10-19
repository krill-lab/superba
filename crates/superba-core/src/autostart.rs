//! Current-user login startup for the Superba GUI, separate from the router.

use serde::{Deserialize, Serialize};

/// The operating system's login registration (not a cached preference).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartStatus {
    /// An enabled registration exists.
    pub enabled: bool,
    /// Registry value or file that owns the registration.
    pub entry: String,
    /// Registered command or entry contents; absent when disabled.
    pub command: Option<String>,
}

/// GUI Run value, separate from the router's registration.
pub const RUN_VALUE: &str = "Superba";
/// Per-user Windows login startup key.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Quoted Windows Run command, within the documented 260-character limit.
pub fn windows_command(executable: &std::path::Path) -> crate::Result<String> {
    let text = executable
        .to_str()
        .ok_or_else(|| crate::Error::invalid("login executable path is not UTF-8"))?;
    let path = crate::paths::display_path_text(text, true);
    let command = format!("\"{path}\" --autostart");
    if path.chars().any(char::is_control)
        || path.contains('"')
        || command.encode_utf16().count() > 260
    {
        return Err(crate::Error::invalid(
            "Windows Run command must be at most 260 characters and contain no quote or control character in its path",
        ));
    }
    Ok(command)
}
