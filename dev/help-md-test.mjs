// Unit tests of the Help renderer (crates/serve/web/help-md.js): escaping, links, tables, code, lists, headings.
// Run: node dev/help-md-test.mjs   (no browser, no dependencies)
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const Md = createRequire(import.meta.url)(join(root, 'crates/serve/web/help-md.js'));
let pass = 0, fail = 0;
const t = (name, ok, extra = '') => { if (ok) pass++; else { fail++; console.error('FAIL  ' + name + (extra ? '\n      ' + extra : '')); } };
const html = (src, page = 'p') => Md.render(src, { page }).html.replace(/ data-label="[^"]*"/g, '');  // the labels have their own test below
const has = (name, src, frag, page) => { const h = html(src, page); t(name, h.includes(frag), `wanted ${JSON.stringify(frag)} in ${JSON.stringify(h)}`); };
t('a table of up to three columns is a card table and each cell carries its header', (() => { const h = Md.render('| Key | Meaning |\n|---|---|\n| a | b |', { page: 'p' }).html; return h.includes('help-table stack stack2') && h.includes('<td data-label="Meaning">b</td>'); })());
t('a table of four columns is not a card table', !Md.render('| a | b | c | d |\n|---|---|---|---|\n| 1 | 2 | 3 | 4 |', { page: 'p' }).html.includes('stack'));
const lacks = (name, src, frag, page) => { const h = html(src, page); t(name, !h.includes(frag), `did not want ${JSON.stringify(frag)} in ${JSON.stringify(h)}`); };

// ---- escaping: nothing of the input reaches the output as markup ----
lacks('a script tag in a paragraph is escaped', 'hello <script>alert(1)</script>', '<script');
has('...and shown as text', 'hello <script>alert(1)</script>', '&lt;script&gt;');
lacks('an image tag with a handler is escaped', '<img src=x onerror=alert(1)>', '<img');
lacks('a tag in bold is escaped', '**<b onclick=x>hi</b>**', '<b onclick');
lacks('a tag in a heading is escaped', '# Title <iframe src=x>', '<iframe');
lacks('a tag in link text is escaped', '[<script>x</script>](other)', '<script');
has('code spans are escaped', 'use `<div class="a">` here', '<code>&lt;div class=&quot;a&quot;&gt;</code>');
has('a fenced block is escaped', '```\n<script>x</script>\n```', '<pre><code>&lt;script&gt;x&lt;/script&gt;</code></pre>');
has('a quote character in text is escaped', 'say "hi" & go', 'say &quot;hi&quot; &amp; go');
lacks('a table cell is escaped', '| a |\n|---|\n| <b>x</b> |', '<b>x');
has('a table cell shows the text', '| a |\n|---|\n| <b>x</b> |', '&lt;b&gt;x&lt;/b&gt;');
lacks('a list item is escaped', '- <u>x</u>', '<u>');
lacks('a block quote is escaped', '> <script>x</script>', '<script');
lacks('a heading id cannot carry markup', '# a {#x"onload=y}', 'onload=y"');

// ---- links ----
has('a page link goes to the help route', '[x](on-air)', '<a href="#/help/on-air">x</a>');
has('a page link with an anchor', '[x](on-air#assist-mode)', 'href="#/help/on-air#assist-mode"');
has('a same-page anchor uses the page', '[x](#queue)', 'href="#/help/glossary#queue"', 'glossary');
has('an app route is kept', '[x](#/settings)', 'href="#/settings"');
has('an https link opens in a new tab safely', '[x](https://example.org/a?b=1&c=2)', 'target="_blank" rel="noopener noreferrer"');
has('an https link escapes its ampersand', '[x](https://example.org/a?b=1&c=2)', 'b=1&amp;c=2');
lacks('a javascript: link is refused', '[x](javascript:alert(1))', 'href');
has('...and its text stays', '[x](javascript:alert(1))', 'x');
lacks('a data: link is refused', '[x](data:text/html;base64,AAAA)', 'href');
lacks('a link with a quote is refused', '[x](https://a.org/"onmouseover="y)', 'onmouseover="');
{ const h = html('[a [b](c) d](e) and [x [y](z)](w)'); t('anchors never nest', !/<a [^>]*>[^<]*<a /.test(h), h); }

