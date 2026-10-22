//! The built-in Help (`crates/serve/help/*.md`): the product's documentation, compiled into the binary and shown in the
//! web UI's Help section (`web/help.js`, rendered by `web/help-md.js`). The text is served as `/static/help-<page>.md`
//! and the list of pages, their groups and the page of each app route as `/static/help-index.json`.
//!
//! Three parts of the documentation are generated from the running service when the page is shown (a line
//! `{{generated:settings}}`, `{{generated:ballistics}}` or `{{generated:transitions}}`), so they cannot drift.
//!
//! **The standing rule (docs/CONTRIBUTING.md):** a change that adds or changes a feature, a setting, an alert or a page
//! updates its Help page in the same commit. The tests at the end of this file fail when a settings key is not laid out
//! on the Settings page and the Help, when an alert code or a playlist check has no entry in the Help, when a page of the
//! web UI has no Help page, or when a link in the Help or a "?" in the app points nowhere.

use serde_json::{json, Value};

/// One page of the Help.
pub struct Page {
    /// The page's name in the address (`#/help/<slug>`) and the file name (`help/<slug>.md`).
    pub slug: &'static str,
    pub group: &'static str,
    pub body: &'static str,
}

/// The groups of the table of contents, in order.
pub const GROUPS: &[&str] = &[
    "Start here",
    "The web pages",
    "How it works",
    "Running the box",
    "Reference",
];

macro_rules! page {
    ($slug:expr, $group:expr) => {
        Page {
            slug: $slug,
            group: $group,
            body: include_str!(concat!("../help/", $slug, ".md")),
        }
    };
}

/// Every page, in the order of the table of contents.
pub const PAGES: &[Page] = &[
    page!("overview", "Start here"),
    page!("getting-started", "Start here"),
    page!("stream-obs", "Start here"),
    page!("on-air", "The web pages"),
    page!("library", "The web pages"),
    page!("item-editor", "The web pages"),
    page!("plex-tools", "The web pages"),
    page!("playlists", "The web pages"),
    page!("timed-blocks", "The web pages"),
    page!("insert-rules", "The web pages"),
    page!("schedule", "The web pages"),
    page!("history", "The web pages"),
    page!("notifications", "The web pages"),
    page!("settings", "The web pages"),
    page!("stations", "The web pages"),
    page!("system", "The web pages"),
    page!("block-engine", "How it works"),
    page!("crossfades", "How it works"),
    page!("markers-and-analysis", "How it works"),
    page!("output-and-level", "How it works"),
    page!("processing", "How it works"),
    page!("status-display", "How it works"),
    page!("failures-and-recovery", "How it works"),
    page!("alerts", "How it works"),
    page!("stream-deck", "Running the box"),
    page!("upgrading-backup", "Running the box"),
    page!("troubleshooting", "Running the box"),
    page!("command-line", "Running the box"),
    page!("settings-reference", "Reference"),
    page!("playlist-checks", "Reference"),
    page!("glossary", "Reference"),
];

/// The page of each app route (the ids of `NAV` and `routes` in `web/app.js`, plus `display`, the status display, and
/// `plex`, the Plex card of the Library page). A route with no entry here fails the coverage test.
pub const ROUTES: &[(&str, &str)] = &[
    ("onair", "on-air"),
    ("library", "library"),
    ("item", "item-editor"),
    ("plex", "plex-tools"),
    ("playlists", "playlists"),
    ("schedule", "schedule"),
    ("insert", "insert-rules"),
    ("history", "history"),
    ("notify", "notifications"),
    ("settings", "settings"),
    ("stations", "stations"),
    ("processing", "processing"),
    ("system", "system"),
    ("display", "status-display"),
    ("help", "overview"),
];

/// A page's title: its first `# ` heading.
pub fn title(body: &str) -> &str {
    body.lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(str::trim)
        .unwrap_or("")
}

/// A page's summary: the first paragraph after the title, as it is written.
pub fn summary(body: &str) -> String {
    let mut para = vec![];
    let mut seen_title = false;
    for l in body.lines() {
        if !seen_title {
            seen_title = l.starts_with("# ");
            continue;
        }
        if l.trim().is_empty() {
            if para.is_empty() {
                continue;
            }
            break;
        }
        if l.starts_with('#') || l.starts_with("{{") {
            break;
        }
        para.push(l.trim());
    }
    para.join(" ")
}

