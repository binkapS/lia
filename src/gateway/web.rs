//! The interface in the browser.
//!
//! The page and its assets are held inside the binary and served by the
//! gateway. Nothing is fetched from anywhere: the machine may be the only
//! one with a network, and a page that asked a third party for its scripts
//! would be handing the conversation to that third party.
//!
//! The source is split across `web/` so each part can be read on its own:
//! `web/index.html` is the markup, `web/css/` the styles, and `web/js/` the
//! behaviour. [`include_str!`] puts them back into the binary, and the
//! gateway serves them under `/assets/`. The speech library is built by
//! Vite from the npm package into `web/dist`, which `build.rs` reads into
//! the binary through the generated [`BUNDLED`] table.

use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

// What build.rs found in web/dist for the binary to serve.
include!(concat!(env!("OUT_DIR"), "/bundled_assets.rs"));

/// The page itself.
pub const INDEX: &str = include_str!("../../web/index.html");

/// The stylesheets, in the order the page links them.
pub const CSS: &[(&str, &str)] = &[
    ("base.css", include_str!("../../web/css/base.css")),
    ("chat.css", include_str!("../../web/css/chat.css")),
    ("composer.css", include_str!("../../web/css/composer.css")),
    ("overlays.css", include_str!("../../web/css/overlays.css")),
    ("controls.css", include_str!("../../web/css/controls.css")),
    (
        "responsive.css",
        include_str!("../../web/css/responsive.css"),
    ),
    ("nexus.css", include_str!("../../web/css/nexus.css")),
    ("inbox.css", include_str!("../../web/css/inbox.css")),
];

/// The scripts, in the order the page loads them.
pub const JS: &[(&str, &str)] = &[
    ("core.js", include_str!("../../web/js/core.js")),
    ("chat.js", include_str!("../../web/js/chat.js")),
    ("input.js", include_str!("../../web/js/input.js")),
    ("commands.js", include_str!("../../web/js/commands.js")),
    ("voice.js", include_str!("../../web/js/voice.js")),
    ("media.js", include_str!("../../web/js/media.js")),
    ("send.js", include_str!("../../web/js/send.js")),
    ("markdown.js", include_str!("../../web/js/markdown.js")),
    ("welcome.js", include_str!("../../web/js/welcome.js")),
    ("effects.js", include_str!("../../web/js/effects.js")),
    ("nexus.js", include_str!("../../web/js/nexus.js")),
    ("boot.js", include_str!("../../web/js/boot.js")),
    ("inbox.js", include_str!("../../web/js/inbox.js")),
];

/// Serve the page.
///
/// It is open to anyone who can reach the gateway: it holds no secrets,
/// it is only a drawing. What it draws still needs the token, and the
/// page asks for it.
pub async fn index() -> impl IntoResponse {
    Html(INDEX)
}

/// What [`asset`] names the file with, and the file's body in bytes.
fn content(name: &str) -> Option<(&'static str, &'static [u8])> {
    if let Some((_, body)) = CSS.iter().find(|(file, _)| *file == name) {
        return Some(("text/css; charset=utf-8", body.as_bytes()));
    }
    if let Some((_, body)) = JS.iter().find(|(file, _)| *file == name) {
        return Some(("text/javascript; charset=utf-8", body.as_bytes()));
    }
    if let Some((_, kind, body)) = BUNDLED.iter().find(|(file, _, _)| *file == name) {
        return Some((kind, *body));
    }
    None
}

/// Serve a stylesheet, script, or one of the files the Vite build emitted,
/// all from inside the binary.
///
/// Only the exact names above are answered. A path is never joined to a
/// directory, so there is nothing for `..` to escape.
pub async fn asset_file(Path(name): Path<String>) -> Response {
    let Some((content_type, body)) = content(&name) else {
        return (StatusCode::NOT_FOUND, "asset not found").into_response();
    };

    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    // The binary may have been rebuilt under a running browser. Asking
    // for the file again is cheap; showing yesterday's interface is not.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}
