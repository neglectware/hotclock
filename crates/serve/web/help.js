// The Help section: pages built into the program (crates/serve/help/*.md), a table of contents, search and deep links.
// Loaded after settings.js (it reuses that file's setting groups and the way a change applies). The text of each page is
// served as /static/help-<page>.md and the list of pages as /static/help-index.json. Three parts of the documentation are
// generated from the running service so they cannot drift from it: the settings, the meter presets and the
// transition presets (a line `{{generated:name}}` in a page).
//
//   #/help                      the first page
//   #/help/<page>#<anchor>      a page and a heading of it
//   #/help/for/<route>          the page of an app page (the "?" links use this)
NAV.push(['help', 'Help']);

const HELP = { index: null, text: new Map(), search: null, api: new Map() };
const helpIndex = () => HELP.index ||= J('/static/help-index.json');
async function helpText(slug) {
  if (!HELP.text.has(slug)) {
    const r = await fetch(`/static/help-${encodeURIComponent(slug)}.md`);
    if (!r.ok) throw new Error('There is no help page called ' + slug);
    HELP.text.set(slug, await r.text());
  }
  return HELP.text.get(slug);
}
const helpApi = url => { if (!HELP.api.has(url)) HELP.api.set(url, J(url)); return HELP.api.get(url); };

// ---- the generated parts ----------------------------------------------------------------------------------------
const helpCode = t => h('code', {}, t);
// Same shape as the tables help-md.js writes (few columns: cards on a phone; more: scroll inside the wrapper).
const helpTable = (head, rows) => {
  for (const r of rows) [...r.children].forEach((td, k) => td.setAttribute('data-label', head[k] || ''));
  return h('div', { class: 'help-table' + (head.length <= 3 ? ` stack stack${head.length}` : ''), style: `--cols:${head.length}` }, h('table', {}, h('thead', {}, h('tr', {}, head.map(x => h('th', {}, x)))), h('tbody', {}, rows)));
};
// A table that is wider than its column scrolls sideways; say so, once, above it (and take the line away if it fits).
function helpScrollHints(root) {
  const ro = new ResizeObserver(() => {
    for (const w of $$('.help-table', root)) {
      const over = w.scrollWidth > w.clientWidth + 1 && getComputedStyle(w).overflowX !== 'visible';
      w.classList.toggle('scrolls', over);
      const has = w.previousElementSibling?.classList.contains('help-scrollhint');
      if (over && !has) w.before(h('div', { class: 'help-scrollhint', 'aria-hidden': 'true' }, 'Scroll sideways to see every column'));
      if (!over && has) w.previousElementSibling.remove();
    }
  });
  ro.observe(root);
  onCleanup(() => ro.disconnect());
}

const HELP_GEN = {
  // Every key of the station file and of the global file, grouped as the Settings page groups them.
  async settings(el) {
    const data = await helpApi('/api/v1/settings');
    const known = new Set(GROUPS.flatMap(g => g[1]));
    const nStation = GROUPS.findIndex(g => g[0] === 'Global');
    const scopes = [['station', 'Station settings', GROUPS.slice(0, nStation)], ['global', 'Settings of this box', GROUPS.slice(nStation)]];
    let count = 0;
    for (const [scope, title, groups] of scopes) {
      const defs = data.catalog[scope];
      const byKey = Object.fromEntries(defs.map(d => [d.key, d]));
      const all = [...groups];
      const extra = defs.filter(d => !known.has(d.key)).map(d => d.key);
      if (extra.length) all.push(['Other', extra]);
      el.append(h('h2', { id: 'settings-' + scope }, title));
      for (const [gname, keys] of all) {
        const rows = keys.filter(k => byKey[k]).map(k => {
          const d = byKey[k], ap = APPLY[d.apply] || [d.apply, '', ''];
          const range = d.options.length ? 'one of ' + d.options.join(', ') : (d.kind === 'int' || d.kind === 'float') && d.max ? `${d.min} to ${d.max}` : '';
          count++;
          return h('tr', { id: 'setting-' + d.key.replace(/\./g, '-'), 'data-key': d.key },
            h('td', {}, helpCode(d.key)), h('td', {}, [d.kind === 'readonly' ? 'fixed' : d.kind === 'numlist' ? 'list of numbers' : d.kind, range ? h('div', { class: 'dim' }, range) : null]),
            h('td', { class: 'mono' }, fmtDefault(d.default)),
            h('td', {}, h('span', { class: 'chip ' + ap[1] }, ap[0])), h('td', {}, d.help));
        });
        if (rows.length) el.append(h('h3', { id: `${scope}-${gname.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')}` }, gname),
          helpTable(['Key', 'Type and range', 'Default', 'Takes effect', 'What it does'], rows));
      }
    }
    el.dataset.count = String(count);
    el.append(h('p', { class: 'dim' }, `${count} settings. This list is built from the running service, so it always matches the Settings page.`));
  },
  // The meter presets, with the numbers the display uses.
  async ballistics(el) {
    const d = await helpApi('/api/v1/display/meters');
    el.append(helpTable(['Preset', 'Reads', 'Rises 20 dB in', 'Falls at', 'Peak tick'], d.presets.map(p => h('tr', { 'data-preset': p.preset },
      h('td', {}, p.label, h('div', { class: 'dim' }, helpCode(p.preset))), h('td', {}, p.source === 'ppm' ? 'the measured PPM, as it is' : 'the sample peak, with this attack and fall'),
      h('td', {}, p.source === 'ppm' ? 'as measured' : p.attack_ms ? p.attack_ms + ' ms' : 'at once'), h('td', {}, p.source === 'ppm' ? `${p.release_db_s} dB/s` : `${(+p.release_db_s).toFixed(1)} dB/s`),
      h('td', {}, p.hold ? `held ${p.hold_ms} ms` + (p.hold_fall_db_s ? `, then falls ${p.hold_fall_db_s} dB/s` : ', then drops') : 'none')))));
  },
  // The transition presets of the library, built in and the user's own.
  async transitions(el) {
    const d = await helpApi('/api/v1/transition-presets');
    const flat = (o, p = '') => Object.entries(o || {}).flatMap(([k, v]) => v && typeof v === 'object' && !Array.isArray(v) ? flat(v, p + k + '.') : [`${p}${k} = ${JSON.stringify(v)}`]);
    el.append(helpTable(['Preset', 'What it does', 'Parameters'], d.items.map(p => h('tr', { 'data-preset': p.name },
      h('td', {}, helpCode(p.name), h('div', { class: 'dim' }, p.builtin ? 'built in' : 'your own')), h('td', {}, p.description || ''),
      h('td', { class: 'mono', style: 'font-size:12px' }, flat(p.params).join('; '))))));
  },
};

