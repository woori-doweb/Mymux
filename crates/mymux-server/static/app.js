// Mymux Console — static front-end (no build chain). Talks to the mymux-server
// HTTP/WS API. xterm.js + fit addon are vendored under /vendor.
//
// Workspace model: tabs → a binary split-tree of panes. Each pane owns one
// server terminal (one /ws/terminals/:id socket + one xterm instance). Saved
// commands and autocomplete act on the *focused* pane.
(function () {
  'use strict';
  const $ = (s) => document.querySelector(s);

  let me = null;
  let tabs = [];          // { id, name, root(node), contentEl, focusedPaneId }
  let panes = [];         // flat list of live panes
  let activeTabId = null;
  let focused = null;     // focused pane
  let seq = 0;
  const uid = (p) => p + (++seq);

  // Agent status (Claude Code hook bridge — nmux-linux-style badges).
  // termMeta mirrors the latest /api/terminals rows; prevAgentState detects
  // transitions so we only notify once per state change.
  let termMeta = {};
  let prevAgentState = {};
  let agentSeeded = false;   // first poll only records a baseline — no notify storm on reload
  const AGENT_BADGE = {
    running:       { icon: '⠋', label: '실행 중',            cls: 'st-running' },
    tool:          { icon: '⚙', label: '도구 실행 중',       cls: 'st-running' },
    needs_input:   { icon: '!', label: '입력 필요',          cls: 'st-needs-input' },
    done:          { icon: '✓', label: '작업 완료',          cls: 'st-done' },
    subagent_done: { icon: '✓', label: '하위 에이전트 완료', cls: 'st-done' },
  };
  const AGENT_RANK = { needs_input: 3, running: 2, tool: 2, done: 1, subagent_done: 1 };

  // Saved commands + autocomplete state
  let commands = [], cmdEditingId = null, cmdFilter = '';
  let acItems = [], acIndex = -1, acNavigated = false, lineBuf = '', acDismissed = false;
  let ctrlArmed = false;
  const KEY_SEQ = { esc: '\x1b', tab: '\t', 'c-c': '\x03', up: '\x1b[A', down: '\x1b[B', right: '\x1b[C', left: '\x1b[D' };

  // fetch wrapper: cookies ride along automatically (same origin); 401 -> login.
  async function api(path, opts) {
    const r = await fetch(path, Object.assign({ headers: { 'Content-Type': 'application/json' } }, opts || {}));
    if (r.status === 401) { location.href = '/login.html'; throw new Error('unauthorized'); }
    return r;
  }

  // Server sends PTY output as base64(standard) of raw bytes.
  function b64ToBytes(b64) {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }

  // ── Panes ─────────────────────────────────────────────────────────
  function wsUrl(termId) {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return proto + '://' + location.host + '/ws/terminals/' + termId;
  }

  function makePane(termId) {
    const el = document.createElement('div');
    el.className = 'pane';
    const host = document.createElement('div');
    host.className = 'pane-term';
    el.appendChild(host);

    const term = new Terminal({
      cursorBlink: true,
      fontFamily: '"D2Coding", "JetBrains Mono", "Cascadia Mono", Consolas, monospace',
      fontSize: 14,
      letterSpacing: 0,     // CJK 2:1 폭 비율 유지 — 자간 추가 시 정렬 붕괴
      lineHeight: 1.2,
      customGlyphs: true,   // 박스 드로잉 문자가 lineHeight 조정에도 끊기지 않도록
      theme: { background: '#0b0e14' },
    });
    const fit = new FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open(host);

    const pane = { id: uid('p'), tabId: null, termId, term, fit, ws: null, el, host, node: null };
    // Focus on click; middle-click pastes the system clipboard (PuTTY/X11 style).
    el.addEventListener('mousedown', (e) => {
      focusPane(pane);
      if (e.button === 1) { e.preventDefault(); pasteInto(pane); }
    });
    // focusin is authoritative: whatever gains DOM focus in this pane owns focus,
    // so a single click can't bounce back to the previously-focused pane.
    el.addEventListener('focusin', () => focusPane(pane));
    // Drag-select auto-copy (PuTTY-style): finishing a selection copies it.
    el.addEventListener('mouseup', () => {
      const sel = pane.term.getSelection();
      if (sel) copyText(sel);
    });
    // Observers only (no preventDefault/setData) — xterm's own core already
    // handles native Ctrl+C/Ctrl+V; this just surfaces whether the browser
    // ever dispatched the event at all, since a blocked/never-focused
    // terminal produces silence that's otherwise indistinguishable from
    // "xterm handled it but the OS clipboard is empty".
    el.addEventListener('copy', () => toast('네이티브 복사 이벤트 감지됨'));
    el.addEventListener('paste', () => toast('네이티브 붙여넣기 이벤트 감지됨'));
    term.onData((d) => {
      if (focused !== pane) focusPane(pane);
      if (ctrlArmed && d.length === 1) { d = applyCtrl(d); ctrlArmed = false; updateCtrlBtn(); }
      sendRaw(d); trackInput(d);
    });
    term.attachCustomKeyEventHandler(keyHandler);

    const ws = new WebSocket(wsUrl(termId));
    pane.ws = ws;
    ws.onopen = () => { fitSoon(pane); if (focused === pane) term.focus(); };
    ws.onmessage = (ev) => {
      let m; try { m = JSON.parse(ev.data); } catch (e) { return; }
      if (m.type === 'output') term.write(b64ToBytes(m.data));
      else if (m.type === 'exit') term.write('\r\n\x1b[33m[process exited]\x1b[0m\r\n');
      else if (m.type === 'error') term.write('\r\n\x1b[31m[error] ' + m.message + '\x1b[0m\r\n');
    };
    ws.onclose = () => { /* keep buffer on screen */ };
    panes.push(pane);
    return pane;
  }

  function fitPane(pane) {
    try {
      pane.fit.fit();
      if (pane.ws && pane.ws.readyState === 1) {
        pane.ws.send(JSON.stringify({ type: 'resize', cols: pane.term.cols, rows: pane.term.rows }));
      }
    } catch (e) { /* not laid out yet */ }
  }
  function fitSoon(pane) { requestAnimationFrame(() => fitPane(pane)); }
  function fitTree(node) { requestAnimationFrame(() => forEachPane(node, fitPane)); }

  function forEachPane(node, fn) {
    if (!node) return;
    if (node.type === 'pane') fn(node.pane);
    else { forEachPane(node.a, fn); forEachPane(node.b, fn); }
  }
  function firstPane(node) {
    if (!node) return null;
    if (node.type === 'pane') return node.pane;
    return firstPane(node.a) || firstPane(node.b);
  }

  function focusPane(pane) {
    if (!pane || focused === pane) return;   // idempotent — re-entrant focusin must not bounce/reset
    focused = pane;
    document.querySelectorAll('.pane.focused').forEach((e) => e.classList.remove('focused'));
    pane.el.classList.add('focused');
    lineBuf = ''; acDismissed = false; acHide(false);   // reset autocomplete on focus change
    const t = tabById(pane.tabId); if (t) t.focusedPaneId = pane.id;
    try { pane.term.focus(); } catch (e) {}
  }

  // Silent, permanent feedback for clipboard actions — Brave/enterprise
  // policy can deny navigator.clipboard without ever showing a permission
  // prompt, so without this the user sees no difference between "worked"
  // and "silently blocked".
  let toastTimer = null;
  function toast(msg) {
    const el = $('#toast');
    if (!el) return;
    el.textContent = msg;
    el.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => el.classList.remove('show'), 1800);
  }

  // Async Clipboard API needs a permission grant that Firefox never
  // implements (readText) and mobile Safari very often silently denies —
  // execCommand('copy') via a throwaway textarea works wherever that happens.
  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(() => toast('복사됨')).catch(() => execCommandCopy(text));
    } else {
      execCommandCopy(text);
    }
  }
  function execCommandCopy(text) {
    const prevActive = document.activeElement;
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.focus(); ta.select();
    let ok = false;
    try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
    document.body.removeChild(ta);
    if (prevActive instanceof HTMLElement) prevActive.focus();
    toast(ok ? '복사됨' : '복사 실패 — 브라우저가 차단했습니다');
  }

  // Read the system clipboard and paste into a pane via xterm's paste() — which
  // wraps it in bracketed-paste when the app enabled it, so multi-line pastes
  // don't run line by line. Falls back to a manual paste box when readText()
  // is unsupported/denied (same permission gap as above — this is the primary
  // paste path on mobile, since there's no Ctrl+V).
  function pasteInto(pane) {
    const p = pane || focused;
    if (!p) return;
    if (navigator.clipboard && navigator.clipboard.readText) {
      navigator.clipboard.readText()
        .then((t) => { if (t) { p.term.paste(t); p.term.focus(); toast('붙여넣음'); } else { openPasteModal(p); } })
        .catch(() => openPasteModal(p));
    } else {
      openPasteModal(p);
    }
  }

  let pastePane = null;
  function openPasteModal(pane) {
    pastePane = pane;
    $('#paste-input').value = '';
    $('#paste-modal').classList.remove('hidden');
    $('#paste-input').focus();
    toast('클립보드 자동 접근 차단됨 — 아래에 직접 붙여넣어 주세요');
  }
  function closePasteModal() {
    $('#paste-modal').classList.add('hidden');
    pastePane = null;
  }
  function insertPaste() {
    const t = $('#paste-input').value;
    const p = pastePane || focused;
    closePasteModal();
    if (p && t) { p.term.paste(t); p.term.focus(); }
  }

  // quiet=true toasts instead of alert()ing — layout restore spawns several
  // terminals in a row and must not stack modal alerts on partial failure.
  async function createServerTerminal(cwd, quiet) {
    const cols = focused ? focused.term.cols : 120;
    const rows = focused ? focused.term.rows : 36;
    const body = { cols, rows };
    if (cwd) body.cwd = cwd;
    const r = await api('/api/terminals', { method: 'POST', body: JSON.stringify(body) });
    if (!r.ok) {
      const j = await r.json().catch(() => ({}));
      const msg = j.error || 'terminal create failed';
      if (quiet) toast(msg); else alert(msg);
      return null;
    }
    const t = await r.json();
    // Register immediately — a layout save can fire before the next 5s poll,
    // and serialization needs this pane's cwd to survive a reload correctly.
    termMeta[t.id] = t;
    return t.id;
  }

  // ── Split-tree DOM ────────────────────────────────────────────────
  // node = { type:'pane', pane } | { type:'split', dir:'row'|'col', ratio, a, b, el, divider, parent }
  function nodeEl(node) { return node.type === 'pane' ? node.pane.el : node.el; }

  function applyFlex(split) {
    const g = split.ratio;
    nodeEl(split.a).style.flex = g + ' 1 0%';
    nodeEl(split.b).style.flex = (1 - g) + ' 1 0%';
  }

  function wireDivider(split) {
    split.divider.addEventListener('mousedown', (e) => {
      e.preventDefault();
      const onMove = (ev) => {
        const r = split.el.getBoundingClientRect();
        const ratio = split.dir === 'row' ? (ev.clientX - r.left) / r.width : (ev.clientY - r.top) / r.height;
        split.ratio = Math.min(0.9, Math.max(0.1, ratio));
        applyFlex(split);
        requestAnimationFrame(() => forEachPane(split, fitPane));
      };
      const onUp = () => {
        document.removeEventListener('mousemove', onMove);
        document.removeEventListener('mouseup', onUp);
        document.body.classList.remove('dragging');
        forEachPane(split, fitPane);
        saveLayoutSoon();
      };
      document.body.classList.add('dragging');
      document.addEventListener('mousemove', onMove);
      document.addEventListener('mouseup', onUp);
    });
  }

  // Build the split's DOM element. NOTE: this MOVES the existing child elements
  // (nodeEl(a), nodeEl(b)) into the new container — callers rely on that.
  function buildSplitEl(split) {
    const el = document.createElement('div');
    el.className = 'split ' + split.dir;
    const divider = document.createElement('div');
    divider.className = 'divider';
    el.appendChild(nodeEl(split.a));
    el.appendChild(divider);
    el.appendChild(nodeEl(split.b));
    split.el = el;
    split.divider = divider;
    applyFlex(split);
    wireDivider(split);
    return el;
  }

  async function splitFocused(dir) {
    const t = tabById(activeTabId);
    if (!t || !focused) return;
    const oldNode = focused.node;      // captured before await
    const oldEl = nodeEl(oldNode);
    const termId = await createServerTerminal();
    if (!termId) return;

    const q = makePane(termId);
    q.tabId = t.id;
    const qNode = { type: 'pane', pane: q, parent: null };
    q.node = qNode;

    const grandEl = oldEl.parentNode;
    const nextSib = oldEl.nextSibling;
    const split = { type: 'split', dir, ratio: 0.5, a: oldNode, b: qNode, el: null, divider: null, parent: oldNode.parent };
    oldNode.parent = split; qNode.parent = split;

    const splitEl = buildSplitEl(split);         // moves oldEl into splitEl
    grandEl.insertBefore(splitEl, nextSib);      // put the split where oldEl was

    if (!split.parent) { t.root = split; }
    else {
      if (split.parent.a === oldNode) split.parent.a = split; else split.parent.b = split;
      applyFlex(split.parent);                   // splitEl now occupies oldEl's flex slot
    }
    fitTree(split);
    focusPane(q);
    saveLayoutSoon();
  }

  function closePane(pane) {
    const t = tabById(pane.tabId);
    if (!t) return;
    const node = pane.node;
    if (node === t.root) { closeTab(t); return; }   // last pane in the tab → closeTab cleans up

    api('/api/terminals/' + pane.termId, { method: 'DELETE' }).catch(() => {});
    try { pane.ws.close(); } catch (e) {}
    try { pane.term.dispose(); } catch (e) {}
    panes = panes.filter((p) => p !== pane);

    const parent = node.parent;                     // a split
    const sibling = parent.a === node ? parent.b : parent.a;
    const parentEl = parent.el;
    const grandEl = parentEl.parentNode;
    const nextSib = parentEl.nextSibling;
    const sibEl = nodeEl(sibling);
    grandEl.insertBefore(sibEl, nextSib);           // hoist sibling out of the collapsing split
    grandEl.removeChild(parentEl);

    sibling.parent = parent.parent;
    if (!sibling.parent) { t.root = sibling; sibEl.style.flex = ''; }
    else {
      if (sibling.parent.a === parent) sibling.parent.a = sibling; else sibling.parent.b = sibling;
      applyFlex(sibling.parent);
    }
    const nf = firstPane(t.root);
    if (nf) focusPane(nf);
    fitTree(t.root);
    saveLayoutSoon();
  }

  // ── Tabs ──────────────────────────────────────────────────────────
  function tabById(id) { return tabs.find((t) => t.id === id); }
  function activeTab() { return tabById(activeTabId); }

  async function newTab(existingTermId, opts) {
    opts = opts || {};
    const termId = existingTermId || await createServerTerminal(opts.cwd, opts.quiet);
    if (!termId) return;
    const pane = makePane(termId);
    const node = { type: 'pane', pane, parent: null };
    pane.node = node;
    mountTab(node, opts.name);
    saveLayoutSoon();
  }

  // Attach an already-built split-tree as a new tab (shared by newTab and
  // layout restore, which rebuilds whole trees before mounting).
  function mountTab(root, name) {
    const contentEl = document.createElement('div');
    contentEl.className = 'tab-root';
    contentEl.appendChild(nodeEl(root));
    $('#tab-content').appendChild(contentEl);
    const tab = { id: uid('t'), name: name || ('Tab ' + (tabs.length + 1)), root, contentEl, focusedPaneId: null };
    forEachPane(root, (p) => { p.tabId = tab.id; });
    const fp = firstPane(root);
    if (fp) tab.focusedPaneId = fp.id;
    tabs.push(tab);
    activateTab(tab.id);
    return tab;
  }

  function activateTab(id) {
    activeTabId = id;
    tabs.forEach((t) => { t.contentEl.style.display = (t.id === id) ? 'flex' : 'none'; });
    updateWelcome();
    renderTabs();
    const t = tabById(id);
    if (!t) return;
    const p = panes.find((x) => x.id === t.focusedPaneId) || firstPane(t.root);
    if (p) focusPane(p);
    fitTree(t.root);
    ackTabAgentStatus(t);       // visiting the tab clears sticky agent badges
  }

  function closeTab(t) {
    forEachPane(t.root, (p) => {
      api('/api/terminals/' + p.termId, { method: 'DELETE' }).catch(() => {});
      try { p.ws.close(); } catch (e) {}
      try { p.term.dispose(); } catch (e) {}
    });
    panes = panes.filter((p) => p.tabId !== t.id);
    t.contentEl.remove();
    tabs = tabs.filter((x) => x !== t);
    if (activeTabId === t.id) {
      if (tabs.length) activateTab(tabs[tabs.length - 1].id);
      else { activeTabId = null; focused = null; updateWelcome(); renderTabs(); }
    } else { renderTabs(); }
    saveLayoutSoon();
  }

  function renderTabs() {
    const bar = $('#tab-bar');
    bar.innerHTML = '';
    tabs.forEach((t) => {
      const chip = document.createElement('div');
      chip.className = 'tab-chip' + (t.id === activeTabId ? ' active' : '');
      const st = tabAgentBadge(t);
      if (st) {
        const b = document.createElement('span');
        b.className = 'agent-badge ' + st.cls;
        b.textContent = st.icon;
        b.title = 'Claude: ' + st.label;
        chip.appendChild(b);
      }
      const label = document.createElement('span');
      label.className = 'tab-label';
      label.textContent = t.name;
      label.onclick = () => activateTab(t.id);
      const x = document.createElement('button');
      x.className = 'tab-x'; x.type = 'button'; x.textContent = '×'; x.title = '탭 닫기';
      x.onclick = (e) => { e.stopPropagation(); closeTab(t); };
      chip.appendChild(label); chip.appendChild(x);
      bar.appendChild(chip);
    });
    const add = document.createElement('button');
    add.className = 'tab-add'; add.type = 'button'; add.textContent = '+'; add.title = '새 탭 (Ctrl+Shift+N)';
    add.onclick = () => newTab();
    bar.appendChild(add);
  }

  function updateWelcome() {
    $('#tab-welcome').classList.toggle('hidden', tabs.length > 0);
  }

  // ── Layout persistence ──────────────────────────────────────────────
  // The tab/split structure (with each pane's termId + cwd) is saved to the
  // server, debounced after every structural change. On boot we rebuild it:
  // sessions that still exist are reattached (scrollback intact), dead ones
  // are respawned at their saved cwd — nmux-linux semantics: the structure
  // survives a reload or server restart, the scrollback doesn't promise to.
  let layoutSaveTimer = null;
  let layoutReady = false;    // block saves until the initial restore finished
  let layoutResume = false;   // lossy restore: resume saving on first user change
  let restoreHadLoss = false; // a pane failed to respawn during restore

  function serLayoutNode(node) {
    if (node.type === 'pane') {
      const m = termMeta[node.pane.termId];
      return { t: 'p', termId: node.pane.termId, cwd: (m && m.cwd) || null };
    }
    return { t: 's', dir: node.dir, ratio: node.ratio, a: serLayoutNode(node.a), b: serLayoutNode(node.b) };
  }
  function layoutData() {
    return { tabs: tabs.map((t) => ({ name: t.name, root: serLayoutNode(t.root) })) };
  }
  function saveLayoutSoon() {
    if (!layoutReady) {
      // After a LOSSY restore (quota/transient spawn failure dropped panes)
      // the saved copy is better than what we're showing — don't overwrite it
      // until the user makes a deliberate structural change (which is exactly
      // when this function gets called), then their new reality wins.
      if (!layoutResume) return;
      layoutReady = true;
      layoutResume = false;
    }
    clearTimeout(layoutSaveTimer);
    layoutSaveTimer = setTimeout(saveLayoutNow, 1500);
  }
  function saveLayoutNow() {
    if (!layoutReady) return;
    clearTimeout(layoutSaveTimer);
    api('/api/layout', { method: 'PUT', body: JSON.stringify({ data: layoutData() }) }).catch(() => {});
  }

  async function restoreLayout() {
    let data = null;
    try { const r = await api('/api/layout'); data = (await r.json()).data; } catch (e) { return; }
    if (!data || !Array.isArray(data.tabs) || !data.tabs.length || tabs.length) return;
    const attached = new Set();
    for (const spec of data.tabs) {
      if (!spec || !spec.root) continue;
      const root = await buildLayoutNode(spec.root, attached, 0);
      if (root) mountTab(root, spec.name);
      else restoreHadLoss = true;             // whole tab dropped
    }
    if (tabs.length) {
      toast(restoreHadLoss
        ? '레이아웃을 일부만 복원했습니다 — 저장본은 다음 변경 전까지 보존됩니다'
        : '이전 레이아웃을 복원했습니다');
    }
  }

  async function buildLayoutNode(spec, attached, depth) {
    if (!spec || depth > 16) return null;     // depth cap: corrupt/crafted blob
    if (spec.t === 'p') {
      let termId = null;
      const live = spec.termId && termMeta[spec.termId];
      if (live && !live.exited && !attached.has(spec.termId) && !panes.some((p) => p.termId === spec.termId)) {
        termId = spec.termId;                                       // reattach
      } else {
        termId = await createServerTerminal(spec.cwd || null, true); // respawn
      }
      if (!termId) { restoreHadLoss = true; return null; }
      attached.add(termId);
      const pane = makePane(termId);
      const node = { type: 'pane', pane, parent: null };
      pane.node = node;
      return node;
    }
    if (spec.t !== 's') return null;
    const a = await buildLayoutNode(spec.a, attached, depth + 1);
    const b = await buildLayoutNode(spec.b, attached, depth + 1);
    if (!a || !b) return a || b;              // degrade to the surviving side
    const ratio = (typeof spec.ratio === 'number' && spec.ratio > 0.05 && spec.ratio < 0.95) ? spec.ratio : 0.5;
    const split = { type: 'split', dir: spec.dir === 'col' ? 'col' : 'row', ratio, a, b, el: null, divider: null, parent: null };
    a.parent = split; b.parent = split;
    buildSplitEl(split);
    return split;
  }

  // "레이아웃 복구": mobile mistaps and drag accidents skew ratios — reset the
  // active tab's splits to an even tiling (nmux-linux fix-layout, web edition).
  function evenLayout() {
    const t = activeTab();
    if (!t) return;
    forEachSplit(t.root, (s) => { s.ratio = 0.5; applyFlex(s); });
    fitTree(t.root);
    saveLayoutSoon();
  }
  function forEachSplit(node, fn) {
    if (!node || node.type !== 'split') return;
    fn(node);
    forEachSplit(node.a, fn);
    forEachSplit(node.b, fn);
  }

  // ── Projects (프로젝트 = 폴더 하나 = 작업 단위 하나) ─────────────────
  let projects = [];

  async function loadProjects() {
    try { const r = await api('/api/projects'); projects = await r.json(); } catch (e) { return; }
    renderProjects();
  }

  // Exact-cwd rollup: a project shows the agent badge of any live terminal
  // spawned at its directory (same visibility rule as the terminal list).
  function projectBadge(p) {
    for (const id in termMeta) {
      const m = termMeta[id];
      if (m.cwd === p.cwd && m.agentStatus && AGENT_BADGE[m.agentStatus.state]) {
        return AGENT_BADGE[m.agentStatus.state];
      }
    }
    return null;
  }

  function renderProjects() {
    const ul = $('#proj-list');
    if (!ul) return;
    ul.innerHTML = '';
    $('#proj-empty').classList.toggle('hidden', projects.length !== 0);
    projects.forEach((p) => {
      const li = document.createElement('li');
      const st = projectBadge(p);
      if (st) {
        const b = document.createElement('span');
        b.className = 'agent-badge ' + st.cls;
        b.textContent = st.icon;
        b.title = 'Claude: ' + st.label;
        li.appendChild(b);
      }
      const name = document.createElement('span');
      name.className = 'proj-name';
      name.textContent = p.name;
      li.appendChild(name);
      const x = mkCmdBtn('×', 'del', () => deleteProject(p));
      x.title = '프로젝트 삭제';
      li.appendChild(x);
      li.title = p.cwd;
      li.onclick = () => openProject(p);
      ul.appendChild(li);
    });
  }

  function openProject(p) {
    closeDrawer();
    const pane = panes.find((x) => {
      const m = termMeta[x.termId];
      return m && m.cwd === p.cwd;
    });
    if (pane) { activateTab(pane.tabId); focusPane(pane); return; }
    newTab(null, { cwd: p.cwd, name: p.name });
  }

  async function deleteProject(p) {
    if (!confirm('프로젝트를 삭제할까요? — ' + p.name + ' (열린 터미널은 닫히지 않습니다)')) return;
    const r = await api('/api/projects/' + p.id, { method: 'DELETE' });
    if (r.ok) await loadProjects();
  }

  function openProjModal() {
    $('#proj-name').value = '';
    $('#proj-cwd').value = '';
    $('#proj-modal-error').textContent = '';
    $('#proj-modal').classList.remove('hidden');
    $('#proj-name').focus();
  }
  function closeProjModal() { $('#proj-modal').classList.add('hidden'); }

  async function saveProject(e) {
    e.preventDefault();
    const body = { name: $('#proj-name').value.trim(), cwd: $('#proj-cwd').value.trim() };
    if (!body.name || !body.cwd) { $('#proj-modal-error').textContent = '이름과 경로는 필수입니다.'; return; }
    const r = await api('/api/projects', { method: 'POST', body: JSON.stringify(body) });
    if (!r.ok) {
      const j = await r.json().catch(() => ({}));
      $('#proj-modal-error').textContent = j.error || '저장 실패';
      return;
    }
    closeProjModal();
    await loadProjects();
  }

  // Sidebar click: focus the pane already showing this terminal, else open it
  // in a new tab (reattach — e.g. after a page reload, or an admin viewing it).
  function openTerm(termId) {
    closeDrawer();                 // on mobile, close the drawer once a terminal is chosen
    const p = panes.find((x) => x.termId === termId);
    if (p) { activateTab(p.tabId); focusPane(p); return; }
    newTab(termId);
  }

  async function refreshList() {
    let list;
    try { const r = await api('/api/terminals'); list = await r.json(); } catch (e) { return; }
    const shown = new Set(panes.map((p) => p.termId));
    const ul = $('#term-list');
    ul.innerHTML = '';
    termMeta = {};
    list.forEach((t) => {
      termMeta[t.id] = t;
      const li = document.createElement('li');
      const st = t.agentStatus && AGENT_BADGE[t.agentStatus.state];
      if (st) {
        const b = document.createElement('span');
        b.className = 'agent-badge ' + st.cls;
        b.textContent = st.icon;
        b.title = 'Claude: ' + st.label;
        li.appendChild(b);
      }
      li.appendChild(document.createTextNode(
        (t.ownerUsername || '') + ' · ' + t.id.slice(0, 14) + (t.exited ? ' (exited)' : '')));
      li.dataset.id = t.id;
      if (shown.has(t.id)) li.className = 'active';
      li.title = shown.has(t.id) ? '열려 있음 — 클릭해 포커스' : '클릭해 새 탭으로 열기';
      li.onclick = () => openTerm(t.id);
      ul.appendChild(li);
    });
    notifyAgentTransitions(list);
    renderTabs();          // tab chips carry the badge too
    renderProjects();      // project rows roll the badge up by cwd
  }

  // ── Agent status: sticky-ack + notifications ────────────────────────
  // nmux-linux rule: done / needs-input stay until the user visits the
  // terminal. "Visit" = the tab is active AND the page is visible; then we
  // ack so the badge clears server-side for everyone.
  function agentStateOf(termId) {
    const m = termMeta[termId];
    return (m && m.agentStatus && m.agentStatus.state) || null;
  }

  function notifyAgentTransitions(list) {
    const seeded = agentSeeded;
    agentSeeded = true;
    list.forEach((t) => {
      const st = t.agentStatus ? t.agentStatus.state : null;
      const prev = prevAgentState[t.id] || null;
      prevAgentState[t.id] = st;
      if (!seeded || !st || st === prev || (st !== 'needs_input' && st !== 'done')) return;
      const pane = panes.find((p) => p.termId === t.id);
      if (pane && pane.tabId === activeTabId && !document.hidden) {
        ackAgentStatus(t.cwd);           // already looking at it — seen
        return;
      }
      const label = AGENT_BADGE[st].label;
      toast('Claude: ' + label + ' — ' + t.id.slice(0, 12));
      maybeNotify('Mymux Console', 'Claude ' + label + ' (' + t.id.slice(0, 12) + ')');
    });
  }

  function ackAgentStatus(cwd) {
    api('/api/agent-status/ack', { method: 'POST', body: JSON.stringify({ cwd }) })
      .then(() => refreshList()).catch(() => {});
  }

  function ackTabAgentStatus(tab) {
    if (document.hidden) return;
    const cwds = new Set();
    forEachPane(tab.root, (p) => {
      const s = agentStateOf(p.termId);
      if ((s === 'needs_input' || s === 'done') && termMeta[p.termId]) cwds.add(termMeta[p.termId].cwd);
    });
    cwds.forEach((c) => ackAgentStatus(c));
  }

  function tabAgentBadge(tab) {
    let best = null;
    forEachPane(tab.root, (p) => {
      const s = agentStateOf(p.termId);
      if (s && AGENT_BADGE[s] && (!best || AGENT_RANK[s] > AGENT_RANK[best])) best = s;
    });
    return best && AGENT_BADGE[best];
  }

  function maybeNotify(title, body) {
    if (localStorage.getItem('mymux_notify') !== 'on') return;
    if (!('Notification' in window) || Notification.permission !== 'granted') return;
    try { new Notification(title, { body }); } catch (e) { /* e.g. Android WebView */ }
  }

  function updateNotifyBtn() {
    const b = $('#btn-notify');
    if (b) b.classList.toggle('active', localStorage.getItem('mymux_notify') === 'on');
  }

  async function toggleNotify() {
    if (localStorage.getItem('mymux_notify') === 'on') {
      localStorage.setItem('mymux_notify', 'off');
      toast('브라우저 알림 끔');
    } else {
      if ('Notification' in window && Notification.permission !== 'granted') {
        const p = await Notification.requestPermission();
        if (p !== 'granted') { toast('브라우저 알림 권한이 거부되었습니다'); updateNotifyBtn(); return; }
      }
      localStorage.setItem('mymux_notify', 'on');
      toast('에이전트 완료·입력 필요 시 알림');
    }
    updateNotifyBtn();
  }

  async function logout() {
    if (!confirm('로그아웃할까요?')) return;
    try { await api('/api/auth/logout', { method: 'POST' }); } catch (e) {}
    location.href = '/login.html';
  }

  async function loadAudit() {
    if (!me || me.role !== 'admin') return;
    const r = await api('/api/audit?limit=50');
    const rows = await r.json();
    $('#audit-box').classList.remove('hidden');
    const ul = $('#audit-list');
    ul.innerHTML = '';
    rows.forEach((a) => {
      const li = document.createElement('li');
      const t = (a.time || '').slice(11, 19);
      li.textContent = t + ' ' + a.action + ' ' + (a.username || '-') + ' [' + a.result + ']';
      ul.appendChild(li);
    });
  }

  // ── Saved Commands ──────────────────────────────────────────────────
  // Insert into the focused terminal. execute=true appends Enter (runs it);
  // execute=false pastes without a newline so the user can review first.
  function sendToActive(text, execute) {
    const w = focused && focused.ws;
    if (!focused || !w || w.readyState !== 1) {
      alert('활성 터미널이 없습니다. 상단 + Tab 으로 터미널을 여세요.');
      return;
    }
    w.send(JSON.stringify({ type: 'input', data: execute ? text + '\r' : text }));
    closeDrawer();                 // on mobile, reveal the terminal after choosing a command
    focused.term.focus();
  }

  async function loadCommands() {
    try {
      const r = await api('/api/commands');
      commands = await r.json();
    } catch (e) { return; }
    renderCommands();
  }

  function mkCmdBtn(label, cls, fn) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = cls;
    b.textContent = label;
    b.onclick = (ev) => { ev.stopPropagation(); fn(); };
    return b;
  }

  function renderCommands() {
    const ul = $('#cmd-list');
    ul.innerHTML = '';
    const f = cmdFilter.trim().toLowerCase();
    const items = commands.filter((c) => !f
      || c.name.toLowerCase().includes(f)
      || c.command.toLowerCase().includes(f)
      || (c.description || '').toLowerCase().includes(f));
    $('#cmd-empty').classList.toggle('hidden', commands.length !== 0);
    items.forEach((c) => {
      const li = document.createElement('li');
      if (c.favorite) li.className = 'fav';
      if (c.description) li.title = c.description;
      const name = document.createElement('span');
      name.className = 'cmd-name';
      name.textContent = (c.favorite ? '★ ' : '') + c.name;
      const txt = document.createElement('span');
      txt.className = 'cmd-text';
      txt.textContent = c.command;                     // textContent → no XSS
      li.appendChild(name);
      li.appendChild(txt);
      // Row click pastes the command (no newline) into the focused terminal.
      li.onclick = () => sendToActive(c.command, false);
      const actions = document.createElement('div');
      actions.className = 'cmd-actions';
      actions.appendChild(mkCmdBtn('▶ Run', 'run', () => sendToActive(c.command, true)));
      actions.appendChild(mkCmdBtn(c.favorite ? '★' : '☆', 'fav-btn', () => toggleFav(c)));
      actions.appendChild(mkCmdBtn('✎', 'edit', () => openCmdModal(c)));
      actions.appendChild(mkCmdBtn('×', 'del', () => deleteCmd(c)));
      li.appendChild(actions);
      ul.appendChild(li);
    });
  }

  function openCmdModal(cmd) {
    cmdEditingId = cmd ? cmd.id : null;
    $('#cmd-modal-title').textContent = cmd ? '명령 편집' : '명령 추가';
    $('#cmd-name').value = cmd ? cmd.name : '';
    $('#cmd-command').value = cmd ? cmd.command : '';
    $('#cmd-desc').value = cmd ? (cmd.description || '') : '';
    $('#cmd-modal-error').textContent = '';
    $('#cmd-modal').classList.remove('hidden');
    $('#cmd-name').focus();
  }

  function closeCmdModal() {
    $('#cmd-modal').classList.add('hidden');
    cmdEditingId = null;
  }

  async function saveCmd(e) {
    e.preventDefault();
    const body = {
      name: $('#cmd-name').value.trim(),
      command: $('#cmd-command').value.trim(),
      description: $('#cmd-desc').value.trim(),
    };
    if (!body.name || !body.command) {
      $('#cmd-modal-error').textContent = '이름과 명령은 필수입니다.';
      return;
    }
    if (cmdEditingId) {
      const existing = commands.find((c) => c.id === cmdEditingId);
      body.favorite = existing ? existing.favorite : false;   // edit form doesn't carry it
    }
    const path = cmdEditingId ? '/api/commands/' + cmdEditingId : '/api/commands';
    const r = await api(path, { method: cmdEditingId ? 'PUT' : 'POST', body: JSON.stringify(body) });
    if (!r.ok) {
      const j = await r.json().catch(() => ({}));
      $('#cmd-modal-error').textContent = j.error || '저장 실패';
      return;
    }
    closeCmdModal();
    await loadCommands();
  }

  async function toggleFav(c) {
    const body = { name: c.name, command: c.command, description: c.description || '', favorite: !c.favorite };
    const r = await api('/api/commands/' + c.id, { method: 'PUT', body: JSON.stringify(body) });
    if (r.ok) await loadCommands();
  }

  async function deleteCmd(c) {
    if (!confirm('삭제할까요? — ' + c.name)) return;
    const r = await api('/api/commands/' + c.id, { method: 'DELETE' });
    if (r.ok) await loadCommands();
  }

  // ── Command autocomplete (type-ahead over the terminal) ─────────────
  // Best-effort: we mirror the current input line locally and, when it
  // prefix-matches a saved command name/text, show a popup. Accepting erases
  // the typed prefix and inserts the full command (no newline).
  function sendRaw(d) {
    const w = focused && focused.ws;
    if (w && w.readyState === 1) w.send(JSON.stringify({ type: 'input', data: d }));
  }

  function trackInput(d) {
    if (d.charCodeAt(0) === 27) return;             // escape seq (arrows/fn) — ignore
    for (const ch of d) {
      const code = ch.charCodeAt(0);
      if (ch === '\r' || ch === '\n' || code === 3) { lineBuf = ''; acDismissed = false; acHide(false); }
      else if (code === 127 || code === 8) { lineBuf = lineBuf.slice(0, -1); }
      else if (code >= 32) { lineBuf += ch; }
    }
    if (!acDismissed) acRefresh();
  }

  function acRefresh() {
    const q = lineBuf.trim().toLowerCase();
    if (!q) { acHide(false); return; }
    acItems = commands.filter((c) =>
      c.name.toLowerCase().startsWith(q) || c.command.toLowerCase().startsWith(q)
    ).slice(0, 8);
    if (!acItems.length) { acHide(false); return; }
    acIndex = 0; acNavigated = false;
    renderAc();
    $('#ac-popup').classList.remove('hidden');
  }

  function renderAc() {
    const ul = $('#ac-list');
    ul.innerHTML = '';
    acItems.forEach((c, i) => {
      const li = document.createElement('li');
      if (i === acIndex) li.className = 'sel';
      const n = document.createElement('span'); n.className = 'n'; n.textContent = c.name;
      const t = document.createElement('span'); t.className = 'c'; t.textContent = c.command;
      li.appendChild(n); li.appendChild(t);
      // mousedown (not click) so the terminal doesn't lose focus first.
      li.addEventListener('mousedown', (e) => { e.preventDefault(); acIndex = i; acAccept(); });
      ul.appendChild(li);
    });
  }

  function acMove(delta) {
    if (!acItems.length) return;
    acIndex = (acIndex + delta + acItems.length) % acItems.length;
    acNavigated = true;
    renderAc();
  }

  function acAccept() {
    const c = acItems[acIndex];
    if (!c) { acHide(true); return; }
    sendRaw('\x7f'.repeat(lineBuf.length) + c.command);   // erase typed prefix, insert full cmd
    lineBuf = c.command;
    acHide(true);
    if (focused) focused.term.focus();
  }

  function acHide(dismiss) {
    $('#ac-popup').classList.add('hidden');
    acItems = []; acIndex = -1; acNavigated = false;
    if (dismiss) acDismissed = true;
  }

  // Runs before xterm processes a key; return false to swallow it. Handles
  // workspace shortcuts (any time) and autocomplete nav (while popup is open).
  function keyHandler(e) {
    if (e.type !== 'keydown') return true;
    if (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey) {
      const k = e.key.toLowerCase();
      if (k === 'd') { e.preventDefault(); splitFocused('row'); return false; }
      if (k === 'e') { e.preventDefault(); splitFocused('col'); return false; }
      if (k === 'w') { e.preventDefault(); if (focused) closePane(focused); return false; }
      if (k === 'n') { e.preventDefault(); newTab(); return false; }
    }
    if ($('#ac-popup').classList.contains('hidden')) return true;
    switch (e.key) {
      case 'ArrowDown': acMove(1); return false;
      case 'ArrowUp': acMove(-1); return false;
      case 'Tab': acAccept(); return false;
      case 'Enter':
        if (acNavigated) { acAccept(); return false; }   // a suggestion was picked → accept
        acHide(false); return true;                       // otherwise let Enter run the line
      case 'Escape': acHide(true); return false;
      default: return true;
    }
  }

  // ── Mobile (drawer, keyboard-aware height, accessory key bar) ───────
  function setAppHeight() {
    const h = window.visualViewport ? window.visualViewport.height : window.innerHeight;
    document.documentElement.style.setProperty('--app-h', h + 'px');
    const t = activeTab(); if (t) forEachPane(t.root, fitPane);
  }
  function openDrawer() { $('#sidebar').classList.add('open'); $('#drawer-backdrop').classList.add('open'); }
  function closeDrawer() { $('#sidebar').classList.remove('open'); $('#drawer-backdrop').classList.remove('open'); }
  function updateCtrlBtn() { const b = $('#kb-ctrl'); if (b) b.classList.toggle('active', ctrlArmed); }
  function applyCtrl(d) {
    const u = d.toUpperCase().charCodeAt(0);
    return (u >= 64 && u <= 95) ? String.fromCharCode(u & 31) : d;   // Ctrl-@ .. Ctrl-_
  }
  function keybarKey(k) {
    if (k === 'ctrl') { ctrlArmed = !ctrlArmed; updateCtrlBtn(); return; }
    if (k.indexOf('lit:') === 0) { sendRaw(k.slice(4)); return; }
    const seq = KEY_SEQ[k];
    if (seq !== undefined) sendRaw(seq);
  }

  async function boot() {
    const r = await api('/api/auth/me');
    me = await r.json();
    $('#whoami').textContent = me.username + ' (' + me.role + ')';
    await refreshList();
    await loadProjects();
    await loadAudit();
    await loadCommands();
    renderTabs();
    updateWelcome();
    await restoreLayout();            // needs termMeta from refreshList above
    layoutReady = !restoreHadLoss;    // lossy → hold the saved copy (see saveLayoutSoon)
    layoutResume = restoreHadLoss;
    $('#btn-new').onclick = () => newTab();
    $('#btn-split-h').onclick = () => splitFocused('row');
    $('#btn-split-v').onclick = () => splitFocused('col');
    $('#btn-even').onclick = evenLayout;
    $('#btn-close').onclick = () => { if (focused) closePane(focused); };
    $('#btn-proj-add').onclick = openProjModal;
    $('#proj-cancel').onclick = closeProjModal;
    $('#proj-form').onsubmit = saveProject;
    $('#proj-modal').onclick = (e) => { if (e.target.id === 'proj-modal') closeProjModal(); };
    // Last-chance layout save when the page goes away (debounce may be pending).
    window.addEventListener('pagehide', () => {
      if (!layoutReady) return;
      try {
        fetch('/api/layout', {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ data: layoutData() }),
          keepalive: true,
        });
      } catch (e) { /* best effort */ }
    });
    $('#btn-paste').onclick = () => pasteInto(focused);
    $('#btn-notify').onclick = toggleNotify;
    updateNotifyBtn();
    // Coming back to the page counts as "visiting" the active tab.
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) { const t = activeTab(); if (t) ackTabAgentStatus(t); }
    });
    $('#btn-logout').onclick = logout;
    $('#btn-cmd-add').onclick = () => openCmdModal(null);
    $('#cmd-search').oninput = (e) => { cmdFilter = e.target.value; renderCommands(); };
    $('#cmd-cancel').onclick = closeCmdModal;
    $('#cmd-form').onsubmit = saveCmd;
    $('#cmd-modal').onclick = (e) => { if (e.target.id === 'cmd-modal') closeCmdModal(); };
    $('#paste-cancel').onclick = closePasteModal;
    $('#paste-form').onsubmit = (e) => { e.preventDefault(); insertPaste(); };
    $('#paste-modal').onclick = (e) => { if (e.target.id === 'paste-modal') closePasteModal(); };
    $('#btn-menu').onclick = openDrawer;
    $('#drawer-backdrop').onclick = closeDrawer;
    document.querySelectorAll('#keybar button').forEach((b) => {
      // pointerdown + preventDefault so tapping a key doesn't blur the terminal
      // (which would dismiss the soft keyboard).
      b.addEventListener('pointerdown', (e) => { e.preventDefault(); keybarKey(b.dataset.k); });
    });
    setAppHeight();
    window.addEventListener('resize', setAppHeight);
    if (window.visualViewport) {
      window.visualViewport.addEventListener('resize', setAppHeight);
      window.visualViewport.addEventListener('scroll', setAppHeight);
    }
    setInterval(refreshList, 5000);
  }

  boot().catch(() => { /* redirected to login on 401 */ });
})();
