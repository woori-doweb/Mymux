//! Small cross-cutting helpers: time formatting, session tokens, cookies, and
//! client-IP / CIDR checks. Kept dependency-light and platform-neutral.

use std::net::IpAddr;

use axum::http::HeaderMap;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::config::NetworkConfig;

const B64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Format a UTC instant as fixed-width second-precision RFC3339 ("…Z").
/// Fixed width is deliberate: timestamps are compared lexically in SQL
/// (lockout windows, session expiry, idle) and only fixed width makes
/// lexical order == chronological order.
pub fn fmt_offset(t: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Current UTC time, fixed-width RFC3339.
pub fn now_rfc3339() -> String {
    fmt_offset(OffsetDateTime::now_utc())
}

/// `secs` seconds ago (used for "recent failures" / idle windows).
pub fn rfc3339_seconds_ago(secs: i64) -> String {
    fmt_offset(OffsetDateTime::now_utc() - time::Duration::seconds(secs))
}

/// `secs` seconds from now (session expiry).
pub fn rfc3339_seconds_from_now(secs: i64) -> String {
    fmt_offset(OffsetDateTime::now_utc() + time::Duration::seconds(secs))
}

/// Generate a session token: returns (raw token for the cookie, sha256 hash for DB).
/// Only the hash is ever persisted.
pub fn generate_session_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    let mut rng = rand::rngs::OsRng;
    rng.fill_bytes(&mut bytes);
    let raw = B64URL.encode(bytes);
    let hash = hash_token(&raw);
    (raw, hash)
}

/// sha256(token), base64url-encoded.
pub fn hash_token(raw: &str) -> String {
    B64URL.encode(Sha256::digest(raw.as_bytes()))
}

/// base64(standard) encode raw PTY bytes for the WebSocket `output` frame.
pub fn b64_std(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Parse the Cookie header and return the value for `name`.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        if let Some((k, v)) = part.trim().split_once('=')
            && k == name
        {
            return Some(v.to_string());
        }
    }
    None
}

/// Resolve the real client IP, honoring X-Forwarded-For ONLY when the immediate
/// peer is a configured trusted proxy.
pub fn resolve_client_ip(headers: &HeaderMap, peer: IpAddr, net: &NetworkConfig) -> IpAddr {
    if net.behind_reverse_proxy
        && is_trusted_proxy(peer, &net.trusted_proxy_ips)
        && let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = xff.split(',').next()
        && let Ok(ip) = first.trim().parse::<IpAddr>()
    {
        return ip;
    }
    peer
}

fn is_trusted_proxy(ip: IpAddr, trusted: &[String]) -> bool {
    trusted
        .iter()
        .filter_map(|s| s.parse::<IpAddr>().ok())
        .any(|t| t == ip)
}

/// Is `ip` inside any allowed CIDR? An empty list means "allow all" (the app
/// trusts that Nginx/Tailscale already restrict reachability).
pub fn cidr_allowed(ip: IpAddr, cidrs: &[String]) -> bool {
    if cidrs.is_empty() {
        return true;
    }
    cidrs.iter().any(|c| {
        if let Ok(net) = c.parse::<ipnet::IpNet>() {
            net.contains(&ip)
        } else if let Ok(single) = c.parse::<IpAddr>() {
            single == ip
        } else {
            false
        }
    })
}