// Fill the placeholders of a rendered page; resolves when every one is done (a failure is written in place).
async function helpFill(root) {
  await Promise.all($$('.help-generated', root).map(async el => {
    const fn = HELP_GEN[el.dataset.generated];
    try { if (!fn) throw new Error('unknown generated block ' + el.dataset.generated); await fn(el); }
    catch (e) { el.append(h('div', { class: 'notice' }, 'This part is built from the running service and it did not answer: ' + e.message)); }
  }));
}

// ---- search -----------------------------------------------------------------------------------------------------
// The text of every page, in sections (a heading and what follows it), built once from the rendered pages.
async function helpSearchIndex() {
  if (HELP.search) return HELP.search;
  const idx = await helpIndex();
  const docs = await Promise.all(idx.pages.map(async p => {
    const box = h('div');
    box.innerHTML = HelpMd.render(await helpText(p.slug), { page: p.slug }).html;
    await helpFill(box);
    let cur = { id: '', heading: p.title, text: '' };
    const secs = [cur];
    for (const n of box.children) {
      if (/^H[1-4]$/.test(n.tagName)) { cur = { id: n.id, heading: n.firstChild?.textContent || '', text: '' }; secs.push(cur); } else cur.text += ' ' + n.textContent;
    }
    return { slug: p.slug, title: p.title, group: p.group, secs };
  }));
  return HELP.search = docs;
}
function helpSearch(docs, query) {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return [];
  const hits = [];
  for (const d of docs) for (const s of d.secs) {
    const head = (d.title + ' ' + s.heading).toLowerCase(), body = s.text.toLowerCase(), all = head + ' ' + body;
    if (!terms.every(t => all.includes(t))) continue;
    let score = terms.reduce((a, t) => a + (s.heading.toLowerCase().includes(t) ? 8 : 0) + (d.title.toLowerCase().includes(t) ? 3 : 0) + Math.min(5, body.split(t).length - 1), 0);
    if (!s.id) score -= 1;
    const at = body.indexOf(terms[0]);
    const snippet = at < 0 ? s.text.trim().slice(0, 140) : (at > 60 ? '...' : '') + s.text.slice(Math.max(0, at - 60), at + 100).trim() + '...';
    hits.push({ d, s, score, snippet });
  }
  return hits.sort((a, b) => b.score - a.score).slice(0, 30);
}
function helpMark(text, terms) {
  const out = []; const low = text.toLowerCase(); let i = 0;
  while (i < text.length) {
    let best = -1, len = 0;
    for (const t of terms) { const k = low.indexOf(t, i); if (k >= 0 && (best < 0 || k < best)) { best = k; len = t.length; } }
    if (best < 0) { out.push(text.slice(i)); break; }
    out.push(text.slice(i, best), h('mark', {}, text.slice(best, best + len))); i = best + len;
  }
  return out;
}

// ---- the page ---------------------------------------------------------------------------------------------------
function helpScroll(anchor) {
  const el = anchor && document.getElementById(anchor);
  if (el) el.scrollIntoView({ block: 'start' }); else window.scrollTo(0, 0);
}

