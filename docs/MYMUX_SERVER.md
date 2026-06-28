# Mymux Web Control Console (`mymux-server`)

An authenticated, Tailscale-internal web terminal for an Ubuntu server. It adds
its own login, per-user sessions, role-based access, per-user WebSocket PTYs,
and an audit log **on top of** an already network-restricted host. It is a new
`crates/mymux-server` crate and does **not** modify the existing Windows/Tauri
desktop app (`crates/mycli-desktop`).

> ⚠️ This is not a public-internet service. It assumes the host is already
> protected by Tailscale + IP allowlist + UFW/Nginx. The app adds a second
> layer (login, ownership, audit), not the only layer.

## 1. Purpose

Operate a remote Ubuntu server from a browser inside the tailnet: open a shell,
type, resize, close — with login, per-user terminal ownership, and an audit
trail, without exposing anything to the public internet.

## 2. Security assumptions

- Reachability is already restricted (Tailscale / Nginx `allow` / UFW).
- The app binds **`127.0.0.1:7070`** by default and is **never** exposed on
  `0.0.0.0`. Nginx terminates TLS and restricts to the Tailscale range.
- The process runs as a **non-root** user.
- Terminals can only be opened **under `workspace_root`**.
- Terminal input/output bodies are **not** written to the audit log by default.

## 3. Install layout (suggested)

| Path | Contents |
|---|---|
| `/opt/mymux-console/mymux-server` | the release binary |
| `/opt/mymux-console/static/` | `static/` from this crate (UI + vendored xterm) |
| `/etc/mymux-console/config.toml` | configuration (chmod 600) |
| `/var/lib/mymux-console/mymux.db` | SQLite database |
| `/var/log/mymux-console/` | logs (if file logging is added) |
| `/srv/mymux-workspaces/` | the only tree terminals may `cd` into |

## 4. Create the service user

```bash
sudo useradd --system --home /var/lib/mymux-console --shell /usr/sbin/nologin mymux-console
sudo mkdir -p /var/lib/mymux-console /srv/mymux-workspaces /opt/mymux-console
sudo chown -R mymux-console:mymux-console /var/lib/mymux-console /srv/mymux-workspaces
```

## 5. Build & place files

```bash
# On a build host with Rust >= 1.85 (edition 2024):
cargo build -p mymux-server --release
sudo install -m 0755 target/release/mymux-server /opt/mymux-console/mymux-server
sudo cp -r crates/mymux-server/static /opt/mymux-console/static
sudo install -d -m 0750 /etc/mymux-console
sudo install -m 0600 deploy/config.example.toml /etc/mymux-console/config.toml
```

Edit `/etc/mymux-console/config.toml` for your host (paths, `public_url`,
`allowed_client_cidrs`).

## 6. Initialize the database & first admin

`serve` and `user create` both run migrations automatically (idempotent). Create
the first admin:

```bash
sudo -u mymux-console /opt/mymux-console/mymux-server user create \
  --username admin --role admin \
  --config /etc/mymux-console/config.toml
# prompts for the password (Argon2id-hashed; never stored in plaintext)
```

Roles: `admin` (see/close all terminals + read audit), `operator` (own
terminals only), `viewer` (login only, no terminals).

## 7. systemd

```bash
sudo cp deploy/mymux-console.service /etc/systemd/system/mymux-console.service
sudo systemctl daemon-reload
sudo systemctl enable --now mymux-console
sudo systemctl status mymux-console
journalctl -u mymux-console -f
```

## 8. Nginx reverse proxy

```bash
sudo cp deploy/nginx.mymux-console.conf /etc/nginx/conf.d/mymux-console.conf
# put the TLS cert/key where the config expects them, then:
sudo nginx -t && sudo systemctl reload nginx
```

The config restricts to `100.64.0.0/10` (Tailscale) and proxies `/ws/` with the
upgrade headers + long timeouts.

