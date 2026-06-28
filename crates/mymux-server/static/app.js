// Mymux Console — static front-end (no build chain). Talks to the mymux-server
// HTTP/WS API. xterm.js + fit addon are vendored under /vendor.
(function () {
  'use strict';
  const $ = (s) => document.querySelector(s);
  let term = null, fit = null, ws = null, current = null, me = null;

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
    term.onData((d) => { if (ws && ws.readyState === 1) ws.send(JSON.stringify({ type: 'input', data: d })); });
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

  async function boot() {
    const r = await api('/api/auth/me');
    me = await r.json();
    $('#whoami').textContent = me.username + ' (' + me.role + ')';
    initTerm();
    await refreshList();
    await loadAudit();
    $('#btn-new').onclick = newTerminal;
    $('#btn-close').onclick = closeCurrent;
    $('#btn-logout').onclick = logout;
    setInterval(refreshList, 5000);
  }

  boot().catch(() => { /* redirected to login on 401 */ });
})();
