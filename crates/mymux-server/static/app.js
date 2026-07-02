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

  // Saved commands + autocomplete state
  let commands = [], cmdEditingId = null, cmdFilter = '';
  let acItems = [], acIndex = -1, acNavigated = false, lineBuf = '', acDismissed = false;

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

    const term = new Terminal({ cursorBlink: true, fontFamily: 'monospace', fontSize: 14, theme: { background: '#0b0e14' } });
    const fit = new FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open(host);

    const pane = { id: uid('p'), tabId: null, termId, term, fit, ws: null, el, host, node: null };
    el.addEventListener('mousedown', () => { if (focused !== pane) focusPane(pane); });
    term.onData((d) => { if (focused !== pane) focusPane(pane); sendRaw(d); trackInput(d); });
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
    if (!pane) return;
    focused = pane;
    document.querySelectorAll('.pane.focused').forEach((e) => e.classList.remove('focused'));
    pane.el.classList.add('focused');
    lineBuf = ''; acDismissed = false; acHide(false);   // reset autocomplete on focus change
    const t = tabById(pane.tabId); if (t) t.focusedPaneId = pane.id;
    try { pane.term.focus(); } catch (e) {}
  }

  async function createServerTerminal() {
    const cols = focused ? focused.term.cols : 120;
    const rows = focused ? focused.term.rows : 36;
    const r = await api('/api/terminals', { method: 'POST', body: JSON.stringify({ cols, rows }) });
    if (!r.ok) { const j = await r.json().catch(() => ({})); alert(j.error || 'terminal create failed'); return null; }
    const t = await r.json();
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
  }

  // ── Tabs ──────────────────────────────────────────────────────────
  function tabById(id) { return tabs.find((t) => t.id === id); }
  function activeTab() { return tabById(activeTabId); }

  async function newTab(existingTermId) {
    const termId = existingTermId || await createServerTerminal();
    if (!termId) return;
    const pane = makePane(termId);
    const node = { type: 'pane', pane, parent: null };
    pane.node = node;
    const contentEl = document.createElement('div');
    contentEl.className = 'tab-root';
    contentEl.appendChild(pane.el);
    $('#tab-content').appendChild(contentEl);
    const tab = { id: uid('t'), name: 'Tab ' + (tabs.length + 1), root: node, contentEl, focusedPaneId: pane.id };
    pane.tabId = tab.id;
    tabs.push(tab);
    activateTab(tab.id);
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
  }

  function renderTabs() {
    const bar = $('#tab-bar');
    bar.innerHTML = '';
    tabs.forEach((t) => {
      const chip = document.createElement('div');
      chip.className = 'tab-chip' + (t.id === activeTabId ? ' active' : '');
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

  // Sidebar click: focus the pane already showing this terminal, else open it
  // in a new tab (reattach — e.g. after a page reload, or an admin viewing it).
  function openTerm(termId) {
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
    list.forEach((t) => {
      const li = document.createElement('li');
      li.textContent = (t.ownerUsername || '') + ' · ' + t.id.slice(0, 14) + (t.exited ? ' (exited)' : '');
      li.dataset.id = t.id;
      if (shown.has(t.id)) li.className = 'active';
      li.title = shown.has(t.id) ? '열려 있음 — 클릭해 포커스' : '클릭해 새 탭으로 열기';
      li.onclick = () => openTerm(t.id);
      ul.appendChild(li);
    });
  }

  async function logout() {
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

  async function boot() {
    const r = await api('/api/auth/me');
    me = await r.json();
    $('#whoami').textContent = me.username + ' (' + me.role + ')';
    await refreshList();
    await loadAudit();
    await loadCommands();
    renderTabs();
    updateWelcome();
    $('#btn-new').onclick = () => newTab();
    $('#btn-split-h').onclick = () => splitFocused('row');
    $('#btn-split-v').onclick = () => splitFocused('col');
    $('#btn-close').onclick = () => { if (focused) closePane(focused); };
    $('#btn-logout').onclick = logout;
    $('#btn-cmd-add').onclick = () => openCmdModal(null);
    $('#cmd-search').oninput = (e) => { cmdFilter = e.target.value; renderCommands(); };
    $('#cmd-cancel').onclick = closeCmdModal;
    $('#cmd-form').onsubmit = saveCmd;
    $('#cmd-modal').onclick = (e) => { if (e.target.id === 'cmd-modal') closeCmdModal(); };
    window.addEventListener('resize', () => { const t = activeTab(); if (t) forEachPane(t.root, fitPane); });
    setInterval(refreshList, 5000);
  }

  boot().catch(() => { /* redirected to login on 401 */ });
})();