## 9. Tailscale / IP restriction

The reverse proxy `allow`s only the Tailscale range. The app additionally
enforces `network.allowed_client_cidrs` and honors `X-Forwarded-For` **only**
from `trusted_proxy_ips`. Tighten the Nginx `allow` to specific 100.x hosts for
production.

## 10. HTTP / WebSocket API

| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/health` | none | liveness |
| POST | `/api/auth/login` | none | login, sets HttpOnly cookie |
| GET | `/api/auth/me` | cookie | current user |
| POST | `/api/auth/logout` | cookie | revoke session |
| GET | `/api/terminals` | cookie | list (own; admin sees all) |
| POST | `/api/terminals` | cookie | create a terminal |
| DELETE | `/api/terminals/:id` | cookie | close (owner/admin) |
| GET | `/api/audit?limit=N` | cookie (admin) | recent audit rows |
| GET | `/ws/terminals/:id` | cookie | attach WebSocket PTY |

WebSocket frames — client→server `{"type":"input","data":"…"}`,
`{"type":"resize","cols":120,"rows":40}`; server→client
`{"type":"output","data":"<base64 bytes>"}`, `{"type":"exit","code":null}`,
`{"type":"error","message":"…"}`.

## 11. Manual verification

1. `mymux-server user create … --role admin`
2. `mymux-server serve --config …` → `curl -s localhost:7070/health` ⇒ `{"status":"ok"}`
3. Wrong password ⇒ row in `login_attempts` and an `auth.login.failure` audit row.
4. `curl localhost:7070/api/terminals` without a cookie ⇒ `401`.
5. Log in via the browser, open a terminal, run `pwd` / `ls` / `echo hello`.
6. Resize the browser → the PTY resizes.
7. Close the terminal ⇒ the shell process is actually gone (`ps -ef | grep bash`).
8. An `operator` attaching another user's terminal ⇒ `403` + `terminal.denied`.
9. Logout ⇒ API calls return `401` again.
10. As `admin`, `GET /api/audit` returns recent events.

## 12. Operational limits (MVP)

- Terminals live in memory only — a server restart drops open PTYs (sessions in
  the DB survive; the shells do not).
- No SFTP / file browser / editor / in-app browser / saved commands yet (planned
  follow-up PRs).
- Output is delivered best-effort over a bounded broadcast channel; a reconnect
  replays the last `replay_buffer_bytes` of scrollback.
- The app must run as non-root, bound to `127.0.0.1`, behind Nginx + Tailscale.

## 13. Known follow-ups / deferred hardening

Surfaced by an independent review and intentionally deferred (acceptable for a
tailnet-internal MVP; tracked here so they aren't forgotten):

- **Live sessions survive logout/revoke.** Revoking a session (logout/disable)
  does not yet terminate an already-open WebSocket PTY — the shell lives until
  the socket disconnects or idles out. Follow-up: track live sockets per session
  and close them on revoke, or periodically re-validate inside the WS loop.
- **Username-keyed lockout = targeted DoS.** A known username can be deliberately
  locked out by failed attempts. Fine inside the tailnet; consider IP-scoped
  counting or exponential backoff later.
- **CIDR check is not uniform.** `allowed_client_cidrs` is enforced at login and
  the WS upgrade but not on the REST terminal/audit endpoints (nginx + Tailscale
  already gate reachability). Follow-up: apply the check uniformly.
- **Reattach output gaps.** Output produced between the replay snapshot and the
  live subscription can be missed; a broadcast `Lagged` burst drops chunks (now
  surfaced to the client as an `error` frame prompting reattach). Follow-up:
  subscribe-then-snapshot under the replay lock for gap-free reattach.
- **No Content-Security-Policy.** `X-Frame-Options`, `X-Content-Type-Options`,
  and `Referrer-Policy` are set; a strict CSP needs `login.html`'s inline script
  moved to a file first.