routes.help = async parts => {
  const idx = await helpIndex();
  let [first, ...rest] = parts;
  let slug = (first || '').split('#')[0], anchor = (first || '').split('#')[1] || '';
  if (slug === 'for') {
    // `#/help/for/<route or page>[#anchor]`: the page of an app page, then the address becomes the page's own
    const target = (rest[0] || '').split('#')[0];
    anchor = (rest[0] || '').split('#')[1] || '';
    slug = idx.routes[target] || (idx.pages.some(p => p.slug === target) ? target : 'overview');
    history.replaceState(null, '', `#/help/${slug}${anchor ? '#' + anchor : ''}`);
  }
  if (!slug) slug = idx.pages[0].slug;
  const page = idx.pages.find(p => p.slug === slug);
  const root = h('div', { class: 'help' });
  const toc = h('aside', { class: 'help-toc', 'aria-label': 'Help contents' });
  const body = h('article', { class: 'help-body' });
  root.append(toc, body);

  const q = h('input', { type: 'search', placeholder: 'Search the help', id: 'help-search', 'aria-label': 'Search the help', autocomplete: 'off' });
  // Wide screens: a column beside the page. Narrow ones: a bar with a Contents button (and the search) that opens the list.
  const tocBtn = h('button', { type: 'button', class: 'help-tocbtn', 'aria-expanded': 'false', 'aria-controls': 'help-contents' }, 'Contents', h('span', { class: 'dim' }, page ? page.title : ''));
  const panel = h('div', { class: 'help-tocpanel', id: 'help-contents' });
  toc.append(h('div', { class: 'help-tocbar' }, tocBtn, q), panel);
  const setToc = open => { toc.classList.toggle('open', open); tocBtn.setAttribute('aria-expanded', String(open)); };
  tocBtn.addEventListener('click', () => setToc(!toc.classList.contains('open')));
  panel.addEventListener('click', ev => { if (ev.target.closest('a')) setToc(false); });
  toc.addEventListener('keydown', ev => { if (ev.key === 'Escape' && toc.classList.contains('open')) { setToc(false); tocBtn.focus(); } });
  for (const g of idx.groups) {
    panel.append(h('h3', {}, g));
    for (const p of idx.pages.filter(x => x.group === g)) panel.append(h('a', { href: '#/help/' + p.slug, class: p.slug === slug ? 'on' : '', 'data-page': p.slug }, p.title));
  }

  let article = null;
  const showPage = async () => {
    if (!page) { body.replaceChildren(h('div', { class: 'notice bad' }, `There is no help page called "${slug}". `, h('a', { href: '#/help' }, 'Open the help home.'))); return; }
    const out = HelpMd.render(await helpText(slug), { page: slug });
    article = h('div', { class: 'help-page', 'data-page': slug });
    article.innerHTML = out.html;
    const heads = out.headings.filter(x => x.level === 2);
    if (heads.length > 2) {
      const on = h('div', { class: 'help-on' }, h('b', {}, 'On this page'), ' ', heads.flatMap(x => [h('a', { href: `#/help/${slug}#${x.id}` }, x.text), ' ']));
      article.querySelector('h1')?.after(on);
    }
    // next and previous page
    const i = idx.pages.findIndex(p => p.slug === slug), prev = idx.pages[i - 1], next = idx.pages[i + 1];
    article.append(h('div', { class: 'help-pager' }, prev ? h('a', { href: '#/help/' + prev.slug }, '< ' + prev.title) : h('span'), next ? h('a', { href: '#/help/' + next.slug }, next.title + ' >') : null));
    body.replaceChildren(article);
    // After the router has put the page in the document: go to the heading, fill the generated parts (which move the
    // page), and go to the heading again.
    setTimeout(async () => {
      helpScroll(anchor);
      const gen = article.querySelector('.help-generated');
      await helpFill(article);
      if (anchor && gen) helpScroll(anchor);
    }, 0);
  };
  let timer;
  q.addEventListener('input', () => {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      const query = q.value.trim();
      if (!query) { body.replaceChildren(article || h('div')); return; }
      setToc(false);
      body.replaceChildren(h('div', { class: 'dim' }, 'Searching...'));
      const docs = await helpSearchIndex();
      const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
      const hits = helpSearch(docs, query);
      body.replaceChildren(h('h1', {}, 'Search'), h('div', { class: 'dim' }, hits.length ? `${hits.length} result${hits.length === 1 ? '' : 's'} for "${query}"` : `Nothing in the help matches "${query}".`),
        ...hits.map(x => h('div', { class: 'help-hit', 'data-slug': x.d.slug, 'data-anchor': x.s.id },
          h('a', { href: `#/help/${x.d.slug}${x.s.id ? '#' + x.s.id : ''}` }, helpMark(x.s.heading && x.s.id ? `${x.d.title}: ${x.s.heading}` : x.d.title, terms)),
          h('div', { class: 'dim' }, helpMark(x.snippet, terms)))));
    }, 150);
  });
  q.addEventListener('keydown', ev => { if (ev.key === 'Escape') { q.value = ''; q.dispatchEvent(new Event('input')); } });
  onCleanup(() => clearTimeout(timer));
  helpScrollHints(body);
  await showPage();
  return root;
};
