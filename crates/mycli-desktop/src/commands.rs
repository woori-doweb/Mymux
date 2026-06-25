use mycli_core::{CommandStore, SavedCommand};
use serde::Serialize;

#[derive(Serialize)]
pub struct CommandDto {
    pub id: String,
    pub name: String,
    pub command: String,
    pub description: String,
    pub favorite: bool,
}

impl From<SavedCommand> for CommandDto {
    fn from(c: SavedCommand) -> Self {
        Self {
            id: c.id,
            name: c.name,
            command: c.command,
            description: c.description,
            favorite: c.favorite,
        }
    }
}

fn store() -> Result<CommandStore, String> {
    CommandStore::new().map_err(|e| e.to_string())
}

/// Built-in shortcut commands, seeded once. The autocomplete matches these by
/// name in any shell (PowerShell / Git Bash / CMD), so typing e.g. `cl` offers
/// `claude --dangerously-skip-permissions`.
const DEFAULT_COMMANDS: &[(&str, &str, &str)] = &[
    ("cl", "claude --dangerously-skip-permissions", "Claude Code (권한 확인 스킵)"),
    ("cc", "claude --continue", "Claude Code 이어서 진행"),
    ("cr", "claude --resume", "Claude Code 세션 재개"),
    ("cp", "claude update", "Claude Code 업데이트"),
    ("cy", "codex --yolo", "Codex (yolo 모드)"),
    ("co", "codex", "Codex 실행"),
    ("cor", "codex resume", "Codex 세션 재개"),
    ("cle", "clear", "화면 지우기"),
    ("c&p", "git add -A && git commit -m \"update\" && git push", "커밋 후 푸시"),
];

/// Seed built-in shortcuts once (tracked by a marker file so deletions stick).
fn seed_default_commands(s: &CommandStore) {
    let marker = match dirs::home_dir() {
        Some(h) => h.join(".mycli").join(".defaults_seeded"),
        None => return,
    };
    if marker.exists() {
        return;
    }
    let existing = s.list().unwrap_or_default();
    for (name, command, desc) in DEFAULT_COMMANDS {
        if !existing.iter().any(|c| c.name == *name) {
            let _ = s.add(SavedCommand::new(
                name.to_string(),
                command.to_string(),
                desc.to_string(),
            ));
        }
    }
    let _ = std::fs::write(&marker, b"1");
}

#[tauri::command]
pub fn list_commands() -> Result<Vec<CommandDto>, String> {
    let s = store()?;
    seed_default_commands(&s);
    let cmds = s.list().map_err(|e| e.to_string())?;
    Ok(cmds.into_iter().map(CommandDto::from).collect())
}

#[tauri::command]
pub fn add_command(name: String, command: String, description: String) -> Result<CommandDto, String> {
    let s = store()?;
    let cmd = SavedCommand::new(name, command, description);
    let dto = CommandDto::from(cmd.clone());
    s.add(cmd).map_err(|e| e.to_string())?;
    Ok(dto)
}

#[tauri::command]
pub fn update_command(id: String, name: String, command: String, description: String) -> Result<(), String> {
    let s = store()?;
    // Preserve the existing favorite flag (the edit form doesn't carry it).
    let favorite = s
        .list()
        .map_err(|e| e.to_string())?
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.favorite)
        .unwrap_or(false);
    let cmd = SavedCommand { id, name, command, description, favorite };
    s.update(cmd).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_favorite(id: String, favorite: bool) -> Result<(), String> {
    let s = store()?;
    let mut cmds = s.list().map_err(|e| e.to_string())?;
    if let Some(c) = cmds.iter_mut().find(|c| c.id == id) {
        c.favorite = favorite;
        s.update(c.clone()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn delete_command(id: String) -> Result<(), String> {
    let s = store()?;
    s.remove(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn execute_command(command_text: String) -> Result<String, String> {
    let output = mycli_core::executor::run(&command_text).map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !stderr.is_empty() && !output.status.success() {
        return Err(stderr);
    }

    let mut result = stdout;
    if !stderr.is_empty() {
        result.push_str(&stderr);
    }
    Ok(result)
}