// ---- inline ----
has('bold', 'a **b** c', '<strong>b</strong>');
has('italic', 'a *b* c', '<em>b</em>');
has('snake_case is not emphasis', 'the target_lufs_x key', 'target_lufs_x');
lacks('cron stars in code stay text', 'run `0 * * * *` now', '<em>');
has('an escaped star', 'a \\*b\\* c', 'a *b* c');
has('code keeps stars', '`a * b * c`', '<code>a * b * c</code>');

// ---- blocks ----
has('a heading has an id', '## The Queue', '<h2 id="the-queue">');
has('a heading takes an explicit id', '## The Queue {#q}', '<h2 id="q">');
has('a heading id is stripped of markup', '### `silence`', '<h3 id="silence">');
const dup = Md.render('## A\n## A\n## A').headings.map(h => h.id).join(',');
t('duplicate headings get distinct ids', dup === 'a,a-2,a-3', dup);
t('the title is the first h1', Md.render('# Hello `x`\ntext').title === 'Hello x');
has('a paragraph joins its lines', 'one\ntwo\n\nthree', '<p>one two</p>');
has('a rule', 'a\n\n---\n\nb', '<hr>');
has('a block quote', '> note\n> more', '<blockquote><p>note more</p></blockquote>');
has('a bullet list', '- a\n- b', '<ul><li>a</li><li>b</li></ul>');
has('a star list', '* a\n* b', '<ul><li>a</li><li>b</li></ul>');
has('a numbered list', '1. a\n2. b', '<ol><li>a</li><li>b</li></ol>');
has('a nested list', '- a\n  - b\n  - c\n- d', '<li>a<ul><li>b</li><li>c</li></ul></li><li>d</li>');
has('a list item continues on the next line', '- a\n  more\n- b', '<li>a more</li>');
has('a list ends at a heading', '- a\n## H', '</ul>\n<h2');
has('a list item keeps inline code', '- `x` does y', '<li><code>x</code> does y</li>');
has('a paragraph stops at a list', 'text\n- a', '<p>text</p>\n<ul>');

// ---- tables ----
const tbl = html('| Key | Meaning |\n|---|---|\n| `a` | one |\n| b | two |');
t('a table has a head', tbl.includes('<thead><tr><th>Key</th><th>Meaning</th></tr></thead>'), tbl);
t('a table has its rows', tbl.includes('<tr><td><code>a</code></td><td>one</td></tr><tr><td>b</td><td>two</td></tr>'), tbl);
has('a short row is padded', '| a | b |\n|---|---|\n| only |', '<td>only</td><td></td>');
has('an escaped pipe stays in the cell', '| a |\n|---|\n| x \\| y |', '<td>x | y</td>');
has('a pipe inside code stays in the cell', '| a | b |\n|---|---|\n| `x|y` | z |', '<td><code>x|y</code></td><td>z</td>');
has('a table sits in a scrolling wrapper', '| a |\n|---|\n| 1 |', '<div class="help-table stack stack1" style="--cols:1"><table>');

// ---- code ----
has('a fence keeps its lines', '```\na\n  b\n```', '<pre><code>a\n  b</code></pre>');
has('a fence takes a language class', '```sh\nls\n```', 'class="lang-sh"');
has('markup inside a fence is not rendered', '```\n**not bold** [x](y)\n```', '**not bold** [x](y)');
has('an unclosed fence runs to the end', '```\na\nb', '<pre><code>a\nb</code></pre>');

// ---- generated blocks ----
has('a generated directive becomes a placeholder', '{{generated:settings}}', '<div class="help-generated" data-generated="settings"></div>');
lacks('a directive with odd characters is plain text', '{{generated:x"><script>}}', '<script');

// ---- slugs ----
t('slugify lower-cases and dashes', Md.slugify('Hold after this') === 'hold-after-this');
t('slugify drops markup', Md.slugify('The `silence` alert') === 'the-silence-alert');
t('slugify of nothing is section', Md.slugify('???') === 'section');

console.log(`${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
