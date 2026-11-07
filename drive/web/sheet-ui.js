// FreedomOffice Sheets in the browser: the editing interface (sheet-ui.js).
//
// Loaded after sheet.js, which owns the grid, the layout, drawing and the live connection and
// hands this file what it needs as `window.FSheet`. Here: bottom sheets (action menus, colour
// palettes, number formats...), the formatting toolbar, row/column/sheet menus, clipboard,
// find and go-to, keyboard shortcuts and the chart painter. Everything is built with
// createElement / textContent (never HTML strings); the page's CSP forbids inline script and
// style, so colours and sizes are set through CSSOM.
(() => {
  'use strict';
  const F = window.FSheet;
  if (!F) return;
  const { S, $, toast, clamp } = F;
  const KEYC = 16384;
  const MAXC = 16384;
  const MAXR = 1048576;
  const MAX_PASTE_CELLS = 20000;
  const MAX_PASTE_CHARS = 4 * 1024 * 1024;

  // ---------------------------------------------------------------------------
  // Small helpers
  // ---------------------------------------------------------------------------

  /** createElement with attributes, event handlers and children (strings, nodes, arrays; falsy skipped). */
  function h(tag, attrs, ...kids) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === 'on') for (const [ev, fn] of Object.entries(v)) e.addEventListener(ev, fn);
      else if (k === 'class') e.className = v;
      else if (/^on/i.test(k)) throw new Error('no event handler attributes');
      else e.setAttribute(k, v === true ? '' : String(v));
    }
    const add = (k) => {
      if (k === null || k === undefined || k === false) return;
      if (Array.isArray(k)) k.forEach(add);
      else e.append(typeof k === 'string' || typeof k === 'number' ? document.createTextNode(String(k)) : k);
    };
    kids.forEach(add);
    return e;
  }

  const NS = 'http://www.w3.org/2000/svg';
  const ICONS = {
    left: 'M4 6h16M4 10h10M4 14h16M4 18h10',
    center: 'M4 6h16M7 10h10M4 14h16M7 18h10',
    right: 'M4 6h16M10 10h10M4 14h16M10 18h10',
    top: 'M5 4h14M12 9v11M8 12l4-4 4 4',
    middle: 'M5 12h14M12 4v5M9 6l3 3 3-3M12 20v-5M9 18l3-3 3 3',
    bottom: 'M5 20h14M12 4v11M8 12l4 4 4-4',
    wrap: 'M4 6h16M4 11h12a3.5 3.5 0 0 1 0 7h-5M11 18l2.5-2.5M11 18l2.5 2.5M4 16h4',
    borders: 'M4 4h16v16H4zM4 12h16M12 4v16',
    merge: 'M3 6h18v12H3zM8 12H3M16 12h5M8 9l-3 3 3 3M16 9l3 3-3 3',
    freeze: 'M3 4h18v16H3zM3 10h18M10 4v16',
    eraser: 'M4 15l8-9 8 6-6 7H8z',
    plus: 'M12 5v14M5 12h14',
  };
  function icon(name) {
    const s = document.createElementNS(NS, 'svg');
    s.setAttribute('viewBox', '0 0 24 24');
    s.setAttribute('width', '20');
    s.setAttribute('height', '20');
    s.setAttribute('aria-hidden', 'true');
    const p = document.createElementNS(NS, 'path');
    p.setAttribute('d', ICONS[name]);
    p.setAttribute('fill', 'none');
    p.setAttribute('stroke', 'currentColor');
    p.setAttribute('stroke-width', '1.8');
    p.setAttribute('stroke-linecap', 'round');
    p.setAttribute('stroke-linejoin', 'round');
    s.append(p);
    return s;
  }

  const isEdit = () => F.canEdit();
  const rng = () => F.normRange(S.rng);
  const meta = () => S.meta.sheets[S.cur];
  const styleAt = (r, c) => (S.cells.get(r * KEYC + c) || {}).st || null;
  const send = (m) => F.liveSend(m);
  const fmt = (set) => send(F.selMsg('fmt', { set }));
  const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;
  const num = (r, c) => `${F.colName(c)}${r + 1}`;

  // ---------------------------------------------------------------------------
  // Bottom sheets and dialogs
  // ---------------------------------------------------------------------------

  let current = null;

  function closeSheet() {
    if (!current) return;
    const o = current;
    current = null;
    document.removeEventListener('keydown', o.key, true);
    $('dlg-root').replaceChildren();
    if (o.onClose) o.onClose();
    $('scroller').focus({ preventScroll: true });
  }

  function openSheet(title, body, opts = {}) {
    closeSheet();
    const sheet = h('div', { class: 'sheet', role: 'dialog', 'aria-modal': 'true', 'aria-label': title || 'Menu' }, h('div', { class: 'grab' }), title ? h('h2', null, title) : null, body);
    const bg = h(
      'div',
      {
        class: 'sheet-bg',
        on: {
          click: (e) => {
            if (e.target === bg) closeSheet();
          },
        },
      },
      sheet,
    );
    $('dlg-root').append(bg);
    const key = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        closeSheet();
      }
    };
    document.addEventListener('keydown', key, true);
    current = { key, onClose: opts.onClose };
    if (opts.focus !== false) {
      // Inputs get the focus; a menu keeps it on the dialog (no ring on the first item).
      const inp = sheet.querySelector('input');
      sheet.tabIndex = -1;
      (inp || sheet).focus({ preventScroll: true });
    }
    return sheet;
  }

  function item(label, fn, o = {}) {
    return h(
      'button',
      {
        type: 'button',
        class: o.danger ? 'sitem danger' : 'sitem',
        disabled: !!o.disabled,
        'aria-pressed': o.pressed === undefined ? null : String(!!o.pressed),
        on: {
          click: () => {
            if (!o.keep) closeSheet();
            fn();
          },
        },
      },
      o.icon ? h('span', { class: 'ic' }, o.icon) : null,
      h('span', null, label),
      o.hint ? h('span', { class: 'hint' }, o.hint) : null,
    );
  }

  /** A line of text and a button; resolves with the text or null. */
  function askText(title, value, okLabel, maxlength) {
    return new Promise((resolve) => {
      let done = false;
      const fin = (v) => {
        if (done) return;
        done = true;
        closeSheet();
        resolve(v);
      };
      const inp = h('input', { type: 'text', value: value || '', maxlength: maxlength || 200, autocomplete: 'off', autocapitalize: 'none', spellcheck: 'false', 'aria-label': title });
      inp.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          fin(inp.value);
        }
      });
      openSheet(title, h('div', { class: 'srow' }, inp, h('button', { type: 'button', on: { click: () => fin(inp.value) } }, okLabel || 'OK')), {
        onClose: () => {
          if (!done) {
            done = true;
            resolve(null);
          }
        },
      });
      inp.select();
    });
  }

  function confirmBox(message, okLabel) {
    return new Promise((resolve) => {
      let done = false;
      const fin = (v) => {
        if (done) return;
        done = true;
        closeSheet();
        resolve(v);
      };
      openSheet('Are you sure?', [h('p', { class: 'smsg' }, message), h('div', { class: 'srow' }, h('button', { type: 'button', class: 'secondary', on: { click: () => fin(false) } }, 'Cancel'), h('button', { type: 'button', on: { click: () => fin(true) } }, okLabel || 'OK'))], {
        onClose: () => {
          if (!done) {
            done = true;
            resolve(false);
          }
        },
      });
    });
  }

  // ---------------------------------------------------------------------------
  // Colours
  // ---------------------------------------------------------------------------

  function mix(hex, t, toward) {
    const n = parseInt(hex.slice(1), 16);
    const ch = (s) => Math.round(((n >> s) & 255) * (1 - t) + toward * t);
    return `#${((1 << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)).toString(16).slice(1)}`;
  }

  const PALETTE = (() => {
    const rows = [['#ffffff', '#f2f2f2', '#d9d9d9', '#a6a6a6', '#7f7f7f', '#595959', '#262626', '#000000']];
    const hues = ['#e53935', '#fb8c00', '#fdd835', '#43a047', '#00897b', '#1e88e5', '#3949ab', '#8e24aa'];
    rows.push(hues.map((c) => mix(c, 0.8, 255)));
    rows.push(hues.map((c) => mix(c, 0.55, 255)));
    rows.push(hues);
    rows.push(hues.map((c) => mix(c, 0.3, 0)));
    rows.push(hues.map((c) => mix(c, 0.55, 0)));
    return rows;
  })();

  let lastColor = '#e53935';
  let lastFill = '#fdd835';

  function colourSheet(title, key, current0, onPick, noneLabel) {
    const grid = h('div', { class: 'sgrid' });
    for (const row of PALETTE) {
      for (const c of row) {
        const b = h('button', { type: 'button', class: 'swatch', 'aria-label': c, 'aria-pressed': current0 && current0.toLowerCase() === c ? 'true' : 'false', on: { click: () => (closeSheet(), onPick(c)) } });
        b.style.background = c;
        grid.append(b);
      }
    }
    const hex = h('input', { type: 'text', placeholder: '#rrggbb', maxlength: 7, autocapitalize: 'none', spellcheck: 'false', 'aria-label': 'Custom colour as #rrggbb' });
    const apply = () => {
      const v = hex.value.trim().toLowerCase();
      if (/^#[0-9a-f]{6}$/.test(v)) (closeSheet(), onPick(v));
      else toast('Type a colour like #1e88e5.', 'error');
    };
    hex.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        apply();
      }
    });
    return openSheet(title, [grid, h('div', { class: 'srow' }, hex, h('button', { type: 'button', class: 'secondary', on: { click: apply } }, 'Use')), item(noneLabel, () => onPick(null))], { focus: false });
  }

  // ---------------------------------------------------------------------------
  // Formatting
  // ---------------------------------------------------------------------------

  const NUMBER_FORMATS = [
    ['General', ''],
    ['Number', '0.00'],
    ['Number with thousands', '#,##0.00'],
    ['Whole number', '0'],
    ['Currency', '$#,##0.00'],
    ['Percent', '0%'],
    ['Percent, 2 decimals', '0.00%'],
    ['Date', 'm/d/yyyy'],
    ['Date, year first', 'yyyy-mm-dd'],
    ['Date, long', 'd mmmm yyyy'],
    ['Time', 'h:mm AM/PM'],
    ['Date and time', 'm/d/yyyy h:mm'],
    ['Scientific', '0.00E+00'],
    ['Text', '@'],
  ];

  function numberSheet() {
    const cur = (S.cells.get(S.sel.r * KEYC + S.sel.c) || {}).nf;
    openSheet('Number format', NUMBER_FORMATS.map(([name, code]) => item(name, () => fmt({ nf: code }), { hint: code || '', pressed: cur === code })));
  }

  function bordersSheet() {
    const presets = [
      ['All borders', 'all'],
      ['Outer border', 'outer'],
      ['Thick outer border', 'thick_outer'],
      ['Top border', 'top'],
      ['Bottom border', 'bottom'],
      ['Left border', 'left'],
      ['Right border', 'right'],
      ['No border', 'none'],
    ];
    openSheet('Borders', presets.map(([name, p]) => item(name, () => fmt({ bd: p }), { icon: icon('borders') })));
  }

  function alignSheet() {
    openSheet('Alignment', [
      h('h2', null, 'Horizontal'),
      item('Left', () => fmt({ h: 'l' }), { icon: icon('left') }),
      item('Center', () => fmt({ h: 'c' }), { icon: icon('center') }),
      item('Right', () => fmt({ h: 'r' }), { icon: icon('right') }),
      item('Automatic (numbers right, text left)', () => fmt({ h: 'g' })),
      h('h2', null, 'Vertical'),
      item('Top', () => fmt({ v: 't' }), { icon: icon('top') }),
      item('Middle', () => fmt({ v: 'c' }), { icon: icon('middle') }),
      item('Bottom', () => fmt({ v: 'b' }), { icon: icon('bottom') }),
      h('h2', null, 'Indent'),
      h('div', { class: 'chips' }, [0, 1, 2, 3, 4].map((n) => h('button', { type: 'button', class: 'secondary', on: { click: () => (closeSheet(), fmt({ ind: n })) } }, String(n)))),
    ]);
  }

  function sizeTextSheet() {
    const st = styleAt(S.sel.r, S.sel.c);
    let pt = st ? Math.round(st.size * 0.75 * 10) / 10 : 11;
    const inp = h('input', { type: 'text', inputmode: 'decimal', value: String(pt), 'aria-label': 'Text size in points' });
    const set = (v) => {
      pt = clamp(Math.round(v * 2) / 2, 6, 72);
      inp.value = String(pt);
    };
    inp.addEventListener('change', () => set(Number(inp.value) || pt));
    openSheet('Text size', [
      h('div', { class: 'stepper' }, h('button', { type: 'button', class: 'secondary', 'aria-label': 'Smaller', on: { click: () => set(pt - 1) } }, '−'), inp, h('button', { type: 'button', class: 'secondary', 'aria-label': 'Larger', on: { click: () => set(pt + 1) } }, '+'), h('button', { type: 'button', on: { click: () => (closeSheet(), fmt({ z: Number(inp.value) || pt })) } }, 'Apply')),
      h('div', { class: 'chips' }, [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 36].map((n) => h('button', { type: 'button', class: 'secondary', on: { click: () => (closeSheet(), fmt({ z: n })) } }, String(n)))),
    ]);
  }

  function clearSheet() {
    openSheet('Clear', [item('Clear contents', () => send(F.selMsg('clear'))), item('Clear formats', () => fmt({ reset: true })), item('Clear all', () => (send(F.selMsg('clear')), fmt({ reset: true })), { danger: true })]);
  }

  function overlapsMerge() {
    const g = rng();
    const L = F.layoutOf(S.cur);
    return L.merges.some((m) => !(m[2] < g.r1 || m[0] > g.r2 || m[3] < g.c1 || m[1] > g.c2));
  }

  function toggleMerge() {
    const g = rng();
    if (overlapsMerge()) return send(F.selMsg('merge', { on: false, clamp: false }));
    if (g.r1 === g.r2 && g.c1 === g.c2) return toast('Select more than one cell to merge.', 'error');
    send(F.selMsg('merge', { on: true, clamp: false }));
  }

  function freezeSheet() {
    const L = F.layoutOf(S.cur);
    const g = rng();
    const cur = L.fr || L.fc ? `${plural(L.fr, 'row')}, ${plural(L.fc, 'column')} frozen` : 'Nothing frozen';
    const frz = (r, c) => send(F.sheetMsg('freeze', { r, c }));
    openSheet('Freeze panes', [
      h('p', { class: 'smsg' }, cur),
      item('Freeze top row', () => frz(1, 0)),
      item('Freeze first column', () => frz(0, 1)),
      item(`Freeze rows above ${g.r1 + 1} and columns left of ${F.colName(g.c1)}`, () => frz(S.kind === 'cols' ? 0 : g.r1, S.kind === 'rows' ? 0 : g.c1), { disabled: S.kind === 'all' }),
      item('Unfreeze', () => frz(0, 0), { disabled: !L.fr && !L.fc }),
    ]);
  }

  // ---------------------------------------------------------------------------
  // The tool row
  // ---------------------------------------------------------------------------

  const TOOLS = [];

  function tool(id, content, aria, run, pressed) {
    const b = h('button', { type: 'button', class: 'tool', id: `t-${id}`, 'aria-label': aria, 'aria-pressed': pressed ? 'false' : null, on: { click: run } }, content);
    TOOLS.push({ id, b, pressed });
    return b;
  }

  function colourTool(id, glyph, aria, run) {
    const sw = h('span', { class: 'sw' });
    const b = tool(id, [h('span', null, glyph), sw], aria, run);
    b.classList.add('col');
    b._sw = sw;
    return b;
  }

  function buildTools() {
    const box = $('tools');
    const sep = () => h('span', { class: 'tsep', role: 'separator' });
    const on = (k) => (st) => !!(st && st[k]);
    const bold = tool('bold', h('b', null, 'B'), 'Bold', () => fmt({ b: !on('b')(styleAt(S.sel.r, S.sel.c)) }), on('b'));
    const italic = tool('italic', h('i', null, 'I'), 'Italic', () => fmt({ i: !on('i')(styleAt(S.sel.r, S.sel.c)) }), on('i'));
    const underline = tool('underline', h('u', null, 'U'), 'Underline', () => fmt({ u: !on('u')(styleAt(S.sel.r, S.sel.c)) }), on('u'));
    const strike = tool('strike', h('s', null, 'S'), 'Strikethrough', () => fmt({ x: !on('x')(styleAt(S.sel.r, S.sel.c)) }), on('x'));
    const color = colourTool('color', 'A', 'Text colour', () => {
      const st = styleAt(S.sel.r, S.sel.c);
      colourSheet(
        'Text colour',
        'color',
        st && st.c,
        (c) => {
          if (c) lastColor = c;
          fmt({ color: c });
          syncTools();
        },
        'Automatic',
      );
    });
    const fill = colourTool('fill', '◧', 'Fill colour', () => {
      const st = styleAt(S.sel.r, S.sel.c);
      colourSheet(
        'Fill colour',
        'fill',
        st && st.f,
        (c) => {
          if (c) lastFill = c;
          fmt({ fill: c });
          syncTools();
        },
        'No fill',
      );
    });
    const al = tool('al', icon('left'), 'Align left', () => fmt({ h: 'l' }), (st) => !!st && st.h === 'l');
    const ac = tool('ac', icon('center'), 'Align centre', () => fmt({ h: 'c' }), (st) => !!st && st.h === 'c');
    const ar = tool('ar', icon('right'), 'Align right', () => fmt({ h: 'r' }), (st) => !!st && st.h === 'r');
    const wrap = tool('wrap', icon('wrap'), 'Wrap text', () => fmt({ w: !on('w')(styleAt(S.sel.r, S.sel.c)) }), on('w'));
    const more = tool('align', 'Align…', 'More alignment', alignSheet);
    const bd = tool('bd', icon('borders'), 'Borders', bordersSheet);
    const nf = tool('nf', '123', 'Number format', numberSheet);
    const mg = tool('merge', icon('merge'), 'Merge or unmerge cells', toggleMerge);
    const fr = tool('freeze', icon('freeze'), 'Freeze panes', freezeSheet);
    const sz = tool('size', 'Aa', 'Text size', sizeTextSheet);
    const clr = tool('clear', icon('eraser'), 'Clear', clearSheet);
    box.replaceChildren(bold, italic, underline, strike, sep(), color, fill, sep(), al, ac, ar, wrap, more, sep(), bd, nf, sz, sep(), mg, fr, sep(), clr);
  }

  function syncTools() {
    const st = styleAt(S.sel.r, S.sel.c);
    for (const t of TOOLS) if (t.pressed) t.b.setAttribute('aria-pressed', String(!!t.pressed(st)));
    const c = $('t-color');
    const f = $('t-fill');
    if (c && c._sw) c._sw.style.background = (st && st.c) || lastColor;
    if (f && f._sw) f._sw.style.background = (st && st.f) || lastFill;
  }

  // ---------------------------------------------------------------------------
  // Rows, columns, cells: menus
  // ---------------------------------------------------------------------------

  /** Runs of hidden rows or columns (from the sheet description) next to or inside a span. */
  function hiddenNear(rows, a, b) {
    const runs = meta()[rows ? 'hidden_rows' : 'hidden_cols'] || [];
    const hit = runs.filter(([x, y]) => !(y < a - 1 || x > b + 1));
    if (!hit.length) return null;
    return [Math.min(...hit.map((r) => r[0])), Math.max(...hit.map((r) => r[1]))];
  }

  function insertDelete(rows, at, n, del) {
    send(F.sheetMsg(del ? 'del' : 'ins', { axis: rows ? 'r' : 'c', at, n }));
  }

  /** The rows or columns a selection covers: [first, last] on the axis. */
  const span = (rows) => {
    const g = rng();
    return rows ? [g.r1, g.r2] : [g.c1, g.c2];
  };

  function clipboardItems() {
    const e = isEdit();
    return [item('Copy', () => doCopy(false), { hint: 'Ctrl+C' }), item('Cut', () => doCopy(true), { disabled: !e, hint: 'Ctrl+X' }), item('Paste', () => doPaste('all'), { disabled: !e, hint: 'Ctrl+V' })];
  }

  function lineItems(rows) {
    const [a, b] = span(rows);
    const n = b - a + 1;
    const e = isEdit();
    const word = rows ? 'row' : 'column';
    const hid = hiddenNear(rows, a, b);
    return [
      item(rows ? `Insert ${plural(n, 'row')} above` : `Insert ${plural(n, 'column')} left`, () => insertDelete(rows, a, n, false), { disabled: !e }),
      item(rows ? `Insert ${plural(n, 'row')} below` : `Insert ${plural(n, 'column')} right`, () => insertDelete(rows, b + 1, n, false), { disabled: !e }),
      item(`Delete ${plural(n, word)}`, () => insertDelete(rows, a, n, true), { disabled: !e, danger: true }),
      item(`Hide ${plural(n, word)}`, () => send(F.sheetMsg('hide', { axis: rows ? 'r' : 'c', a, b, on: true })), { disabled: !e }),
      hid ? item(`Unhide hidden ${word}s`, () => send(F.sheetMsg('hide', { axis: rows ? 'r' : 'c', a: hid[0], b: hid[1], on: false })), { disabled: !e }) : null,
      item(rows ? 'Row height…' : 'Column width…', () => sizeSheet(rows), { disabled: !e }),
      item('Clear contents', () => send(F.selMsg('clear')), { disabled: !e }),
    ];
  }

  function headerMenu() {
    if (S.kind === 'cols' || S.kind === 'rows') {
      const rows = S.kind === 'rows';
      const [a, b] = span(rows);
      const name = rows ? (a === b ? `Row ${a + 1}` : `Rows ${a + 1}–${b + 1}`) : a === b ? `Column ${F.colName(a)}` : `Columns ${F.colName(a)}–${F.colName(b)}`;
      openSheet(name, [...clipboardItems(), ...lineItems(rows), item(rows ? 'Freeze rows up to here' : 'Freeze columns up to here', () => send(F.sheetMsg('freeze', rows ? { r: b + 1, c: F.layoutOf(S.cur).fc } : { r: F.layoutOf(S.cur).fr, c: b + 1 })), { disabled: !isEdit() })]);
    } else {
      openSheet('Whole sheet', [...clipboardItems(), item('Clear contents', () => send(F.selMsg('clear')), { disabled: !isEdit() }), item('Clear formats', () => fmt({ reset: true }), { disabled: !isEdit() }), item('Freeze panes…', freezeSheet, { disabled: !isEdit() }), item('Find…', openFind)]);
    }
  }

  function cellMenu() {
    const g = rng();
    const e = isEdit();
    const multi = g.r1 !== g.r2 || g.c1 !== g.c2;
    const rn = g.r2 - g.r1 + 1;
    const cn = g.c2 - g.c1 + 1;
    const title = multi ? `${num(g.r1, g.c1)}:${num(g.r2, g.c2)}` : num(g.r1, g.c1);
    openSheet(title, [
      item('Edit', () => F.startEdit(), { disabled: !e || multi }),
      ...clipboardItems(),
      item('Paste values only', () => doPaste('values'), { disabled: !e }),
      item('Clear contents', () => send(F.selMsg('clear')), { disabled: !e, hint: 'Delete' }),
      multi ? item('Fill down', () => send(F.selMsg('fill', { dir: 'down' })), { disabled: !e || rn < 2, hint: 'Ctrl+D' }) : null,
      multi ? item('Fill right', () => send(F.selMsg('fill', { dir: 'right' })), { disabled: !e || cn < 2, hint: 'Ctrl+R' }) : null,
      item(`Insert ${plural(rn, 'row')} above`, () => insertDelete(true, g.r1, rn, false), { disabled: !e }),
      item(`Insert ${plural(cn, 'column')} left`, () => insertDelete(false, g.c1, cn, false), { disabled: !e }),
      item(`Delete ${plural(rn, 'row')}`, () => insertDelete(true, g.r1, rn, true), { disabled: !e, danger: true }),
      item(`Delete ${plural(cn, 'column')}`, () => insertDelete(false, g.c1, cn, true), { disabled: !e, danger: true }),
      item('Select row', () => F.selectLines(true, g.r1, g.r2)),
      item('Select column', () => F.selectLines(false, g.c1, g.c2)),
      item('Find…', openFind, { hint: 'Ctrl+F' }),
    ]);
  }

  /** Width or height with a live preview. */
  function sizeSheet(rows) {
    const [a, b] = span(rows);
    const L = F.layoutOf(S.cur);
    const start = Math.round((rows ? L.rh : L.cw)[a]);
    const [lo, hi, step, def] = rows ? [4, 600, 4, meta().default_row_h || 20] : [8, 800, 8, meta().default_col_w || 64];
    let v = start;
    let done = false;
    const inp = h('input', { type: 'text', inputmode: 'numeric', value: String(v), 'aria-label': rows ? 'Row height in pixels' : 'Column width in pixels' });
    const apply = (nv) => {
      v = clamp(Math.round(nv), lo, hi);
      inp.value = String(v);
      F.previewSize(rows, a, b, v);
    };
    inp.addEventListener('change', () => apply(Number(inp.value) || v));
    openSheet(rows ? 'Row height' : 'Column width', [
      h('div', { class: 'stepper' }, h('button', { type: 'button', class: 'secondary', 'aria-label': 'Smaller', on: { click: () => apply(v - step) } }, '−'), inp, h('button', { type: 'button', class: 'secondary', 'aria-label': 'Larger', on: { click: () => apply(v + step) } }, '+')),
      h(
        'div',
        { class: 'srow' },
        h(
          'button',
          {
            type: 'button',
            class: 'secondary',
            on: {
              click: () => {
                done = true;
                closeSheet();
                send(F.selMsg('size', { axis: rows ? 'r' : 'c', a, b, px: null }));
              },
            },
          },
          `Default (${def})`,
        ),
        h(
          'button',
          {
            type: 'button',
            on: {
              click: () => {
                done = true;
                closeSheet();
                if (v !== start) F.commitSize(rows, a, b, v);
              },
            },
          },
          'Done',
        ),
      ),
    ], {
      onClose: () => {
        if (!done) F.revertLayout();
      },
    });
  }

  // ---------------------------------------------------------------------------
  // Sheet tabs
  // ---------------------------------------------------------------------------

  let tabDrag = null;

  function buildTabs() {
    const nav = $('tabs');
    nav.replaceChildren();
    nav.setAttribute('role', 'tablist');
    S.meta.sheets.forEach((s, i) => {
      if (s.hidden) return;
      const b = h('button', { type: 'button', class: 'tab', role: 'tab', 'data-i': String(i), 'aria-selected': i === S.cur ? 'true' : 'false' }, h('bdi', null, s.name));
      if (s.tab && /^#[0-9a-f]{6}$/.test(s.tab)) b.style.borderBottomColor = i === S.cur ? '' : s.tab;
      tabEvents(b, i);
      nav.append(b);
    });
    if (isEdit()) nav.append(h('button', { type: 'button', class: 'tab addtab', 'aria-label': 'Add sheet', on: { click: () => send({ t: 'sheet_add' }) } }, '+'));
  }

  function tabEvents(b, i) {
    let st = null;
    b.addEventListener('click', () => {
      if (st && st.suppress) {
        // The menu opens from the click that ends a long press, never under the finger before it.
        const s = st;
        st = null;
        clearTimeout(s.menuTimer);
        if (s.wantMenu) tabMenu(i);
        return;
      }
      F.switchSheet(i);
    });
    b.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      if (isEdit() || S.meta.sheets.length) tabMenu(i);
    });
    b.addEventListener('pointerdown', (e) => {
      if (e.button !== 0 || !isEdit()) return;
      const s = { x: e.clientX, id: e.pointerId, lifted: false, dx: 0, suppress: false, timer: 0 };
      st = s;
      s.timer = setTimeout(() => {
        s.lifted = true;
        tabDrag = s;
        b.classList.add('lifting');
        try {
          b.setPointerCapture(s.id);
        } catch {
          /* not capturable */
        }
        if (navigator.vibrate && navigator.userActivation && navigator.userActivation.hasBeenActive) navigator.vibrate(8);
      }, 450);
    });
    b.addEventListener('pointermove', (e) => {
      if (!st) return;
      if (!st.lifted) {
        if (Math.abs(e.clientX - st.x) > 10) clearTimeout(st.timer);
        return;
      }
      st.dx = e.clientX - st.x;
      b.style.transform = `translateX(${st.dx}px)`;
    });
    const end = () => {
      const s = st;
      if (!s) return;
      clearTimeout(s.timer);
      if (!s.lifted) {
        st = null;
        return;
      }
      s.suppress = true;
      tabDrag = null;
      b.classList.remove('lifting');
      b.style.transform = '';
      if (Math.abs(s.dx) < 12) {
        s.wantMenu = true;
        s.menuTimer = setTimeout(() => {
          if (st === s && s.wantMenu) {
            s.wantMenu = false;
            st = null;
            tabMenu(i);
          }
        }, 400);
        return;
      }
      // Where the dragged tab's centre lies among the others.
      const r = b.getBoundingClientRect();
      const cx = r.left + r.width / 2 + s.dx;
      let to = i;
      for (const t of $('tabs').querySelectorAll('.tab[data-i]')) {
        const tr = t.getBoundingClientRect();
        if (cx >= tr.left && cx <= tr.right) to = Number(t.getAttribute('data-i'));
      }
      if (to !== i) send(F.sheetMsg('sheet_move', { suid: S.meta.sheets[i].uid, sheet: i, to }));
    };
    b.addEventListener('pointerup', end);
    b.addEventListener('pointercancel', () => {
      if (st && st.lifted) {
        tabDrag = null;
        b.classList.remove('lifting');
        b.style.transform = '';
      }
      if (st) clearTimeout(st.timer);
      st = null;
    });
  }

  function tabMenu(i) {
    const s = S.meta.sheets[i];
    const e = isEdit();
    const target = { suid: s.uid, sheet: i };
    const hiddenOnes = S.meta.sheets.map((x, k) => [x, k]).filter(([x]) => x.hidden);
    const visible = S.meta.sheets.filter((x) => !x.hidden).length;
    openSheet(s.name, [
      item(
        'Rename…',
        async () => {
          const name = await askText('Sheet name', s.name, 'Rename', 31);
          if (name !== null && name.trim() !== s.name) send({ t: 'sheet_rename', ...target, name });
        },
        { disabled: !e },
      ),
      item('Move left', () => send({ t: 'sheet_move', ...target, to: i - 1 }), { disabled: !e || i === 0 }),
      item('Move right', () => send({ t: 'sheet_move', ...target, to: i + 1 }), { disabled: !e || i === S.meta.sheets.length - 1 }),
      item('Tab colour…', () => colourSheet('Tab colour', 'tab', s.tab, (c) => send({ t: 'sheet_color', ...target, color: c }), 'No colour'), { disabled: !e }),
      item('Hide sheet', () => send({ t: 'sheet_hide', ...target, on: true }), { disabled: !e || visible <= 1 }),
      hiddenOnes.length
        ? item('Show a hidden sheet…', () => openSheet('Hidden sheets', hiddenOnes.map(([x, k]) => item(x.name, () => send({ t: 'sheet_hide', suid: x.uid, sheet: k, on: false })))), { disabled: !e, keep: true })
        : null,
      item('Add sheet', () => send({ t: 'sheet_add' }), { disabled: !e }),
      item(
        'Delete sheet',
        async () => {
          if (await confirmBox(`Delete the sheet "${s.name}" and everything on it? Formulas in other sheets that refer to it will show #REF!. You can undo this right after.`, 'Delete')) send({ t: 'sheet_del', ...target });
        },
        { disabled: !e || visible <= 1, danger: true },
      ),
    ]);
  }

  // ---------------------------------------------------------------------------
  // Clipboard
  // ---------------------------------------------------------------------------

  let clip = null; // the last copy made in this page: {id, text, cut, session}

  const norm = (t) => (t || '').replace(/\r\n?/g, '\n').replace(/\n$/, '');

  /** Writes text (a promise of it, so Safari accepts it after the server's answer) to the system clipboard. */
  function toSystem(promise) {
    const nav = navigator.clipboard;
    if (nav && nav.write && window.ClipboardItem) {
      try {
        return nav.write([new ClipboardItem({ 'text/plain': promise.then((t) => new Blob([t], { type: 'text/plain' })) })]);
      } catch {
        /* fall through */
      }
    }
    return promise.then((t) => {
      if (nav && nav.writeText) return nav.writeText(t);
      const ta = h('textarea', { 'aria-hidden': 'true', readonly: true });
      ta.value = t;
      document.body.append(ta);
      ta.select();
      const ok = document.execCommand && document.execCommand('copy');
      ta.remove();
      if (!ok) throw new Error('no clipboard');
      return undefined;
    });
  }

  async function doCopy(cut) {
    if (cut && !isEdit()) return toast('You have view-only access to this workbook.', 'error');
    if (!(S.live.ready && S.live.state === 'open')) return toast('Copying needs the live connection. Try again in a moment.', 'error');
    const answer = F.liveCall(F.selMsg('copy', { cut }));
    const text = answer.then((m) => {
      clip = { id: m.clip, text: m.text, cut: m.cut, session: S.live.session };
      return m.text;
    });
    try {
      await toSystem(text);
      toast(cut ? 'Cut. Paste where you want it.' : 'Copied.');
    } catch (e) {
      try {
        await text;
        toast(cut ? 'Cut (the browser would not share the clipboard; paste works inside this page).' : 'Copied inside this page (the browser would not share the clipboard).');
      } catch (e2) {
        toast(e2 && e2.message ? e2.message : "Couldn't copy.", 'error');
      }
    }
  }

  async function doPaste(what) {
    if (!isEdit()) return toast('You have view-only access to this workbook.', 'error');
    let text = '';
    try {
      text = await navigator.clipboard.readText();
    } catch {
      if (!clip) return toast('Allow pasting when the browser asks, or use the keyboard (Ctrl/Cmd+V).', 'error');
    }
    pasteText(text, what || 'all');
  }

  /** TSV as the spreadsheets write it: quotes around cells with tabs, line breaks or quotes. */
  function parseTsv(text) {
    text = text.replace(/\r\n?/g, '\n');
    if (text.endsWith('\n')) text = text.slice(0, -1);
    const rows = [];
    let row = [];
    let cur = '';
    let q = false;
    for (let i = 0; i < text.length; i++) {
      const ch = text[i];
      if (q) {
        if (ch === '"') {
          if (text[i + 1] === '"') {
            cur += '"';
            i++;
          } else {
            q = false;
          }
        } else {
          cur += ch;
        }
      } else if (ch === '"' && cur === '') {
        q = true;
      } else if (ch === '\t') {
        row.push(cur);
        cur = '';
      } else if (ch === '\n') {
        row.push(cur);
        rows.push(row);
        row = [];
        cur = '';
      } else {
        cur += ch;
      }
    }
    row.push(cur);
    rows.push(row);
    return rows;
  }

  function pasteText(text, what) {
    if (!isEdit()) return toast('You have view-only access to this workbook.', 'error');
    const g = rng();
    const here = { r: g.r1, c: g.c1 };
    if (clip && clip.session === S.live.session && (!text || norm(text) === norm(clip.text))) {
      const m = F.sheetMsg('paste', { ...here, clip: clip.id, what });
      if (S.kind === 'cell') {
        m.r2 = g.r2;
        m.c2 = g.c2;
      }
      send(m);
      if (clip.cut) clip = null;
      return;
    }
    if (!text) return toast('Nothing to paste.', 'error');
    if (text.length > MAX_PASTE_CHARS) return toast('That is too much text to paste at once.', 'error');
    const grid = parseTsv(text);
    const cells = grid.reduce((n, r) => n + r.length, 0);
    if (cells > MAX_PASTE_CELLS) return toast(`That is ${cells.toLocaleString('en')} cells; paste at most ${MAX_PASTE_CELLS.toLocaleString('en')} at once.`, 'error');
    if (what === 'values' || what === 'all' || what === 'formats') send(F.sheetMsg('paste', { ...here, grid }));
  }

  // ---------------------------------------------------------------------------
  // Find and go to
  // ---------------------------------------------------------------------------

  const FD = { hits: [], i: -1, q: '', opt: { case: false, whole: false, formulas: false, all: true }, seq: 0 };

  function openFind() {
    $('findbar').hidden = false;
    const q = $('find-q');
    q.focus();
    q.select();
  }

  function closeFind() {
    $('findbar').hidden = true;
    FD.hits = [];
    FD.i = -1;
    $('find-n').textContent = '';
    $('scroller').focus({ preventScroll: true });
  }

  function findLabel(more) {
    const n = FD.hits.length;
    $('find-n').textContent = !FD.q ? '' : n ? `${FD.i + 1} of ${n}${more ? '+' : ''}` : 'No matches';
  }

  async function runFind(step, jump) {
    const q = $('find-q').value;
    if (!q) {
      FD.hits = [];
      FD.i = -1;
      return findLabel();
    }
    if (!(S.live.ready && S.live.state === 'open')) return toast('Search needs the live connection. Try again in a moment.', 'error');
    const mine = ++FD.seq;
    let m;
    try {
      m = await F.liveCall({ t: 'find', q, case: FD.opt.case, whole: FD.opt.whole, formulas: FD.opt.formulas, all: FD.opt.all, sheet: S.cur, suid: meta().uid });
    } catch (e) {
      return toast(e.message, 'error');
    }
    if (mine !== FD.seq) return;
    FD.q = q;
    FD.hits = m.hits;
    // The first match at or after the selection, in the current sheet's order of reading.
    const key = (s, r, c) => (s * MAXR + r) * KEYC + c;
    const here = key(S.cur, S.sel.r, S.sel.c);
    let start = FD.hits.findIndex((x) => key(x[0], x[1], x[2]) >= here);
    if (start < 0) start = 0;
    FD.i = step < 0 ? (start - 1 + FD.hits.length) % Math.max(1, FD.hits.length) : start;
    findLabel(m.more);
    if (jump && FD.hits.length) goHit();
  }

  function goHit() {
    const x = FD.hits[FD.i];
    if (!x) return;
    goTo(x[0], x[1], x[2], x[1], x[2]);
    findLabel();
  }

  async function findStep(d) {
    const q = $('find-q').value;
    if (q !== FD.q || !FD.hits.length) return runFind(d, true);
    FD.i = (FD.i + d + FD.hits.length) % FD.hits.length;
    goHit();
  }

  function findOptions() {
    const o = FD.opt;
    const flip = (k) => () => {
      o[k] = !o[k];
      runFind(1, true);
    };
    openSheet('Search options', [item('Match case', flip('case'), { pressed: o.case }), item('Whole cell only', flip('whole'), { pressed: o.whole }), item('Look in formulas too', flip('formulas'), { pressed: o.formulas }), item(o.all ? 'Searching the whole workbook' : 'Searching this sheet only', () => ((o.all = !o.all), runFind(1, true)))]);
  }

  /** Selects a range, on another sheet if needed. */
  function goTo(sheet, r1, c1, r2, c2) {
    if (sheet !== S.cur) F.switchSheet(sheet);
    F.setRange(r1, c1, r2, c2, true);
  }

  function colIndex(s) {
    let n = 0;
    for (const ch of s.toUpperCase()) n = n * 26 + (ch.charCodeAt(0) - 64);
    return n - 1;
  }

  /** "B7", "Sheet2!C3", "A1:C5", "B:D", "3:5". */
  function parseRef(text) {
    let t = text.trim();
    let sheet = S.cur;
    const bang = t.lastIndexOf('!');
    if (bang >= 0) {
      const name = t.slice(0, bang).replace(/^'|'$/g, '').replace(/''/g, "'");
      const i = S.meta.sheets.findIndex((s) => s.name.toLowerCase() === name.toLowerCase());
      if (i < 0) return { error: `There is no sheet called "${name}".` };
      sheet = i;
      t = t.slice(bang + 1);
    }
    t = t.replace(/\$/g, '');
    let m = /^([A-Za-z]{1,3})(\d{1,7})(?::([A-Za-z]{1,3})(\d{1,7}))?$/.exec(t);
    if (m) {
      const c1 = colIndex(m[1]);
      const r1 = Number(m[2]) - 1;
      const c2 = m[3] ? colIndex(m[3]) : c1;
      const r2 = m[4] ? Number(m[4]) - 1 : r1;
      if (r1 < 0 || r2 < 0 || c1 >= MAXC || c2 >= MAXC || r1 >= MAXR || r2 >= MAXR) return { error: 'That address is outside the sheet.' };
      return { sheet, r1: Math.min(r1, r2), c1: Math.min(c1, c2), r2: Math.max(r1, r2), c2: Math.max(c1, c2) };
    }
    m = /^([A-Za-z]{1,3}):([A-Za-z]{1,3})$/.exec(t);
    if (m) return { sheet, cols: [Math.min(colIndex(m[1]), colIndex(m[2])), Math.max(colIndex(m[1]), colIndex(m[2]))] };
    m = /^(\d{1,7}):(\d{1,7})$/.exec(t);
    if (m) return { sheet, rows: [Math.min(Number(m[1]), Number(m[2])) - 1, Math.max(Number(m[1]), Number(m[2])) - 1] };
    return { error: 'Type a cell like B7, a range like A1:C5, or Sheet2!C3.' };
  }

  async function gotoDialog() {
    const text = await askText('Go to', '', 'Go', 80);
    if (text === null || !text.trim()) return;
    const p = parseRef(text);
    if (p.error) return toast(p.error, 'error');
    if (p.sheet !== S.cur) F.switchSheet(p.sheet);
    if (p.cols) F.selectLines(false, p.cols[0], p.cols[1]);
    else if (p.rows) F.selectLines(true, p.rows[0], p.rows[1]);
    else F.setRange(p.r1, p.c1, p.r2, p.c2, true);
    if (p.cols) F.scrollIntoView(0, p.cols[0]);
    if (p.rows) F.scrollIntoView(p.rows[0], 0);
  }

  // ---------------------------------------------------------------------------
  // Keyboard
  // ---------------------------------------------------------------------------

  function key(e) {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) {
      if (e.key === ' ' && e.shiftKey) {
        e.preventDefault();
        F.selectLines(true, S.sel.r, S.rng.r2);
        return true;
      }
      if (e.key === 'Escape' && S.handles) {
        S.handles = false;
        F.requestDraw();
        F.positionOverlays();
        return true;
      }
      return false;
    }
    const k = e.key.toLowerCase();
    const edit = isEdit();
    switch (k) {
      case 'c':
        e.preventDefault();
        doCopy(false);
        return true;
      case 'x':
        e.preventDefault();
        if (edit) doCopy(true);
        return true;
      case 'a':
        e.preventDefault();
        F.selectAll();
        return true;
      case 'd':
      case 'r':
        e.preventDefault();
        if (edit) send(F.selMsg('fill', { dir: k === 'd' ? 'down' : 'right' }));
        return true;
      case 'b':
      case 'i':
      case 'u': {
        e.preventDefault();
        if (edit) {
          const st = styleAt(S.sel.r, S.sel.c);
          fmt({ [k]: !(st && st[k]) });
        }
        return true;
      }
      case 'f':
        e.preventDefault();
        openFind();
        return true;
      case 'g':
        e.preventDefault();
        gotoDialog();
        return true;
      case ' ':
        e.preventDefault();
        F.selectLines(false, S.sel.c, S.rng.c2);
        return true;
      default:
        return false;
    }
  }

  // ---------------------------------------------------------------------------
  // Charts, drawn from the data the server resolves
  // ---------------------------------------------------------------------------

  const compact = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 2 });
  const plain = new Intl.NumberFormat('en', { maximumFractionDigits: 2 });
  const tick = (v) => (Math.abs(v) >= 10000 ? compact.format(v) : plain.format(v));

  function niceScale(min, max, base) {
    if (base) {
      min = Math.min(min, 0);
      max = Math.max(max, 0);
    }
    if (!(max > min)) max = min + 1;
    const raw = (max - min) / 5;
    const mag = 10 ** Math.floor(Math.log10(raw));
    const f = raw / mag;
    const step = (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * mag;
    const lo = Math.floor(min / step) * step;
    const hi = Math.ceil(max / step) * step;
    const ticks = [];
    for (let v = lo; v <= hi + step / 2 && ticks.length < 12; v += step) ticks.push(Math.abs(v) < step / 1e6 ? 0 : v);
    return { lo, hi, ticks };
  }

  function drawChart(ctx, ch, x, y, w, hh, P, z) {
    const dark = S.dark;
    const fg = dark ? '#d6dcd8' : '#3a423e';
    const grid = dark ? '#39433e' : '#e1e5e3';
    ctx.save();
    ctx.beginPath();
    ctx.rect(x, y, w, hh);
    ctx.clip();
    ctx.fillStyle = dark ? '#1f2522' : '#ffffff';
    ctx.fillRect(x, y, w, hh);
    ctx.strokeStyle = dark ? '#4a554f' : '#c9cfcc';
    ctx.lineWidth = 1;
    ctx.strokeRect(x + 0.5, y + 0.5, w - 1, hh - 1);
    const fs = Math.max(8, 11 * z);
    ctx.font = `${fs}px system-ui, -apple-system, sans-serif`;
    ctx.textBaseline = 'middle';
    ctx.fillStyle = fg;
    let top = y + 8 * z;
    if (ch.title) {
      ctx.font = `600 ${Math.max(9, 13 * z)}px system-ui, -apple-system, sans-serif`;
      ctx.textAlign = 'center';
      ctx.fillText(ch.title, x + w / 2, y + 16 * z, w - 16);
      ctx.textAlign = 'start';
      ctx.font = `${fs}px system-ui, -apple-system, sans-serif`;
      top = y + 32 * z;
    }
    const series = ch.series || [];
    const round = ch.kind === 'pie' || ch.kind === 'doughnut';
    const n = Math.max(ch.cats.length, ...series.map((s) => s.values.length), 0);
    let bottom = y + hh - 8 * z;
    // Legend along the bottom.
    const names = round ? ch.cats.slice(0, 12) : series.map((s) => s.name);
    const cols = round ? (series[0] && series[0].colors) || [] : series.map((s) => s.color);
    if (ch.legend !== false && names.length > (round ? 0 : 1)) {
      let lx = x + 10 * z;
      const ly = y + hh - 12 * z;
      for (let i = 0; i < names.length; i++) {
        const label = names[i] || `Series ${i + 1}`;
        const tw = ctx.measureText(label).width;
        if (lx + tw + 24 * z > x + w) break;
        ctx.fillStyle = cols[i] || '#888';
        ctx.fillRect(lx, ly - 4 * z, 8 * z, 8 * z);
        ctx.fillStyle = fg;
        ctx.fillText(label, lx + 12 * z, ly);
        lx += tw + 24 * z;
      }
      bottom -= 18 * z;
    }
    const noData = !series.length || n === 0;
    if (noData) {
      ctx.fillStyle = fg;
      ctx.textAlign = 'center';
      ctx.fillText('No data to show', x + w / 2, y + hh / 2);
      ctx.textAlign = 'start';
      ctx.restore();
      return;
    }
    if (round) {
      drawRound(ctx, ch, series[0], x, top, w, bottom - top, z, fg);
    } else if (ch.kind === 'radar') {
      drawRadar(ctx, ch, series, x, top, w, bottom - top, z, fg, grid);
    } else if (ch.kind === 'scatter') {
      drawScatter(ctx, ch, series, n, x, top, w, bottom - top, z, fg, grid);
    } else {
      drawAxes(ctx, ch, series, n, x, top, w, bottom - top, z, fg, grid);
    }
    ctx.restore();
  }

  function drawRound(ctx, ch, s, x, top, w, hh, z, fg) {
    const vals = s.values.map((v) => (v && v > 0 ? v : 0));
    const total = vals.reduce((a, b) => a + b, 0);
    const cx = x + w / 2;
    const cy = top + hh / 2;
    const r = Math.max(10, Math.min(w, hh) / 2 - 8 * z);
    if (!total) return;
    let a0 = -Math.PI / 2;
    vals.forEach((v, i) => {
      const a1 = a0 + (v / total) * Math.PI * 2;
      ctx.beginPath();
      ctx.moveTo(cx, cy);
      ctx.arc(cx, cy, r, a0, a1);
      ctx.closePath();
      ctx.fillStyle = (s.colors && s.colors[i]) || '#888';
      ctx.fill();
      ctx.strokeStyle = S.dark ? '#1f2522' : '#fff';
      ctx.lineWidth = 1.5;
      ctx.stroke();
      a0 = a1;
    });
    if (ch.kind === 'doughnut') {
      ctx.beginPath();
      ctx.arc(cx, cy, r * 0.55, 0, Math.PI * 2);
      ctx.fillStyle = S.dark ? '#1f2522' : '#fff';
      ctx.fill();
    }
  }

  function drawAxes(ctx, ch, series, n, x, top, w, hh, z, fg, grid) {
    const horizontal = ch.kind === 'bar';
    const stacked = ch.grouping === 'stacked' || ch.grouping === 'percent';
    const pct = ch.grouping === 'percent';
    const val = (s, i) => {
      const v = s.values[i];
      return typeof v === 'number' && isFinite(v) ? v : 0;
    };
    let min = 0;
    let max = 0;
    if (stacked) {
      for (let i = 0; i < n; i++) {
        let p = 0;
        let q = 0;
        for (const s of series) (val(s, i) >= 0 ? (p += val(s, i)) : (q += val(s, i)));
        max = Math.max(max, pct ? 1 : p);
        min = Math.min(min, pct ? 0 : q);
      }
    } else {
      for (const s of series) {
        for (let i = 0; i < n; i++) {
          max = Math.max(max, val(s, i));
          min = Math.min(min, val(s, i));
        }
      }
    }
    const sc = niceScale(min, max, true);
    const left = x + (horizontal ? 52 : 44) * z;
    const right = x + w - 12 * z;
    const pb = top + hh - 22 * z;
    const pw = Math.max(10, right - left);
    const ph = Math.max(10, pb - top);
    const toV = (v) => (horizontal ? left + ((v - sc.lo) / (sc.hi - sc.lo)) * pw : pb - ((v - sc.lo) / (sc.hi - sc.lo)) * ph);
    ctx.lineWidth = 1;
    ctx.textAlign = horizontal ? 'center' : 'right';
    for (const t of sc.ticks) {
      const p = toV(t);
      ctx.strokeStyle = grid;
      ctx.beginPath();
      if (horizontal) {
        ctx.moveTo(p, top);
        ctx.lineTo(p, pb);
      } else {
        ctx.moveTo(left, p);
        ctx.lineTo(right, p);
      }
      ctx.stroke();
      ctx.fillStyle = fg;
      const label = pct ? `${Math.round(t * 100)}%` : tick(t);
      if (horizontal) ctx.fillText(label, p, pb + 11 * z);
      else ctx.fillText(label, left - 5 * z, p);
    }
    ctx.textAlign = 'center';
    const slot = (horizontal ? ph : pw) / n;
    const every = Math.max(1, Math.ceil((horizontal ? 14 * z : 46 * z) / slot));
    for (let i = 0; i < n; i += every) {
      const label = ch.cats[i] === undefined ? String(i + 1) : ch.cats[i];
      const c = (horizontal ? top + slot * (n - i - 0.5) : left + slot * (i + 0.5)) + 0;
      if (horizontal) {
        ctx.textAlign = 'right';
        ctx.fillText(label.slice(0, 8), left - 5 * z, c);
      } else {
        ctx.fillText(label.slice(0, 8), c, pb + 11 * z, slot * every);
      }
    }
    ctx.textAlign = 'start';
    // Series.
    const zero = toV(0);
    const kinds = ch.kind;
    const acc = new Array(n).fill(0);
    const accNeg = new Array(n).fill(0);
    const sums = pct ? Array.from({ length: n }, (_, i) => series.reduce((a, s) => a + Math.abs(val(s, i)), 0) || 1) : null;
    series.forEach((s, si) => {
      ctx.fillStyle = s.color;
      ctx.strokeStyle = s.line || s.color;
      const pts = [];
      for (let i = 0; i < n; i++) {
        let v = val(s, i);
        if (pct) v /= sums[i];
        let from = 0;
        if (stacked) {
          if (v >= 0) {
            from = acc[i];
            acc[i] += v;
          } else {
            from = accNeg[i];
            accNeg[i] += v;
          }
        }
        const to = from + v;
        if (kinds === 'column' || kinds === 'bar') {
          const bw = (horizontal ? ph : pw) / n;
          const gap = bw * 0.2;
          const sw = stacked ? bw - 2 * gap : (bw - 2 * gap) / series.length;
          const off = gap + (stacked ? 0 : sw * si);
          if (horizontal) {
            const yy = top + bw * (n - i - 1) + off;
            ctx.fillRect(Math.min(toV(from), toV(to)), yy, Math.abs(toV(to) - toV(from)), Math.max(1, sw - 1));
          } else {
            const xx = left + bw * i + off;
            ctx.fillRect(xx, Math.min(toV(from), toV(to)), Math.max(1, sw - 1), Math.abs(toV(to) - toV(from)));
          }
        } else {
          pts.push([left + (pw / n) * (i + 0.5), toV(to), toV(from)]);
        }
      }
      if (pts.length) {
        ctx.lineWidth = 2 * Math.min(1.5, z);
        if (kinds === 'area') {
          ctx.globalAlpha = 0.55;
          ctx.beginPath();
          pts.forEach((p, i) => (i ? ctx.lineTo(p[0], p[1]) : ctx.moveTo(p[0], p[1])));
          for (let i = pts.length - 1; i >= 0; i--) ctx.lineTo(pts[i][0], stacked ? pts[i][2] : zero);
          ctx.closePath();
          ctx.fill();
          ctx.globalAlpha = 1;
        }
        ctx.beginPath();
        pts.forEach((p, i) => (i ? ctx.lineTo(p[0], p[1]) : ctx.moveTo(p[0], p[1])));
        ctx.stroke();
        if (kinds === 'line' && pts.length < 60) for (const p of pts) ctx.fillRect(p[0] - 2.5, p[1] - 2.5, 5, 5);
      }
    });
    ctx.strokeStyle = fg;
    ctx.lineWidth = 1;
    ctx.beginPath();
    if (horizontal) {
      ctx.moveTo(zero, top);
      ctx.lineTo(zero, pb);
    } else {
      ctx.moveTo(left, zero);
      ctx.lineTo(right, zero);
    }
    ctx.stroke();
  }

  function drawScatter(ctx, ch, series, n, x, top, w, hh, z, fg, grid) {
    const xsOf = (s) => s.xs || ch.xs;
    let xmin = Infinity;
    let xmax = -Infinity;
    let ymin = Infinity;
    let ymax = -Infinity;
    for (const s of series) {
      const xs = xsOf(s);
      s.values.forEach((v, i) => {
        const xv = xs && typeof xs[i] === 'number' ? xs[i] : i + 1;
        if (typeof v !== 'number') return;
        xmin = Math.min(xmin, xv);
        xmax = Math.max(xmax, xv);
        ymin = Math.min(ymin, v);
        ymax = Math.max(ymax, v);
      });
    }
    if (!isFinite(xmin)) return;
    const sx = niceScale(xmin, xmax, false);
    const sy = niceScale(ymin, ymax, false);
    const left = x + 44 * z;
    const right = x + w - 12 * z;
    const pb = top + hh - 22 * z;
    const X = (v) => left + ((v - sx.lo) / (sx.hi - sx.lo)) * (right - left);
    const Y = (v) => pb - ((v - sy.lo) / (sy.hi - sy.lo)) * (pb - top);
    ctx.lineWidth = 1;
    for (const t of sy.ticks) {
      ctx.strokeStyle = grid;
      ctx.beginPath();
      ctx.moveTo(left, Y(t));
      ctx.lineTo(right, Y(t));
      ctx.stroke();
      ctx.fillStyle = fg;
      ctx.textAlign = 'right';
      ctx.fillText(tick(t), left - 5 * z, Y(t));
    }
    ctx.textAlign = 'center';
    for (const t of sx.ticks) ctx.fillText(tick(t), X(t), pb + 11 * z);
    ctx.textAlign = 'start';
    for (const s of series) {
      const xs = xsOf(s);
      ctx.fillStyle = s.color;
      s.values.forEach((v, i) => {
        if (typeof v !== 'number') return;
        const xv = xs && typeof xs[i] === 'number' ? xs[i] : i + 1;
        ctx.beginPath();
        ctx.arc(X(xv), Y(v), 3.5, 0, Math.PI * 2);
        ctx.fill();
      });
    }
  }

  function drawRadar(ctx, ch, series, x, top, w, hh, z, fg, grid) {
    const n = Math.max(3, ch.cats.length, ...series.map((s) => s.values.length));
    const cx = x + w / 2;
    const cy = top + hh / 2;
    const r = Math.max(10, Math.min(w, hh) / 2 - 16 * z);
    let max = 0;
    for (const s of series) for (const v of s.values) if (typeof v === 'number') max = Math.max(max, v);
    if (!max) return;
    ctx.strokeStyle = grid;
    ctx.lineWidth = 1;
    for (let ring = 1; ring <= 4; ring++) {
      ctx.beginPath();
      for (let i = 0; i < n; i++) {
        const a = -Math.PI / 2 + (i / n) * Math.PI * 2;
        const rr = (r * ring) / 4;
        ctx[i ? 'lineTo' : 'moveTo'](cx + Math.cos(a) * rr, cy + Math.sin(a) * rr);
      }
      ctx.closePath();
      ctx.stroke();
    }
    for (const s of series) {
      ctx.strokeStyle = s.color;
      ctx.lineWidth = 2;
      ctx.beginPath();
      for (let i = 0; i < n; i++) {
        const a = -Math.PI / 2 + (i / n) * Math.PI * 2;
        const rr = (r * (typeof s.values[i] === 'number' ? s.values[i] : 0)) / max;
        ctx[i ? 'lineTo' : 'moveTo'](cx + Math.cos(a) * rr, cy + Math.sin(a) * rr);
      }
      ctx.closePath();
      ctx.stroke();
    }
  }

  // ---------------------------------------------------------------------------
  // Wiring
  // ---------------------------------------------------------------------------

  function wire() {
    buildTools();
    $('find-btn').addEventListener('click', openFind);
    $('find-x').addEventListener('click', closeFind);
    $('find-next').addEventListener('click', () => findStep(1));
    $('find-prev').addEventListener('click', () => findStep(-1));
    $('find-opt').addEventListener('click', findOptions);
    let deb = 0;
    $('find-q').addEventListener('input', () => {
      clearTimeout(deb);
      deb = setTimeout(() => runFind(1, true), 280);
    });
    $('find-q').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        clearTimeout(deb);
        findStep(e.shiftKey ? -1 : 1);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        closeFind();
      }
    });
    $('addr').addEventListener('click', gotoDialog);
    $('addr').setAttribute('role', 'button');
    $('addr').setAttribute('tabindex', '0');
    $('addr').setAttribute('aria-label', 'Selected cell. Tap to go to a cell.');
    $('addr').addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        gotoDialog();
      }
    });
    // Ctrl/Cmd+F finds in the workbook wherever the focus is; paste arrives as an event.
    document.addEventListener('keydown', (e) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'f' && !$('app').hidden) {
        e.preventDefault();
        openFind();
      }
    });
    document.addEventListener('paste', (e) => {
      const t = e.target;
      if ($('app').hidden || (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable))) return;
      e.preventDefault();
      pasteText(e.clipboardData ? e.clipboardData.getData('text/plain') : '', 'all');
    });
    // The tab strip must not scroll while a tab is being dragged.
    $('tabs').addEventListener(
      'touchmove',
      (e) => {
        if (tabDrag) e.preventDefault();
      },
      { passive: false },
    );
  }

  F.ui.buildTabs = buildTabs;
  F.ui.onSelect = syncTools;
  F.ui.onMeta = () => {
    syncTools();
    updateTabsForRole();
  };
  F.ui.cellMenu = cellMenu;
  F.ui.headerMenu = headerMenu;
  F.ui.editCell = () => {
    if (isEdit()) F.startEdit();
  };
  F.ui.key = key;
  F.ui.drawChart = drawChart;
  F.ui.closeSheet = closeSheet;
  F.ui.parseTsv = parseTsv;
  F.ui.parseRef = parseRef;

  /** The "+" tab depends on the role. */
  function updateTabsForRole() {
    const has = !!$('tabs').querySelector('.addtab');
    if (has !== isEdit()) buildTabs();
  }

  wire();
})();
