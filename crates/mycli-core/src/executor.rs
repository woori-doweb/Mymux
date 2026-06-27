use std::process::{Command, Output};

use crate::error::CoreError;

/// Execute a shell command and return its output.
pub fn run(command_text: &str) -> Result<Output, CoreError> {
    // On Windows, spawn the helper shell with CREATE_NO_WINDOW (0x0800_0000) so
    // no console ("ghost") window flashes while the command runs.
    #[cfg(windows)]
    let output = {
        use std::os::windows::process::CommandExt;
        Command::new("cmd")
            .args(["/C", command_text])
            .creation_flags(0x0800_0000)
            .output()
    };
    #[cfg(not(windows))]
    let output = Command::new("sh").args(["-c", command_text]).output();

    output.map_err(|e| CoreError::Io {
        path: "<command>".into(),
        source: e,
    })
}
