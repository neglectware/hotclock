# Contributing

## The standing rule: the Help is part of the change

Hotclock's documentation is built into the program (the **Help** section of the web UI, `crates/serve/help/*.md`). It is the
only documentation an operator is sure to have, and it must match the build they run.

**Every change that adds or changes a feature, a setting, an alert, a playlist check or a page of the web UI updates its Help page
in the same commit.** Not a later commit, not a follow-up. A reviewer can refuse a change for this alone.

Tests enforce the part that can be checked by a machine (`crates/serve/src/help.rs`, `cargo test -p serve --lib help`):

| A change that... | fails the build unless... |
|---|---|
| adds a key to the settings catalog (`crates/serve/src/settings.rs`) | the key is in a group of `GROUPS` in `crates/serve/web/settings.js`, so the Settings page and the Help's settings reference both lay it out. Its help text must be a real sentence. The reference itself is generated from the catalog and needs no editing. |
| adds an alert code (`station/src/alerts.rs`, or `CODES` in `serve/src/notify.rs`) | `help/alerts.md` has a `### \`code\`` section for it, with a `**Default delivery:**` line that agrees with `notify::default_rule`. A code removed from the program must leave the page. |
| adds a playlist check (a code passed to `bad(` or `push(` in `station/src/programming.rs`) | `help/playlist-checks.md` has a `### \`code\`` section for it. |
| adds a page, a route or a sidebar item of the web UI (`app.js`, `settings.js`, `help.js`) | `ROUTES` in `crates/serve/src/help.rs` names its Help page, and the page exists. Give the page title a `helpLink('<route>')` "?" button. |
| writes a link in a Help page, or a `helpLink(...)` in the app | the page and the heading it names exist. |

The tests do not check that your sentences are true. That is your job, and the reviewer's: read the code you describe.

## Writing a Help page

- A page is a markdown file `crates/serve/help/<slug>.md`, listed in `PAGES` of `crates/serve/src/help.rs` with its group. The
  first `# ` line is its title. **The first paragraph is one sentence that says what the page is for.**
- Plain, direct American English. Short sentences. Task first: what the operator wants to do, then what the system does. No
  marketing words. No em dashes (a test fails on them). Write a number and a unit, not "fast" or "a while".
- **Say what is built.** A feature that is designed and not built goes under **Coming** and says so in the first words. The table in
  `overview.md` ("What is built and what is coming") lists them. Move an item out of "Coming" in the commit that builds it.
- **Generic product documentation.** The Help may ship publicly. No host names, addresses, people, stations or programs of any
  particular installation (a test checks the obvious ones).
- Verify every claim against the code or a test. Say exact values where they exist (defaults, ranges, time limits).
- Headings get ids from their text (`## Hold after this` is `#hold-after-this`). Give a heading an explicit id with
  `## Hold after this {#hold}` when other pages or a "?" button link to it, so that a rewording does not break the link.
- Link with `[text](page)`, `[text](page#anchor)`, `[text](#anchor)` (the same page), `[text](#/settings)` (a page of the app) or an
  `https://` address.

### What the renderer supports (`crates/serve/web/help-md.js`)

Headings `#` to `####`, paragraphs, bullet and numbered lists (nested by indentation), pipe tables, fenced code, block quotes,
rules, `code`, **bold**, *italic* and links. Raw HTML is never passed through: everything is escaped. The renderer's unit tests are
`node dev/help-md-test.mjs`.

A line `{{generated:settings}}`, `{{generated:ballistics}}` or `{{generated:transitions}}` is replaced when the page is shown by a
table built from the running service (`crates/serve/web/help.js`, `HELP_GEN`). Use that, and not a copy of the numbers, for
anything the program already lists in an API.

## Checks to run before you hand a change over

    dev/linux-test.sh cargo fmt --all -- --check
    dev/linux-test.sh cargo clippy --workspace --all-targets -- -D warnings
    dev/linux-test.sh cargo test --workspace -j 2 --no-fail-fast -- --test-threads=1 --skip real_audio
    node dev/help-md-test.mjs                                   # the renderer
    dev/linux-test.sh cargo build -p hotclock && dev/help-ui-check.sh   # the Help in headless Chrome (muted)

`dev/gaps-ui-check.sh` drives the Schedule editor, quarantine release, playlist Enabled toggle and queue transition menu.

Other UI checks (`dev/*-ui-check.sh`) take their ports from the worktree path (`dev/check-env.sh`).

## Where a feature in progress puts its Help

A feature developed on its own branch writes its Help page or section on that branch. The loudness controller, for example,
replaces the "The loudness controller" section of `help/output-and-level.md`, moves its row in `overview.md` from "Coming" to
"Built", and removes the `loudness_at_limit` and `loudness_frozen` "Not raised in this version" notes in `help/alerts.md` once the
alerts are raised. A new card on a page of the web UI gets a `helpLink` and a heading in that page's Help page.
