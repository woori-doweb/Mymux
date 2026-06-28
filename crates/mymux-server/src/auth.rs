//! Authentication: roles, the `AuthUser` extractor, password hashing, and the
//! login / me / logout handlers. Sessions are cookie-based; only sha256(token)
//! is stored. All time comparisons use fixed-width RFC3339 strings (see util).

use std::str::FromStr;
use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::Json;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::HeaderMap;
use axum::http::header::{SET_COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use uuid::Uuid;

use crate::audit;
use crate::db::UserRow;
use crate::error::AppError;
use crate::state::AppState;
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Operator,
    Viewer,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Viewer => "viewer",
        }
    }
    pub fn can_create_terminal(&self) -> bool {
        matches!(self, Role::Admin | Role::Operator)
    }
    pub fn is_admin(&self) -> bool {
        matches!(self, Role::Admin)
    }
}

impl FromStr for Role {
    type Err = AppError;
    fn from_str(s: &str) -> Result<Self, AppError> {
        match s.to_lowercase().as_str() {
            "admin" => Ok(Role::Admin),
            "operator" => Ok(Role::Operator),
            "viewer" => Ok(Role::Viewer),
            other => Err(AppError::BadRequest(format!("unknown role: {other}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub role: Role,
}

#[derive(sqlx::FromRow)]
struct SessionUser {
    id: String,
    username: String,
    role: String,
    disabled: i64,
    session_id: String,
    expires_at: String,
    last_seen_at: String,
    revoked_at: Option<String>,
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let raw = util::cookie_value(&parts.headers, &state.config.auth.cookie_name)
            .ok_or(AppError::Unauthorized)?;
        let hash = util::hash_token(&raw);

        let row = sqlx::query_as::<_, SessionUser>(
            "SELECT u.id AS id, u.username AS username, u.role AS role, u.disabled AS disabled, \
                    s.id AS session_id, s.expires_at AS expires_at, s.last_seen_at AS last_seen_at, \
                    s.revoked_at AS revoked_at \
             FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.session_hash = ?",
        )
        .bind(&hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

        let now = util::now_rfc3339();
        let idle_floor = util::rfc3339_seconds_ago(state.config.auth.idle_timeout_seconds);
        if row.revoked_at.is_some()
            || row.disabled != 0
            || row.expires_at.as_str() <= now.as_str()
            || row.last_seen_at.as_str() < idle_floor.as_str()
        {
            return Err(AppError::Unauthorized);
        }

        // Slide the idle window forward.
        let _ = sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id = ?")
            .bind(&now)
            .bind(&row.session_id)
            .execute(&state.db)
            .await;

        let role = Role::from_str(&row.role).unwrap_or(Role::Viewer);
        Ok(AuthUser {
            id: row.id,
            username: row.username,
            role,
        })
    }
}

pub fn hash_password(pw: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("hash: {e}")))?
        .to_string())
}

pub fn verify_password(pw: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(pw.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A real-but-throwaway hash, verified when the username is unknown so login
/// timing doesn't leak account existence.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("mymux-nonexistent-account").unwrap_or_default())
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<Response, AppError> {
    let ip = util::resolve_client_ip(&headers, peer.ip(), &state.config.network);
    if !util::cidr_allowed(ip, &state.config.network.allowed_client_cidrs) {
        return Err(AppError::Forbidden);
    }
    let ip_s = ip.to_string();

    // Lockout: too many recent failures for this username.
    let window = util::rfc3339_seconds_ago(state.config.auth.login_lock_seconds);
    let fails: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM login_attempts WHERE username = ? AND success = 0 AND created_at >= ?",
    )
    .bind(&req.username)
    .bind(&window)
    .fetch_one(&state.db)
    .await?;
    if fails >= state.config.auth.login_fail_limit {
        record_attempt(&state, &req.username, &ip_s, false, "locked").await;
        audit::record(
            &state,
            None,
            Some(&ip_s),
            "auth.login.failure",
            Some(&req.username),
            json!({"reason":"locked"}),
            "denied",
        )
        .await;
        return Err(AppError::TooManyRequests);
    }

    let user = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE username = ?")
        .bind(&req.username)
        .fetch_optional(&state.db)
        .await?;

    let ok = match &user {
        Some(u) if u.disabled == 0 => verify_password(&req.password, &u.password_hash),
        _ => {
            // Spend the same work on a dummy hash to flatten timing.
            verify_password(&req.password, dummy_hash());
            false
        }
    };

    if !ok {
        record_attempt(&state, &req.username, &ip_s, false, "bad_credentials").await;
        audit::record(
            &state,
            None,
            Some(&ip_s),
            "auth.login.failure",
            Some(&req.username),
            json!({}),
            "denied",
        )
        .await;
        return Err(AppError::Unauthorized);
    }

    let user = user.expect("ok implies Some(user)");
    let (raw, hash) = util::generate_session_token();
    let sid = Uuid::new_v4().to_string();
    let now = util::now_rfc3339();
    let expires = util::rfc3339_seconds_from_now(state.config.auth.session_timeout_seconds);
    let ua = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    sqlx::query(
        "INSERT INTO sessions (id, user_id, session_hash, ip, user_agent, created_at, last_seen_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&sid)
    .bind(&user.id)
    .bind(&hash)
    .bind(&ip_s)
    .bind(&ua)
    .bind(&now)
    .bind(&now)
    .bind(&expires)
    .execute(&state.db)
    .await?;

    record_attempt(&state, &req.username, &ip_s, true, "ok").await;
    audit::record(
        &state,
        None,
        Some(&ip_s),
        "auth.login.success",
        Some(&user.username),
        json!({"user_id": user.id}),
        "ok",
    )
    .await;

    let cookie = build_cookie(
        &state.config.auth.cookie_name,
        &raw,
        state.config.auth.cookie_secure,
        state.config.auth.session_timeout_seconds,
    );
    let body = Json(json!({ "username": user.username, "role": user.role }));
    Ok(([(SET_COOKIE, cookie)], body).into_response())
}

pub async fn me(user: AuthUser) -> Json<serde_json::Value> {
    Json(json!({ "id": user.id, "username": user.username, "role": user.role.as_str() }))
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    user: AuthUser,
) -> Result<Response, AppError> {
    if let Some(raw) = util::cookie_value(&headers, &state.config.auth.cookie_name) {
        let hash = util::hash_token(&raw);
        let now = util::now_rfc3339();
        let _ = sqlx::query("UPDATE sessions SET revoked_at = ? WHERE session_hash = ?")
            .bind(&now)
            .bind(&hash)
            .execute(&state.db)
            .await;
    }
    audit::record(
        &state,
        Some(&user),
        None,
        "auth.logout",
        None,
        json!({}),
        "ok",
    )
    .await;
    let cookie = clear_cookie(
        &state.config.auth.cookie_name,
        state.config.auth.cookie_secure,
    );
    Ok(([(SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

async fn record_attempt(state: &AppState, username: &str, ip: &str, success: bool, reason: &str) {
    let now = util::now_rfc3339();
    let _ = sqlx::query(
        "INSERT INTO login_attempts (username, ip, success, reason, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(username)
    .bind(ip)
    .bind(success as i64)
    .bind(reason)
    .bind(&now)
    .execute(&state.db)
    .await;
}

fn build_cookie(name: &str, value: &str, secure: bool, max_age: i64) -> String {
    let mut c = format!("{name}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}");
    if secure {
        c.push_str("; Secure");
    }
    c
}

fn clear_cookie(name: &str, secure: bool) -> String {
    let mut c = format!("{name}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    if secure {
        c.push_str("; Secure");
    }
    c
}
