// The Help pages' markdown renderer: a small, safe subset. It turns text into an HTML string in which every piece of
// the input is escaped; there is no way to pass raw HTML through. Supported: headings (# to ####, with an optional
// {#id}), paragraphs, bullet and numbered lists (nested by indentation), pipe tables, fenced code, block quotes, rules,
// `code`, **bold**, *italic*, links, and a line `{{generated:name}}` that becomes an empty block the Help page fills
// from the live API (the settings, the meter presets, the transition presets).
//
// Links: `[text](page)` and `[text](page#anchor)` go to a Help page, `[text](#anchor)` to a heading of the same page,
// `[text](#/route)` to a page of the app, `[text](https://...)` to another site. Anything else (javascript:, data:)
// is written as plain text. The module works in the browser (window.HelpMd) and in node (dev/help-md-test.mjs).
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api; else root.HelpMd = api;
})(typeof self !== 'undefined' ? self : this, function () {
  const esc = s => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

  // The heading id: text without markup, lower case, runs of anything but letters and digits as one dash.
  function slugify(text) {
    return String(text).replace(/[`*]/g, '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'section';
  }

  // Where a link goes, or null when it is not one this renderer will write.
  function href(url, page) {
    url = url.trim();
    if (/^https?:\/\/[^\s"<>]+$/i.test(url)) return { url, external: true };
    if (url.startsWith('#/')) return /^#\/[A-Za-z0-9_\-\/#.?=&]*$/.test(url) ? { url } : null;
    if (url.startsWith('#')) return /^#[A-Za-z0-9_-]+$/.test(url) ? { url: '#/help/' + (page || 'overview') + url } : null;
    const m = /^([a-z0-9-]+)(#[A-Za-z0-9_-]+)?$/.exec(url);
    return m ? { url: '#/help/' + m[1] + (m[2] || '') } : null;
  }

  // Inline markup of one run of text.
  function inline(s, page, noLinks) {
    let out = '', i = 0;
    while (i < s.length) {
      const c = s[i];
      if (c === '\\' && i + 1 < s.length && /[\\`*_{}\[\]()#+\-.!|>]/.test(s[i + 1])) { out += esc(s[i + 1]); i += 2; continue; }
      if (c === '`') {
        const j = s.indexOf('`', i + 1);
        if (j > i) { out += '<code>' + esc(s.slice(i + 1, j)) + '</code>'; i = j + 1; continue; }
      }
      if (c === '[' && !noLinks) {
        const close = s.indexOf('](', i + 1);
        const end = close > 0 ? s.indexOf(')', close + 2) : -1;
        if (close > 0 && end > 0) {
          const label = s.slice(i + 1, close), h = href(s.slice(close + 2, end), page);
          if (h) {
            out += '<a href="' + esc(h.url) + '"' + (h.external ? ' target="_blank" rel="noopener noreferrer"' : '') + '>' + inline(label, page, true) + '</a>';
          } else out += inline(label, page, true);
          i = end + 1; continue;
        }
      }
      if (c === '*' && s[i + 1] === '*') {
        const j = s.indexOf('**', i + 2);
        if (j > i + 2) { out += '<strong>' + inline(s.slice(i + 2, j), page, noLinks) + '</strong>'; i = j + 2; continue; }
      }
      if (c === '*' && s[i + 1] && s[i + 1] !== ' ' && s[i + 1] !== '*') {
        const j = s.indexOf('*', i + 1);
        if (j > i + 1 && s[j - 1] !== ' ') { out += '<em>' + inline(s.slice(i + 1, j), page, noLinks) + '</em>'; i = j + 1; continue; }
      }
      out += esc(c); i++;
    }
    return out;
  }

  const FENCE = /^```([A-Za-z0-9_-]*)\s*$/;
  const HEADING = /^(#{1,4})\s+(.*?)(?:\s+\{#([a-z0-9-]+)\})?\s*$/;
  const RULE = /^(---+|\*\*\*+)\s*$/;
  const ITEM = /^(\s*)([-*]|\d+\.)\s+(.*)$/;
  const TABLE_SEP = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;
  const GENERATED = /^\{\{generated:([a-z-]+)\}\}\s*$/;

  function splitRow(line) {
    let t = line.trim();
    if (t.startsWith('|')) t = t.slice(1);
    if (t.endsWith('|') && !t.endsWith('\\|')) t = t.slice(0, -1);
    const cells = []; let cur = '', code = false;
    for (let i = 0; i < t.length; i++) {
      const c = t[i];
      if (c === '\\' && t[i + 1] === '|') { cur += '|'; i++; continue; }
      if (c === '`') code = !code;
      if (c === '|' && !code) { cells.push(cur.trim()); cur = ''; continue; }
      cur += c;
    }
    cells.push(cur.trim());
    return cells;
  }

  const startsBlock = l => FENCE.test(l) || HEADING.test(l) || RULE.test(l) || ITEM.test(l) || /^>/.test(l) || GENERATED.test(l);

  // Render markdown. Returns { html, headings: [{level, text, id}], title }.
  function render(src, opts) {
    const page = (opts && opts.page) || '';
    const lines = String(src).replace(/\r\n?/g, '\n').split('\n');
    const headings = [], used = new Set();
    let title = '';
    const out = [];
    const uniq = base => { let id = base, n = 2; while (used.has(id)) id = base + '-' + n++; used.add(id); return id; };

    function list(start, indent) {
      // One list whose markers sit at `indent`; returns [html, next line].
      const first = ITEM.exec(lines[start]);
      const ordered = /\d/.test(first[2]);
      let i = start; const items = [];
      while (i < lines.length) {
        const m = ITEM.exec(lines[i]);
        if (!m || m[1].length < indent || m[1].length > indent) break;
        let text = m[3]; i++;
        let nested = '';
        while (i < lines.length) {
          const l = lines[i];
          if (!l.trim()) {
            // a blank line ends the item unless the next non-blank line is indented under it
            let k = i; while (k < lines.length && !lines[k].trim()) k++;
            if (k < lines.length && /^\s{2,}\S/.test(lines[k]) && lines[k].search(/\S/) > indent) { i = k; continue; }
            break;
          }
          const n = ITEM.exec(l);
          if (n && n[1].length > indent) { const [h, ni] = list(i, n[1].length); nested += h; i = ni; continue; }
          if (n || l.search(/\S/) <= indent) break;
          text += ' ' + l.trim(); i++;
        }
        items.push('<li>' + inline(text, page) + nested + '</li>');
      }
      const tag = ordered ? 'ol' : 'ul';
      return ['<' + tag + '>' + items.join('') + '</' + tag + '>', i];
    }

    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (!line.trim()) { i++; continue; }
      let m;
      if ((m = FENCE.exec(line))) {
        const body = []; i++;
        while (i < lines.length && !/^```\s*$/.test(lines[i])) body.push(lines[i++]);
        i++;
        out.push('<pre><code' + (m[1] ? ' class="lang-' + esc(m[1]) + '"' : '') + '>' + esc(body.join('\n')) + '</code></pre>');
        continue;
      }
      if ((m = HEADING.exec(line))) {
        const level = m[1].length, text = m[2].trim();
        const id = uniq(m[3] || slugify(text));
        headings.push({ level, text: text.replace(/[`*]/g, ''), id });
        if (level === 1 && !title) title = text.replace(/[`*]/g, '');
        out.push('<h' + level + ' id="' + esc(id) + '">' + inline(text, page, true) + '<a class="anchor" href="#/help/' + esc(page || 'overview') + '#' + esc(id) + '" title="Link to this section">#</a></h' + level + '>');
        i++; continue;
      }
      if (RULE.test(line)) { out.push('<hr>'); i++; continue; }
      if ((m = GENERATED.exec(line))) { out.push('<div class="help-generated" data-generated="' + esc(m[1]) + '"></div>'); i++; continue; }
      if (line.trim().startsWith('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1])) {
        const head = splitRow(line); i += 2;
        const rows = [];
        while (i < lines.length && lines[i].trim().startsWith('|')) rows.push(splitRow(lines[i++]));
        // Few columns (a term and what it means): on a phone each row can become a card (CSS, class stack, the header text
        // is in data-label); more columns scroll sideways inside the wrapper, each with a minimum width (--cols).
        const stack = head.length <= 3, label = c => esc(c.replace(/[`*]/g, ''));
        out.push('<div class="help-table' + (stack ? ' stack stack' + head.length : '') + '" style="--cols:' + head.length + '"><table><thead><tr>' + head.map(c => '<th>' + inline(c, page) + '</th>').join('') + '</tr></thead><tbody>' +
          rows.map(r => '<tr>' + head.map((c, k) => '<td data-label="' + label(c) + '">' + inline(r[k] || '', page) + '</td>').join('') + '</tr>').join('') + '</tbody></table></div>');
        continue;
      }
      if (/^>/.test(line)) {
        const body = [];
        while (i < lines.length && /^>/.test(lines[i])) body.push(lines[i++].replace(/^>\s?/, ''));
        out.push('<blockquote>' + render(body.join('\n'), { page }).html + '</blockquote>');
        continue;
      }
      if (ITEM.test(line)) { const [h, ni] = list(i, ITEM.exec(line)[1].length); out.push(h); i = ni; continue; }
      // a paragraph
      const para = [line.trim()]; i++;
      while (i < lines.length && lines[i].trim() && !startsBlock(lines[i]) && !(lines[i].trim().startsWith('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]))) para.push(lines[i++].trim());
      out.push('<p>' + inline(para.join(' '), page) + '</p>');
    }
    return { html: out.join('\n'), headings, title };
  }

  return { render, inline, slugify, href, esc };
});
