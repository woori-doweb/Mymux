//! Mymux Web Control Console — entrypoint.
//!
//! Subcommands:
//!   mymux-server serve       --config <path>
//!   mymux-server user create --username <u> --role <admin|operator|viewer> --config <path>

mod audit;
mod auth;
mod config;
mod db;
mod error;
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
        Commands::User {
            command:
                UserCommands::Create {
                    username,
                    role,
                    config,
                },
        } => create_user(&config, &username, &role).await,
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
