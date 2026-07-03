#!/usr/bin/env bash
# Deploy mymux-server to this host.
#
# Default (no args): sync static/ only — this is the fast path for
# frontend-only commits (app.js/style.css/*.html) and needs no service
# restart, since ServeDir reads straight off disk on every request.
#
# --build: also rebuild the release binary and restart the systemd service —
# use this after backend (src/**) changes.
#
# Usage:
#   deploy/deploy.sh              # static assets only
#   deploy/deploy.sh --build      # binary + static + restart
#   deploy/deploy.sh --dry-run    # show what would change, do nothing

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_STATIC="$REPO_ROOT/crates/mymux-server/static"
DEPLOY_STATIC="/opt/mymux-console/static"
DEPLOY_BIN="/opt/mymux-console/mymux-server"
SERVICE="mymux-console"
HEALTH_URL="http://127.0.0.1:7070/health"

MODE="static"
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --build|--full) MODE="build" ;;
    --static) MODE="static" ;;
    --dry-run) DRY_RUN=1 ;;
    *) echo "unknown arg: $arg (expected --build | --static | --dry-run)" >&2; exit 1 ;;
  esac
done

if git -C "$REPO_ROOT" status --porcelain -- crates/mymux-server | grep -q .; then
  echo "note: uncommitted changes under crates/mymux-server — deploying working tree, not HEAD" >&2
fi

sync_static() {
  echo "== static: diff against $DEPLOY_STATIC =="
  sudo rsync -rlt --delete -i --dry-run "$REPO_STATIC"/ "$DEPLOY_STATIC"/

  if [ "$DRY_RUN" = "1" ]; then
    echo "== dry-run: not applying =="
    return
  fi

  echo "== static: syncing =="
  sudo rsync -rlt --delete --chown=root:root --chmod=D755,F644 \
    "$REPO_STATIC"/ "$DEPLOY_STATIC"/

  echo "== static: verifying served content matches repo =="
  for f in app.js style.css index.html login.html; do
    cmp -s "$REPO_STATIC/$f" "$DEPLOY_STATIC/$f" || { echo "mismatch after sync: $f" >&2; exit 1; }
  done
  curl -sf "$HEALTH_URL" >/dev/null && echo "health OK ($HEALTH_URL)" || { echo "health check FAILED" >&2; exit 1; }
}

build_and_deploy() {
  if [ "$DRY_RUN" = "1" ]; then
    echo "== dry-run: would cargo build --release -p mymux-server, install, restart $SERVICE =="
    return
  fi

  echo "== build: cargo build --release -p mymux-server =="
  # /home/yms/.local/bin/cc is a Claude Code wrapper script, not a compiler,
  # and shadows /usr/bin/cc earlier in PATH — pin the real toolchain.
  CC=/usr/bin/gcc CXX=/usr/bin/g++ \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/gcc \
    cargo build --release -p mymux-server --manifest-path "$REPO_ROOT/Cargo.toml"

  echo "== build: installing binary =="
  sudo install -o root -g root -m 755 \
    "$REPO_ROOT/target/release/mymux-server" "$DEPLOY_BIN"

  echo "== build: restarting $SERVICE =="
  sudo systemctl restart "$SERVICE"
  sleep 1
  sudo systemctl is-active --quiet "$SERVICE" || {
    echo "service failed to become active:" >&2
    sudo systemctl status "$SERVICE" --no-pager >&2
    exit 1
  }
  curl -sf "$HEALTH_URL" >/dev/null && echo "health OK ($HEALTH_URL)" || { echo "health check FAILED" >&2; exit 1; }
}

sync_static
if [ "$MODE" = "build" ]; then
  build_and_deploy
fi

echo "== done ($MODE) =="
