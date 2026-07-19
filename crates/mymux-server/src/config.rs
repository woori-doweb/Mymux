//! Configuration loaded from a TOML file (see deploy/config.example.toml).
//!
//! Security-relevant defaults: `bind` has no default here on purpose — the
//! example config pins 127.0.0.1:7070 and the docs forbid 0.0.0.0. Public
//! exposure is Nginx's job (Tailscale/IP allowlist).

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    pub auth: AuthConfig,
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

/// Claude Code hook → agent-status bridge (POST /api/agent-status).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentConfig {
    /// Shared token the hook script presents. Empty (default) disables the
    /// endpoint entirely. Generate with e.g. `openssl rand -hex 32`.
    #[serde(default)]
    pub status_token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    #[serde(default)]
    pub public_url: String,
    pub static_dir: PathBuf,
    pub database_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NetworkConfig {
    #[serde(default)]
    pub behind_reverse_proxy: bool,
    #[serde(default)]
    pub trusted_proxy_ips: Vec<String>,
    #[serde(default)]
    pub allowed_client_cidrs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_cookie_name")]
    pub cookie_name: String,
    #[serde(default = "default_true")]
    pub cookie_secure: bool,
    #[serde(default = "d_session_timeout")]
    pub session_timeout_seconds: i64,
    #[serde(default = "d_idle_timeout")]
    pub idle_timeout_seconds: i64,
    #[serde(default = "d_fail_limit")]
    pub login_fail_limit: i64,
    #[serde(default = "d_lock_seconds")]
    pub login_lock_seconds: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalConfig {
    #[serde(default = "default_shell")]
    pub default_shell: String,
    #[serde(default = "default_allowed_shells")]
    pub allowed_shells: Vec<String>,
    pub workspace_root: PathBuf,
    #[serde(default = "d_max_sessions")]
    pub max_sessions_per_user: usize,
    #[serde(default = "d_idle_timeout")]
    pub idle_timeout_seconds: i64,
    #[serde(default = "d_max_lifetime")]
    pub max_lifetime_seconds: i64,
    #[serde(default = "d_replay")]
    pub replay_buffer_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuditConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    // Reserved for a follow-up PR: opt-in terminal I/O body capture. Off by
    // default; the fields exist now so config files are forward-compatible.
    #[serde(default)]
    #[allow(dead_code)]
    pub log_terminal_input: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub log_terminal_output: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_terminal_input: false,
            log_terminal_output: false,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read config {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("parse config {}: {e}", path.display()))
    }
}

fn default_cookie_name() -> String {
    "mymux_session".into()
}
fn default_true() -> bool {
    true
}
fn d_session_timeout() -> i64 {
    28800
}
fn d_idle_timeout() -> i64 {
    1800
}
fn d_fail_limit() -> i64 {
    5
}
fn d_lock_seconds() -> i64 {
    600
}
fn default_shell() -> String {
    "/bin/bash".into()
}
fn default_allowed_shells() -> Vec<String> {
    vec!["/bin/bash".into(), "/bin/sh".into()]
}
fn d_max_sessions() -> usize {
    5
}
fn d_max_lifetime() -> i64 {
    28800
}
fn d_replay() -> usize {
    262144
}
