// The settings history of a station (the server side is `serve::config_history`): a panel that lists
// every saved version of the station's configuration file, newest first, with what changed, a full before/after view and
// a Restore for each. "Undo last change" at the top restores the version before the newest. Used by the Processing page
// and the station's Settings page: `ConfigHistoryUI.button(station, onRestored)` returns the button that opens it.
// Loaded after common.js; it needs h, J, jsend, toast from there.
const ConfigHistoryUI = (() => {
  const SHOW_FIRST = 3;
  const when = ms => new Date(ms).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const whenFull = ms => new Date(ms).toLocaleString([], { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const srcLabel = v => v.source === 'restore' && v.of_ts_ms ? 'restore of ' + when(v.of_ts_ms) : v.source;

  function open(station, onRestored) {
    const url = `/api/v1/stations/${station.id}/config-history`;
    const list = h('div', { class: 'hist-list', 'data-hist-list': station.id });
    const undo = h('button', { class: 'primary', 'data-hist-undo': '' }, 'Undo last change');
    const close = () => { back.remove(); document.removeEventListener('keydown', onKey); };
    const onKey = ev => { if (ev.key === 'Escape') close(); };
    const card = h('div', { class: 'card hist-card', role: 'dialog', 'aria-modal': 'true', 'aria-label': 'Settings history of ' + station.display_name },
      h('div', { class: 'hist-head' }, h('h3', {}, 'History of ' + station.display_name, helpLink('processing', 'settings-history')), h('div', { class: 'grow' }), undo, h('button', { 'data-hist-close': '', onclick: close }, 'Close')),
      h('div', { class: 'dim hist-note' }, 'Every save of this station’s settings is a version. Changes in a row from the same place within 10 seconds are one version. The newest 200 are kept. A restore is a version too, so it can be undone.'),
      list);
    const back = h('div', { class: 'hist-back', onclick: ev => { if (ev.target === back) close(); } }, card);
    document.body.append(back);
    document.addEventListener('keydown', onKey);

    let versions = [];
    async function load() {
      let j;
      try { j = await J(url); } catch (e) { list.replaceChildren(h('div', { class: 'notice bad' }, 'History: ' + e.message)); undo.disabled = true; return; }
      versions = j.versions;
      paint();
    }
    function paint() {
      undo.disabled = versions.length < 2;
      undo.title = versions.length < 2 ? 'There is no earlier version' : 'Go back to the settings of ' + when(versions[1].ts_ms);
      if (!versions.length) { list.replaceChildren(h('div', { class: 'dim' }, 'No version is stored yet.')); return; }
      list.replaceChildren(...versions.map((v, i) => row(v, i)));
    }
    async function restore(v, label) {
      if (!confirm(`Restore the settings of ${whenFull(v.ts_ms)}?\n\n${label || 'The station goes back to them now; what cannot change while it plays is listed afterwards.'}`)) return;
      let r;
      try { r = await jsend(`${url}/${v.id}/restore`, {}); } catch (e) { toast('Restore: ' + e.message, true); return; }
      toast(r.message + (r.notes && r.notes.length ? ' ' + r.notes.join(' ') : ''), !!(r.needs_restart && r.needs_restart.length));
      try { if (onRestored) await onRestored(r); } catch (e) { toast(e.message, true); }
      await load();
    }
    undo.addEventListener('click', () => {
      if (versions.length < 2) return;
      const newest = versions[0];
      restore(versions[1], `This undoes: ${newest.changes.length ? newest.changes.slice(0, 3).join('; ') : 'the last change'}${newest.changes.length > 3 ? ` and ${newest.changes.length - 3} more` : ''}.`);
    });

    function changeList(v) {
      const ch = v.changes;
      if (!ch.length) return h('div', { class: 'dim' }, versions.indexOf(v) === versions.length - 1 ? 'The oldest version kept.' : 'No setting changed (only comments or layout).');
      let all = false;
      const ul = h('ul', { class: 'hist-changes' });
      const more = h('button', { class: 'link', type: 'button', onclick: () => { all = !all; fill(); } });
      function fill() {
        ul.replaceChildren(...(all ? ch : ch.slice(0, SHOW_FIRST)).map(c => h('li', {}, c)));
        if (ch.length > SHOW_FIRST) { more.textContent = all ? 'show fewer' : `+${ch.length - SHOW_FIRST} more`; ul.append(h('li', { class: 'hist-more' }, more)); }
      }
      fill();
      return ul;
    }
    function row(v, i) {
      const detail = h('div', { class: 'hist-detail', hidden: true });
      const showBtn = h('button', { 'data-hist-show': v.id, 'aria-expanded': 'false', onclick: async () => {
        if (!detail.hidden) { detail.hidden = true; showBtn.setAttribute('aria-expanded', 'false'); showBtn.textContent = 'Show'; return; }
        detail.hidden = false; showBtn.setAttribute('aria-expanded', 'true'); showBtn.textContent = 'Hide';
        detail.replaceChildren(h('div', { class: 'dim' }, 'Loading...'));
        try { detail.replaceChildren(...(await detailView(v)).filter(Boolean)); } catch (e) { detail.replaceChildren(h('div', { class: 'notice bad' }, e.message)); }
      } }, 'Show');
      const restoreBtn = h('button', { 'data-hist-restore': v.id, title: i === 0 ? 'This is the newest version: the settings are in use now' : 'Go back to the settings of this version', disabled: i === 0, onclick: () => restore(v) }, 'Restore');
      return h('div', { class: 'hist-row' + (i === 0 ? ' now' : ''), 'data-hist-id': v.id },
        h('div', { class: 'hist-when' }, h('b', { title: v.time }, when(v.ts_ms)), h('span', { class: 'chip' + (v.source === 'restore' ? ' warn' : '') }, srcLabel(v)), i === 0 ? h('span', { class: 'chip ok' }, 'in use') : null),
        h('div', { class: 'hist-what' }, changeList(v)),
        h('div', { class: 'hist-btns' }, showBtn, restoreBtn),
        detail);
    }
    const cellText = s => s == null ? h('span', { class: 'dim' }, '(not set)') : s;
    function table(rows, heads) {
      if (!rows.length) return h('div', { class: 'dim' }, 'Nothing differs.');
      return h('div', { class: 'hist-tablewrap' }, h('table', { class: 'hist-table' }, h('thead', {}, h('tr', {}, heads.map(x => h('th', {}, x)))),
        h('tbody', {}, rows.map(c => h('tr', {},
          h('td', { class: 'hist-key' }, h('div', { class: 'hist-label' }, c.label || c.key), c.label && c.label !== c.key ? h('div', { class: 'hist-raw mono dim', title: 'The key in the settings file' }, c.key) : null),
          h('td', { class: 'mono before' }, c.section ? '' : cellText(c.before)),
          h('td', { class: 'mono after' }, c.section ? 'settings ' + c.section : cellText(c.after)))))));
    }
    async function detailView(v) {
      const d = await J(`${url}/${v.id}`);
      return [
        h('div', { class: 'hist-sub' }, 'What this version changed'),
        table(d.detail, ['Setting', 'Before', 'After']),
        h('div', { class: 'hist-sub' }, d.same_as_current ? 'This is what is in use now.' : 'What a restore of it would change now'),
        d.same_as_current ? null : table(d.vs_current, ['Setting', 'In use now', 'This version']),
      ];
    }
    load();
    undo.focus();
    return { reload: load, close };
  }

  function button(station, onRestored) {
    return h('button', { 'data-history': station.id, title: 'Every saved version of this station’s settings: see what changed, go back to one, undo the last change', onclick: () => open(station, onRestored) }, 'History');
  }
  return { open, button };
})();
