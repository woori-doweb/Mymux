# Feature Gap — desktop app (`mycli-desktop`) → `mymux-server`

Migration review: the web console (`mymux-server`) intentionally reimplements a
subset of the original Tauri desktop app (`crates/mycli-desktop` + `mycli-core`).
This tracks what was **dropped** in the migration, whether it should be ported
(the server is authenticated + multi-user, unlike the single-user desktop app),
and the current status.

## Flagged by the operator

### 1. Left UI / terminal "differentiation" (tabs + panes) — **done (this change)**

Desktop had a **tabs > panes > session-tree** model:

- Terminal **tabs** with rename (double-click) / close; `Ctrl+Shift+N` new tab
  (`mycli-desktop/frontend/app.js:2860-2904`).
- **Split panes**: horizontal `Ctrl+Shift+D`, vertical `Ctrl+Shift+E`, draggable
  dividers, per-pane statusbar (label + cwd + close), `Alt+Arrows` to move,
  `Ctrl+Shift+W` to close (`app.js:294-326,1544-1623`).
- Right **session tree** grouped by tab, drag sessions between tabs, rename
  (`app.js:2972-3056`).

`mymux-server` has a single flat terminal list (`static/index.html` `#term-list`),
click-to-switch only. Not listed in `MYMUX_SERVER.md §12`.

**Portability:** frontend-only — each pane reuses one existing
`/ws/terminals/:id`; N xterm instances in a CSS grid. Backend unchanged.

**Ported (this change):** tabs (a binary split-tree of panes each), toolbar
`+ Tab` / `⬌` / `⬍` / `Close` and shortcuts `Ctrl+Shift+D` (split left/right),
`Ctrl+Shift+E` (split top/bottom), `Ctrl+Shift+W` (close pane), `Ctrl+Shift+N`
(new tab); draggable dividers; click/typing to focus a pane; the sidebar list
reattaches an existing server terminal into a new tab (reload/admin recovery).
Not yet ported: per-tab session-tree drag-between-tabs, tab/pane rename.

### 2. Saved commands / "paste command directly" — **implemented (this change)**

Desktop had a personal command library (`~/.mycli/commands.json`, `mycli-core`
`storage.rs`/`models.rs`, `mycli-desktop/src/commands.rs`) with CRUD, favorites,
seeded shortcuts (`cl`,`cc`,`cr`,…), search, plus three insertion paths:

- **Send / double-click** → active terminal **with Enter** (execute)
  (`app.js:2941-2948`).
- Context **"Send to active session"** → **without newline** (paste for review).
- **Autocomplete popup** while typing (Tab/Enter accept) (`app.js:3469-3598`).

**Ported to the server** as a per-user feature: table `saved_commands`
(owner-scoped), `/api/commands` CRUD (auth-gated), a sidebar panel, and
insertion into the active WebSocket terminal — **row click = paste (no newline,
safe default)**, **▶ Run = execute (with Enter)**. **Autocomplete is also done**
(this change): typing at the terminal shows a prefix-matched popup (↑↓ navigate,
Tab/Enter accept, Esc dismiss); accepting erases the typed prefix and inserts the
full command. Tracking is a best-effort local mirror of the input line.

## Full gap table

| Feature | Desktop source | Server | Portability | Status |
|---|---|---|---|---|
| Terminal tabs/splits (분화) | index 18-19,83 | ✓ tabs+splits | frontend-only | **done (this change)** |
| Saved commands + insert | commands.rs, index 67-78 | — | DB+REST+UI | **done (this change)** |
| Command autocomplete popup | index 217 | ✓ | frontend + commands API | **done (this change)** |
| File explorer (drives/favs/search/ops) | index 46-64, explorer.rs | — | needs `workspace_root` sandbox | pending (P2) |
| File viewer/editor (find/autosave) | index 146-158 | — | needs sandbox | pending (P2) |
| In-app browser (WebView + Playwright/CDP) | index 86-143, browser.rs | — | **desktop-only** (WebView) | excluded |
| SSH-out connection mgmt (saved hosts/tmux) | index 257-281 | — | server IS the host — different model | excluded / N/A |
| Themes / accent color | index 20-25 | dark only | frontend-only | pending (P3) |
| Session restore on restart | index 231-242 | dropped by design (§12) | trade-off | won't-do |
| Context menu (Copy / Send) | index 221-228 | — | frontend-only | pending (P3) |
| Toast / auto-update | index 16,283 | — | auto-update N/A on server | partial |

## Port priority (multi-user server)

1. **Terminal tabs/splits (분화) — DONE.**
2. **Saved commands + insertion — DONE.**
3. **Command autocomplete — DONE.**
4. File explorer / viewer — with a `workspace_root` sandbox.
5. Themes / context menu — polish.

Excluded (conflict with the server model): in-app WebView browser, app
auto-update, SSH-out. Won't-do: session restore (PTYs are intentionally dropped
on restart).
