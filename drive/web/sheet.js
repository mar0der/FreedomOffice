// FreedomOffice Sheets in the browser (/drive/sheet/<id>): the grid viewer.
//
// - No framework, no build step; served by freedomoffice-server with a strict CSP (no inline
//   script or style, same-origin requests only). All text reaches the page as text nodes /
//   textContent or is painted on the canvas; nothing is ever parsed as HTML.
// - The browser session cookie is HttpOnly; the CSRF token lives in memory only. Nothing is
//   kept in browser storage or in URLs.
// - The grid is drawn on a canvas that follows a native scroller (so iOS momentum scrolling,
//   scroll bars and the keyboard work), and only the cells in view are fetched, in blocks:
//   GET /api/items/<id>/sheet/cells?sheet=&r1=&r2=&c1=&c2=
// - Values, number formats and formulas come from the server (the app's own engine); this
//   file only lays out and draws.
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const MAXR = 1048576;
  const MAXC = 16384;
  const BR = 100; // rows per fetched block
  const BC = 20; // columns per fetched block
  const SPACER_CAP = 8000000; // CSS px; larger sheets scroll proportionally
  const KEYC = MAXC; // cell key = r * KEYC + c

  const S = {
    id: '',
    csrf: '',
    meta: null,
    rev: '',
    cur: 0,
    z: 1,
    layouts: new Map(), // sheet index -> layout
    cells: new Map(), // key -> {t, k, st, l}
    blocks: new Map(), // "sheet:br:bc" -> {keys, used, loading}
    sel: { r: 0, c: 0 },
    dark: false,
    inflight: 0,
    drawQueued: false,
    cellInfo: new Map(), // "rev:sheet:r:c" -> info
    fetchSeq: 0,
  };

  // ---------------------------------------------------------------------------
  // Small helpers
  // ---------------------------------------------------------------------------

  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));

  function colName(c) {
    let s = '';
    c += 1;
    while (c > 0) {
      const m = (c - 1) % 26;
      s = String.fromCharCode(65 + m) + s;
      c = Math.floor((c - 1) / 26);
    }
    return s;
  }

  const cellName = (r, c) => colName(c) + String(r + 1);

  /** Bidi embedding/override/isolate controls make text read in a different order than it is
   *  written; cell text is shown without them (the formula bar shows them as a visible mark). */
  const BIDI = /[‪-‮⁦-⁩]/g;
  const stripBidi = (s) => s.replace(BIDI, '');
  const markBidi = (s) => s.replace(BIDI, '¤');

  function el(tag, attrs, ...kids) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === 'on') for (const [ev, fn] of Object.entries(v)) e.addEventListener(ev, fn);
      else if (k === 'class') e.className = v;
      else if (/^on/i.test(k)) throw new Error('no event handler attributes');
      else e.setAttribute(k, v === true ? '' : String(v));
    }
    for (const k of kids) e.append(typeof k === 'string' ? document.createTextNode(k) : k);
    return e;
  }

  function screen(name) {
    for (const id of ['boot', 'signin', 'problem', 'app']) $(id).hidden = id !== name;
  }

  class ApiError extends Error {
    constructor(status, body) {
      super((body && body.error) || `HTTP ${status}`);
      this.status = status;
      this.code = (body && body.code) || '';
    }
  }

  async function get(path) {
    let res;
    try {
      res = await fetch(path, { credentials: 'same-origin', cache: 'no-store' });
    } catch {
      throw new ApiError(0, { error: "Can't reach the server. Check your connection." });
    }
    if (res.ok) return res.json();
    let j = {};
    try {
      j = await res.json();
    } catch {
      /* no body */
    }
    throw new ApiError(res.status, j);
  }

  const itemPath = (rest) => `/api/items/${encodeURIComponent(S.id)}${rest}`;

  // ---------------------------------------------------------------------------
  // Boot, sign in
  // ---------------------------------------------------------------------------

  async function session() {
    const res = await fetch('/api/web/session', { credentials: 'same-origin', cache: 'no-store' });
    if (res.status === 401) return false;
    if (!res.ok) throw new ApiError(res.status, {});
    S.csrf = (await res.json()).csrf;
    return true;
  }

  async function boot() {
    const m = /^\/drive\/sheet\/([A-Za-z0-9._-]{1,128})\/?$/.exec(location.pathname);
    if (!m) return problem('This address is not a workbook.', {});
    S.id = m[1];
    $('back').href = '/drive';
    try {
      if (!(await session())) return showSignIn();
    } catch (e) {
      return problem(e.message, {});
    }
    await open();
  }

  function showSignIn() {
    screen('signin');
    $('signin-pass').value = '';
    $('signin-user').focus();
  }

  async function onSignIn(e) {
    e.preventDefault();
    const err = $('signin-error');
    const username = $('signin-user').value.trim();
    const password = $('signin-pass').value;
    err.hidden = true;
    if (!username || !password) {
      err.textContent = 'Enter your username and password.';
      err.hidden = false;
      return;
    }
    $('signin-submit').disabled = true;
    try {
      const res = await fetch('/api/web/login', {
        method: 'POST',
        credentials: 'same-origin',
        cache: 'no-store',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      });
      let j = {};
      try {
        j = await res.json();
      } catch {
        /* no body */
      }
      if (!res.ok) {
        err.textContent = res.status === 401 ? 'The username or password is incorrect.' : j.error || `Sign-in failed (HTTP ${res.status}).`;
        err.hidden = false;
        return;
      }
      $('signin-pass').value = '';
      if (j.user && j.user.must_change_password) {
        err.textContent = 'Choose a new password in FreedomOffice Drive first.';
        err.hidden = false;
        return;
      }
      S.csrf = j.csrf;
      await open();
    } catch {
      err.textContent = "Can't reach the server. Check your connection.";
      err.hidden = false;
    } finally {
      $('signin-submit').disabled = false;
    }
  }

  function problem(text, { download, title, retry } = {}) {
    screen('problem');
    $('problem-title').textContent = title || "This workbook can't be shown";
    $('problem-text').textContent = text;
    const d = $('problem-download');
    d.hidden = !download;
    if (download) d.href = `${itemPath('/content')}?download=1`;
    $('problem-retry').hidden = !retry;
  }

  // ---------------------------------------------------------------------------
  // Opening a workbook
  // ---------------------------------------------------------------------------

  async function open() {
    screen('boot');
    let meta;
    try {
      meta = await get(itemPath('/sheet'));
    } catch (e) {
      if (e.status === 401) return showSignIn();
      if (e.status === 404) return problem("This workbook doesn't exist, or you don't have access to it.", { title: 'Not found' });
      const canDownload = ['unsupported', 'too_large', 'timeout', 'unreadable'].includes(e.code);
      return problem(e.message, { download: canDownload, retry: e.status === 0 || e.status >= 500 });
    }
    S.meta = meta;
    S.rev = meta.rev;
    const first = meta.sheets.findIndex((s) => !s.hidden);
    S.cur = clamp(meta.active, 0, meta.sheets.length - 1);
    if (meta.sheets[S.cur] && meta.sheets[S.cur].hidden) S.cur = Math.max(0, first);
    S.z = window.innerWidth < 700 ? 1.2 : 1;
    screen('app');
    setupChrome();
    buildTabs();
    switchSheet(S.cur, true);
    setStatus();
    startPolling();
  }

  function setupChrome() {
    const it = S.meta.item;
    const t = $('title');
    t.replaceChildren(el('bdi', null, it.name));
    document.title = `${it.name.replace(BIDI, '')} - FreedomOffice Sheets`;
    $('back').href = it.parent && !S.meta.link ? `/drive#/files/${encodeURIComponent(it.parent)}` : '/drive';
    const base = `/api/items/${encodeURIComponent(S.id)}`;
    $('m-xlsx').href = `${base}/content?download=1`;
    $('m-native').href = `freedomoffice://open?server=${encodeURIComponent(location.origin)}&item=${encodeURIComponent(S.id)}`;
    updateCsvLink();
  }

  function updateCsvLink() {
    $('m-csv').href = `${itemPath('/sheet/csv')}?sheet=${S.cur}`;
  }

  function setStatus(text) {
    const st = $('status');
    if (text !== undefined) {
      st.textContent = text;
      return;
    }
    st.textContent = S.meta && S.meta.live ? 'View · live' : 'View only';
  }

  function buildTabs() {
    const nav = $('tabs');
    nav.replaceChildren();
    nav.setAttribute('role', 'tablist');
    S.meta.sheets.forEach((s, i) => {
      if (s.hidden) return;
      const b = el('button', { type: 'button', class: 'tab', role: 'tab', 'aria-selected': i === S.cur ? 'true' : 'false', on: { click: () => switchSheet(i) } }, el('bdi', null, s.name));
      if (s.tab && /^#[0-9a-f]{6}$/.test(s.tab) && i !== S.cur) b.style.borderBottomColor = s.tab;
      nav.append(b);
    });
  }

  // ---------------------------------------------------------------------------
  // Layout of a sheet: column x / row y offsets (in unzoomed px)
  // ---------------------------------------------------------------------------

  function layoutOf(i) {
    let L = S.layouts.get(i);
    if (L && L.rev === S.rev) return L;
    const sh = S.meta.sheets[i];
    const nc = clamp(Math.max(sh.cols + 8, 26), 1, MAXC);
    const nr = clamp(Math.max(sh.rows + 30, 100), 1, MAXR);
    const cw = new Float32Array(nc).fill(sh.default_col_w || 64);
    const rh = new Float32Array(nr).fill(sh.default_row_h || 20);
    for (const [c, w] of sh.col_w) if (c < nc) cw[c] = clamp(w, 0, 2000);
    for (const [r, h] of sh.row_h) if (r < nr) rh[r] = clamp(h, 0, 2000);
    for (const [a, b] of sh.hidden_cols) for (let c = a; c <= b && c < nc; c++) cw[c] = 0;
    for (const [a, b] of sh.hidden_rows) for (let r = a; r <= b && r < nr; r++) rh[r] = 0;
    const colX = new Float64Array(nc + 1);
    for (let c = 0; c < nc; c++) colX[c + 1] = colX[c] + cw[c];
    const rowY = new Float64Array(nr + 1);
    for (let r = 0; r < nr; r++) rowY[r + 1] = rowY[r] + rh[r];
    const fr = clamp(sh.freeze[0], 0, Math.min(nr, 200));
    const fc = clamp(sh.freeze[1], 0, Math.min(nc, 50));
    // Merged areas: by anchor, and a list for drawing.
    const merges = sh.merges.filter((m) => m[0] < nr && m[1] < nc);
    const anchor = new Map();
    for (const m of merges) anchor.set(m[0] * KEYC + m[1], m);
    L = { rev: S.rev, nc, nr, colX, rowY, fr, fc, merges, anchor, sh };
    S.layouts.set(i, L);
    return L;
  }

  /** Largest index i with off[i] <= u (off has n + 1 entries). */
  function indexAt(off, n, u) {
    let lo = 0;
    let hi = n - 1;
    if (u <= 0) return 0;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (off[mid] <= u) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  }

  // ---------------------------------------------------------------------------
  // Geometry: header sizes, the scroll origin, panes
  // ---------------------------------------------------------------------------

  const HH = 26; // column header height
  const geo = { hw: 40, vw: 0, vh: 0, ox: 0, oy: 0, kx: 1, ky: 1, sw: 0, sh: 0, cw: 0, ch: 0 };

  function rowHeaderWidth(L) {
    return Math.max(34, 14 + String(L.sh.rows + 30).length * 8);
  }

  /** Sizes the spacer and returns where scrolling is now (ox, oy in unzoomed px past the frozen part). */
  function measure() {
    const L = layoutOf(S.cur);
    const stage = $('stage');
    geo.vw = stage.clientWidth;
    geo.vh = stage.clientHeight;
    geo.hw = rowHeaderWidth(L);
    const z = S.z;
    const fw = L.colX[L.fc] * z;
    const fh = L.rowY[L.fr] * z;
    // Scrollable content past the frozen panes, in CSS px.
    geo.cw = (L.colX[L.nc] - L.colX[L.fc]) * z;
    geo.ch = (L.rowY[L.nr] - L.rowY[L.fr]) * z;
    const viewW = Math.max(1, geo.vw - geo.hw - fw);
    const viewH = Math.max(1, geo.vh - HH - fh);
    // The spacer is the content plus the part of the viewport covered by headers and frozen panes
    // (the scroller itself covers the whole stage).
    geo.sw = Math.min(geo.cw + geo.hw + fw, SPACER_CAP);
    geo.sh = Math.min(geo.ch + HH + fh, SPACER_CAP);
    geo.viewW = viewW;
    geo.viewH = viewH;
    geo.fw = fw;
    geo.fh = fh;
    const sp = $('spacer');
    sp.style.width = `${Math.round(geo.sw)}px`;
    sp.style.height = `${Math.round(geo.sh)}px`;
    return L;
  }

  /** Scroll offsets in unzoomed px from the scroller's position (proportional when the spacer is capped). */
  function readScroll() {
    const sc = $('scroller');
    const maxX = Math.max(0, geo.sw - geo.vw);
    const maxY = Math.max(0, geo.sh - geo.vh);
    const spanX = Math.max(0, geo.cw - geo.viewW);
    const spanY = Math.max(0, geo.ch - geo.viewH);
    const fx = maxX > 0 ? clamp(sc.scrollLeft / maxX, 0, 1) : 0;
    const fy = maxY > 0 ? clamp(sc.scrollTop / maxY, 0, 1) : 0;
    geo.ox = (fx * spanX) / S.z;
    geo.oy = (fy * spanY) / S.z;
  }

  function writeScroll(ox, oy) {
    const sc = $('scroller');
    const maxX = Math.max(0, geo.sw - geo.vw);
    const maxY = Math.max(0, geo.sh - geo.vh);
    const spanX = Math.max(0, geo.cw - geo.viewW);
    const spanY = Math.max(0, geo.ch - geo.viewH);
    sc.scrollLeft = spanX > 0 ? clamp((ox * S.z) / spanX, 0, 1) * maxX : 0;
    sc.scrollTop = spanY > 0 ? clamp((oy * S.z) / spanY, 0, 1) * maxY : 0;
  }

  // ---------------------------------------------------------------------------
  // Switching sheets, refreshing
  // ---------------------------------------------------------------------------

  function switchSheet(i, first) {
    if (!S.meta.sheets[i]) return;
    S.cur = i;
    S.sel = { r: 0, c: 0 };
    for (const b of $('tabs').children) b.setAttribute('aria-selected', 'false');
    const visible = S.meta.sheets.map((s, k) => [s, k]).filter(([s]) => !s.hidden);
    const pos = visible.findIndex(([, k]) => k === i);
    if (pos >= 0 && $('tabs').children[pos]) {
      $('tabs').children[pos].setAttribute('aria-selected', 'true');
      if (!first) $('tabs').children[pos].scrollIntoView({ inline: 'nearest', block: 'nearest' });
    }
    updateCsvLink();
    dropData();
    const sc = $('scroller');
    sc.scrollLeft = 0;
    sc.scrollTop = 0;
    measure();
    readScroll();
    renderBar();
    requestDraw();
    ensureBlocks();
    sc.focus({ preventScroll: true });
  }

  function dropData() {
    S.cells.clear();
    S.blocks.clear();
    S.cellInfo.clear();
  }

  /** The workbook changed on the server: reload its description and refetch what is in view. */
  async function reload() {
    let meta;
    try {
      meta = await get(itemPath('/sheet'));
    } catch (e) {
      if (e.status === 401) return showSignIn();
      return;
    }
    if (meta.rev === S.rev) return;
    const ox = geo.ox;
    const oy = geo.oy;
    S.meta = meta;
    S.rev = meta.rev;
    S.layouts.clear();
    dropData();
    if (!meta.sheets[S.cur]) S.cur = 0;
    buildTabs();
    measure();
    writeScroll(ox, oy);
    readScroll();
    renderBar();
    requestDraw();
    ensureBlocks();
    setStatus();
  }

  let poll = 0;
  function startPolling() {
    clearInterval(poll);
    poll = setInterval(() => {
      if (document.visibilityState === 'visible' && !$('app').hidden) reload();
    }, 10000);
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible' && !$('app').hidden) reload();
    });
  }

  // ---------------------------------------------------------------------------
  // Fetching cells in blocks
  // ---------------------------------------------------------------------------

  function visibleRanges() {
    const L = layoutOf(S.cur);
    const z = S.z;
    const c0 = indexAt(L.colX, L.nc, L.colX[L.fc] + geo.ox);
    const c1 = indexAt(L.colX, L.nc, L.colX[L.fc] + geo.ox + geo.viewW / z);
    const r0 = indexAt(L.rowY, L.nr, L.rowY[L.fr] + geo.oy);
    const r1 = indexAt(L.rowY, L.nr, L.rowY[L.fr] + geo.oy + geo.viewH / z);
    return { L, c0, c1, r0, r1 };
  }

  function ensureBlocks() {
    if (!S.meta) return;
    const { L, c0, c1, r0, r1 } = visibleRanges();
    const rowSets = [[Math.max(0, r0 - 12), Math.min(L.nr - 1, r1 + 12)]];
    const colSets = [[Math.max(0, c0 - 3), Math.min(L.nc - 1, c1 + 3)]];
    if (L.fr > 0) rowSets.push([0, L.fr - 1]);
    if (L.fc > 0) colSets.push([0, L.fc - 1]);
    const want = new Set();
    for (const [ra, rb] of rowSets) {
      for (const [ca, cb] of colSets) {
        for (let br = Math.floor(ra / BR); br <= Math.floor(rb / BR); br++) {
          for (let bc = Math.floor(ca / BC); bc <= Math.floor(cb / BC); bc++) want.add(`${S.cur}:${br}:${bc}`);
        }
      }
    }
    const todo = [];
    for (const k of want) {
      const b = S.blocks.get(k);
      if (b) b.used = ++S.fetchSeq;
      else todo.push(k);
    }
    for (const k of todo) fetchBlock(k);
    // Keep memory bounded: drop the least recently used blocks that are not in view.
    if (S.blocks.size > 90) {
      const old = [...S.blocks.entries()].filter(([k, b]) => !want.has(k) && !b.loading).sort((a, b) => a[1].used - b[1].used);
      for (const [k, b] of old.slice(0, S.blocks.size - 70)) {
        for (const key of b.keys) S.cells.delete(key);
        S.blocks.delete(k);
      }
    }
    $('busy').hidden = ![...S.blocks.values()].some((b) => b.loading);
  }

  const queue = [];

  function fetchBlock(k) {
    S.blocks.set(k, { keys: [], used: ++S.fetchSeq, loading: true });
    queue.push(k);
    pump();
  }

  function pump() {
    while (S.inflight < 4 && queue.length) {
      const k = queue.shift();
      const [sheet, br, bc] = k.split(':').map(Number);
      const rev = S.rev;
      if (sheet !== S.cur) {
        S.blocks.delete(k);
        continue;
      }
      const L = layoutOf(sheet);
      const r1 = br * BR;
      const r2 = Math.min(L.nr - 1, r1 + BR - 1);
      const c1 = bc * BC;
      const c2 = Math.min(L.nc - 1, c1 + BC - 1);
      S.inflight++;
      get(`${itemPath('/sheet/cells')}?sheet=${sheet}&r1=${r1}&r2=${r2}&c1=${c1}&c2=${c2}`)
        .then((w) => {
          if (w.rev !== S.rev || rev !== S.rev) {
            S.blocks.delete(k);
            reload();
            return;
          }
          const b = S.blocks.get(k);
          if (!b) return;
          absorb(b, w);
          b.loading = false;
          requestDraw();
        })
        .catch((e) => {
          S.blocks.delete(k);
          if (e.status === 401) showSignIn();
          else if (e.status === 404) problem("This workbook doesn't exist any more, or you no longer have access to it.", { title: 'Not found' });
          else if (e.code === 'timeout' || e.status >= 500) setStatus('Server problem; pull down to retry');
        })
        .finally(() => {
          S.inflight--;
          $('busy').hidden = ![...S.blocks.values()].some((b) => b.loading) && queue.length === 0;
          pump();
        });
    }
  }

  function absorb(b, w) {
    const styles = w.styles.map(prepStyle);
    for (const row of w.cells) {
      const [r, c, t, k, si, link] = row;
      const key = r * KEYC + c;
      S.cells.set(key, { t: stripBidi(t), k, st: si ? styles[si - 1] : null, l: !!link });
      b.keys.push(key);
    }
  }

  function prepStyle(s) {
    const size = (s.z || 11) * (96 / 72);
    return { b: !!s.b, i: !!s.i, u: !!s.u, x: !!s.x, c: s.c || null, f: s.f || null, h: s.h || '', v: s.v || 'b', w: !!s.w, size, n: s.n || 0, d: s.d || null };
  }

  // ---------------------------------------------------------------------------
  // Drawing
  // ---------------------------------------------------------------------------

  const PAL = {
    light: { bg: '#ffffff', grid: '#d4d8d6', hbg: '#f1f3f2', hfg: '#505a55', hline: '#c4cac7', fg: '#1d2320', sel: '#217346', selFill: 'rgba(33,115,70,.10)', hsel: '#d7eadd' },
    dark: { bg: '#1a1e1c', grid: '#2c3330', hbg: '#232a26', hfg: '#a4b0aa', hline: '#39433e', fg: '#e4e9e6', sel: '#4caf7a', selFill: 'rgba(76,175,122,.14)', hsel: '#2d4a3a' },
  };

  function lum(hex) {
    const n = parseInt(hex.slice(1), 16);
    return (0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255)) / 255;
  }

  function requestDraw() {
    if (S.drawQueued) return;
    S.drawQueued = true;
    requestAnimationFrame(() => {
      S.drawQueued = false;
      draw();
    });
  }

  function sizeCanvas() {
    const cv = $('cv');
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(geo.vw * dpr));
    const h = Math.max(1, Math.round(geo.vh * dpr));
    if (cv.width !== w || cv.height !== h) {
      cv.width = w;
      cv.height = h;
    }
    return dpr;
  }

  function draw() {
    if (!S.meta || $('app').hidden) return;
    const L = layoutOf(S.cur);
    geo.vw = $('stage').clientWidth;
    geo.vh = $('stage').clientHeight;
    if (!geo.vw || !geo.vh) return;
    const dpr = sizeCanvas();
    const ctx = $('cv').getContext('2d');
    const P = S.dark ? PAL.dark : PAL.light;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = P.bg;
    ctx.fillRect(0, 0, geo.vw, geo.vh);
    const z = S.z;
    const { c0, c1, r0, r1 } = visibleRanges();
    // Four panes: frozen corner, frozen rows, frozen columns, the scrolling body.
    const bx = geo.hw;
    const by = HH;
    const sx = bx + geo.fw;
    const sy = by + geo.fh;
    const ux = L.colX[L.fc] + geo.ox;
    const uy = L.rowY[L.fr] + geo.oy;
    const panes = [];
    panes.push({ x: sx, y: sy, w: geo.vw - sx, h: geo.vh - sy, ca: c0, cb: c1, ra: r0, rb: r1, ux, uy, ox: sx, oy: sy });
    if (L.fc > 0) panes.push({ x: bx, y: sy, w: geo.fw, h: geo.vh - sy, ca: 0, cb: L.fc - 1, ra: r0, rb: r1, ux: 0, uy, ox: bx, oy: sy });
    if (L.fr > 0) panes.push({ x: sx, y: by, w: geo.vw - sx, h: geo.fh, ca: c0, cb: c1, ra: 0, rb: L.fr - 1, ux, uy: 0, ox: sx, oy: by });
    if (L.fr > 0 && L.fc > 0) panes.push({ x: bx, y: by, w: geo.fw, h: geo.fh, ca: 0, cb: L.fc - 1, ra: 0, rb: L.fr - 1, ux: 0, uy: 0, ox: bx, oy: by });
    for (const p of panes) {
      if (p.w <= 0 || p.h <= 0) continue;
      ctx.save();
      ctx.beginPath();
      ctx.rect(p.x, p.y, p.w, p.h);
      ctx.clip();
      drawPane(ctx, L, p, P, z);
      ctx.restore();
    }
    drawHeaders(ctx, L, panes, P, z);
    if (L.fr > 0 || L.fc > 0) {
      ctx.strokeStyle = P.hline;
      ctx.lineWidth = 2;
      ctx.beginPath();
      if (L.fc > 0) {
        ctx.moveTo(sx, 0);
        ctx.lineTo(sx, geo.vh);
      }
      if (L.fr > 0) {
        ctx.moveTo(0, sy);
        ctx.lineTo(geo.vw, sy);
      }
      ctx.stroke();
    }
  }

  const px = (p, L, c, z) => p.ox + (L.colX[c] - p.ux) * z;
  const py = (p, L, r, z) => p.oy + (L.rowY[r] - p.uy) * z;

  function drawPane(ctx, L, p, P, z) {
    const gridOn = L.sh.gridlines !== false;
    // Grid lines.
    if (gridOn) {
      ctx.strokeStyle = P.grid;
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (let c = p.ca; c <= p.cb + 1 && c <= L.nc; c++) {
        const x = Math.round(px(p, L, c, z)) + 0.5;
        ctx.moveTo(x, p.y);
        ctx.lineTo(x, p.y + p.h);
      }
      for (let r = p.ra; r <= p.rb + 1 && r <= L.nr; r++) {
        const y = Math.round(py(p, L, r, z)) + 0.5;
        ctx.moveTo(p.x, y);
        ctx.lineTo(p.x + p.w, y);
      }
      ctx.stroke();
    }
    // Merged areas intersecting the pane (anchor outside the pane included).
    const mergedHere = [];
    const covered = new Set();
    for (const m of L.merges) {
      if (m[2] < p.ra || m[0] > p.rb || m[3] < p.ca || m[1] > p.cb) continue;
      mergedHere.push(m);
      for (let r = Math.max(m[0], p.ra); r <= Math.min(m[2], p.rb); r++) {
        for (let c = Math.max(m[1], p.ca); c <= Math.min(m[3], p.cb); c++) if (r !== m[0] || c !== m[1]) covered.add(r * KEYC + c);
      }
    }
    // Fills.
    for (let r = p.ra; r <= p.rb; r++) {
      const h = L.rowY[r + 1] - L.rowY[r];
      if (h <= 0) continue;
      for (let c = p.ca; c <= p.cb; c++) {
        const w = L.colX[c + 1] - L.colX[c];
        if (w <= 0) continue;
        const key = r * KEYC + c;
        if (covered.has(key)) {
          continue;
        }
        const cell = S.cells.get(key);
        if (cell && cell.st && cell.st.f && !L.anchor.has(key)) {
          ctx.fillStyle = cell.st.f;
          ctx.fillRect(px(p, L, c, z), py(p, L, r, z), w * z + 1, h * z + 1);
        }
      }
    }
    for (const m of mergedHere) {
      const a = S.cells.get(m[0] * KEYC + m[1]);
      const x = px(p, L, m[1], z);
      const y = py(p, L, m[0], z);
      const w = (L.colX[m[3] + 1] - L.colX[m[1]]) * z;
      const h = (L.rowY[m[2] + 1] - L.rowY[m[0]]) * z;
      if (a && a.st && a.st.f) {
        ctx.fillStyle = a.st.f;
        ctx.fillRect(x, y, w + 1, h + 1);
      } else if (gridOn) {
        ctx.fillStyle = P.bg;
        ctx.fillRect(x + 1, y + 1, w - 1, h - 1);
      }
    }
    // Text and borders.
    for (let r = p.ra; r <= p.rb; r++) {
      const h = L.rowY[r + 1] - L.rowY[r];
      if (h <= 0) continue;
      for (let c = p.ca; c <= p.cb; c++) {
        const w = L.colX[c + 1] - L.colX[c];
        const key = r * KEYC + c;
        if (w <= 0 || covered.has(key)) continue;
        const cell = S.cells.get(key);
        if (!cell) continue;
        const m = L.anchor.get(key);
        const cw = m ? (L.colX[m[3] + 1] - L.colX[m[1]]) * z : w * z;
        const ch = m ? (L.rowY[m[2] + 1] - L.rowY[m[0]]) * z : h * z;
        drawCell(ctx, L, p, cell, r, c, px(p, L, c, z), py(p, L, r, z), cw, ch, P, z);
      }
    }
    // Merged areas whose anchor lies outside the pane, so the loop above didn't reach them.
    for (const m of mergedHere) {
      if (m[0] >= p.ra && m[0] <= p.rb && m[1] >= p.ca && m[1] <= p.cb) continue;
      const cell = S.cells.get(m[0] * KEYC + m[1]);
      if (!cell) continue;
      const cw = (L.colX[m[3] + 1] - L.colX[m[1]]) * z;
      const ch = (L.rowY[m[2] + 1] - L.rowY[m[0]]) * z;
      drawCell(ctx, L, p, cell, m[0], m[1], px(p, L, m[1], z), py(p, L, m[0], z), cw, ch, P, z);
    }
    // Selection.
    const sel = S.sel;
    const m = L.merges.find((mm) => sel.r >= mm[0] && sel.r <= mm[2] && sel.c >= mm[1] && sel.c <= mm[3]);
    const [sr1, sc1, sr2, sc2] = m || [sel.r, sel.c, sel.r, sel.c];
    if (sr2 >= p.ra && sr1 <= p.rb && sc2 >= p.ca && sc1 <= p.cb) {
      const x = px(p, L, sc1, z);
      const y = py(p, L, sr1, z);
      const w = (L.colX[sc2 + 1] - L.colX[sc1]) * z;
      const h = (L.rowY[sr2 + 1] - L.rowY[sr1]) * z;
      ctx.fillStyle = P.selFill;
      ctx.fillRect(x, y, w, h);
      ctx.strokeStyle = P.sel;
      ctx.lineWidth = 2;
      ctx.strokeRect(x + 1, y + 1, Math.max(0, w - 2), Math.max(0, h - 2));
    }
  }

  const BORDER_W = [0, 1, 2, 3, 3, 1, 1];

  function line(ctx, x1, y1, x2, y2, code, color) {
    if (!code) return;
    ctx.strokeStyle = color;
    ctx.lineWidth = BORDER_W[code] || 1;
    ctx.setLineDash(code === 5 ? [4, 3] : code === 6 ? [1.5, 2] : []);
    ctx.beginPath();
    ctx.moveTo(x1, y1);
    ctx.lineTo(x2, y2);
    ctx.stroke();
    ctx.setLineDash([]);
  }

  function textColor(st, P) {
    const dark = S.dark;
    if (st) {
      if (st.c && st.f) return st.c;
      if (st.c) {
        const l = lum(st.c);
        if (dark && l < 0.35) return P.fg;
        if (!dark && l > 0.8) return P.fg;
        return st.c;
      }
      if (st.f) return lum(st.f) < 0.5 ? '#ffffff' : '#1d2320';
    }
    return P.fg;
  }

  function drawCell(ctx, L, p, cell, r, c, x, y, w, h, P, z) {
    const st = cell.st;
    // Borders first (they sit on the cell edge).
    if (st && st.d) {
      const bc = S.dark ? '#c9d1cd' : '#222';
      line(ctx, x + 0.5, y, x + 0.5, y + h, st.d[0], bc);
      line(ctx, x + w - 0.5, y, x + w - 0.5, y + h, st.d[1], bc);
      line(ctx, x, y + 0.5, x + w, y + 0.5, st.d[2], bc);
      line(ctx, x, y + h - 0.5, x + w, y + h - 0.5, st.d[3], bc);
    }
    const text = cell.t;
    if (!text) return;
    const size = (st ? st.size : 14.67) * z;
    if (size * 1.2 > h + 4 && h < size * 0.9) return;
    ctx.font = `${st && st.i ? 'italic ' : ''}${st && st.b ? 'bold ' : ''}${size}px system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`;
    ctx.fillStyle = textColor(st, P);
    const pad = 3 * z + (st ? st.n * 9 * z : 0);
    let align = st && st.h;
    if (!align) align = cell.k === 'n' ? 'r' : cell.k === 'b' || cell.k === 'e' ? 'c' : 'l';
    const wrap = st && st.w && cell.k === 's';
    ctx.textBaseline = 'alphabetic';
    if (wrap) {
      drawWrapped(ctx, text, x, y, w, h, pad, size, align, st, P);
      return;
    }
    let tw = text.length * size * 0.45 > w ? ctx.measureText(text).width : -1;
    let shown = text;
    if (tw < 0) tw = ctx.measureText(text).width;
    const avail = w - 2 * pad;
    let ax = x;
    let aw = w;
    if (tw > avail) {
      if (cell.k === 'n') {
        // A number that doesn't fit is drawn a little smaller (down to 70%), else as hashes.
        const f = avail / tw;
        if (f >= 0.7) {
          ctx.font = ctx.font.replace(/([\d.]+)px/, (m, n) => `${(Number(n) * f).toFixed(2)}px`);
          tw = ctx.measureText(text).width;
        } else {
          shown = '#'.repeat(Math.max(1, Math.floor(avail / Math.max(1, ctx.measureText('#').width))));
          tw = ctx.measureText(shown).width;
        }
      } else if (align === 'l') {
        aw = w + spareRight(L, r, c, z, tw - avail);
      } else if (align === 'r') {
        const extra = spareLeft(L, r, c, z, tw - avail);
        ax = x - extra;
        aw = w + extra;
      } else {
        aw = w;
      }
    }
    const baseline = vpos(st, y, h, size);
    ctx.save();
    ctx.beginPath();
    ctx.rect(ax, y, aw, h);
    ctx.clip();
    let tx = ax + pad;
    if (align === 'r') tx = ax + aw - pad - tw;
    else if (align === 'c') tx = ax + (aw - tw) / 2;
    ctx.fillText(shown, tx, baseline);
    if (st && (st.u || cell.l)) ctx.fillRect(tx, baseline + 1.5, tw, Math.max(1, size / 14));
    if (st && st.x) ctx.fillRect(tx, baseline - size * 0.3, tw, Math.max(1, size / 14));
    ctx.restore();
  }

  function vpos(st, y, h, size) {
    const v = st ? st.v : 'b';
    if (v === 't') return y + size * 1.0 + 2;
    if (v === 'c') return y + h / 2 + size * 0.35;
    return y + h - Math.max(3, size * 0.25);
  }

  /** Width (CSS px) of the empty cells to the right of (r, c), up to `need`. */
  function spareRight(L, r, c, z, need) {
    let got = 0;
    for (let k = c + 1; k < L.nc && k < c + 30 && got < need; k++) {
      if (S.cells.has(r * KEYC + k) && S.cells.get(r * KEYC + k).t) break;
      got += (L.colX[k + 1] - L.colX[k]) * z;
    }
    return Math.min(got, need + 4);
  }

  function spareLeft(L, r, c, z, need) {
    let got = 0;
    for (let k = c - 1; k >= 0 && k > c - 30 && got < need; k--) {
      const o = S.cells.get(r * KEYC + k);
      if (o && o.t) break;
      got += (L.colX[k + 1] - L.colX[k]) * z;
    }
    return Math.min(got, need + 4);
  }

  function drawWrapped(ctx, text, x, y, w, h, pad, size, align, st, P) {
    const maxW = Math.max(10, w - 2 * pad);
    const lines = [];
    for (const para of text.split(/\r?\n/)) {
      let cur = '';
      for (const word of para.split(/(\s+)/)) {
        const t = cur + word;
        if (cur && ctx.measureText(t).width > maxW) {
          lines.push(cur.trimEnd());
          cur = word.trimStart();
        } else {
          cur = t;
        }
        if (lines.length > 60) break;
      }
      lines.push(cur);
    }
    const lh = size * 1.2;
    const total = lines.length * lh;
    let top = y + 2;
    if (st && st.v === 'c') top = y + (h - total) / 2;
    else if (!st || st.v === 'b') top = y + h - total - 2;
    if (total > h) top = y + 2; // more text than room: show the start
    ctx.save();
    ctx.beginPath();
    ctx.rect(x, y, w, h);
    ctx.clip();
    lines.forEach((ln, i) => {
      const tw = ctx.measureText(ln).width;
      let tx = x + pad;
      if (align === 'r') tx = x + w - pad - tw;
      else if (align === 'c') tx = x + (w - tw) / 2;
      ctx.fillText(ln, tx, top + i * lh + size);
    });
    ctx.restore();
  }

  function drawHeaders(ctx, L, panes, P, z) {
    const hs = Math.round(13 * Math.min(1.3, Math.max(0.85, z)));
    ctx.font = `600 ${hs}px system-ui, -apple-system, sans-serif`;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'center';
    const selC = S.sel.c;
    const selR = S.sel.r;
    // Column header strip.
    ctx.fillStyle = P.hbg;
    ctx.fillRect(0, 0, geo.vw, HH);
    ctx.fillRect(0, 0, geo.hw, geo.vh);
    ctx.strokeStyle = P.hline;
    ctx.lineWidth = 1;
    for (const p of panes) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(p.x, 0, p.w, HH);
      ctx.clip();
      for (let c = p.ca; c <= p.cb; c++) {
        const w = (L.colX[c + 1] - L.colX[c]) * z;
        if (w <= 0) continue;
        const x = px(p, L, c, z);
        if (c === selC) {
          ctx.fillStyle = P.hsel;
          ctx.fillRect(x, 0, w, HH);
        }
        ctx.fillStyle = P.hfg;
        if (w > 12) ctx.fillText(colName(c), x + w / 2, HH / 2 + 1);
        ctx.beginPath();
        ctx.moveTo(Math.round(x + w) + 0.5, 0);
        ctx.lineTo(Math.round(x + w) + 0.5, HH);
        ctx.stroke();
      }
      ctx.restore();
      ctx.save();
      ctx.beginPath();
      ctx.rect(0, p.y, geo.hw, p.h);
      ctx.clip();
      for (let r = p.ra; r <= p.rb; r++) {
        const h = (L.rowY[r + 1] - L.rowY[r]) * z;
        if (h <= 0) continue;
        const y = py(p, L, r, z);
        if (r === selR) {
          ctx.fillStyle = P.hsel;
          ctx.fillRect(0, y, geo.hw, h);
        }
        ctx.fillStyle = P.hfg;
        if (h > 9) ctx.fillText(String(r + 1), geo.hw / 2, y + h / 2 + 1);
        ctx.beginPath();
        ctx.moveTo(0, Math.round(y + h) + 0.5);
        ctx.lineTo(geo.hw, Math.round(y + h) + 0.5);
        ctx.stroke();
      }
      ctx.restore();
    }
    ctx.beginPath();
    ctx.moveTo(geo.hw + 0.5, 0);
    ctx.lineTo(geo.hw + 0.5, geo.vh);
    ctx.moveTo(0, HH + 0.5);
    ctx.lineTo(geo.vw, HH + 0.5);
    ctx.stroke();
    ctx.textAlign = 'start';
  }

  // ---------------------------------------------------------------------------
  // Selecting a cell, the formula bar
  // ---------------------------------------------------------------------------

  function hit(x, y) {
    const L = layoutOf(S.cur);
    if (x < geo.hw || y < HH) return null;
    const z = S.z;
    const sx = geo.hw + geo.fw;
    const sy = HH + geo.fh;
    let u;
    let v;
    if (x < sx && L.fc > 0) u = (x - geo.hw) / z;
    else u = L.colX[L.fc] + geo.ox + (x - sx) / z;
    if (y < sy && L.fr > 0) v = (y - HH) / z;
    else v = L.rowY[L.fr] + geo.oy + (y - sy) / z;
    const c = indexAt(L.colX, L.nc, u);
    const r = indexAt(L.rowY, L.nr, v);
    if (u >= L.colX[L.nc] || v >= L.rowY[L.nr]) return null;
    return { r, c };
  }

  function select(r, c, scroll) {
    const L = layoutOf(S.cur);
    const m = L.merges.find((mm) => r >= mm[0] && r <= mm[2] && c >= mm[1] && c <= mm[3]);
    S.sel = m ? { r: m[0], c: m[1] } : { r, c };
    if (scroll) scrollIntoView(S.sel.r, S.sel.c);
    renderBar();
    requestDraw();
  }

  function scrollIntoView(r, c) {
    const L = layoutOf(S.cur);
    const z = S.z;
    let { ox, oy } = geo;
    if (c >= L.fc) {
      const left = L.colX[c] - L.colX[L.fc];
      const right = L.colX[c + 1] - L.colX[L.fc];
      if (left < ox) ox = left;
      else if (right > ox + geo.viewW / z) ox = right - geo.viewW / z;
    }
    if (r >= L.fr) {
      const top = L.rowY[r] - L.rowY[L.fr];
      const bottom = L.rowY[r + 1] - L.rowY[L.fr];
      if (top < oy) oy = top;
      else if (bottom > oy + geo.viewH / z) oy = bottom - geo.viewH / z;
    }
    writeScroll(ox, oy);
    readScroll();
    ensureBlocks();
  }

  let barSeq = 0;

  function renderBar() {
    const { r, c } = S.sel;
    $('addr').textContent = cellName(r, c);
    const fx = $('fx');
    const mine = ++barSeq;
    const cached = S.cell(r, c);
    fx.textContent = cached ? markBidi(cached.input) : '';
    if (!cached) {
      // The shown text is a good first answer while the exact input arrives.
      const quick = S.cells.get(r * KEYC + c);
      fx.textContent = quick ? markBidi(quick.t) : '';
      get(`${itemPath('/sheet/cell')}?sheet=${S.cur}&r=${r}&c=${c}`)
        .then((info) => {
          if (info.rev !== S.rev) return;
          S.cellInfo.set(`${S.rev}:${S.cur}:${r}:${c}`, info);
          if (mine === barSeq) fx.textContent = markBidi(info.hidden_formula ? '(formula hidden)' : info.input || info.text);
        })
        .catch(() => {});
    }
  }

  S.cell = (r, c) => S.cellInfo.get(`${S.rev}:${S.cur}:${r}:${c}`);

  // ---------------------------------------------------------------------------
  // Zoom
  // ---------------------------------------------------------------------------

  function setZoom(z, anchorX, anchorY) {
    const nz = clamp(z, 0.5, 3);
    if (Math.abs(nz - S.z) < 0.001) return;
    const L = layoutOf(S.cur);
    // Keep the unit under the anchor where it is.
    const ax = anchorX === undefined ? geo.hw + geo.fw + geo.viewW / 2 : anchorX;
    const ay = anchorY === undefined ? HH + geo.fh + geo.viewH / 2 : anchorY;
    const uBefore = L.colX[L.fc] + geo.ox + (ax - geo.hw - geo.fw) / S.z;
    const vBefore = L.rowY[L.fr] + geo.oy + (ay - HH - geo.fh) / S.z;
    S.z = nz;
    measure();
    writeScroll(uBefore - L.colX[L.fc] - (ax - geo.hw - geo.fw) / nz, vBefore - L.rowY[L.fr] - (ay - HH - geo.fh) / nz);
    readScroll();
    requestDraw();
    ensureBlocks();
  }

  // ---------------------------------------------------------------------------
  // Events
  // ---------------------------------------------------------------------------

  function wire() {
    const sc = $('scroller');
    sc.addEventListener(
      'scroll',
      () => {
        readScroll();
        requestDraw();
        ensureBlocksSoon();
      },
      { passive: true },
    );
    sc.addEventListener('click', (e) => {
      const rect = sc.getBoundingClientRect();
      const h = hit(e.clientX - rect.left, e.clientY - rect.top);
      if (h) select(h.r, h.c, false);
      sc.focus({ preventScroll: true });
    });
    sc.addEventListener('keydown', onKey);
    // Pinch to change the text size.
    let pinch = null;
    sc.addEventListener(
      'touchstart',
      (e) => {
        if (e.touches.length === 2) {
          pinch = { d: dist(e.touches), z: S.z };
        }
      },
      { passive: true },
    );
    sc.addEventListener(
      'touchmove',
      (e) => {
        if (pinch && e.touches.length === 2) {
          e.preventDefault();
          const rect = sc.getBoundingClientRect();
          const cx = (e.touches[0].clientX + e.touches[1].clientX) / 2 - rect.left;
          const cy = (e.touches[0].clientY + e.touches[1].clientY) / 2 - rect.top;
          setZoom(pinch.z * (dist(e.touches) / pinch.d), cx, cy);
        }
      },
      { passive: false },
    );
    const end = (e) => {
      if (e.touches.length < 2) pinch = null;
    };
    sc.addEventListener('touchend', end, { passive: true });
    sc.addEventListener('touchcancel', end, { passive: true });
    // Safari's own pinch gesture events (trackpads, older iOS).
    document.addEventListener('gesturestart', (e) => e.preventDefault());
    sc.addEventListener(
      'wheel',
      (e) => {
        if (e.ctrlKey) {
          e.preventDefault();
          setZoom(S.z * (e.deltaY < 0 ? 1.08 : 1 / 1.08));
        }
      },
      { passive: false },
    );
    $('zoom-in').addEventListener('click', () => setZoom(S.z * 1.15));
    $('zoom-out').addEventListener('click', () => setZoom(S.z / 1.15));
    $('fx').addEventListener('click', () => $('fx').classList.toggle('one'));
    $('more').addEventListener('click', () => toggleMenu());
    $('m-refresh').addEventListener('click', () => {
      toggleMenu(false);
      reload();
    });
    document.addEventListener('click', (e) => {
      if (!$('menu').hidden && !$('menu').contains(e.target) && e.target !== $('more')) toggleMenu(false);
    });
    for (const a of ['m-xlsx', 'm-csv', 'm-native']) $(a).addEventListener('click', () => toggleMenu(false));
    window.addEventListener('resize', () => {
      if ($('app').hidden) return;
      measure();
      readScroll();
      requestDraw();
      ensureBlocks();
    });
    window.addEventListener('orientationchange', () => setTimeout(() => window.dispatchEvent(new Event('resize')), 250));
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    S.dark = mq.matches;
    mq.addEventListener('change', (e) => {
      S.dark = e.matches;
      requestDraw();
    });
    $('signin-form').addEventListener('submit', onSignIn);
    $('problem-retry').addEventListener('click', () => open());
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') toggleMenu(false);
    });
  }

  const dist = (t) => Math.hypot(t[0].clientX - t[1].clientX, t[0].clientY - t[1].clientY);

  function toggleMenu(on) {
    const m = $('menu');
    const open = on === undefined ? m.hidden : on;
    m.hidden = !open;
    $('more').setAttribute('aria-expanded', String(open));
  }

  let soon = 0;
  function ensureBlocksSoon() {
    if (soon) return;
    soon = setTimeout(() => {
      soon = 0;
      ensureBlocks();
    }, 60);
  }

  function onKey(e) {
    const L = layoutOf(S.cur);
    let { r, c } = S.sel;
    const page = Math.max(1, Math.floor(geo.viewH / S.z / 20) - 1);
    switch (e.key) {
      case 'ArrowDown':
        r = nextVisible(L.rowY, r, 1, L.nr);
        break;
      case 'ArrowUp':
        r = nextVisible(L.rowY, r, -1, L.nr);
        break;
      case 'ArrowRight':
        c = nextVisible(L.colX, c, 1, L.nc);
        break;
      case 'ArrowLeft':
        c = nextVisible(L.colX, c, -1, L.nc);
        break;
      case 'PageDown':
        r = Math.min(L.nr - 1, r + page);
        break;
      case 'PageUp':
        r = Math.max(0, r - page);
        break;
      case 'Home':
        c = 0;
        if (e.ctrlKey) r = 0;
        break;
      default:
        return;
    }
    e.preventDefault();
    select(r, c, true);
  }

  function nextVisible(off, i, d, n) {
    let k = i + d;
    while (k >= 0 && k < n && off[k + 1] - off[k] <= 0) k += d;
    return k < 0 || k >= n ? i : k;
  }

  // Test hook: lets the browser check read the drawn state without touching the canvas.
  window.__fsheet = {
    state: () => ({ sel: S.sel, z: S.z, cur: S.cur, rev: S.rev, cells: S.cells.size, blocks: S.blocks.size, ox: geo.ox, oy: geo.oy, sheets: S.meta ? S.meta.sheets.map((s) => s.name) : [] }),
    cellText: (r, c) => (S.cells.get(r * KEYC + c) || {}).t,
    select: (r, c) => select(r, c, true),
    zoom: (z) => setZoom(z),
    switchSheet: (i) => switchSheet(i),
    scrollTo: (ox, oy) => {
      writeScroll(ox, oy);
      readScroll();
      requestDraw();
      ensureBlocks();
    },
  };

  wire();
  boot().catch((e) => problem(e && e.message ? e.message : 'Something went wrong.', { retry: true }));
})();
