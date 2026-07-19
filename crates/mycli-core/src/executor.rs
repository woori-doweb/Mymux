use std::process::{Command, Output};

use crate::error::CoreError;

/// Windows process-creation flag that stops a console window from briefly
/// flashing when a GUI (console-less) process spawns a console program like
/// `cmd`. Without it, every `cmd /C ...` pops a stray "ghost" window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Execute a shell command and return its output.
pub fn run(command_text: &str) -> Result<Output, CoreError> {
    // On Windows, spawn the helper shell with CREATE_NO_WINDOW (0x0800_0000) so
    // no console ("ghost") window flashes while the command runs.
    #[cfg(windows)]
    let output = {
        use std::os::windows::process::CommandExt;
        Command::new("cmd")
            .args(["/C", command_text])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    };
    #[cfg(not(windows))]
    let output = Command::new("sh").args(["-c", command_text]).output();

    output.map_err(|e| CoreError::Io {
        path: "<command>".into(),
        source: e,
    })
}
