#!/usr/bin/env bash
# Claude Code hook → mymux-server agent-status bridge (nmux-linux-inspired).
#
# Claude Code pipes a JSON hook payload on stdin; we forward {event, cwd} to
# the local console over loopback with a shared token. Registration snippet:
#
#   mymux-server hooks print --config /etc/mymux-console/config.toml
#
# Fail-open on purpose: every error path exits 0 so a broken console can
# never break Claude Code itself.
set -u

URL="${MYMUX_AGENT_STATUS_URL:-http://127.0.0.1:7070/api/agent-status}"
TOKEN_FILE="${MYMUX_AGENT_TOKEN_FILE:-$HOME/.config/mymux/agent-token}"

[ -r "$TOKEN_FILE" ] || exit 0
command -v curl >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"

if command -v jq >/dev/null 2>&1; then
  body="$(printf '%s' "$payload" | jq -c '{event: .hook_event_name, cwd: (.cwd // "")}' 2>/dev/null)" || exit 0
else
  # sed fallback: fine for the plain-ASCII paths this server actually uses.
  event="$(printf '%s' "$payload" | sed -n 's/.*"hook_event_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  cwd="$(printf '%s' "$payload" | sed -n 's/.*"cwd"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$event" ] || exit 0
  cwd=${cwd//\\/\\\\}   # re-escape backslashes so the body stays valid JSON
  body="{\"event\":\"$event\",\"cwd\":\"$cwd\"}"
fi

curl -fsS -m 2 -X POST "$URL" \
  -H 'Content-Type: application/json' \
  -H "X-Mymux-Agent-Token: $(cat "$TOKEN_FILE")" \
  -d "$body" >/dev/null 2>&1 || true
exit 0