/// The list of pages for the table of contents and the search.
pub fn index() -> Value {
    json!({
        "groups": GROUPS,
        "pages": PAGES.iter().map(|p| json!({
            "slug": p.slug, "group": p.group, "title": title(p.body), "summary": summary(p.body),
        })).collect::<Vec<_>>(),
        "routes": ROUTES.iter().map(|(r, s)| (r.to_string(), json!(s))).collect::<serde_json::Map<_, _>>(),
    })
}

/// `help-index.json` and `help-<page>.md`, as `(content type, body)`.
pub fn asset(name: &str) -> Option<(&'static str, String)> {
    if name == "help-index.json" {
        return Some(("application/json", index().to_string()));
    }
    let slug = name.strip_prefix("help-")?.strip_suffix(".md")?;
    PAGES
        .iter()
        .find(|p| p.slug == slug)
        .map(|p| ("text/markdown; charset=utf-8", p.body.to_string()))
}

/// The heading ids of a page, as `web/help-md.js` makes them: the explicit `{#id}`, else the text without markup,
/// lower case, runs of anything but letters and digits as one dash, a number added to a repeat.
pub fn heading_ids(body: &str) -> Vec<(usize, String, String)> {
    let mut out: Vec<(usize, String, String)> = vec![];
    let mut fenced = false;
    for line in body.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        if !(1..=4).contains(&level) || !line[level..].starts_with(' ') {
            continue;
        }
        let mut text = line[level..].trim().to_string();
        let mut explicit = None;
        if let (Some(i), true) = (text.rfind(" {#"), text.ends_with('}')) {
            explicit = Some(text[i + 3..text.len() - 1].to_string());
            text.truncate(i);
        }
        let plain: String = text.chars().filter(|c| *c != '`' && *c != '*').collect();
        let base = explicit.unwrap_or_else(|| slug(&plain));
        let mut id = base.clone();
        let mut n = 2;
        while out.iter().any(|(_, _, i)| *i == id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        out.push((level, plain, id));
    }
    out
}

