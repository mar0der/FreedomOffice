// FreedomOffice Drive: the web app at /drive. Plain JavaScript, no framework, no build step.
//
// Security rules this file keeps (drive/src/lib.rs checks some of them):
// - every name and message from the server is rendered as text (textContent / text nodes),
//   never parsed as HTML; no inline scripts or styles (the CSP forbids them anyway);
// - the session cookie is HttpOnly; the CSRF token lives only in memory (`S.csrf`) and is sent
//   as the X-FO-CSRF header on every request that isn't GET/HEAD; it never goes into a URL or
//   into storage (sessionStorage holds UI preferences only: the sort order);
// - all requests go to this origin with credentials "same-origin".
'use strict';

(() => {
  // ---------------------------------------------------------------------------
  // Pure helpers (no DOM)
  // ---------------------------------------------------------------------------

  const ID_RE = /^[0-9a-f]{32}$/;
  const SHEET_EXT = new Set(['xlsx', 'xlsm', 'csv', 'fsheet']);
  const IMAGE_EXT = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp']);
  const PREVIEW_MAX = 30 * 1024 * 1024;
  const SHEETS_DOWNLOAD = 'https://github.com/mar0der/FreedomOffice/releases'; // EXTERNAL-LINK-OK

  function ext(name) {
    const i = name.lastIndexOf('.');
    return i > 0 ? name.slice(i + 1).toLowerCase() : '';
  }

  /** The icon / behaviour family of an item. */
  function kindOf(it) {
    if (it.kind === 'folder') return 'folder';
    const e = ext(it.name);
    if (SHEET_EXT.has(e) || e === 'xls' || e === 'ods') return 'sheet';
    if (IMAGE_EXT.has(e) || e === 'svg' || e === 'bmp' || e === 'heic') return 'image';
    if (e === 'pdf') return 'pdf';
    if (['doc', 'docx', 'odt', 'rtf'].includes(e)) return 'doc';
    if (['ppt', 'pptx', 'odp'].includes(e)) return 'slides';
    if (['zip', '7z', 'tar', 'gz', 'rar', 'xz'].includes(e)) return 'archive';
    if (['mp3', 'm4a', 'wav', 'ogg', 'oga', 'flac'].includes(e)) return 'audio';
    if (['mp4', 'm4v', 'webm', 'mov', 'mkv'].includes(e)) return 'video';
    if (['txt', 'md', 'log', 'json', 'xml', 'html', 'htm'].includes(e)) return 'text';
    return 'file';
  }

  const isSheet = (it) => it.kind === 'file' && SHEET_EXT.has(ext(it.name));
  const isImage = (it) => it.kind === 'file' && IMAGE_EXT.has(ext(it.name));
  const isPdf = (it) => it.kind === 'file' && ext(it.name) === 'pdf';
  const canEdit = (it) => it && (it.role === 'edit' || it.role === 'owner');

  function fmtSize(n) {
    if (n === null || n === undefined || Number.isNaN(n)) return '';
    if (n < 1024) return `${n} B`;
    const units = ['KB', 'MB', 'GB', 'TB'];
    let v = n;
    let i = -1;
    do {
      v /= 1024;
      i++;
    } while (v >= 1024 && i < units.length - 1);
    return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
  }

  /** Short local date: the time today, "12 Mar, 14:05" this year, "12 Mar 2024" before. */
  function fmtDate(iso, now = new Date()) {
    const d = new Date(iso);
    if (!iso || Number.isNaN(d.getTime())) return '';
    const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (d.toDateString() === now.toDateString()) return time;
    if (d.getFullYear() === now.getFullYear()) return `${d.toLocaleDateString([], { day: 'numeric', month: 'short' })}, ${time}`;
    return d.toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' });
  }

  function fmtFull(iso) {
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? '' : d.toLocaleString([], { dateStyle: 'full', timeStyle: 'short' });
  }

  const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

  /** Folders first, then by `key` ("name", "modified", "by", "size"); ties by name. */
  function compareItems(a, b, key, dir) {
    if ((a.kind === 'folder') !== (b.kind === 'folder')) return a.kind === 'folder' ? -1 : 1;
    let c = 0;
    if (key === 'modified') c = (Date.parse(a.deleted || a.modified) || 0) - (Date.parse(b.deleted || b.modified) || 0);
    else if (key === 'size') c = (a.kind === 'folder' ? 0 : a.size || 0) - (b.kind === 'folder' ? 0 : b.size || 0);
    else if (key === 'by') c = collator.compare(a.modified_by || '', b.modified_by || '');
    if (c === 0) c = collator.compare(a.name, b.name);
    return c * dir;
  }

  function contentUrl(id, { version, download } = {}) {
    const q = new URLSearchParams();
    if (version !== undefined && version !== null) q.set('version', String(version));
    if (download) q.set('download', '1');
    const qs = q.toString();
    return `/api/items/${encodeURIComponent(id)}/content${qs ? `?${qs}` : ''}`;
  }

  function sheetsUrl(origin, id) {
    return `freedomoffice://open?server=${encodeURIComponent(origin)}&item=${encodeURIComponent(id)}`;
  }

  function uploadUrl(parent, name, conflict) {
    const q = new URLSearchParams({ parent, name, conflict });
    return `/api/files?${q.toString()}`;
  }

  /** "photo.jpg" -> [0, 5]: what to select when renaming (the name without its extension). */
  function baseRange(name, isFolder) {
    const i = name.lastIndexOf('.');
    return !isFolder && i > 0 ? [0, i] : [0, name.length];
  }

  /** A friendly sentence for an error answer. */
  function friendly(status, body) {
    const msg = body && typeof body.error === 'string' ? body.error : '';
    switch (status) {
      case 0:
        return "Can't reach the server. Check your connection and try again.";
      case 404:
        return msg || "This item doesn't exist any more, or you no longer have access to it.";
      case 408:
        return 'The upload stalled for a minute and the server stopped it. Check the connection and try again.';
      case 409:
        return msg || 'An item with that name already exists here.';
      case 412:
        return 'Someone saved a newer version meanwhile. Reload and try again.';
      case 413:
        return msg || 'The file is too large for this server.';
      case 423:
        return 'This workbook is being edited live in FreedomSheets. Try again when everyone has closed it.';
      case 429:
        return 'Too many attempts. Wait a few minutes and try again.';
      case 507:
        return "The server's disk is nearly full, so nothing more can be stored. Ask your administrator to free some space.";
      default:
        if (status >= 500) return `The server had a problem (HTTP ${status}). Try again in a moment.`;
        return msg || `That didn't work (HTTP ${status}).`;
    }
  }

  // ---------------------------------------------------------------------------
  // DOM helpers
  // ---------------------------------------------------------------------------

  const $ = (id) => document.getElementById(id);

  /** `h('a', {href, class, on: {click}}, 'text', node, [more])`: text is always a text node. */
  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === undefined || v === null || v === false) continue;
      if (k === 'on') {
        for (const [ev, fn] of Object.entries(v)) el.addEventListener(ev, fn);
      } else if (k === 'class') {
        el.className = v;
      } else if (k === 'text') {
        el.textContent = v;
      } else if (k === 'dataset') {
        Object.assign(el.dataset, v);
      } else if (k === 'value') {
        el.value = v;
      } else if (k === 'checked') {
        el.checked = !!v;
      } else if (/^on/i.test(k)) {
        throw new Error('no event handler attributes');
      } else {
        el.setAttribute(k, v === true ? '' : String(v));
      }
    }
    append(el, kids);
    return el;
  }

  function append(el, kids) {
    for (const k of kids.flat(Infinity)) {
      if (k === null || k === undefined || k === false) continue;
      el.append(typeof k === 'string' || typeof k === 'number' ? document.createTextNode(String(k)) : k);
    }
  }

  function clear(el) {
    el.replaceChildren();
    return el;
  }

  function show(el, on) {
    el.hidden = !on;
  }

  // ---------------------------------------------------------------------------
  // State
  // ---------------------------------------------------------------------------

  const S = {
    csrf: '', // in memory only
    user: null, // from the session
    me: null, // GET /api/me: orgs, storage, server limits
    view: null, // {kind: 'folder'|'shared'|'recent'|'trash', id?}
    folder: null, // the folder shown (folder view)
    crumbs: [], // [{id?, name, href}]
    section: 'files', // which nav entry is current
    items: [],
    selected: new Set(),
    anchor: null, // shift-click range start
    focusId: null,
    sort: loadSort(),
    seq: 0, // ignores answers to superseded loads
    itemCache: new Map(), // parents for breadcrumbs
  };

  function loadSort() {
    try {
      const s = JSON.parse(sessionStorage.getItem('fo-drive-sort') || 'null');
      if (s && ['name', 'modified', 'by', 'size'].includes(s.key) && (s.dir === 1 || s.dir === -1)) return s;
    } catch {
      /* no preference */
    }
    return { key: 'name', dir: 1 };
  }

  function saveSort() {
    try {
      sessionStorage.setItem('fo-drive-sort', JSON.stringify(S.sort));
    } catch {
      /* private mode */
    }
  }

  // ---------------------------------------------------------------------------
  // The API
  // ---------------------------------------------------------------------------

  class ApiError extends Error {
    constructor(status, body) {
      super(friendly(status, body));
      this.status = status;
      this.code = (body && body.code) || '';
      this.body = body || {};
    }
  }

  async function readJson(res) {
    try {
      return await res.json();
    } catch {
      return {};
    }
  }

  async function refreshSession() {
    const res = await fetch('/api/web/session', { credentials: 'same-origin', cache: 'no-store' });
    if (res.status === 401) return null;
    if (!res.ok) throw new ApiError(res.status, await readJson(res));
    const s = await res.json();
    S.csrf = s.csrf;
    S.user = s.user;
    return s;
  }

  /**
   * `api('GET', '/api/me')`. JSON in and out; 401 shows the sign-in; a 403 `csrf_failed`
   * re-reads the session and tries once more.
   */
  async function api(method, path, body, { retried = false } = {}) {
    const opts = { method, credentials: 'same-origin', cache: 'no-store', headers: {} };
    if (method !== 'GET' && method !== 'HEAD') opts.headers['X-FO-CSRF'] = S.csrf;
    if (body !== undefined) {
      opts.headers['Content-Type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    let res;
    try {
      res = await fetch(path, opts);
    } catch {
      throw new ApiError(0, {});
    }
    if (res.ok) return res.status === 204 ? null : readJson(res);
    const j = await readJson(res);
    if (res.status === 401) {
      signedOut('Your session has ended. Sign in again.');
      throw new ApiError(401, j);
    }
    if (res.status === 403 && j.code === 'csrf_failed' && !retried) {
      const s = await refreshSession();
      if (!s) {
        signedOut('Your session has ended. Sign in again.');
        throw new ApiError(401, j);
      }
      return api(method, path, body, { retried: true });
    }
    if (res.status === 403 && j.code === 'password_change_required') {
      showPasswordChange(true);
    }
    throw new ApiError(res.status, j);
  }

  const itemPath = (id, rest = '') => `/api/items/${encodeURIComponent(id)}${rest}`;

  // ---------------------------------------------------------------------------
  // Screens: sign in, password change, the app
  // ---------------------------------------------------------------------------

  function screen(name) {
    show($('boot'), name === 'boot');
    show($('signin'), name === 'signin');
    show($('pwchange'), name === 'pwchange');
    show($('shell'), name === 'shell');
  }

  function showSignIn(note) {
    screen('signin');
    show($('signin-error'), false);
    $('signin-note').textContent = note || '';
    show($('signin-note'), !!note);
    $('signin-pass').value = '';
    $('signin-user').focus();
  }

  function signedOut(note) {
    const was = !!S.user;
    abortAllUploads();
    closeAllDialogs();
    S.csrf = '';
    S.user = null;
    S.me = null;
    S.itemCache.clear();
    clear($('rows'));
    if ($('signin').hidden || was) showSignIn(note);
  }

  async function onSignIn(e) {
    e.preventDefault();
    const err = $('signin-error');
    const username = $('signin-user').value.trim();
    const password = $('signin-pass').value;
    show($('signin-note'), false);
    if (!username || !password) {
      err.textContent = 'Enter your username and password.';
      show(err, true);
      return;
    }
    const btn = $('signin-submit');
    btn.disabled = true;
    btn.textContent = 'Signing in…';
    try {
      const res = await fetch('/api/web/login', {
        method: 'POST',
        credentials: 'same-origin',
        cache: 'no-store',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      });
      const j = await readJson(res);
      if (!res.ok) {
        err.textContent = res.status === 401 ? 'The username or password is incorrect.' : friendly(res.status, j);
        show(err, true);
        $('signin-pass').select();
        return;
      }
      S.csrf = j.csrf;
      S.user = j.user;
      $('signin-pass').value = '';
      if (j.user.must_change_password) showPasswordChange(true);
      else await enterApp();
    } catch {
      err.textContent = friendly(0);
      show(err, true);
    } finally {
      btn.disabled = false;
      btn.textContent = 'Sign in';
    }
  }

  let pwForced = false;

  function showPasswordChange(forced) {
    pwForced = forced;
    closeAllDialogs();
    screen('pwchange');
    $('pw-title').textContent = forced ? 'Choose a new password' : 'Change your password';
    show($('pw-intro'), forced);
    show($('pw-cancel'), !forced);
    show($('pw-error'), false);
    $('pw-username').value = S.user ? S.user.username : '';
    for (const id of ['pw-old', 'pw-new', 'pw-confirm']) $(id).value = '';
    $('pw-old').focus();
  }

  async function onPasswordChange(e) {
    e.preventDefault();
    const err = $('pw-error');
    const oldPw = $('pw-old').value;
    const newPw = $('pw-new').value;
    const say = (m, focus) => {
      err.textContent = m;
      show(err, true);
      if (focus) $(focus).focus();
    };
    if (!oldPw) return say('Enter your current password.', 'pw-old');
    if (newPw.length < 12) return say('The new password needs at least 12 characters.', 'pw-new');
    if (newPw !== $('pw-confirm').value) return say("The two new passwords don't match.", 'pw-confirm');
    const btn = $('pw-submit');
    btn.disabled = true;
    try {
      const r = await api('POST', '/api/me/password', { old_password: oldPw, new_password: newPw });
      S.user = r.user;
      for (const id of ['pw-old', 'pw-new', 'pw-confirm']) $(id).value = '';
      await enterApp();
      toast(r.signed_out_devices ? `Password changed. ${r.signed_out_devices} other sign-in(s) ended.` : 'Password changed.');
    } catch (ex) {
      if (ex.status === 401) return;
      say(ex.code === 'wrong_password' ? 'The current password is incorrect.' : ex.message, ex.code === 'wrong_password' ? 'pw-old' : 'pw-new');
    } finally {
      btn.disabled = false;
    }
  }

  async function signOut() {
    try {
      await api('POST', '/api/web/logout');
    } catch {
      /* signed out either way */
    }
    signedOut('You have signed out.');
  }

  async function enterApp() {
    S.me = await api('GET', '/api/me');
    S.user = S.me;
    if (S.me.must_change_password) return showPasswordChange(true);
    renderNav();
    screen('shell');
    if (!location.hash || location.hash === '#' || location.hash === '#/') history.replaceState(null, '', '#/files');
    await route();
  }

  function renderNav() {
    const me = S.me;
    $('who-name').textContent = me.display_name || me.username;
    $('who-name').title = me.username;
    const orgNav = clear($('org-nav'));
    for (const o of me.orgs || []) {
      orgNav.append(h('li', null, h('a', { href: `#/files/${o.root}`, dataset: { nav: `org:${o.id}` } }, h('span', { class: 'ficon org', 'aria-hidden': 'true' }), o.name)));
    }
    show($('orgs-block'), (me.orgs || []).length > 0);
    const manages = me.is_admin || (me.orgs || []).some((o) => o.role === 'owner' || o.role === 'admin');
    show($('admin-link'), manages);
    const st = me.storage;
    show($('storage'), !!st);
    if (st) $('storage-text').textContent = `${fmtSize(st.bytes || 0)} used · ${st.files || 0} file${st.files === 1 ? '' : 's'}`;
  }

  function markNav() {
    for (const a of document.querySelectorAll('#sidenav a[data-nav]')) {
      if (a.dataset.nav === S.section) a.setAttribute('aria-current', 'page');
      else a.removeAttribute('aria-current');
    }
  }

  // ---------------------------------------------------------------------------
  // Routing (#/files/<id>, #/shared, #/recent, #/trash)
  // ---------------------------------------------------------------------------

  function parseHash() {
    const parts = location.hash.replace(/^#\/?/, '').split('/');
    switch (parts[0]) {
      case '':
      case 'files':
        return { kind: 'folder', id: parts[1] || S.me.root };
      case 'shared':
        return { kind: 'shared' };
      case 'recent':
        return { kind: 'recent' };
      case 'trash':
        return { kind: 'trash' };
      default:
        return { kind: 'folder', id: S.me.root };
    }
  }

  async function route(keepSelection) {
    if (!S.me) return;
    closeNavOnPhone();
    const v = parseHash();
    const sameView = S.view && S.view.kind === v.kind && S.view.id === v.id;
    S.view = v;
    if (!(keepSelection && sameView)) {
      S.selected.clear();
      S.anchor = null;
      S.focusId = null;
    }
    await loadView();
  }

  async function loadView() {
    const seq = ++S.seq;
    const v = S.view;
    const slow = setTimeout(() => seq === S.seq && listState('loading'), 150);
    $('list').setAttribute('aria-busy', 'true');
    try {
      if (v.kind === 'folder') {
        if (!ID_RE.test(v.id)) throw new ApiError(404, {});
        const [folder, kids] = await Promise.all([api('GET', itemPath(v.id)), api('GET', itemPath(v.id, '/children')).catch((e) => e)]);
        if (folder.kind !== 'folder') {
          // A file's address: show its folder with the file selected.
          if (folder.parent) {
            S.selected = new Set([folder.id]);
            S.focusId = folder.id;
            history.replaceState(null, '', `#/files/${folder.parent}`);
            S.view = { kind: 'folder', id: folder.parent };
            clearTimeout(slow);
            return loadView();
          }
          throw new ApiError(404, {});
        }
        if (kids instanceof Error) throw kids;
        const crumbs = await crumbsFor(folder);
        if (seq !== S.seq) return;
        S.folder = folder;
        S.items = kids.items;
        S.crumbs = crumbs.list;
        S.section = crumbs.section;
      } else {
        const path = { shared: '/api/shared-with-me', recent: '/api/recent', trash: '/api/trash' }[v.kind];
        const r = await api('GET', path);
        if (seq !== S.seq) return;
        S.folder = null;
        S.items = r.items;
        S.section = v.kind;
        S.crumbs = [{ name: { shared: 'Shared with me', recent: 'Recent', trash: 'Recycle bin' }[v.kind] }];
      }
      // Drop selections of items that went away.
      const ids = new Set(S.items.map((i) => i.id));
      for (const id of [...S.selected]) if (!ids.has(id)) S.selected.delete(id);
      clearTimeout(slow);
      render();
    } catch (e) {
      clearTimeout(slow);
      if (seq !== S.seq || e.status === 401) return;
      S.items = [];
      S.folder = null;
      S.crumbs = [{ name: v.kind === 'folder' ? 'Folder' : '' }];
      render();
      listState('error', e);
    } finally {
      if (seq === S.seq) $('list').removeAttribute('aria-busy');
    }
  }

  /** The folder's breadcrumbs, and which nav entry it belongs to. */
  async function crumbsFor(folder) {
    const chain = [folder];
    let cur = folder;
    for (let depth = 0; cur.parent && depth < 64; depth++) {
      let p = S.itemCache.get(cur.parent);
      if (!p) {
        try {
          p = await api('GET', itemPath(cur.parent));
        } catch (e) {
          if (e.status === 404 || e.status === 403) break; // above what is shared with me
          throw e;
        }
        S.itemCache.set(p.id, p);
      }
      chain.unshift(p);
      cur = p;
    }
    const org = (S.me.orgs || []).find((o) => o.root === chain[0].id);
    let section = 'shared';
    const list = chain.map((it) => ({ id: it.id, name: it.name, href: `#/files/${it.id}` }));
    if (chain[0].id === S.me.root) {
      section = 'files';
      list[0].name = 'My files';
    } else if (org) {
      section = `org:${org.id}`;
      list[0].name = org.name;
    } else {
      list.unshift({ name: 'Shared with me', href: '#/shared' });
    }
    return { list, section };
  }

  // ---------------------------------------------------------------------------
  // Rendering the list
  // ---------------------------------------------------------------------------

  function sorted() {
    return [...S.items].sort((a, b) => compareItems(a, b, S.sort.key, S.sort.dir));
  }

  function listState(kind, err) {
    const box = clear($('list-state'));
    show(box, !!kind);
    if (kind === 'loading') {
      box.className = 'liststate loading';
      box.append(h('span', { class: 'spinner', 'aria-hidden': 'true' }), 'Loading…');
    } else if (kind === 'error') {
      box.className = 'liststate msg error';
      const notFound = err && (err.status === 404 || err.status === 403);
      box.append(
        h('p', null, notFound ? "This folder doesn't exist any more, or you no longer have access to it." : `Couldn't load this view. ${err ? err.message : ''}`),
        h('div', { class: 'buttons' }, notFound ? h('a', { class: 'button secondary', href: '#/files' }, 'Go to My files') : h('button', { type: 'button', class: 'secondary', on: { click: () => loadView() } }, 'Try again')),
      );
      show($('empty'), false);
    }
  }

  function render() {
    listState(null);
    renderCrumbs();
    markNav();
    const v = S.view;
    const trash = v.kind === 'trash';
    $('col-mod-label').textContent = trash ? 'Deleted' : 'Modified';
    const listHadFocus = $('list').contains(document.activeElement);
    const rows = clear($('rows'));
    const items = sorted();
    if (S.focusId && !items.some((i) => i.id === S.focusId)) S.focusId = null;
    if (!S.focusId && items.length) S.focusId = items[0].id;
    for (const it of items) rows.append(rowFor(it, trash));
    // Sort indicators.
    for (const th of document.querySelectorAll('#list th[data-col]')) {
      const on = th.dataset.col === S.sort.key;
      if (on) th.setAttribute('aria-sort', S.sort.dir === 1 ? 'ascending' : 'descending');
      else th.removeAttribute('aria-sort');
      th.classList.toggle('sorted', on);
      th.classList.toggle('desc', on && S.sort.dir === -1);
    }
    const empty = $('empty');
    show(empty, items.length === 0);
    show($('list'), items.length > 0);
    if (!items.length) {
      clear(empty).append(...emptyState());
    }
    const note = $('view-note');
    note.textContent = trash
      ? 'Deleted items stay here until an administrator empties the Recycle bin. Restore puts an item back where it was (or at the top of its drive, if that folder is gone).'
      : v.kind === 'folder' && S.folder && S.folder.role === 'view'
        ? 'You can view this folder but not change it.'
        : '';
    show(note, !!note.textContent);
    updateSelection();
    if (listHadFocus) focusRow(S.focusId);
  }

  function emptyState() {
    const v = S.view;
    const writable = v.kind === 'folder' && canEdit(S.folder);
    const texts = {
      folder: writable ? ['This folder is empty', 'Drop files here, or use Upload or New folder.'] : ['This folder is empty', ''],
      shared: ['Nothing is shared with you yet', 'Files and folders other people share with you appear here.'],
      recent: ['No recent files', 'Files you open or upload appear here.'],
      trash: ['The Recycle bin is empty', 'Deleted files and folders appear here; you can restore them.'],
    }[v.kind];
    return [h('span', { class: `ficon big ${v.kind === 'trash' ? 'trash' : 'folder'}`, 'aria-hidden': 'true' }), h('p', { class: 'empty-title' }, texts[0]), texts[1] ? h('p', { class: 'muted' }, texts[1]) : null];
  }

  function renderCrumbs() {
    const ol = clear($('crumbs'));
    const list = S.crumbs;
    list.forEach((c, i) => {
      const last = i === list.length - 1;
      ol.append(h('li', null, last || !c.href ? h('span', { 'aria-current': last ? 'page' : null }, c.name) : h('a', { href: c.href }, c.name)));
    });
    show($('crumbs').parentElement, list.length > 1);
    $('view-title').textContent = list.length ? list[list.length - 1].name : '';
    document.title = `${$('view-title').textContent || 'Files'} - FreedomOffice Drive`;
  }

  function rowFor(it, trash) {
    const sel = S.selected.has(it.id);
    const k = kindOf(it);
    const label = it.kind === 'folder' ? `${it.name}, folder` : it.name;
    const name =
      it.kind === 'folder' && !trash
        ? h('a', { href: `#/files/${it.id}`, class: 'name', tabindex: '-1' }, it.name)
        : h('button', { type: 'button', class: 'name', tabindex: '-1', dataset: { open: '1' } }, it.name);
    const extra = [];
    if (it.org && S.view.kind !== 'folder') extra.push(h('span', { class: 'tag' }, it.org.name));
    if (S.view.kind === 'shared' && it.owner && it.owner.display_name && !it.org) extra.push(h('span', { class: 'tag' }, `from ${it.owner.display_name}`));
    if (it.role === 'view' && S.view.kind !== 'folder') extra.push(h('span', { class: 'tag' }, 'view only'));
    const when = trash ? it.deleted : it.modified;
    return h(
      'tr',
      { class: sel ? 'row selected' : 'row', tabindex: it.id === S.focusId ? '0' : '-1', 'aria-selected': sel ? 'true' : 'false', 'aria-label': label, dataset: { id: it.id } },
      h('td', { class: 'col-check' }, h('input', { type: 'checkbox', tabindex: '-1', checked: sel, 'aria-label': `Select ${it.name}` })),
      h('td', { class: 'col-name' }, h('span', { class: 'namecell' }, h('span', { class: `ficon ${k}`, 'aria-hidden': 'true', dataset: { ext: it.kind === 'file' ? ext(it.name).slice(0, 4) : '' } }), name, extra)),
      h('td', { class: 'col-mod' }, h('time', { datetime: when || '', title: fmtFull(when) }, fmtDate(when))),
      h('td', { class: 'col-by' }, it.modified_by || ''),
      h('td', { class: 'col-size num' }, it.kind === 'folder' ? '' : fmtSize(it.size)),
    );
  }

  function itemById(id) {
    return S.items.find((i) => i.id === id);
  }

  function selectedItems() {
    return sorted().filter((i) => S.selected.has(i.id));
  }

  /** The items an action applies to: the selection, else the focused row. */
  function targets() {
    const sel = selectedItems();
    if (sel.length) return sel;
    const f = S.focusId && itemById(S.focusId);
    return f ? [f] : [];
  }

  function updateSelection() {
    for (const tr of $('rows').children) {
      const on = S.selected.has(tr.dataset.id);
      tr.classList.toggle('selected', on);
      tr.setAttribute('aria-selected', on ? 'true' : 'false');
      tr.querySelector('input[type=checkbox]').checked = on;
    }
    const n = S.selected.size;
    const all = $('select-all');
    all.checked = n > 0 && n === S.items.length;
    all.indeterminate = n > 0 && n < S.items.length;
    updateToolbar();
  }

  function updateToolbar() {
    const v = S.view;
    const sel = selectedItems();
    const one = sel.length === 1 ? sel[0] : null;
    const trash = v.kind === 'trash';
    const writableFolder = v.kind === 'folder' && canEdit(S.folder);
    show($('act-new-folder'), writableFolder);
    show($('act-upload'), writableFolder);
    $('sel-count').textContent = sel.length ? `${sel.length} selected` : '';
    const allEdit = sel.length > 0 && sel.every(canEdit);
    const vis = {
      open: !trash && !!one,
      sheets: !trash && !!one && isSheet(one),
      download: !trash && !!one && one.kind === 'file',
      share: !trash && !!one && canEdit(one),
      rename: !trash && !!one && canEdit(one),
      move: !trash && allEdit,
      versions: !trash && !!one && one.kind === 'file',
      restore: trash && sel.length > 0,
      delete: !trash && allEdit,
    };
    for (const b of document.querySelectorAll('#toolbar [data-act]')) show(b, !!vis[b.dataset.act]);
    const hint = $('sheets-hint');
    const showHint = vis.sheets;
    if (showHint && !hint.childNodes.length) {
      hint.append(
        "Don't have FreedomSheets? ",
        // The download page: a plain link (this page makes no request to it).
        h('a', { href: SHEETS_DOWNLOAD, target: '_blank', rel: 'noopener noreferrer' }, 'Download it for Windows, Linux or macOS'),
        ', then sign in to this server in File > Account.',
      );
    }
    show(hint, showHint);
  }

  function focusRow(id) {
    const rows = $('rows');
    let target = null;
    for (const tr of rows.children) {
      const on = tr.dataset.id === id;
      tr.tabIndex = on ? 0 : -1;
      if (on) target = tr;
    }
    if (target) {
      S.focusId = id;
      target.focus();
    }
  }

  function selectOnly(id) {
    S.selected = new Set(id ? [id] : []);
    S.anchor = id;
  }

  function selectRange(toId, add) {
    const order = sorted().map((i) => i.id);
    const a = order.indexOf(S.anchor ?? toId);
    const b = order.indexOf(toId);
    if (a < 0 || b < 0) return selectOnly(toId);
    if (!add) S.selected.clear();
    for (let i = Math.min(a, b); i <= Math.max(a, b); i++) S.selected.add(order[i]);
  }

  function toggle(id) {
    if (S.selected.has(id)) S.selected.delete(id);
    else S.selected.add(id);
    S.anchor = id;
  }

  function onRowClick(e) {
    const tr = e.target.closest('tr.row');
    if (!tr) return;
    const id = tr.dataset.id;
    S.focusId = id;
    if (e.target.matches('input[type=checkbox]')) {
      if (e.shiftKey && S.anchor) selectRange(id, true);
      else toggle(id);
      updateSelection();
      focusRow(id);
      return;
    }
    if (e.target.closest('[data-open]')) {
      e.preventDefault();
      selectOnly(id);
      updateSelection();
      openItem(itemById(id));
      return;
    }
    if (e.target.closest('a.name')) {
      if (e.ctrlKey || e.metaKey || e.shiftKey) return; // new tab
      return; // the hash change opens the folder
    }
    if (e.shiftKey) selectRange(id, e.ctrlKey || e.metaKey);
    else if (e.ctrlKey || e.metaKey) toggle(id);
    else selectOnly(id);
    updateSelection();
    focusRow(id);
  }

  function onRowDblClick(e) {
    const tr = e.target.closest('tr.row');
    if (!tr || e.target.closest('a, button, input')) return;
    openItem(itemById(tr.dataset.id));
  }

  function onListKey(e) {
    const tr = e.target.closest('tr.row');
    if (!tr) return;
    const order = sorted().map((i) => i.id);
    const idx = order.indexOf(tr.dataset.id);
    const move = (to) => {
      e.preventDefault();
      const id = order[Math.max(0, Math.min(order.length - 1, to))];
      if (e.shiftKey) {
        if (!S.anchor) S.anchor = order[idx];
        selectRange(id, false);
        updateSelection();
      }
      focusRow(id);
    };
    switch (e.key) {
      case 'ArrowDown':
        return move(idx + 1);
      case 'ArrowUp':
        return move(idx - 1);
      case 'Home':
        return move(0);
      case 'End':
        return move(order.length - 1);
      case ' ':
        e.preventDefault();
        toggle(order[idx]);
        updateSelection();
        return;
      case 'Enter':
        e.preventDefault();
        if (!S.selected.has(order[idx])) {
          selectOnly(order[idx]);
          updateSelection();
        }
        openItem(itemById(order[idx]));
        return;
      case 'Delete':
        e.preventDefault();
        if (S.view.kind === 'trash') return restoreItems(targets());
        return deleteItems(targets().filter(canEdit));
      case 'F2': {
        e.preventDefault();
        const t = targets();
        if (t.length === 1 && canEdit(t[0]) && S.view.kind !== 'trash') renameDialog(t[0]);
        return;
      }
      case 'Escape':
        if (S.selected.size) {
          e.preventDefault();
          selectOnly(null);
          updateSelection();
        }
        return;
      default:
        if ((e.key === 'a' || e.key === 'A') && (e.ctrlKey || e.metaKey)) {
          e.preventDefault();
          S.selected = new Set(order);
          updateSelection();
        }
    }
  }

  // ---------------------------------------------------------------------------
  // Opening, downloading, previews
  // ---------------------------------------------------------------------------

  function navigateTo(url) {
    const a = h('a', { href: url, rel: 'noopener' });
    document.body.append(a);
    a.click();
    a.remove();
  }

  function download(it, version) {
    navigateTo(contentUrl(it.id, { version, download: true }));
  }

  function openInSheets(it) {
    navigateTo(sheetsUrl(location.origin, it.id));
    toast('Opening in FreedomSheets…');
  }

  function openItem(it) {
    if (!it) return;
    if (S.view.kind === 'trash') return;
    if (it.kind === 'folder') {
      location.hash = `#/files/${it.id}`;
    } else if (isImage(it) && it.size <= PREVIEW_MAX) {
      previewImage(it);
    } else if (isPdf(it)) {
      // A new tab with the server's own PDF answer (Content-Disposition: inline).
      window.open(contentUrl(it.id), '_blank', 'noopener');
    } else if (isSheet(it)) {
      openInSheets(it);
    } else {
      download(it);
    }
  }

  async function previewImage(it) {
    let url = null;
    const img = h('img', { class: 'preview-img', alt: it.name });
    const status = h('p', { class: 'muted', role: 'status' }, 'Loading…');
    const d = openDialog(it.name, (ctl) => {
      ctl.content.append(h('div', { class: 'preview' }, status, img));
      ctl.actions(h('button', { type: 'button', class: 'secondary', on: { click: () => download(it) } }, 'Download'), h('button', { type: 'button', on: { click: () => ctl.close() } }, 'Close'));
    }, { wide: true, onClose: () => url && URL.revokeObjectURL(url) });
    img.hidden = true;
    try {
      const res = await fetch(contentUrl(it.id), { credentials: 'same-origin', cache: 'no-store' });
      if (res.status === 401) return signedOut('Your session has ended. Sign in again.');
      if (!res.ok) throw new ApiError(res.status, await readJson(res));
      const type = (res.headers.get('content-type') || '').toLowerCase();
      if (!type.startsWith('image/') || type.startsWith('image/svg')) throw new Error("This file can't be previewed. Download it instead.");
      const blob = await res.blob();
      if (!d.dlg.open) return;
      url = URL.createObjectURL(blob);
      img.src = url;
      img.hidden = false;
      status.hidden = true;
    } catch (e) {
      status.textContent = e.message;
    }
  }

  // ---------------------------------------------------------------------------
  // Dialogs and toasts
  // ---------------------------------------------------------------------------

  let dlgSeq = 0;

  /** A modal `<dialog>`: `build(ctl)` fills `ctl.content` and calls `ctl.actions(...)`. */
  function openDialog(title, build, { wide = false, onClose } = {}) {
    const prev = document.activeElement;
    const id = `dlg-${++dlgSeq}`;
    const err = h('div', { class: 'msg error', role: 'alert', hidden: true });
    const content = h('div', { class: 'dlg-body' });
    const foot = h('div', { class: 'dlg-foot' });
    const dlg = h('dialog', { class: wide ? 'dlg wide' : 'dlg', 'aria-labelledby': `${id}-t` });
    let resolve;
    const ctl = {
      dlg,
      content,
      value: undefined,
      closed: new Promise((r) => {
        resolve = r;
      }),
      close(v) {
        if (v !== undefined) ctl.value = v;
        if (dlg.open) dlg.close();
      },
      error(msg) {
        err.textContent = msg || '';
        show(err, !!msg);
      },
      actions(...buttons) {
        clear(foot).append(...buttons);
      },
    };
    dlg.append(
      h('div', { class: 'dlg-head' }, h('h2', { id: `${id}-t` }, title), h('button', { type: 'button', class: 'icon-btn', 'aria-label': 'Close', on: { click: () => ctl.close() } }, '×')),
      err,
      content,
      foot,
    );
    dlg.addEventListener('close', () => {
      dlg.remove();
      if (onClose) onClose();
      if (prev && prev.isConnected && typeof prev.focus === 'function') prev.focus();
      resolve(ctl.value);
    });
    build(ctl);
    document.body.append(dlg);
    dlg.showModal();
    const first = dlg.querySelector('[autofocus]') || content.querySelector('input, select, textarea, button, a[href]') || foot.querySelector('button');
    if (first) first.focus();
    return ctl;
  }

  function closeAllDialogs() {
    for (const d of document.querySelectorAll('dialog[open]')) d.close();
  }

  /** Asks for a name; `act(name)` runs while the dialog stays open (errors are shown in it). */
  function nameDialog({ title, label, value, submit, folder, act }) {
    return openDialog(title, (ctl) => {
      const input = h('input', { id: `${ctl.dlg.getAttribute('aria-labelledby')}-in`, value, autocomplete: 'off', spellcheck: 'false', required: true, maxlength: '255' });
      const btn = h('button', { type: 'submit' }, submit);
      const form = h(
        'form',
        {
          class: 'stack',
          on: {
            submit: async (e) => {
              e.preventDefault();
              const name = input.value.trim();
              if (!name) return ctl.error('Enter a name.');
              btn.disabled = true;
              try {
                await act(name);
                ctl.close(name);
              } catch (ex) {
                if (ex.status !== 401) ctl.error(ex.message);
              } finally {
                btn.disabled = false;
              }
            },
          },
        },
        h('label', { for: input.id }, label),
        input,
        h('div', { class: 'dlg-foot inline' }, h('button', { type: 'button', class: 'secondary', on: { click: () => ctl.close() } }, 'Cancel'), btn),
      );
      ctl.content.append(form);
      setTimeout(() => {
        input.focus();
        const [a, b] = baseRange(value, folder);
        input.setSelectionRange(a, b);
      }, 0);
    });
  }

  /** Buttons `[{label, value, kind}]`; resolves to the chosen value (undefined: closed). */
  function choose(title, message, buttons, extra) {
    const d = openDialog(title, (ctl) => {
      ctl.content.append(h('p', null, message));
      if (extra) ctl.content.append(extra);
      ctl.actions(...buttons.map((b) => h('button', { type: 'button', class: b.kind || 'secondary', on: { click: () => ctl.close(b.value) } }, b.label)));
    });
    return d.closed;
  }

  function toast(message, { action, onAction, error = false, timeout } = {}) {
    const box = $('toasts');
    const t = h('div', { class: error ? 'toast error' : 'toast', role: error ? 'alert' : null });
    t.append(h('span', null, message));
    let timer;
    const close = () => {
      clearTimeout(timer);
      t.remove();
    };
    if (action) {
      t.append(
        h(
          'button',
          {
            type: 'button',
            class: 'link',
            on: {
              click: () => {
                close();
                onAction();
              },
            },
          },
          action,
        ),
      );
    }
    t.append(h('button', { type: 'button', class: 'icon-btn', 'aria-label': 'Dismiss', on: { click: close } }, '×'));
    box.append(t);
    timer = setTimeout(close, timeout || (action ? 10000 : error ? 8000 : 4000));
    return close;
  }

  const toastError = (e) => e && e.status !== 401 && toast(e.message, { error: true });

  // ---------------------------------------------------------------------------
  // Actions
  // ---------------------------------------------------------------------------

  async function refresh() {
    S.itemCache.clear();
    await loadView();
  }

  function newFolderDialog() {
    if (!S.folder) return;
    const parent = S.folder.id;
    nameDialog({
      title: 'New folder',
      label: 'Folder name',
      value: 'New folder',
      submit: 'Create',
      folder: true,
      act: async (name) => {
        const it = await api('POST', '/api/folders', { parent, name });
        S.focusId = it.id;
        selectOnly(it.id);
        await refresh();
        toast(`Created ${it.name}.`);
      },
    });
  }

  function renameDialog(it) {
    nameDialog({
      title: `Rename ${it.kind === 'folder' ? 'folder' : 'file'}`,
      label: 'New name',
      value: it.name,
      submit: 'Rename',
      folder: it.kind === 'folder',
      act: async (name) => {
        if (name === it.name) return;
        const u = await api('PATCH', itemPath(it.id), { name });
        await refresh();
        toast(`Renamed to ${u.name}.`);
      },
    });
  }

  async function deleteItems(items) {
    if (!items.length) return;
    const done = [];
    for (const it of items) {
      try {
        await api('DELETE', itemPath(it.id));
        done.push(it);
      } catch (e) {
        toastError(e);
        if (e.status === 401) return;
      }
    }
    selectOnly(null);
    await refresh();
    if (done.length) {
      const what = done.length === 1 ? done[0].name : `${done.length} items`;
      toast(`Moved ${what} to the Recycle bin.`, { action: 'Undo', onAction: () => restoreItems(done, true) });
    }
  }

  async function restoreItems(items, undo) {
    if (!items.length) return;
    let n = 0;
    for (const it of items) {
      try {
        await api('POST', itemPath(it.id, '/restore'));
        n++;
      } catch (e) {
        toastError(e);
        if (e.status === 401) return;
      }
    }
    selectOnly(null);
    await refresh();
    if (n) toast(undo ? 'Restored.' : `Restored ${n === 1 ? items[0].name : `${n} items`}.`);
  }

  /** The root folder an item lives under (its drive): My files or an organisation's folder. */
  function driveRootOf(it) {
    if (S.view.kind === 'folder' && S.crumbs.length && S.crumbs[0].id && S.section !== 'shared') return { id: S.crumbs[0].id, name: S.crumbs[0].name };
    if (it.org) {
      const o = (S.me.orgs || []).find((x) => x.id === it.org.id);
      if (o) return { id: o.root, name: o.name };
    }
    return { id: S.me.root, name: 'My files' };
  }

  async function moveDialog(items) {
    if (!items.length) return;
    const root = driveRootOf(items[0]);
    const moving = new Set(items.map((i) => i.id));
    const fromParent = items[0].parent;
    let cur = S.view.kind === 'folder' && S.folder && S.section !== 'shared' ? S.folder.id : root.id;
    const title = items.length === 1 ? `Move ${items[0].name}` : `Move ${items.length} items`;
    openDialog(title, (ctl) => {
      const crumbs = h('ol', { class: 'crumbs inline' });
      const list = h('ul', { class: 'picker', 'aria-label': 'Folders' });
      const status = h('p', { class: 'muted', role: 'status' });
      const here = h('button', { type: 'button' }, 'Move here');
      ctl.content.append(h('p', { class: 'muted small' }, 'Choose the folder to move to.'), h('nav', { 'aria-label': 'Folder path' }, crumbs), status, list);
      ctl.actions(h('button', { type: 'button', class: 'secondary', on: { click: () => ctl.close() } }, 'Cancel'), here);
      let path = [];
      const load = async (id) => {
        status.textContent = 'Loading…';
        clear(list);
        here.disabled = true;
        try {
          const [folder, kids] = await Promise.all([api('GET', itemPath(id)), api('GET', itemPath(id, '/children'))]);
          const chain = await crumbsFor(folder);
          cur = id;
          path = chain.list.filter((c) => c.id);
          // Stay inside the drive the items are in.
          const at = path.findIndex((c) => c.id === root.id);
          if (at > 0) path = path.slice(at);
          path[0] = { ...path[0], name: root.name };
          clear(crumbs);
          path.forEach((c, i) => {
            crumbs.append(h('li', null, i === path.length - 1 ? h('span', { 'aria-current': 'location' }, c.name) : h('button', { type: 'button', class: 'link', on: { click: () => load(c.id) } }, c.name)));
          });
          const folders = kids.items.filter((k) => k.kind === 'folder' && !moving.has(k.id)).sort((a, b) => collator.compare(a.name, b.name));
          for (const f of folders) {
            list.append(h('li', null, h('button', { type: 'button', class: 'picker-item', on: { click: () => load(f.id) } }, h('span', { class: 'ficon folder', 'aria-hidden': 'true' }), f.name)));
          }
          status.textContent = folders.length ? '' : 'No folders in here.';
          const inside = path.some((c) => moving.has(c.id));
          here.disabled = inside || id === fromParent || !canEdit(folder);
          here.title = id === fromParent ? 'The items are already here' : inside ? "A folder can't go inside itself" : !canEdit(folder) ? 'You can only view this folder' : '';
          ctl.error('');
        } catch (e) {
          status.textContent = '';
          if (e.status !== 401) ctl.error(e.message);
        }
      };
      here.addEventListener('click', async () => {
        here.disabled = true;
        let n = 0;
        for (const it of items) {
          try {
            await api('PATCH', itemPath(it.id), { parent: cur });
            n++;
          } catch (e) {
            if (e.status === 401) return;
            ctl.error(`${it.name}: ${e.message}`);
            here.disabled = false;
            if (n) await refresh();
            return;
          }
        }
        ctl.close();
        selectOnly(null);
        await refresh();
        toast(`Moved ${n === 1 ? items[0].name : `${n} items`} to ${path.length ? path[path.length - 1].name : 'the folder'}.`);
      });
      load(cur);
    });
  }

  async function versionsDialog(it) {
    openDialog(`Version history: ${it.name}`, (ctl) => {
      const status = h('p', { class: 'muted', role: 'status' }, 'Loading…');
      const body = h('tbody');
      const table = h(
        'div',
        { class: 'table' },
        h('table', null, h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Version'), h('th', { scope: 'col' }, 'Saved'), h('th', { scope: 'col', class: 'col-by' }, 'By'), h('th', { scope: 'col', class: 'num' }, 'Size'), h('th', { scope: 'col' }, h('span', { class: 'sr' }, 'Actions')))), body),
      );
      table.hidden = true;
      ctl.content.append(status, table, h('p', { class: 'muted small' }, 'Restoring saves the old version as a new current version; nothing is lost.'));
      ctl.actions(h('button', { type: 'button', on: { click: () => ctl.close() } }, 'Close'));
      const load = async () => {
        try {
          const r = await api('GET', itemPath(it.id, '/versions'));
          clear(body);
          const list = [...r.versions].sort((a, b) => b.version - a.version);
          for (const v of list) {
            const restore = canEdit(it) && !v.current
              ? h(
                  'button',
                  {
                    type: 'button',
                    class: 'secondary small',
                    'aria-label': `Restore version ${v.version}`,
                    on: {
                      click: async (e) => {
                        e.target.disabled = true;
                        try {
                          const saved = await api('POST', itemPath(it.id, `/versions/${v.version}/restore`));
                          toast(`Version ${v.version} restored as version ${saved.version}.`);
                          ctl.error('');
                          await load();
                          refresh();
                        } catch (ex) {
                          e.target.disabled = false;
                          if (ex.status !== 401) ctl.error(ex.message);
                        }
                      },
                    },
                  },
                  'Restore',
                )
              : null;
            body.append(
              h(
                'tr',
                null,
                h('td', null, `${v.version}`, v.current ? h('span', { class: 'tag on' }, 'current') : null),
                h('td', null, h('time', { datetime: v.modified, title: fmtFull(v.modified) }, fmtDate(v.modified))),
                h('td', { class: 'col-by' }, v.modified_by || ''),
                h('td', { class: 'num' }, fmtSize(v.size)),
                h('td', { class: 'actions' }, h('a', { class: 'button secondary small', href: contentUrl(it.id, { version: v.version, download: true }), 'aria-label': `Download version ${v.version}` }, 'Download'), ' ', restore),
              ),
            );
          }
          status.textContent = list.length ? '' : 'No versions.';
          status.hidden = !list.length ? false : true;
          table.hidden = !list.length;
        } catch (e) {
          status.textContent = '';
          if (e.status !== 401) ctl.error(e.message);
        }
      };
      load();
    }, { wide: true });
  }

  // ---------------------------------------------------------------------------
  // Sharing
  // ---------------------------------------------------------------------------

  function shareDialog(it) {
    openDialog(`Share ${it.name}`, (ctl) => {
      const access = h('ul', { class: 'access', 'aria-label': 'People and organisations with access' });
      const accessStatus = h('p', { class: 'muted', role: 'status' }, 'Loading…');
      // People.
      const pid = `${ctl.dlg.getAttribute('aria-labelledby')}-p`;
      const who = h('input', { id: pid, type: 'search', autocomplete: 'off', spellcheck: 'false', placeholder: 'Name or exact username', 'aria-describedby': `${pid}-help` });
      const prole = h('select', { 'aria-label': 'Access for this person' }, h('option', { value: 'view' }, 'Can view'), h('option', { value: 'edit' }, 'Can edit'));
      const suggestions = h('ul', { class: 'suggest', 'aria-label': 'Matching people' });
      const add = h('button', { type: 'submit' }, 'Share');
      const peopleForm = h(
        'form',
        { class: 'stack', on: { submit: (e) => (e.preventDefault(), grantPerson()) } },
        h('label', { for: pid }, 'Share with a person'),
        h('div', { class: 'row' }, who, prole, add),
        h('p', { id: `${pid}-help`, class: 'muted small' }, 'People in your organisations are suggested; anyone else by their exact username.'),
        suggestions,
      );
      // Organisations.
      const orgs = S.me.orgs || [];
      const oid = `${ctl.dlg.getAttribute('aria-labelledby')}-o`;
      const orgSel = h('select', { id: oid }, orgs.map((o) => h('option', { value: o.id }, o.name)));
      const orole = h('select', { 'aria-label': 'Access for the organisation' }, h('option', { value: 'view' }, 'Can view'), h('option', { value: 'edit' }, 'Can edit'));
      const orgForm = orgs.length
        ? h('form', { class: 'stack', on: { submit: (e) => (e.preventDefault(), grantOrg()) } }, h('label', { for: oid }, 'Share with everyone in an organisation'), h('div', { class: 'row' }, orgSel, orole, h('button', { type: 'submit', class: 'secondary' }, 'Share')))
        : null;
      // Links.
      const linksBox = h('div', { class: 'links' });
      ctl.content.append(peopleForm, orgForm, h('h3', null, 'Who has access'), accessStatus, access, linksBox);
      ctl.actions(h('button', { type: 'button', on: { click: () => ctl.close() } }, 'Done'));

      let timer = null;
      who.addEventListener('input', () => {
        clearTimeout(timer);
        timer = setTimeout(search, 250);
      });
      async function search() {
        const q = who.value.trim();
        clear(suggestions);
        if (!q) return;
        try {
          const r = await api('GET', `/api/users?query=${encodeURIComponent(q)}`);
          if (who.value.trim() !== q) return;
          for (const u of r.users.slice(0, 8)) {
            suggestions.append(
              h(
                'li',
                null,
                h(
                  'button',
                  {
                    type: 'button',
                    class: 'suggest-item',
                    on: {
                      click: () => {
                        who.value = u.username;
                        clear(suggestions);
                        who.focus();
                      },
                    },
                  },
                  h('span', null, u.display_name || u.username),
                  ' ',
                  h('span', { class: 'muted' }, u.username),
                ),
              ),
            );
          }
        } catch (e) {
          if (e.status !== 401) ctl.error(e.message);
        }
      }
      async function grantPerson() {
        const username = who.value.trim();
        if (!username) return ctl.error('Enter a username.');
        add.disabled = true;
        try {
          await api('POST', itemPath(it.id, '/permissions'), { username, role: prole.value });
          who.value = '';
          clear(suggestions);
          ctl.error('');
          toast(`Shared with ${username}.`);
          await loadAccess();
        } catch (e) {
          if (e.status !== 401) ctl.error(e.message);
        } finally {
          add.disabled = false;
        }
      }
      async function grantOrg() {
        try {
          await api('POST', itemPath(it.id, '/permissions'), { org: orgSel.value, role: orole.value });
          ctl.error('');
          toast(`Shared with everyone in ${orgSel.selectedOptions[0].textContent}.`);
          await loadAccess();
        } catch (e) {
          if (e.status !== 401) ctl.error(e.message);
        }
      }
      const roleText = (r) => (r === 'edit' ? 'Can edit' : r === 'owner' ? 'Owner' : 'Can view');
      async function loadAccess() {
        try {
          const r = await api('GET', itemPath(it.id, '/permissions'));
          clear(access);
          if (r.owner) access.append(h('li', null, h('span', { class: 'who' }, r.owner.display_name || r.owner.username, r.owner.username ? h('span', { class: 'muted' }, ` ${r.owner.username}`) : null), h('span', { class: 'role' }, r.owner.org ? 'Organisation folder' : 'Owner')));
          for (const p of r.permissions) {
            access.append(
              h(
                'li',
                null,
                h('span', { class: 'who' }, p.display_name || p.username, h('span', { class: 'muted' }, ` ${p.username}`)),
                h('span', { class: 'role' }, roleText(p.role), p.inherited ? h('span', { class: 'muted small' }, ` from ${p.inherited_from ? p.inherited_from.name : 'a parent folder'}`) : null),
                p.inherited ? null : removeBtn(`Remove ${p.display_name || p.username}`, itemPath(it.id, `/permissions/${encodeURIComponent(p.username)}`)),
              ),
            );
          }
          for (const g of r.orgs) {
            access.append(
              h(
                'li',
                null,
                h('span', { class: 'who' }, h('span', { class: 'ficon org', 'aria-hidden': 'true' }), `Everyone in ${g.org.name}`),
                h('span', { class: 'role' }, roleText(g.role), g.inherited ? h('span', { class: 'muted small' }, ` from ${g.inherited_from ? g.inherited_from.name : 'a parent folder'}`) : null),
                g.inherited ? null : removeBtn(`Remove ${g.org.name}`, itemPath(it.id, `/permissions/org/${encodeURIComponent(g.org.id)}`)),
              ),
            );
          }
          accessStatus.textContent = r.permissions.length || r.orgs.length ? '' : 'Only the owner has access so far.';
        } catch (e) {
          accessStatus.textContent = '';
          if (e.status !== 401) ctl.error(e.message);
        }
      }
      function removeBtn(label, path) {
        return h(
          'button',
          {
            type: 'button',
            class: 'link danger-text',
            'aria-label': label,
            on: {
              click: async () => {
                try {
                  await api('DELETE', path);
                  ctl.error('');
                  await loadAccess();
                } catch (e) {
                  if (e.status !== 401) ctl.error(e.message);
                }
              },
            },
          },
          'Remove',
        );
      }
      loadAccess();
      if (!S.me.server || S.me.server.allow_links !== false) linksSection(it, linksBox, ctl);
    }, { wide: true });
  }

  async function linksSection(it, box, ctl) {
    const list = h('ul', { class: 'access', 'aria-label': 'Share links' });
    const fresh = h('div', { class: 'fresh-link', hidden: true });
    const lid = `${ctl.dlg.getAttribute('aria-labelledby')}-l`;
    const role = h('select', { id: `${lid}-r` }, h('option', { value: 'view' }, 'Can view'), h('option', { value: 'edit' }, 'Can edit'));
    const days = h('select', { id: `${lid}-d` }, h('option', { value: '0' }, 'Never'), h('option', { value: '1' }, '1 day'), h('option', { value: '7', selected: true }, '7 days'), h('option', { value: '30' }, '30 days'));
    const pw = h('input', { id: `${lid}-p`, type: 'password', autocomplete: 'new-password', placeholder: 'Optional' });
    const create = h('button', { type: 'submit', class: 'secondary' }, 'Create link');
    const form = h(
      'form',
      { class: 'linkform', on: { submit: (e) => (e.preventDefault(), make()) } },
      h('label', null, 'Access', role),
      h('label', null, 'Expires', days),
      h('label', null, 'Password', pw),
      create,
    );
    box.append(h('h3', null, 'Share links'), h('p', { class: 'muted small' }, 'Anyone with a link can open this item without an account. Each link is shown once, when you create it.'), form, fresh, list);
    const load = async () => {
      try {
        const r = await api('GET', itemPath(it.id, '/links'));
        clear(list);
        for (const l of r.links) {
          const bits = [l.role === 'edit' ? 'Can edit' : 'Can view', l.expired ? 'expired' : l.expires ? `until ${fmtDate(l.expires)}` : 'no expiry', l.has_password ? 'password' : null].filter(Boolean).join(' · ');
          list.append(
            h(
              'li',
              null,
              h('span', { class: 'who' }, `Link by ${l.created_by || 'someone'}`, h('span', { class: 'muted small' }, ` ${fmtDate(l.created)}`)),
              h('span', { class: 'role' }, bits),
              h(
                'button',
                {
                  type: 'button',
                  class: 'link danger-text',
                  'aria-label': `Delete the link created ${fmtDate(l.created)}`,
                  on: {
                    click: async () => {
                      try {
                        await api('DELETE', itemPath(it.id, `/links/${encodeURIComponent(l.id)}`));
                        await load();
                      } catch (e) {
                        if (e.status !== 401) ctl.error(e.message);
                      }
                    },
                  },
                },
                'Delete',
              ),
            ),
          );
        }
      } catch (e) {
        if (e.status === 403) box.hidden = true;
        else if (e.status !== 401) ctl.error(e.message);
      }
    };
    const make = async () => {
      create.disabled = true;
      try {
        const body = { role: role.value, expires_days: Number(days.value) };
        if (pw.value) body.password = pw.value;
        const l = await api('POST', itemPath(it.id, '/links'), body);
        pw.value = '';
        const field = h('input', { type: 'text', readonly: true, value: l.url, 'aria-label': 'The new link', class: 'secret' });
        const copy = h(
          'button',
          {
            type: 'button',
            class: 'secondary',
            on: {
              click: async () => {
                try {
                  await navigator.clipboard.writeText(l.url);
                  copy.textContent = 'Copied';
                } catch {
                  field.select();
                }
              },
            },
          },
          'Copy',
        );
        clear(fresh).append(h('p', { class: 'small' }, "Copy the link now: it won't be shown again."), h('div', { class: 'row' }, field, copy));
        fresh.hidden = false;
        field.focus();
        field.select();
        ctl.error('');
        await load();
      } catch (e) {
        if (e.status === 403 && e.code === 'links_disabled') {
          clear(box).append(h('h3', null, 'Share links'), h('p', { class: 'muted small' }, 'Share links are switched off on this server.'));
        } else if (e.status !== 401) ctl.error(e.message);
      } finally {
        create.disabled = false;
      }
    };
    load();
  }

  // ---------------------------------------------------------------------------
  // Uploads (XMLHttpRequest: fetch has no upload progress)
  // ---------------------------------------------------------------------------

  const uploads = [];
  const MAX_PARALLEL = 3; // the server takes about 4 at once per person (429 beyond)
  let refreshTimer = null;

  function maxUploadBytes() {
    const mb = S.me && S.me.server && S.me.server.max_upload_mb;
    return mb ? mb * 1024 * 1024 : Infinity;
  }

  async function startUploads(files, folder) {
    if (!files.length || !folder) return;
    const names = new Map();
    if (S.folder && S.folder.id === folder.id) for (const it of S.items) names.set(it.name.toLowerCase(), it);
    let forAll = null;
    const list = [...files];
    for (let i = 0; i < list.length; i++) {
      const file = list[i];
      const u = addUpload(file, folder);
      if (file.size > maxUploadBytes()) {
        finish(u, 'error', `Too large: this server takes files up to ${S.me.server.max_upload_mb} MB.`);
        continue;
      }
      const existing = names.get(file.name.toLowerCase());
      if (existing) {
        const c = forAll || (await askConflict(file.name, existing.kind === 'folder', list.length - i - 1));
        if (c.all) forAll = c;
        if (c.choice === 'skip' || !c.choice) {
          finish(u, 'skipped', 'Skipped');
          continue;
        }
        u.conflict = existing.kind === 'folder' && c.choice === 'replace' ? 'rename' : c.choice;
      }
      names.set(file.name.toLowerCase(), { kind: 'file' });
      u.state = 'queued';
      u.status.textContent = 'Waiting…';
    }
    pump();
  }

  async function askConflict(name, isFolder, remaining) {
    const all = h('input', { type: 'checkbox', id: `all-${dlgSeq + 1}` });
    const extra = remaining > 0 ? h('label', { class: 'check', for: all.id }, all, `Do the same for other files that already exist`) : null;
    const buttons = [
      isFolder ? null : { label: 'Replace', value: 'replace', kind: 'secondary' },
      { label: 'Keep both', value: 'rename', kind: isFolder ? '' : 'secondary' },
      { label: 'Skip', value: 'skip', kind: 'secondary' },
    ].filter(Boolean);
    const msg = isFolder ? `There's already a folder named “${name}” here. Upload the file under another name?` : `“${name}” already exists here. Replace it (the old file stays in its version history), keep both, or skip this file?`;
    const choice = await choose('File already exists', msg, buttons, extra);
    return { choice, all: !!(extra && all.checked && choice) };
  }

  function addUpload(file, folder) {
    const bar = h('progress', { max: '1', value: '0', 'aria-label': `Upload of ${file.name}` });
    const status = h('span', { class: 'up-status muted small' });
    const cancel = h('button', { type: 'button', class: 'link', 'aria-label': `Cancel the upload of ${file.name}` }, 'Cancel');
    const li = h('li', { class: 'upload' }, h('div', { class: 'up-row' }, h('span', { class: 'up-name' }, file.name), h('span', { class: 'up-size muted small' }, fmtSize(file.size)), cancel), bar, status);
    const u = { file, name: file.name, parent: folder.id, folderName: folder.name, conflict: 'fail', state: 'new', xhr: null, li, bar, status, cancel, retried: false };
    cancel.addEventListener('click', () => cancelUpload(u));
    uploads.push(u);
    $('upload-list').append(li);
    show($('uploads'), true);
    updateUploadsTitle();
    return u;
  }

  function cancelUpload(u) {
    if (u.state === 'uploading' && u.xhr) u.xhr.abort();
    else if (['queued', 'new', 'conflict', 'waiting'].includes(u.state)) finish(u, 'cancelled', 'Cancelled');
  }

  function abortAllUploads() {
    for (const u of uploads) if (['uploading', 'queued', 'waiting'].includes(u.state)) cancelUpload(u);
  }

  function finish(u, state, text) {
    u.state = state;
    u.status.textContent = text;
    u.li.classList.add(state);
    u.cancel.hidden = true;
    if (state === 'done') u.bar.value = u.bar.max;
    updateUploadsTitle();
    pump();
  }

  function updateUploadsTitle() {
    const busy = uploads.filter((u) => ['uploading', 'queued', 'waiting'].includes(u.state)).length;
    const done = uploads.filter((u) => u.state === 'done').length;
    $('uploads-title').textContent = busy ? `Uploading ${busy} file${busy === 1 ? '' : 's'}` : `Uploads: ${done} done`;
  }

  function pump() {
    let active = uploads.filter((u) => u.state === 'uploading').length;
    for (const u of uploads) {
      if (active >= MAX_PARALLEL) break;
      if (u.state === 'queued') {
        send(u);
        active++;
      }
    }
  }

  function send(u) {
    u.state = 'uploading';
    u.status.textContent = 'Starting…';
    const xhr = new XMLHttpRequest();
    u.xhr = xhr;
    xhr.open('POST', uploadUrl(u.parent, u.name, u.conflict));
    xhr.withCredentials = true;
    xhr.setRequestHeader('X-FO-CSRF', S.csrf);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.upload.addEventListener('progress', (e) => {
      if (!e.lengthComputable) return;
      u.bar.max = e.total || 1;
      u.bar.value = e.loaded;
      u.status.textContent = `${Math.floor((e.loaded / (e.total || 1)) * 100)}% · ${fmtSize(e.loaded)} of ${fmtSize(e.total)}`;
    });
    xhr.addEventListener('load', async () => {
      let j = {};
      try {
        j = JSON.parse(xhr.responseText || '{}');
      } catch {
        /* not JSON */
      }
      const st = xhr.status;
      if (st === 200 || st === 201) {
        finish(u, 'done', st === 200 ? `Uploaded as a new version of ${j.name || u.name}` : j.name && j.name !== u.name ? `Uploaded as ${j.name}` : 'Uploaded');
        scheduleRefresh(u.parent);
      } else if (st === 401) {
        finish(u, 'error', 'Not uploaded: signed out.');
        signedOut('Your session has ended. Sign in again.');
      } else if (st === 403 && j.code === 'csrf_failed' && !u.retried) {
        u.retried = true;
        u.state = 'queued';
        try {
          if (await refreshSession()) return pump();
        } catch {
          /* below */
        }
        finish(u, 'error', 'Not uploaded: signed out.');
      } else if (st === 429 && (u.busyRetries || 0) < 3) {
        // Too many uploads at once (other tabs or devices): wait and try again.
        u.busyRetries = (u.busyRetries || 0) + 1;
        u.state = 'waiting';
        u.status.textContent = 'The server is busy with other uploads; trying again shortly…';
        setTimeout(() => {
          if (u.state !== 'waiting') return;
          u.state = 'queued';
          pump();
        }, 3000 * u.busyRetries);
      } else if (st === 429) {
        finish(u, 'error', 'Not uploaded: too many uploads are running at once. Try again when the others are done.');
      } else if (st === 409 && u.conflict === 'fail') {
        u.state = 'conflict';
        u.status.textContent = 'Already exists';
        const c = await askConflict(u.name, false, 0);
        if (c.choice === 'replace' || c.choice === 'rename') {
          u.conflict = c.choice;
          u.state = 'queued';
          pump();
        } else finish(u, 'skipped', 'Skipped');
      } else {
        finish(u, 'error', friendly(st, j));
      }
    });
    // Also when the server ends a stalled upload (no progress for a minute).
    xhr.addEventListener('error', () => finish(u, 'error', 'The upload was interrupted. Check the connection and try again.'));
    xhr.addEventListener('abort', () => finish(u, 'cancelled', 'Cancelled'));
    xhr.send(u.file);
  }

  function scheduleRefresh(folderId) {
    if (!S.folder || S.folder.id !== folderId) return;
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => loadView(), 300);
  }

  function filesFromDrop(e) {
    const dt = e.dataTransfer;
    const files = [];
    let folders = 0;
    if (dt.items && dt.items.length) {
      for (const item of dt.items) {
        if (item.kind !== 'file') continue;
        const entry = item.webkitGetAsEntry ? item.webkitGetAsEntry() : null;
        if (entry && entry.isDirectory) {
          folders++;
          continue;
        }
        const f = item.getAsFile();
        if (f) files.push(f);
      }
    } else {
      files.push(...dt.files);
    }
    if (folders) toast("Folders can't be uploaded by dropping them yet: open the folder and drop the files inside.", { error: true });
    return files;
  }

  function canDropHere() {
    return S.view && S.view.kind === 'folder' && canEdit(S.folder);
  }

  // ---------------------------------------------------------------------------
  // Wiring
  // ---------------------------------------------------------------------------

  function runAction(act) {
    const t = targets();
    const one = t.length === 1 ? t[0] : null;
    switch (act) {
      case 'open':
        return one && openItem(one);
      case 'sheets':
        return one && openInSheets(one);
      case 'download':
        return one && download(one);
      case 'share':
        return one && shareDialog(one);
      case 'rename':
        return one && renameDialog(one);
      case 'move':
        return moveDialog(t.filter(canEdit));
      case 'versions':
        return one && versionsDialog(one);
      case 'restore':
        return restoreItems(t);
      case 'delete':
        return deleteItems(t.filter(canEdit));
    }
  }

  function closeNavOnPhone() {
    const nav = $('sidenav');
    if (nav.classList.contains('open')) {
      nav.classList.remove('open');
      $('nav-toggle').setAttribute('aria-expanded', 'false');
    }
  }

  function wire() {
    $('signin-form').addEventListener('submit', onSignIn);
    $('pw-form').addEventListener('submit', onPasswordChange);
    $('pw-signout').addEventListener('click', signOut);
    $('pw-cancel').addEventListener('click', () => {
      if (!pwForced && S.me) {
        screen('shell');
        $('main').focus();
      }
    });
    $('pw-open').addEventListener('click', () => showPasswordChange(false));
    $('signout').addEventListener('click', signOut);
    $('nav-toggle').addEventListener('click', () => {
      const nav = $('sidenav');
      const open = !nav.classList.contains('open');
      nav.classList.toggle('open', open);
      $('nav-toggle').setAttribute('aria-expanded', open ? 'true' : 'false');
      if (open) nav.querySelector('a').focus();
    });
    window.addEventListener('hashchange', () => route());
    $('act-new-folder').addEventListener('click', newFolderDialog);
    $('act-upload').addEventListener('click', () => $('file-input').click());
    $('file-input').addEventListener('change', (e) => {
      const files = [...e.target.files];
      e.target.value = '';
      startUploads(files, S.folder);
    });
    for (const b of document.querySelectorAll('#toolbar [data-act]')) b.addEventListener('click', () => runAction(b.dataset.act));
    for (const b of document.querySelectorAll('#list button.sort')) {
      b.addEventListener('click', () => {
        const key = b.dataset.sort;
        S.sort = S.sort.key === key ? { key, dir: -S.sort.dir } : { key, dir: key === 'name' || key === 'by' ? 1 : -1 };
        saveSort();
        render();
      });
    }
    $('select-all').addEventListener('change', (e) => {
      S.selected = e.target.checked ? new Set(S.items.map((i) => i.id)) : new Set();
      updateSelection();
    });
    const rows = $('rows');
    rows.addEventListener('click', onRowClick);
    rows.addEventListener('dblclick', onRowDblClick);
    rows.addEventListener('keydown', onListKey);
    rows.addEventListener('focusin', (e) => {
      const tr = e.target.closest('tr.row');
      if (tr && tr.dataset.id !== S.focusId) {
        S.focusId = tr.dataset.id;
        for (const r of rows.children) r.tabIndex = r === tr ? 0 : -1;
      }
    });
    // Drag and drop onto the list.
    const zone = $('dropzone');
    let depth = 0;
    const hasFiles = (e) => e.dataTransfer && [...e.dataTransfer.types].includes('Files');
    zone.addEventListener('dragenter', (e) => {
      if (!hasFiles(e) || !canDropHere()) return;
      e.preventDefault();
      depth++;
      zone.classList.add('dragging');
    });
    zone.addEventListener('dragover', (e) => {
      if (!hasFiles(e) || !canDropHere()) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = 'copy';
    });
    zone.addEventListener('dragleave', () => {
      depth = Math.max(0, depth - 1);
      if (!depth) zone.classList.remove('dragging');
    });
    zone.addEventListener('drop', (e) => {
      depth = 0;
      zone.classList.remove('dragging');
      if (!hasFiles(e)) return;
      e.preventDefault();
      if (!canDropHere()) return toast("You can't upload here.", { error: true });
      startUploads(filesFromDrop(e), S.folder);
    });
    // Files dropped anywhere else must not replace the page.
    window.addEventListener('dragover', (e) => hasFiles(e) && e.preventDefault());
    window.addEventListener('drop', (e) => hasFiles(e) && e.preventDefault());
    $('uploads-hide').addEventListener('click', () => show($('uploads'), false));
    $('uploads-clear').addEventListener('click', () => {
      for (let i = uploads.length - 1; i >= 0; i--) {
        if (!['uploading', 'queued', 'conflict', 'new', 'waiting'].includes(uploads[i].state)) {
          uploads[i].li.remove();
          uploads.splice(i, 1);
        }
      }
      updateUploadsTitle();
      if (!uploads.length) show($('uploads'), false);
    });
    window.addEventListener('beforeunload', (e) => {
      if (uploads.some((u) => ['uploading', 'queued', 'waiting'].includes(u.state))) {
        e.preventDefault();
        e.returnValue = '';
      }
    });
  }

  async function boot() {
    wire();
    try {
      const s = await refreshSession();
      if (!s) return showSignIn();
      if (s.user.must_change_password) return showPasswordChange(true);
      await enterApp();
    } catch (e) {
      if (e.status === 401) return;
      screen('boot');
      clear($('boot')).append(h('p', null, `FreedomOffice Drive couldn't start. ${e.message}`), h('button', { type: 'button', on: { click: () => location.reload() } }, 'Try again'));
    }
  }

  // For tests in a browser console only; nothing secret in here.
  window.FODrive = { fmtSize, fmtDate, compareItems, contentUrl, sheetsUrl, uploadUrl, kindOf, baseRange, friendly };

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot);
  else boot();
})();
