//! Claude Code agent-status bridge (concept borrowed from nmux-linux).
//!
//! Claude Code hooks (see deploy/mymux-claude-hook.sh) POST lifecycle events
//! here; the registry keys them by the directory Claude runs in, and the
//! terminal list surfaces each session's status so the sidebar/tabs can show
//! "실행 중 / 입력 필요 / 완료" badges.
//!
//! Security model: the endpoint is not cookie-authenticated (hooks have no
//! browser session) — instead it requires ALL of: a shared token from the
//! server config, a loopback TCP peer, and the absence of any
//! proxy-forwarding header. The hook talks straight to the loopback bind, so
//! a legitimate request can never carry Forwarded/X-Forwarded-For/X-Real-IP;
//! nginx always appends them, which rejects every proxied path regardless of
//! how [network] is configured (a resolved-IP check alone would pass when
//! behind_reverse_proxy is left false — the proxied peer is 127.0.0.1 too).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::util;

/// Entries older than this are dropped (a dead Claude never sends Stop).
const EXPIRE_SECONDS: i64 = 12 * 3600;

/// Hard cap on registry size; oldest entry is evicted first. Bounds memory
/// and the O(n) lookup that runs per session on every terminal-list poll.
const MAX_ENTRIES: usize = 512;

/// One agent's latest known state, keyed in the registry by the working
/// directory Claude runs in (the hook payload's `cwd`).
#[derive(Clone)]
pub struct AgentStatus {
    pub state: &'static str,
    pub updated_at: String,
    updated_instant: OffsetDateTime,
}

impl AgentStatus {
    /// done / needs_input persist until the user visits the terminal (ack);
    /// running / tool states are transient and simply get overwritten.
    fn sticky(&self) -> bool {
        matches!(self.state, "done" | "needs_input")
    }
}

fn event_to_state(event: &str) -> Option<&'static str> {
    match event {
        "UserPromptSubmit" => Some("running"),
        "PreToolUse" => Some("tool"),
        "Notification" => Some("needs_input"),
        "Stop" => Some("done"),
        "SubagentStop" => Some("subagent_done"),
        _ => None,
    }
}

#[derive(Default)]
pub struct AgentStatusRegistry {
    by_cwd: Mutex<HashMap<String, AgentStatus>>,
}

impl AgentStatusRegistry {
    /// Record a hook event. Unknown events are ignored on purpose so newer
    /// Claude versions with extra hook types don't need a server update.
    pub fn record(&self, cwd: String, event: &str) {
        let Some(state) = event_to_state(event) else {
            return;
        };
        let now = OffsetDateTime::now_utc();
        let mut map = self.by_cwd.lock().unwrap();
        map.retain(|_, s| (now - s.updated_instant).whole_seconds() < EXPIRE_SECONDS);
        if map.len() >= MAX_ENTRIES
            && !map.contains_key(&cwd)
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, s)| s.updated_instant)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(
            cwd,
            AgentStatus {
                state,
                updated_at: util::fmt_offset(now),
                updated_instant: now,
            },
        );
    }

    /// Best status for a terminal spawned at `session_cwd`: the most recently
    /// updated entry at that directory — or, with `subtree` (admins only),
    /// anywhere below it. Subtree rollup is admin-only on purpose: agents in
    /// a sibling user's subdirectory must not surface on a non-admin's
    /// workspace-root terminal.
    pub fn lookup(&self, session_cwd: &str, subtree: bool) -> Option<AgentStatus> {
        let map = self.by_cwd.lock().unwrap();
        map.iter()
            .filter(|(cwd, _)| {
                if subtree {
                    under(session_cwd, cwd)
                } else {
                    same_dir(session_cwd, cwd)
                }
            })
            .max_by_key(|(_, s)| s.updated_instant)
            .map(|(_, s)| s.clone())
    }

    /// Clear sticky (done / needs_input) entries at `session_cwd` (subtree
    /// for admins, matching what lookup showed them) — called when the user
    /// visits the terminal, mirroring nmux-linux's "status persists until you
    /// visit the window" rule.
    pub fn ack(&self, session_cwd: &str, subtree: bool) {
        let mut map = self.by_cwd.lock().unwrap();
        map.retain(|cwd, s| {
            let hit = if subtree {
                under(session_cwd, cwd)
            } else {
                same_dir(session_cwd, cwd)
            };
            !(s.sticky() && hit)
        });
    }
}

