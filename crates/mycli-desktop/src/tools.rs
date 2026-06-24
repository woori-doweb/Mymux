//! Lightweight PATH lookup so the UI can tell whether a CLI (e.g. `claude`,
//! `codex`) is already installed — used to suppress the startup install guide
//! once setup is done.

use std::path::PathBuf;

/// Returns true if `name` resolves to an executable on the process PATH.
#[tauri::command]
pub fn tool_installed(name: String) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for cand in candidates(&name) {
        for dir in std::env::split_paths(&path) {
            let full: PathBuf = dir.join(&cand);
            if full.is_file() {
                return true;
            }
        }
    }
    false
}

/// Filenames to probe for a given command. On Windows, npm shims and native
/// installers land as .cmd/.exe/.ps1/etc., so expand by PATHEXT.
fn candidates(name: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        let mut v = vec![name.to_string()];
        for ext in exts.split(';') {
            let ext = ext.trim();
            if !ext.is_empty() {
                v.push(format!("{name}{}", ext.to_uppercase()));
                v.push(format!("{name}{}", ext.to_lowercase()));
            }
        }
        v
    }
    #[cfg(not(windows))]
    {
        vec![name.to_string()]
    }
}
