// Mymux Console — static front-end (no build chain). Talks to the mymux-server
// HTTP/WS API. xterm.js + fit addon are vendored under /vendor.
(function () {
  'use strict';
  const $ = (s) => document.querySelector(s);
  let term = null, fit = null, ws = null, current = null, me = null;
  let commands = [], cmdEditingId = null, cmdFilter = '';
  let acItems = [], acIndex = -1, acNavigated = false, lineBuf = '', acDismissed = false;

  // fetch wrapper: cookies ride along automatically (same origin); 401 -> login.
  async function api(path, opts) {
    const r = await fetch(path, Object.assign({ headers: { 'Content-Type': 'application/json' } }, opts || {}));
    if (r.status === 401) { location.href = '/login.html'; throw new Error('unauthorized'); }
    return r;
  }

  function initTerm() {
    term = new Terminal({ cursorBlink: true, fontFamily: 'monospace', fontSize: 14, theme: { background: '#0b0e14' } });
    fit = new FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open($('#terminal'));
    fit.fit();
    term.onData((d) => { sendRaw(d); trackInput(d); });
    term.attachCustomKeyEventHandler(acKeyHandler);
    window.addEventListener('resize', doFit);
  }

  function doFit() {
    try {
      fit.fit();
      if (ws && ws.readyState === 1) ws.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }));
    } catch (e) { /* not ready */ }
  }

  // Server sends PTY output as base64(standard) of raw bytes.
  function b64ToBytes(b64) {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }

  function attach(id) {
    if (ws) { try { ws.close(); } catch (e) {} ws = null; }
    current = id;
    lineBuf = ''; acDismissed = false; acHide(false);
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    ws = new WebSocket(proto + '://' + location.host + '/ws/terminals/' + id);
    ws.onopen = () => { doFit(); term.focus(); };
    ws.onmessage = (ev) => {
      let m; try { m = JSON.parse(ev.data); } catch (e) { return; }
      if (m.type === 'output') term.write(b64ToBytes(m.data));
      else if (m.type === 'exit') term.write('\r\n\x1b[33m[process exited]\x1b[0m\r\n');
      else if (m.type === 'error') term.write('\r\n\x1b[31m[error] ' + m.message + '\x1b[0m\r\n');
    };
    ws.onclose = () => { /* keep buffer on screen */ };
    highlight();
  }

  function highlight() {
    document.querySelectorAll('#term-list li').forEach((li) => li.classList.toggle('active', li.dataset.id === current));
  }

  async function refreshList() {
    const r = await api('/api/terminals');
    const list = await r.json();
    const ul = $('#term-list');
    ul.innerHTML = '';
    list.forEach((t) => {
      const li = document.createElement('li');
      li.textContent = (t.ownerUsername || '') + ' · ' + t.id.slice(0, 14) + (t.exited ? ' (exited)' : '');
      li.dataset.id = t.id;
      if (t.id === current) li.className = 'active';
      li.onclick = () => attach(t.id);
      ul.appendChild(li);
    });
  }

  async function newTerminal() {
    const r = await api('/api/terminals', { method: 'POST', body: JSON.stringify({ cols: (term && term.cols) || 120, rows: (term && term.rows) || 36 }) });
    if (!r.ok) { const j = await r.json().catch(() => ({})); alert(j.error || 'create failed'); return; }
    const t = await r.json();
    await refreshList();
    attach(t.id);
  }

  async function closeCurrent() {
    if (!current) return;
    await api('/api/terminals/' + current, { method: 'DELETE' });
    if (ws) { try { ws.close(); } catch (e) {} }
    current = null;
    await refreshList();
    if (term) term.clear();
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
  // Insert into the active terminal. execute=true appends Enter (runs it);
  // execute=false pastes without a newline so the user can review first.
  function sendToActive(text, execute) {
    if (!current || !ws || ws.readyState !== 1) {
      alert('활성 터미널이 없습니다. 먼저 터미널을 열거나 선택하세요.');
      return;
    }
    ws.send(JSON.stringify({ type: 'input', data: execute ? text + '\r' : text }));
    if (term) term.focus();
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
      // Row click pastes the command (no newline) into the active terminal.
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
  // the typed prefix and inserts the full command (no newline, so it doesn't
  // auto-run). Heuristic tracking — good at a prompt, not a full line editor.
  function sendRaw(d) {
    if (ws && ws.readyState === 1) ws.send(JSON.stringify({ type: 'input', data: d }));
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
    if (term) term.focus();
  }

  function acHide(dismiss) {
    $('#ac-popup').classList.add('hidden');
    acItems = []; acIndex = -1; acNavigated = false;
    if (dismiss) acDismissed = true;
  }

  // Runs before xterm processes a key; return false to swallow it (keeps it off
  // the PTY). Only intercepts while the popup is visible.
  function acKeyHandler(e) {
    if (e.type !== 'keydown') return true;
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
    initTerm();
    await refreshList();
    await loadAudit();
    await loadCommands();
    $('#btn-new').onclick = newTerminal;
    $('#btn-close').onclick = closeCurrent;
    $('#btn-logout').onclick = logout;
    $('#btn-cmd-add').onclick = () => openCmdModal(null);
    $('#cmd-search').oninput = (e) => { cmdFilter = e.target.value; renderCommands(); };
    $('#cmd-cancel').onclick = closeCmdModal;
    $('#cmd-form').onsubmit = saveCmd;
    $('#cmd-modal').onclick = (e) => { if (e.target.id === 'cmd-modal') closeCmdModal(); };
    setInterval(refreshList, 5000);
  }

  boot().catch(() => { /* redirected to login on 401 */ });
})();
