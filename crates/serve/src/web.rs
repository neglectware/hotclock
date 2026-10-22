//! The embedded web pages (9, 11): plain HTML, CSS and JavaScript compiled into the binary, so the box
//! needs no Node toolchain and no files beside the executable. The product name is written into a page
//! when it is served (from `DISPLAY_NAME` or the config), never in the files themselves.

use crate::http::Response;

struct Asset {
    name: &'static str,
    content_type: &'static str,
    body: &'static str,
}

const ASSETS: &[Asset] = &[
    Asset {
        name: "display.html",
        content_type: "text/html; charset=utf-8",
        body: include_str!("../web/display.html"),
    },
    Asset {
        name: "app.html",
        content_type: "text/html; charset=utf-8",
        body: include_str!("../web/app.html"),
    },
    Asset {
        name: "common.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/common.js"),
    },
    Asset {
        name: "ballistics.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/ballistics.js"),
    },
    Asset {
        name: "curve.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/curve.js"),
    },
    Asset {
        name: "procview.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/procview.js"),
    },
    Asset {
        name: "screen.html",
        content_type: "text/html; charset=utf-8",
        body: include_str!("../web/screen.html"),
    },
    Asset {
        name: "screen.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/screen.js"),
    },
    Asset {
        name: "display.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/display.js"),
    },
    Asset {
        name: "onairlayout.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/onairlayout.js"),
    },
    Asset {
        name: "app.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/app.js"),
    },
    Asset {
        name: "eqmath.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/eqmath.js"),
    },
    Asset {
        name: "procwidgets.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/procwidgets.js"),
    },
    Asset {
        name: "history.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/history.js"),
    },
    Asset {
        name: "onair.css",
        content_type: "text/css; charset=utf-8",
        body: include_str!("../web/onair.css"),
    },
    Asset {
        name: "history.css",
        content_type: "text/css; charset=utf-8",
        body: include_str!("../web/history.css"),
    },
    Asset {
        name: "procrack.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/procrack.js"),
    },
    Asset {
        name: "processing.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/processing.js"),
    },
    Asset {
        name: "settings.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/settings.js"),
    },
    Asset {
        name: "stations.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/stations.js"),
    },
    Asset {
        name: "stations.css",
        content_type: "text/css; charset=utf-8",
        body: include_str!("../web/stations.css"),
    },
    Asset {
        name: "help-md.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/help-md.js"),
    },
    Asset {
        name: "help.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../web/help.js"),
    },
    Asset {
        name: "style.css",
        content_type: "text/css; charset=utf-8",
        body: include_str!("../web/style.css"),
    },
    Asset {
        name: "favicon.svg",
        content_type: "image/svg+xml",
        body: include_str!("../web/favicon.svg"),
    },
];

/// An HTML page with the product name and the API base path written in.
pub fn page(name: &str, product: &str, base: &str) -> Response {
    let Some(a) = ASSETS.iter().find(|a| a.name == name) else {
        return Response::error(404, "not_found", "no such page");
    };
    let body = a
        .body
        .replace("{{product}}", &html_escape(product))
        .replace("{{base}}", base);
    Response::text(200, a.content_type, body).with_header("cache-control", "no-cache")
}

pub fn static_asset(name: &str) -> Response {
    // The Help pages and their index (`help-index.json`, `help-<page>.md`), compiled in from `crates/serve/help/`.
    if let Some((content_type, body)) = crate::help::asset(name) {
        return Response::text(200, content_type, body).with_header("cache-control", "no-cache");
    }
    match ASSETS
        .iter()
        .find(|a| a.name == name && !a.name.ends_with(".html"))
    {
        Some(a) => {
            Response::text(200, a.content_type, a.body).with_header("cache-control", "no-cache")
        }
        None => Response::error(404, "not_found", "no such file"),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
