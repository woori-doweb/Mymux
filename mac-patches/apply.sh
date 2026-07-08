#!/usr/bin/env bash
# Re-apply the macOS fixes after a `git pull` overwrites the source.
#
# Upstream ChoiGyber/Mymux is built Windows-first; these edits make the macOS
# build usable and are NOT upstream, so every pull replaces the patched files.
# Run this before building the Mac universal bundle.
#
#   ./mac-patches/apply.sh          # re-apply everything (idempotent)
#   ./mac-patches/apply.sh --check  # report what's applied / still applies
#
# Steps: (1) source fixes (terminal.rs/explorer.rs/app.js) via 3-way patch,
# (2) xterm.min.js Korean-IME patch, (3) tauri.conf.json stable code signing.
# If a step conflicts (upstream changed that region), re-apply by hand from the
# memory note "Mymux macOS 소스 수정 (pull 후 재적용)".
set -euo pipefail
cd "$(dirname "$0")/.."

PATCH="mac-patches/mymux-mac-fixes.patch"
MEM_PATCH="/Users/rockg/.claude/projects/-Users-rockg-Project/memory/mymux-mac-fixes.patch"
[ -f "$PATCH" ] || PATCH="$MEM_PATCH"

XTERM="crates/mycli-desktop/frontend/vendor/xterm.min.js"
CONF="crates/mycli-desktop/tauri.conf.json"
SIGN_ID="Apple Development: Gwonseung Choi (3N3MW99Q9A)"
ICNS="crates/mycli-desktop/icons/icon.icns"
ICNS_SRC="mac-patches/icon-transparent.icns"

# --- xterm Korean-IME patch (Python, exact-string replace; idempotent) --------
# This WKWebView delivers Hangul with NO compositionstart/end events — instead it
# fires input events with inputType "insertReplacementText", each REPLACING the
# in-progress syllable (오→왜→…). Stock xterm 5.5.0 only handles "insertText", so
# every replacement is dropped and only the first jamo of each syllable reaches
# the PTY (broken). The patch rewrites `_inputEvent` to ALSO emit
# insertReplacementText, prefixed with DEL (\x7f) so the shell erases the prior
# syllable before the new one — reconstructing correct Hangul. (It also drops the
# original `!e.composed||!this._keyDownSeen` gate that blocked composed input.)
xterm_patch() { # arg1: "check" or "apply"
  python3 - "$XTERM" "$1" <<'PY'
import sys
p, mode = sys.argv[1], sys.argv[2]
s = open(p, encoding='utf-8').read()
OLD = '_inputEvent(e){if(e.data&&"insertText"===e.inputType&&(!e.composed||!this._keyDownSeen)&&!this.optionsService.rawOptions.screenReaderMode){if(this._keyPressHandled)return!1;this._unprocessedDeadKey=!1;const t=e.data;return this.coreService.triggerDataEvent(t,!0),this.cancel(e),!0}return!1}'
NEW = '_inputEvent(e){if(e.data&&("insertText"===e.inputType||"insertReplacementText"===e.inputType)&&!this.optionsService.rawOptions.screenReaderMode){if(this._keyPressHandled)return!1;this._unprocessedDeadKey=!1;const t=("insertReplacementText"===e.inputType?"\\x7f":"")+e.data;return this.coreService.triggerDataEvent(t,!0),this.cancel(e),!0}return!1}'
applied = 'insertReplacementText' in s and OLD not in s
if mode == 'check':
    print('applied' if applied else ('appliable' if OLD in s else 'conflict')); sys.exit(0)
if applied:
    print('already applied'); sys.exit(0)
if OLD not in s:
    print('conflict: original _inputEvent not found (upstream xterm changed)'); sys.exit(2)
open(p, 'w', encoding='utf-8').write(s.replace(OLD, NEW))
print('applied'); sys.exit(0)
PY
}

if [ "${1:-}" = "--check" ]; then
  if grep -q "IS_MAC" crates/mycli-desktop/frontend/app.js 2>/dev/null; then
    echo "SOURCE FIXES: already applied."
  elif git apply --3way --check "$PATCH" 2>/dev/null || git apply --check "$PATCH" 2>/dev/null; then
    echo "SOURCE FIXES: not applied yet but applies cleanly."
  else
    echo "SOURCE FIXES: CONFLICT — re-apply by hand (memory note)."
  fi
  echo "XTERM IME PATCH: $(xterm_patch check)"
  echo "SIGNING: $(grep -q signingIdentity "$CONF" 2>/dev/null && echo present || echo missing)"
  echo "ICON: $(cmp -s "$ICNS" "$ICNS_SRC" 2>/dev/null && echo "transparent (applied)" || echo "needs restore")"
  exit 0
fi

# 1) Source fixes (terminal.rs / explorer.rs / app.js) — idempotent.
if grep -q "IS_MAC" crates/mycli-desktop/frontend/app.js 2>/dev/null; then
  echo "Source fixes already present — skipping."
elif git apply --3way "$PATCH" 2>/dev/null || git apply "$PATCH"; then
  echo "Applied $PATCH"
else
  echo "git apply failed — re-apply by hand from the memory note."; exit 1
fi

# 2) xterm.min.js Korean-IME patch — idempotent.
out="$(xterm_patch apply)"; echo "xterm IME patch: $out"
case "$out" in conflict*) exit 1;; esac
if ! node --check "$XTERM" 2>/dev/null; then
  echo "ERROR: xterm patch produced invalid JS — restore from git!"; exit 1
fi

# 3) Build-time code signing (tauri.conf.json) — idempotent.
# Ad-hoc signing has no stable Designated Requirement, so macOS TCC can never
# remember "Allow" → the Documents/Desktop/Downloads prompt reappears forever.
# A stable identity at build time (updater tar.gz then wraps the signed app too)
# makes TCC persist the grant after one "Allow".
if grep -q "signingIdentity" "$CONF" 2>/dev/null; then
  echo "tauri.conf.json signingIdentity already present."
elif security find-identity -v -p codesigning 2>/dev/null | grep -qF "$SIGN_ID"; then
  tmp=$(mktemp)
  jq --arg id "$SIGN_ID" '.bundle.macOS.signingIdentity = $id' "$CONF" > "$tmp" && mv "$tmp" "$CONF"
  echo "Added macOS signingIdentity to $CONF"
else
  echo "WARN: signing identity not in keychain — build will be ad-hoc (TCC will re-prompt)."
fi

# 4) macOS app icon — transparent squircle (idempotent, checksum compare).
# Upstream ships icon.icns with a navy (#1A1A2E) background baked into the
# corners, so macOS renders a black border around the blue tile in the Dock /
# Finder. Restore the transparent-cornered squircle version (regenerated from
# mac-patches/icon-master-1024.png; see README step 6).
if cmp -s "$ICNS" "$ICNS_SRC" 2>/dev/null; then
  echo "icon.icns already transparent."
elif [ -f "$ICNS_SRC" ]; then
  cp "$ICNS_SRC" "$ICNS" && echo "Restored transparent icon.icns"
else
  echo "WARN: $ICNS_SRC missing — cannot restore transparent icon.icns."
fi
