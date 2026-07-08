# macOS fixes for Mymux (re-apply after every `git pull`)

Upstream `ChoiGyber/Mymux` is built Windows-first. The source edits here make the
macOS build usable and are **not** in the tracked app source, so a `git pull`
overwrites those files. This `mac-patches/` folder itself is committed to the
repo (so the tooling and prebuilt assets travel with a clone), but the edits it
applies still live only in your working tree — re-apply them with `apply.sh`
before building.

## Quick use

```bash
./mac-patches/apply.sh           # re-apply after a pull (uses a 3-way merge)
./mac-patches/apply.sh --check   # applied? / still applies cleanly?
```

A durable copy of the patch also lives in the Claude memory dir
(`~/.claude/projects/-Users-rockg-Project/memory/mymux-mac-fixes.patch`) in case
the repo is re-cloned. Verified to re-apply cleanly across 0.1.5 → 0.1.6 via
`git apply --3way`.

## What the patch changes

1. **`src/terminal.rs` — shell spawning was Windows-only.** The shell-id logic
   was wrapped entirely in `#[cfg(windows)]`, so on macOS the empty "Default
   Shell" id and `powershell`/`cmd` ids were spawned as literal programs → *"No
   such file or directory"*. Added a `#[cfg(not(windows))]` branch in
   `build_command` (Windows ids → default shell; `bash`/`zsh`/`sh` → absolute
   paths) and in `default_shell_builder` (lock to `$SHELL` → `/bin/zsh` → bash →
   sh, with `-i`).

2. **`frontend/app.js` — Mac shell lock + font.**
   - `IS_MAC` platform constant.
   - `createXterm`: on Mac use `SF Mono`/`Menlo`/`Monaco` instead of the wide
     `D2Coding` (fixes too-wide letter spacing).
   - `getDefaultShellId`: on Mac always return `undefined` (system zsh).
   - Hide the Windows shell `<select>` and the PowerShell/CMD/Bash quick-buttons
     on Mac; relabel "Default Shell" → "Terminal (zsh)".

3. **`src/explorer.rs` — TCC infinite-prompt mitigation.** Listing `~` stat-ed
   every child via `entry.metadata()`; on macOS stat-ing the protected folders
   (Desktop/Documents/Downloads/Pictures/Music) triggers a TCC prompt each, and
   ad-hoc signing can't remember the grant → endless prompts. Now uses
   `entry.file_type()` (readdir d_type, no stat) for dirs and only stats files.
   (Only `explorer_list_local` — the SFTP lister is remote, no TCC concern.)

4. **`vendor/xterm.min.js` — Korean IME composition fix (root cause confirmed by
   instrumentation).** This WKWebView delivers Hangul with **no compositionstart/
   end events** (`isComposing` always false); instead it fires `input` events with
   `inputType="insertReplacementText"`, each REPLACING the in-progress syllable
   (오→왜→왷→왜). xterm 5.5.0's `_inputEvent` only handles `insertText` and ignores
   `insertReplacementText`, so only the first jamo of each syllable reached the
   PTY (garbled). `apply.sh` rewrites `_inputEvent` (Python exact-string replace,
   idempotent) to emit BOTH input types, prefixing `insertReplacementText` with
   **DEL (`\x7f`)** so the shell erases the prior syllable before the new one —
   reconstructing correct Hangul. (It also drops the old `!e.composed||
   !this._keyDownSeen` gate.) Vendored file (overwritten on pull) → applied as a
   separate `apply.sh` step, not in the .patch. Validated with `node --check` and
   confirmed by hand-typing Korean. To re-diagnose if it regresses: add temporary
   composition/input/blur listeners on the helper textarea that dump events to
   `~/.mycli/session.json` via `session_save`, type Korean, read the file.
   Caveat: the backspace approach assumes a UTF-8 line-editing PTY (1 syllable =
   1 grapheme = 1 DEL) — fine for shells and most TUIs.

   (Note: `app.js startFocusKeeper` is also gated off on macOS via `if (IS_MAC)
   return;`. That was a wrong first guess at the IME cause, but the keeper is a
   Windows/WebView2-only focus workaround — harmless and unnecessary on Mac — so
   the gate stays. The real IME fix is the xterm patch above.)

5. **`tauri.conf.json` — stable code signing (stops the endless TCC prompt).**
   Ad-hoc signing has no stable Designated Requirement, so macOS TCC can never
   remember "Allow" → the Documents/Desktop/Downloads prompt reappears on every
   launch. `apply.sh` sets `bundle.macOS.signingIdentity` to
   `Apple Development: Gwonseung Choi (3N3MW99Q9A)` (via jq, idempotent) so
   `cargo tauri build` signs the app with a stable identity at build time — the
   updater `tar.gz` then wraps the signed app too, so auto-updates stay signed.
   Grant "Allow" once and macOS never asks again. (One-time stale-entry reset
   after first switching from ad-hoc: `tccutil reset SystemPolicyDocumentsFolder
   com.mycli.desktop`.)

   > ⚠️ Apple **Development** certs are for this Mac / registered devices. A DMG
   > signed this way is rejected by Gatekeeper on *other* Macs. For public
   > distribution you'd need a Developer ID cert + notarization instead.

6. **`icons/icon.icns` — transparent squircle (kills the black icon border).**
   Upstream's `icon.icns` (generated from `icons/ios/AppIcon-512@2x.png`) has a
   navy `#1A1A2E` background baked into the corners, so macOS draws a black
   square border around the blue tile in the Dock / Finder. `apply.sh` step 4
   restores a transparent-cornered version from `mac-patches/icon-transparent.icns`
   (idempotent checksum compare). This is a binary file, so it lives as a prebuilt
   `.icns` rather than in the `.patch`. The blue squircle tile geometry
   (866×866 tile, ~120px corner radius, 79px margin in a 1024 canvas) is
   preserved exactly — only the navy background became transparent.

   To regenerate from scratch (e.g. after an artwork change), rebuild from the
   transparent 1024 master `mac-patches/icon-master-1024.png`:

   ```bash
   ISET=/tmp/Mymux.iconset; rm -rf "$ISET"; mkdir -p "$ISET"
   M=mac-patches/icon-master-1024.png
   for s in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" \
            "128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" \
            "512 512x512"; do set -- $s; magick "$M" -resize ${1}x${1} PNG32:"$ISET/icon_$2.png"; done
   cp "$M" "$ISET/icon_512x512@2x.png"
   iconutil -c icns "$ISET" -o mac-patches/icon-transparent.icns
   cp mac-patches/icon-transparent.icns crates/mycli-desktop/icons/icon.icns
   ```

   The transparent master itself was made by flood-filling the navy background of
   `AppIcon-512@2x.png` to transparent, then rebuilding the tile as a clean
   antialiased squircle (`roundrectangle 79,79 945,945 120,120`, fill `#0F9EF0`)
   with the white chevron re-composited from the source (luminance level 55%,90%).

## Known still-open (not in this patch)

- **Korean IME** is patched (#4) but verify after big upstream xterm changes.
- **Public distribution** still needs Developer ID + notarization; the current
  signing is Apple Development (this Mac only).
