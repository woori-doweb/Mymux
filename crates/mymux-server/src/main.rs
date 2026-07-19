//! Mymux Web Control Console — entrypoint.
//!
//! Subcommands:
//!   mymux-server serve       --config <path>
//!   mymux-server user create --username <u> --role <admin|operator|viewer> --config <path>

mod agent_status;
mod audit;
mod auth;
mod commands;
mod config;
mod db;
mod error;
mod layout;
mod projects;
mod routes;
mod state;
mod terminal;
mod util;
mod ws_terminal;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::state::AppState;
use crate::terminal::TerminalManager;

#[derive(Parser)]
#[command(name = "mymux-server", version, about = "Mymux Web Control Console")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the HTTP/WebSocket server.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// User administration.
    User {
        #[command(subcommand)]
        command: UserCommands,
    },
    /// Claude Code integration helpers.
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Print the Claude Code settings.json hook snippet + setup steps.
    /// Nothing is modified — apply it yourself (deliberate: we never touch
    /// ~/.claude/settings.json automatically).
    Print {
        #[arg(long)]
        config: PathBuf,
        /// Installed path of the hook script clients will run.
        #[arg(long, default_value = "/opt/mymux-console/mymux-claude-hook.sh")]
        script: String,
    },
}

#[derive(Subcommand)]
enum UserCommands {
    /// Create a new user (prompts for a password).
    Create {
        #[arg(long)]
        username: String,
        #[arg(long, default_value = "operator")]
        role: String,
        #[arg(long)]
        config: PathBuf,
    },
    /// Change a user's password (prompts) and revoke their active sessions.
    SetPassword {
        #[arg(long)]
        username: String,
        #[arg(long)]
        config: PathBuf,
    },
    /// Disable a user (blocks login) and revoke their active sessions.
    Disable {
        #[arg(long)]
        username: String,
        #[arg(long)]
        config: PathBuf,
    },
    /// Re-enable a disabled user.
    Enable {
        #[arg(long)]
        username: String,
        #[arg(long)]
        config: PathBuf,
    },
    /// List users (username, role, status).
    List {
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    match Cli::parse().command {
        Commands::Serve { config } => serve(&config).await,
        Commands::User { command } => match command {
            UserCommands::Create {
                username,
                role,
                config,
            } => create_user(&config, &username, &role).await,
            UserCommands::SetPassword { username, config } => {
                set_password(&config, &username).await
            }
            UserCommands::Disable { username, config } => {
                set_disabled(&config, &username, true).await
            }
            UserCommands::Enable { username, config } => {
                set_disabled(&config, &username, false).await
            }
            UserCommands::List { config } => list_users(&config).await,
        },
        Commands::Hooks { command } => match command {
            HooksCommands::Print { config, script } => hooks_print(&config, &script),
        },
    }
}

async fn serve(config_path: &Path) -> Result<(), String> {
    let config = Config::load(config_path)?;
    let addr: SocketAddr = config
        .server
        .bind
        .parse()
        .map_err(|e| format!("bad bind {}: {e}", config.server.bind))?;

    let db = db::connect(&config.server.database_url)
        .await
        .map_err(|e| e.to_string())?;
    db::migrate(&db).await.map_err(|e| e.to_string())?;

    let terminals = Arc::new(TerminalManager::new(config.terminal.clone()));
    let state = AppState {
        config: Arc::new(config),
        db,
        terminals: Arc::clone(&terminals),
        agent_status: Arc::new(agent_status::AgentStatusRegistry::default()),
    };

    // Periodic reaper for exited / idle / over-lifetime terminals.
    let reaper = Arc::clone(&terminals);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            reaper.reap();
        }
    });

    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    tracing::info!("mymux-server listening on http://{addr}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|e| format!("server error: {e}"))?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

/// Print (never apply) the Claude Code hook registration for the agent-status
/// bridge. Keeping this print-only is a design decision carried over from the
/// nmux-linux review: installers that silently edit ~/.claude/settings.json
/// are exactly what we chose not to absorb.
fn hooks_print(config_path: &Path, script: &str) -> Result<(), String> {
    let config = Config::load(config_path)?;
    let token = &config.agent.status_token;
    if token.is_empty() {
        return Err(format!(
            "[agent] status_token is empty in {} — generate one first:\n\
             \n    openssl rand -hex 32\n\n\
             then set it in the config and restart the service.",
            config_path.display()
        ));
    }

    let hook = serde_json::json!([{ "hooks": [{ "type": "command", "command": script }] }]);
    let snippet = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook,
            "PreToolUse": hook,
            "Notification": hook,
            "Stop": hook,
            "SubagentStop": hook,
        }
    });

    println!("# Mymux agent-status — Claude Code hook setup (manual, 3 steps)");
    println!("#");
    println!("# 1) Store the token for the hook script (as the user running Claude):");
    println!("#");
    println!("#      mkdir -p ~/.config/mymux");
    println!("#      (umask 077; printf '%s' '{token}' > ~/.config/mymux/agent-token)");
    println!("#");
    println!("# 2) Make sure the hook script is installed and executable:");
    println!("#      {script}");
    println!("#");
    println!("# 3) Merge this into ~/.claude/settings.json (top-level \"hooks\" key):");
    println!(
        "{}",
        serde_json::to_string_pretty(&snippet).map_err(|e| e.to_string())?
    );
    Ok(())
}