/// `slugify` of `web/help-md.js`.
pub fn slug(text: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !s.is_empty() {
                s.push('-');
            }
            dash = false;
            s.push(c);
        } else {
            dash = true;
        }
    }
    if s.is_empty() {
        "section".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    const APP_JS: &str = include_str!("../web/app.js");
    const SETTINGS_JS: &str = include_str!("../web/settings.js");
    const PROCESSING_JS: &str = include_str!("../web/processing.js");
    const HELP_JS: &str = include_str!("../web/help.js");
    const STATIONS_JS: &str = include_str!("../web/stations.js");
    const ONAIRLAYOUT_JS: &str = include_str!("../web/onairlayout.js");
    const ALERTS_RS: &str = include_str!("../../station/src/alerts.rs");
    const PROGRAMMING_RS: &str = include_str!("../../station/src/programming.rs");

    fn page(slug: &str) -> &'static Page {
        PAGES
            .iter()
            .find(|p| p.slug == slug)
            .unwrap_or_else(|| panic!("no help page {slug}"))
    }

    /// The ids named in `NAV` and `NAV.splice/push` lines, and in `routes.<id> =` of the web scripts (the Processing
    /// page is in `processing.js`).
    fn app_pages() -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        for src in [APP_JS, SETTINGS_JS, STATIONS_JS, PROCESSING_JS, HELP_JS] {
            for line in src.lines() {
                let t = line.trim_start();
                if t.starts_with("const NAV =")
                    || t.starts_with("NAV.splice(")
                    || t.starts_with("NAV.push(")
                {
                    // pairs like ['onair', 'On Air']
                    let mut rest = line;
                    while let Some(i) = rest.find("['") {
                        rest = &rest[i + 2..];
                        if let Some(j) = rest.find('\'') {
                            ids.insert(rest[..j].to_string());
                        }
                    }
                }
                if let Some(r) = t.strip_prefix("routes.") {
                    if let Some(j) = r.find(" =") {
                        ids.insert(r[..j].to_string());
                    }
                }
            }
        }
        ids
    }

    #[test]
    fn every_page_is_well_formed() {
        let mut slugs = BTreeSet::new();
        for p in PAGES {
            assert!(slugs.insert(p.slug), "page {} is listed twice", p.slug);
            assert!(
                GROUPS.contains(&p.group),
                "page {} has the unknown group {}",
                p.slug,
                p.group
            );
            assert!(
                !title(p.body).is_empty(),
                "page {} has no `# ` title",
                p.slug
            );
            assert_eq!(
                p.body.lines().filter(|l| l.starts_with("# ")).count(),
                1,
                "page {} must have exactly one `# ` heading",
                p.slug
            );
            assert!(
                summary(p.body).len() > 20,
                "page {} must start with a sentence saying what the page is for",
                p.slug
            );
            assert!(
                !p.body.contains('\u{2014}') && !p.body.contains('\u{2013}'),
                "page {} has a dash character; write it another way",
                p.slug
            );
            let words = p.body.split_whitespace().count();
            assert!(words >= 120, "page {} has only {words} words", p.slug);
            for g in p
                .body
                .lines()
                .filter_map(|l| l.strip_prefix("{{generated:"))
            {
                let name = g.trim_end_matches("}}");
                assert!(
                    ["settings", "ballistics", "transitions"].contains(&name),
                    "page {} names the unknown generated block {name}",
                    p.slug
                );
            }
        }
        // the table of contents has no empty group
        for g in GROUPS {
            assert!(PAGES.iter().any(|p| p.group == *g), "group {g} has no page");
        }
    }

    #[test]
    fn the_standing_rule_every_page_of_the_web_ui_has_a_help_page() {
        let mut want = app_pages();
        want.insert("plex".into());
        want.insert("display".into());
        let have: BTreeSet<String> = ROUTES.iter().map(|(r, _)| r.to_string()).collect();
        assert_eq!(
            want.difference(&have).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "a page of the web UI has no Help page: add it to ROUTES in help.rs and write the page"
        );
        for (route, slug) in ROUTES {
            assert!(
                PAGES.iter().any(|p| p.slug == *slug),
                "route {route} names the missing page {slug}"
            );
        }
        // the sidebar itself: every entry of the nav is a page that exists in the Help
        assert!(
            app_pages().contains("help"),
            "the Help is not in the sidebar"
        );
    }

    #[test]
    fn every_settings_key_is_laid_out_on_the_settings_page_and_the_help() {
        // The Settings page and the Help's reference lay keys out by the `GROUPS` array of settings.js; a key that no group
        // names would be missing from the layout (both pages add an "Other" group as a safety net, which this test makes
        // a failure so the key gets a proper place).
        let start = SETTINGS_JS
            .find("const GROUPS = [")
            .expect("GROUPS in settings.js");
        let end = SETTINGS_JS[start..].find("];").expect("end of GROUPS") + start;
        let groups = &SETTINGS_JS[start..end];
        let mut missing = vec![];
        let mut keys = 0;
        for d in crate::settings::catalog() {
            keys += 1;
            if !groups.contains(&format!("'{}'", d.key)) {
                missing.push(format!("{}:{}", d.scope, d.key));
            }
            assert!(
                d.help.trim().len() > 10,
                "setting {} has no help text",
                d.key
            );
            assert!(
                [
                    "live",
                    "next_boundary",
                    "station_restart",
                    "library_restart",
                    "display_restart"
                ]
                .contains(&d.apply),
                "setting {} has an apply class the Help does not describe",
                d.key
            );
        }
        assert!(missing.is_empty(), "settings keys no group of settings.js names (so the Settings page and the Help do not lay them out): {missing:?}");
        assert!(keys > 50, "the catalog has only {keys} keys");
        // and the reference page is the generated one
        assert!(page("settings-reference")
            .body
            .contains("{{generated:settings}}"));
        assert!(page("status-display")
            .body
            .contains("{{generated:ballistics}}"));
        assert!(page("crossfades")
            .body
            .contains("{{generated:transitions}}"));
    }

    /// The codes between `### ` and the end of the line, for headings written as `### \`code\``.
    fn coded_headings(slug: &str) -> BTreeSet<String> {
        page(slug)
            .body
            .lines()
            .filter_map(|l| l.strip_prefix("### `"))
            .filter_map(|l| l.split('`').next())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_alert_code_is_in_the_help() {
        let mut codes: BTreeSet<String> =
            crate::notify::CODES.iter().map(|c| c.to_string()).collect();
        for l in ALERTS_RS.lines() {
            let t = l.trim();
            if let Some(r) = t.strip_prefix("pub const ") {
                if let Some((_, v)) = r.split_once(": &str = \"") {
                    if let Some(c) = v.strip_suffix("\";") {
                        codes.insert(c.to_string());
                    }
                }
            }
        }
        assert!(codes.len() >= 38, "found only {} alert codes", codes.len());
        let have = coded_headings("alerts");
        let missing: Vec<_> = codes.difference(&have).collect();
        assert!(
            missing.is_empty(),
            "alert codes with no `### \\`code\\`` section in help/alerts.md: {missing:?}"
        );
        let stale: Vec<_> = have.difference(&codes).collect();
        assert!(
            stale.is_empty(),
            "help/alerts.md documents codes the program does not have: {stale:?}"
        );
    }

    #[test]
    fn the_default_delivery_in_the_help_is_the_programs() {
        // Each section of alerts.md says "**Default delivery:** <how>." and the program has `notify::default_rule`.
        let mut checked = 0;
        let mut code = String::new();
        for l in page("alerts").body.lines() {
            if let Some(c) = l.strip_prefix("### `") {
                code = c.split('`').next().unwrap_or("").to_string();
            }
            if let Some(i) = l.find("**Default delivery:** ") {
                let said = l[i + "**Default delivery:** ".len()..].to_lowercase();
                let rule = crate::notify::default_rule(&code);
                let want = match rule.delivery.as_str() {
                    "immediate" => "immediate",
                    "hourly_digest" => "hourly digest",
                    "daily_digest" => "daily digest",
                    "off" => "off",
                    other => panic!("unknown delivery {other}"),
                };
                assert!(
                    said.starts_with(want),
                    "{code}: the Help says `{said}` and the program delivers {want}"
                );
                assert_eq!(
                    said.contains("also when it clears"),
                    rule.notify_on_clear,
                    "{code}: notify on clear differs from the program"
                );
                checked += 1;
            }
        }
        assert_eq!(
            checked,
            coded_headings("alerts").len(),
            "every alert section needs a **Default delivery:** line"
        );
    }

    /// The codes of the playlist validator: the code argument of each `bad(` and `push(` call (after an optional `b,`
    /// and an optional `true,` or `false,`), and each `code: "..."` of a problem built by hand.
    fn validator_codes() -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let src: String = PROGRAMMING_RS
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for open in ["bad(", "push("] {
            for part in src.split(open).skip(1) {
                let mut a = part.trim_start();
                if let Some(r) = a.strip_prefix("b,") {
                    a = r.trim_start();
                }
                if let Some(r) = a.strip_prefix("true,").or_else(|| a.strip_prefix("false,")) {
                    a = r.trim_start();
                }
                if let Some(r) = a.strip_prefix('"') {
                    if let Some(j) = r.find('"') {
                        let c = &r[..j];
                        if !c.is_empty() && c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')
                        {
                            out.insert(c.to_string());
                        }
                    }
                }
            }
        }
        for part in src.split("code: \"").skip(1) {
            if let Some(j) = part.find('"') {
                out.insert(part[..j].to_string());
            }
        }
        out
    }

    #[test]
    fn every_playlist_check_is_in_the_help() {
        let codes = validator_codes();
        assert!(
            codes.len() >= 15,
            "found only {} playlist checks: {codes:?}",
            codes.len()
        );
        let have = coded_headings("playlist-checks");
        let missing: Vec<_> = codes.difference(&have).collect();
        assert!(
            missing.is_empty(),
            "playlist checks with no section in help/playlist-checks.md: {missing:?}"
        );
        let stale: Vec<_> = have.difference(&codes).collect();
        assert!(
            stale.is_empty(),
            "help/playlist-checks.md documents checks the validator does not have: {stale:?}"
        );
    }

    #[test]
    fn every_link_lands_on_a_heading() {
        let mut ids: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for p in PAGES {
            ids.insert(
                p.slug,
                heading_ids(p.body).into_iter().map(|(_, _, i)| i).collect(),
            );
        }
        let ok = |slug: &str, anchor: &str, from: &str| {
            let Some(set) = ids.get(slug) else {
                panic!("{from}: there is no help page {slug}");
            };
            // headings of the generated settings reference are made when the page is shown
            let generated = slug == "settings-reference"
                && (anchor.starts_with("station-")
                    || anchor.starts_with("global-")
                    || anchor.starts_with("setting-")
                    || anchor.starts_with("settings-"));
            assert!(
                anchor.is_empty() || set.contains(anchor) || generated,
                "{from}: page {slug} has no heading with the id #{anchor}"
            );
        };
        // links inside the pages: [text](page), [text](page#anchor), [text](#anchor)
        for p in PAGES {
            let mut fenced = false;
            for line in p.body.lines() {
                if line.starts_with("```") {
                    fenced = !fenced;
                }
                if fenced {
                    continue;
                }
                let mut rest = line;
                while let Some(i) = rest.find("](") {
                    rest = &rest[i + 2..];
                    let Some(j) = rest.find(')') else { break };
                    let url = &rest[..j];
                    if url.starts_with("http") || url.starts_with("#/") {
                        continue;
                    }
                    let from = format!("help/{}.md", p.slug);
                    match url.split_once('#') {
                        Some(("", a)) => ok(p.slug, a, &from),
                        Some((s, a)) => ok(s, a, &from),
                        None => ok(url, "", &from),
                    }
                }
            }
        }
        // the "?" links of the app: helpLink('route-or-page', 'anchor')
        for (file, src) in [
            ("app.js", APP_JS),
            ("settings.js", SETTINGS_JS),
            ("processing.js", PROCESSING_JS),
            ("stations.js", STATIONS_JS),
            ("onairlayout.js", ONAIRLAYOUT_JS),
        ] {
            let mut rest = src;
            let mut n = 0;
            while let Some(i) = rest.find("helpLink(") {
                rest = &rest[i + 9..];
                let args: &str = &rest[..rest.find(')').unwrap_or(0)];
                let mut parts = args.split(',').map(|a| a.trim().trim_matches('\''));
                let target = parts.next().unwrap_or("");
                if target.is_empty()
                    || target.starts_with('(')
                    || target.contains(' ')
                    || target == "target"
                {
                    continue; // the definition itself
                }
                let anchor = parts.next().unwrap_or("");
                let slug = ROUTES
                    .iter()
                    .find(|(r, _)| *r == target)
                    .map(|(_, s)| *s)
                    .unwrap_or(target);
                ok(
                    slug,
                    if anchor.starts_with('`') { "" } else { anchor },
                    &format!("{file} helpLink({target})"),
                );
                n += 1;
            }
            assert!(
                n > 0 || file == "settings.js",
                "no helpLink found in {file}"
            );
        }
        // every app page has a "?" in its title
        for r in [
            "onair",
            "library",
            "item",
            "playlists",
            "schedule",
            "insert",
            "history",
            "notify",
            "settings",
            "stations",
            "system",
        ] {
            assert!(
                APP_JS.contains(&format!("helpLink('{r}')"))
                    || SETTINGS_JS.contains(&format!("helpLink('{r}')"))
                    || STATIONS_JS.contains(&format!("helpLink('{r}')")),
                "the {r} page has no \"?\" link to its Help page"
            );
        }
    }

    #[test]
    fn the_index_and_the_assets_are_served() {
        let (ct, body) = asset("help-index.json").unwrap();
        assert_eq!(ct, "application/json");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["pages"].as_array().unwrap().len(), PAGES.len());
        assert_eq!(v["routes"]["onair"], "on-air");
        assert_eq!(v["pages"][0]["slug"], "overview");
        assert!(v["pages"][0]["title"].as_str().unwrap().len() > 2);
        let (ct, body) = asset("help-on-air.md").unwrap();
        assert!(ct.starts_with("text/markdown"));
        assert!(body.starts_with("# "));
        assert!(asset("help-nope.md").is_none());
        assert!(asset("help-.md").is_none());
        assert!(asset("app.js").is_none());
        assert!(asset("help-../x.md").is_none());
    }

    #[test]
    fn heading_ids_match_the_renderer() {
        let ids: Vec<String> = heading_ids("# T\n## The Queue\n## The Queue\n### `silence`\n## Named {#mine}\n```\n## not a heading\n```\n#nospace\n").into_iter().map(|x| x.2).collect();
        assert_eq!(ids, ["t", "the-queue", "the-queue-2", "silence", "mine"]);
        assert_eq!(slug("Hold after this"), "hold-after-this");
        assert_eq!(slug("???"), "section");
    }

    #[test]
    fn no_private_material_in_the_help() {
        // The Help is product documentation that could ship publicly: no addresses, no host or person names.
        let banned = [
            "192.168.",
            "100.64.",
            "behringer",
            "plexamp",
        ];
        for p in PAGES {
            let low = p.body.to_lowercase();
            for b in banned {
                // Plexamp is named once, as a style the `plexamp` preset is modelled on; the word may not appear otherwise.
                if b == "plexamp" {
                    continue;
                }
                assert!(!low.contains(b), "help/{}.md contains {b:?}", p.slug);
            }
        }
    }
}
