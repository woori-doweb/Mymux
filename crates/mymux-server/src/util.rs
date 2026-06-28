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
    {
        // Walk right-to-left: the rightmost hops are the addresses our trusted
        // proxy actually observed; leftmost entries are client-supplied and so
        // spoofable. Return the first hop that is NOT itself a trusted proxy.
        for hop in xff.rsplit(',') {
            if let Ok(ip) = hop.trim().parse::<IpAddr>()
                && !is_trusted_proxy(ip, &net.trusted_proxy_ips)
            {
                return ip;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NetworkConfig;
    use std::net::IpAddr;

    fn net(proxy: bool, trusted: &[&str], cidrs: &[&str]) -> NetworkConfig {
        NetworkConfig {
            behind_reverse_proxy: proxy,
            trusted_proxy_ips: trusted.iter().map(|s| s.to_string()).collect(),
            allowed_client_cidrs: cidrs.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn cidr_empty_allows_all() {
        assert!(cidr_allowed(ip("8.8.8.8"), &[]));
    }

    #[test]
    fn cidr_in_and_out_of_range() {
        let cidrs = ["100.64.0.0/10".to_string()];
        assert!(cidr_allowed(ip("100.100.1.1"), &cidrs));
        assert!(!cidr_allowed(ip("10.0.0.1"), &cidrs));
    }

    #[test]
    fn cidr_single_ip() {
        let cidrs = ["127.0.0.1".to_string()];
        assert!(cidr_allowed(ip("127.0.0.1"), &cidrs));
        assert!(!cidr_allowed(ip("127.0.0.2"), &cidrs));
    }

    #[test]
    fn client_ip_ignores_xff_when_not_behind_proxy() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        assert_eq!(
            resolve_client_ip(&h, ip("100.64.0.5"), &net(false, &[], &[])),
            ip("100.64.0.5")
        );
    }

    #[test]
    fn client_ip_ignores_xff_from_untrusted_peer() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        assert_eq!(
            resolve_client_ip(&h, ip("100.64.0.5"), &net(true, &["127.0.0.1"], &[])),
            ip("100.64.0.5")
        );
    }

    #[test]
    fn client_ip_uses_rightmost_not_spoofed_leftmost() {
        let mut h = HeaderMap::new();
        // Attacker prepends a spoofed value; nginx appends the real client.
        h.insert("x-forwarded-for", "9.9.9.9, 100.64.0.7".parse().unwrap());
        assert_eq!(
            resolve_client_ip(&h, ip("127.0.0.1"), &net(true, &["127.0.0.1"], &[])),
            ip("100.64.0.7")
        );
    }

    #[test]
    fn client_ip_skips_trusted_proxies_in_chain() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "100.64.0.7, 127.0.0.1".parse().unwrap());
        assert_eq!(
            resolve_client_ip(&h, ip("127.0.0.1"), &net(true, &["127.0.0.1"], &[])),
            ip("100.64.0.7")
        );
    }
}