async fn create_user(config_path: &Path, username: &str, role: &str) -> Result<(), String> {
    let config = Config::load(config_path)?;
    let role = role.parse::<auth::Role>().map_err(|e| format!("{e}"))?;

    let db = db::connect(&config.server.database_url)
        .await
        .map_err(|e| e.to_string())?;
    db::migrate(&db).await.map_err(|e| e.to_string())?;

    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&db)
        .await
        .map_err(|e| e.to_string())?;
    if exists.is_some() {
        return Err(format!("user '{username}' already exists"));
    }

    let pw = rpassword::prompt_password(format!("Password for {username}: "))
        .map_err(|e| e.to_string())?;
    let pw2 = rpassword::prompt_password("Confirm password: ").map_err(|e| e.to_string())?;
    if pw != pw2 {
        return Err("passwords do not match".into());
    }
    if pw.len() < 8 {
        return Err("password must be at least 8 characters".into());
    }

    let hash = auth::hash_password(&pw).map_err(|e| e.to_string())?;
    let now = util::now_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(&hash)
    .bind(role.as_str())
    .bind(&now)
    .bind(&now)
    .execute(&db)
    .await
    .map_err(|e| e.to_string())?;

    println!("created user '{username}' with role '{}'", role.as_str());
    Ok(())
}

/// Open the DB from a config path and run migrations (shared by the user admin
/// subcommands).
async fn open_db(config_path: &Path) -> Result<sqlx::SqlitePool, String> {
    let config = Config::load(config_path)?;
    let db = db::connect(&config.server.database_url)
        .await
        .map_err(|e| e.to_string())?;
    db::migrate(&db).await.map_err(|e| e.to_string())?;
    Ok(db)
}

/// Prompt for a new password twice and validate it (min 8 chars, must match).
fn prompt_new_password(username: &str) -> Result<String, String> {
    let pw = rpassword::prompt_password(format!("New password for {username}: "))
        .map_err(|e| e.to_string())?;
    let pw2 = rpassword::prompt_password("Confirm password: ").map_err(|e| e.to_string())?;
    if pw != pw2 {
        return Err("passwords do not match".into());
    }
    if pw.len() < 8 {
        return Err("password must be at least 8 characters".into());
    }
    Ok(pw)
}

async fn set_password(config_path: &Path, username: &str) -> Result<(), String> {
    let db = open_db(config_path).await?;
    let uid: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&db)
        .await
        .map_err(|e| e.to_string())?;
    let uid = uid.ok_or_else(|| format!("user '{username}' not found"))?;

    let pw = prompt_new_password(username)?;
    let hash = auth::hash_password(&pw).map_err(|e| e.to_string())?;
    let now = util::now_rfc3339();
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(&hash)
        .bind(&now)
        .bind(&uid)
        .execute(&db)
        .await
        .map_err(|e| e.to_string())?;
    // Revoke live sessions so the old password can't keep a session alive.
    let revoked =
        sqlx::query("UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
            .bind(&now)
            .bind(&uid)
            .execute(&db)
            .await
            .map_err(|e| e.to_string())?;
    println!(
        "password updated for '{username}'; revoked {} active session(s)",
        revoked.rows_affected()
    );
    Ok(())
}

async fn set_disabled(config_path: &Path, username: &str, disabled: bool) -> Result<(), String> {
    let db = open_db(config_path).await?;
    let now = util::now_rfc3339();
    let res = sqlx::query("UPDATE users SET disabled = ?, updated_at = ? WHERE username = ?")
        .bind(disabled as i64)
        .bind(&now)
        .bind(username)
        .execute(&db)
        .await
        .map_err(|e| e.to_string())?;
    if res.rows_affected() == 0 {
        return Err(format!("user '{username}' not found"));
    }
    if disabled {
        // Kick any live sessions of the now-disabled user.
        let revoked = sqlx::query(
            "UPDATE sessions SET revoked_at = ? \
             WHERE user_id = (SELECT id FROM users WHERE username = ?) AND revoked_at IS NULL",
        )
        .bind(&now)
        .bind(username)
        .execute(&db)
        .await
        .map_err(|e| e.to_string())?;
        println!(
            "disabled '{username}'; revoked {} active session(s)",
            revoked.rows_affected()
        );
    } else {
        println!("enabled '{username}'");
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct UserListRow {
    username: String,
    role: String,
    disabled: i64,
    created_at: String,
}

async fn list_users(config_path: &Path) -> Result<(), String> {
    let db = open_db(config_path).await?;
    let rows = sqlx::query_as::<_, UserListRow>(
        "SELECT username, role, disabled, created_at FROM users ORDER BY username COLLATE NOCASE",
    )
    .fetch_all(&db)
    .await
    .map_err(|e| e.to_string())?;
    println!("{:<20} {:<10} {:<9} CREATED", "USERNAME", "ROLE", "STATUS");
    for r in rows {
        let status = if r.disabled != 0 { "disabled" } else { "active" };
        println!(
            "{:<20} {:<10} {:<9} {}",
            r.username, r.role, status, r.created_at
        );
    }
    Ok(())
}