/// Is `path` equal to or inside `base`? Component-aware, not a raw prefix
/// match ("/srv/app" must not claim "/srv/app2").
fn under(base: &str, path: &str) -> bool {
    let b = base.trim_end_matches('/');
    let p = path.trim_end_matches('/');
    p == b || p.starts_with(&format!("{b}/"))
}

/// Same directory, ignoring trailing slashes.
fn same_dir(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

// ── HTTP handlers ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StatusReq {
    pub event: String,
    pub cwd: String,
}

pub async fn post_status(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<StatusReq>,
) -> AppResult<Json<serde_json::Value>> {
    let expected = &state.config.agent.status_token;
    if expected.is_empty() {
        return Err(AppError::NotFound); // feature disabled — don't advertise it
    }
    // Direct loopback connections only — see the module doc. This gate is
    // deliberately independent of [network] proxy configuration.
    if !peer.ip().is_loopback() {
        return Err(AppError::Forbidden);
    }
    for h in ["forwarded", "x-forwarded-for", "x-real-ip"] {
        if headers.contains_key(h) {
            return Err(AppError::Forbidden);
        }
    }
    let presented = headers
        .get("x-mymux-agent-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !token_eq(presented, expected) {
        return Err(AppError::Forbidden);
    }
    if req.cwd.is_empty() {
        return Err(AppError::BadRequest("cwd required".into()));
    }
    // Canonicalize so hook cwds compare equal to spawn-time session cwds
    // (which are canonicalized). If the dir vanished, keep the raw path.
    let cwd = std::fs::canonicalize(&req.cwd)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or(req.cwd);
    state.agent_status.record(cwd, &req.event);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct AckReq {
    pub cwd: String,
}

/// Browser-side ack — cookie-authenticated like every other UI API. Scoped:
/// the cwd must belong to one of the caller's own live terminals (admins may
/// ack any session's cwd, with subtree semantics matching their lookup).
pub async fn ack_status(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<AckReq>,
) -> AppResult<Json<serde_json::Value>> {
    let is_admin = user.role.is_admin();
    let owns = state
        .terminals
        .list()
        .iter()
        .any(|s| (is_admin || s.owner_user_id == user.id) && same_dir(&s.cwd, &req.cwd));
    if !owns {
        return Err(AppError::Forbidden);
    }
    state.agent_status.ack(&req.cwd, is_admin);
    Ok(Json(json!({ "ok": true })))
}

/// Compare hashes, not strings — makes the comparison time independent of
/// where the first mismatching byte is.
fn token_eq(presented: &str, expected: &str) -> bool {
    Sha256::digest(presented.as_bytes()) == Sha256::digest(expected.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_mapping() {
        let r = AgentStatusRegistry::default();
        r.record("/w/a".into(), "UserPromptSubmit");
        assert_eq!(r.lookup("/w/a", false).unwrap().state, "running");
        r.record("/w/a".into(), "Stop");
        assert_eq!(r.lookup("/w/a", false).unwrap().state, "done");
        r.record("/w/a".into(), "SomeFutureEvent"); // ignored, keeps "done"
        assert_eq!(r.lookup("/w/a", false).unwrap().state, "done");
    }

    #[test]
    fn subtree_lookup_is_component_aware_and_admin_only() {
        let r = AgentStatusRegistry::default();
        r.record("/w/app2".into(), "UserPromptSubmit");
        assert!(r.lookup("/w/app", true).is_none()); // /w/app2 NOT under /w/app
        assert!(r.lookup("/w", true).is_some()); // subtree (admin) rolls up
        assert!(r.lookup("/w", false).is_none()); // exact (non-admin) does not
        assert!(r.lookup("/w/app2", false).is_some());
    }

    #[test]
    fn ack_clears_only_sticky_and_respects_scope() {
        let r = AgentStatusRegistry::default();
        r.record("/w/a".into(), "Stop"); // sticky
        r.record("/w/b".into(), "UserPromptSubmit"); // transient
        r.ack("/w", false); // exact scope — nothing at /w itself
        assert!(r.lookup("/w/a", false).is_some());
        r.ack("/w", true); // admin subtree scope
        assert!(r.lookup("/w/a", false).is_none());
        assert_eq!(r.lookup("/w/b", false).unwrap().state, "running");
    }
}
