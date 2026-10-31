//! The MHome logo and what a browser or a phone takes from it (#542): the
//! favicon, the home-screen icons, the web app manifest, and the mark at the
//! head of the sidebar.
//!
//! # The drawing
//!
//! Track A5 of the issue, chosen on 2026-10-26: a double gable drawn as one
//! stroke, which reads as an M, a lit window under the left gable and an
//! arched door under the right one, white on a tile of the accent. The
//! reference SVG is the issue's; `brand/favicon.svg` is it, byte for byte
//! but for the trailing newline.
//!
//! Every other file in `brand/` comes from that drawing through the scripts
//! of `brand/tools/` (the recipe is at the head of `draw.py`): the PNGs are
//! rasterised by headless Chromium, the `.ico` packs the 16 and 32 px
//! renders, and the two logo lockups (`logo.svg` in colour, `logo-mono.svg`
//! in `currentColor`) set the word "MHome" beside the symbol as outlines of
//! the self-hosted Fraunces at weight 600 — outlines and not `<text>`,
//! because an SVG shown as an image loads no web font. The lockups are not
//! served: they are the logo for whoever needs it outside the app.
//!
//! # How it is served
//!
//! Like the stylesheet and the scripts (`crate::assets`): out of the binary
//! (`include_bytes!`), under `/assets/<stem>-<digest>.<ext>`, `immutable`
//! for a year. Not out of `apps/web/assets/`, which `ServeDir` serves under
//! names that carry no digest — a new icon would never reach a browser that
//! cached the old one.
//!
//! Two addresses cannot carry a digest, and are served without the
//! `immutable` promise (`REVALIDATE`): `/manifest.webmanifest`, which the
//! browser re-reads to notice an update and which the issue names, and
//! `/favicon.ico`, which browsers and feed readers ask for at the root
//! without reading any page.

use std::sync::LazyLock;

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};

use crate::assets::fingerprinted_url;

/// The app's name, as the manifest gives it to a home screen.
pub const NAME: &str = "MHome";

/// The accent and the page background of the light theme
/// (`style.css`, `:root`): the manifest's `theme_color` and
/// `background_color`, which a browser paints before any CSS is read. The
/// test `the_manifest_colours_are_the_sheets_tokens` holds them to the
/// sheet.
pub const THEME_COLOR: &str = "#14706E";
pub const BACKGROUND_COLOR: &str = "#FBFAF7";

/// Where the manifest is served — the issue's address, with no digest.
pub const MANIFEST_PATH: &str = "/manifest.webmanifest";

/// Where browsers look for a favicon when a page names none.
pub const FAVICON_ROOT_PATH: &str = "/favicon.ico";

/// The cache header of the two undigested addresses: kept, but asked about
/// again before each use, so a deploy reaches them.
pub const REVALIDATE: &str = "no-cache";

/// One file of `brand/` the app serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// The favicon most browsers take: vector, sharp at any density.
    FaviconSvg,
    /// The favicon of the others, 16 and 32 px.
    FaviconIco,
    /// The home-screen icon of iOS, 180 px, square and opaque: iOS rounds
    /// the corners itself and paints transparency black.
    AppleTouch,
    /// The manifest's icons: the rounded tile at 192 and 512 px…
    Any192,
    Any512,
    /// …and the one a launcher may crop to any shape: the tile to the edge,
    /// the symbol shrunk to 80 % so it stays inside the safe circle (40 %
    /// of the side in radius).
    Maskable512,
}

impl Icon {
    pub const ALL: [Icon; 6] = [
        Icon::FaviconSvg,
        Icon::FaviconIco,
        Icon::AppleTouch,
        Icon::Any192,
        Icon::Any512,
        Icon::Maskable512,
    ];

    fn stem(self) -> &'static str {
        match self {
            Icon::FaviconSvg | Icon::FaviconIco => "favicon",
            Icon::AppleTouch => "apple-touch-icon",
            Icon::Any192 => "icon-192",
            Icon::Any512 => "icon-512",
            Icon::Maskable512 => "icon-maskable-512",
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Icon::FaviconSvg => "svg",
            Icon::FaviconIco => "ico",
            _ => "png",
        }
    }

    /// What the file holds.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Icon::FaviconSvg => include_bytes!("brand/favicon.svg"),
            Icon::FaviconIco => include_bytes!("brand/favicon.ico"),
            Icon::AppleTouch => include_bytes!("brand/apple-touch-icon.png"),
            Icon::Any192 => include_bytes!("brand/icon-192.png"),
            Icon::Any512 => include_bytes!("brand/icon-512.png"),
            Icon::Maskable512 => include_bytes!("brand/icon-maskable-512.png"),
        }
    }

    /// The media type it is served as. With `X-Content-Type-Options:
    /// nosniff` (infra/Caddyfile), a browser takes it at its word.
    pub fn content_type(self) -> &'static str {
        match self {
            Icon::FaviconSvg => "image/svg+xml",
            Icon::FaviconIco => "image/x-icon",
            _ => "image/png",
        }
    }

    /// The URL it is served under, computed once per process.
    pub fn href(self) -> &'static str {
        static HREFS: LazyLock<Vec<String>> = LazyLock::new(|| {
            Icon::ALL
                .iter()
                .map(|i| fingerprinted_url(i.stem(), i.ext(), i.bytes()))
                .collect()
        });
        let at = Icon::ALL
            .iter()
            .position(|i| *i == self)
            .expect("Icon::ALL lists every icon");
        &HREFS[at]
    }
}

/// The manifest, given the URL of each icon. Split from `manifest` so the
/// shape can be tested on URLs of its own.
fn manifest_json(href: impl Fn(Icon) -> String) -> String {
    let icon = |icon: Icon, sizes: &str, purpose: &str| {
        serde_json::json!({
            "src": href(icon),
            "sizes": sizes,
            "type": icon.content_type(),
            "purpose": purpose,
        })
    };
    serde_json::json!({
        "name": NAME,
        "short_name": NAME,
        "id": "/",
        "start_url": "/",
        "display": "standalone",
        "lang": "fr",
        "theme_color": THEME_COLOR,
        "background_color": BACKGROUND_COLOR,
        "icons": [
            icon(Icon::Any192, "192x192", "any"),
            icon(Icon::Any512, "512x512", "any"),
            icon(Icon::Maskable512, "512x512", "maskable"),
        ],
    })
    .to_string()
}

/// The manifest as served, computed once per process.
pub fn manifest() -> &'static str {
    static MANIFEST: LazyLock<String> =
        LazyLock::new(|| manifest_json(|icon| icon.href().to_string()));
    &MANIFEST
}

/// What `app::document` puts in every `<head>`: the two favicons, the
/// home-screen icon of iOS and the manifest.
fn head_links_with(href: impl Fn(Icon) -> String) -> String {
    format!(
        r#"<link rel="icon" href="{ico}" sizes="32x32"/>
<link rel="icon" href="{svg}" type="image/svg+xml"/>
<link rel="apple-touch-icon" href="{touch}"/>
<link rel="manifest" href="{MANIFEST_PATH}"/>"#,
        ico = href(Icon::FaviconIco),
        svg = href(Icon::FaviconSvg),
        touch = href(Icon::AppleTouch),
    )
}

/// `head_links_with` for the files served, computed once per process.
pub fn head_links() -> &'static str {
    static LINKS: LazyLock<String> =
        LazyLock::new(|| head_links_with(|icon| icon.href().to_string()));
    &LINKS
}

/// The symbol at the head of the sidebar (`app::app_header`), inline so it
/// takes the family's accent (#538) and the theme's: the tile in
/// `--accent`, the drawing in `--accent-fg`, both set by `.logo` in the
/// sheet — the drawing's own attributes carry only its geometry (and
/// `fill="none"` on the two strokes). The word
/// "MHome" beside it stays text, in the Fraunces the page has loaded
/// anyway: the outlined lockup weighs 5.6 kB, which every page would pay.
pub const SIDEBAR_MARK: &str = concat!(
    r#"<svg class="logo" viewBox="0 0 32 32" aria-hidden="true">"#,
    r#"<rect width="32" height="32" rx="7.5"/>"#,
    r#"<path fill="none" stroke-width="3" d="M6.5 24.5V14.5L11.25 9L16 14.5L20.75 9L25.5 14.5V24.5"/>"#,
    r#"<path fill="none" stroke-width="2.2" d="M19.25 24.5V20.5a1.5 1.5 0 0 1 3 0V24.5"/>"#,
    r#"<rect x="9.5" y="17" width="3.5" height="3.5" rx=".6"/>"#,
    "</svg>",
);

/// `GET <Icon::href>` — the file, straight out of the binary.
pub async fn serve_icon(icon: Icon) -> Response {
    (
        [(CONTENT_TYPE, HeaderValue::from_static(icon.content_type()))],
        icon.bytes(),
    )
        .into_response()
}

/// `GET /favicon.ico` — the same bytes as the digested `.ico`, revalidated.
pub async fn serve_root_favicon() -> Response {
    (
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static(Icon::FaviconIco.content_type()),
            ),
            (CACHE_CONTROL, HeaderValue::from_static(REVALIDATE)),
        ],
        Icon::FaviconIco.bytes(),
    )
        .into_response()
}

/// `GET /manifest.webmanifest`.
pub async fn serve_manifest() -> Response {
    (
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("application/manifest+json"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static(REVALIDATE)),
        ],
        manifest(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Width and height out of a PNG's IHDR chunk.
    fn png_size(bytes: &[u8]) -> (u32, u32) {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        assert_eq!(&bytes[12..16], b"IHDR");
        let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        (be(16), be(20))
    }

    fn manifest_value() -> serde_json::Value {
        serde_json::from_str(&manifest_json(|i| format!("/x/{i:?}"))).expect("the manifest is JSON")
    }

    // -- the manifest -----------------------------------------------------

    #[test]
    fn the_manifest_names_the_app_and_opens_it_standalone_at_the_root() {
        let m = manifest_value();
        assert_eq!(m["name"], "MHome");
        assert_eq!(m["short_name"], "MHome");
        assert_eq!(m["start_url"], "/");
        assert_eq!(m["id"], "/");
        assert_eq!(m["display"], "standalone");
        assert_eq!(m["lang"], "fr");
        assert_eq!(m["theme_color"], THEME_COLOR);
        assert_eq!(m["background_color"], BACKGROUND_COLOR);
    }

    #[test]
    fn the_manifest_lists_192_512_and_a_maskable_512() {
        let m = manifest_value();
        let icons = m["icons"].as_array().expect("an icons array");
        let entries: Vec<(String, String, String, String)> = icons
            .iter()
            .map(|i| {
                (
                    i["src"].as_str().unwrap().to_string(),
                    i["sizes"].as_str().unwrap().to_string(),
                    i["type"].as_str().unwrap().to_string(),
                    i["purpose"].as_str().unwrap_or("any").to_string(),
                )
            })
            .collect();
        let e = |src: &str, sizes: &str, purpose: &str| {
            (
                src.to_string(),
                sizes.to_string(),
                "image/png".to_string(),
                purpose.to_string(),
            )
        };
        assert_eq!(
            entries,
            vec![
                e("/x/Any192", "192x192", "any"),
                e("/x/Any512", "512x512", "any"),
                e("/x/Maskable512", "512x512", "maskable"),
            ]
        );
    }

    #[test]
    fn the_served_manifest_points_at_the_served_icons() {
        let m: serde_json::Value = serde_json::from_str(manifest()).expect("JSON");
        let srcs: Vec<&str> = m["icons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["src"].as_str().unwrap())
            .collect();
        assert_eq!(
            srcs,
            vec![
                Icon::Any192.href(),
                Icon::Any512.href(),
                Icon::Maskable512.href()
            ]
        );
    }

    #[test]
    fn the_manifest_colours_are_the_sheets_tokens() {
        // The light theme's `--accent` and `--bg`, as `:root` declares
        // them. A token moved without the manifest would paint the splash
        // screen and the title bar of the installed app in the old colour.
        let sheet = include_str!("style.css");
        let root = &sheet[..sheet.find("}").expect("a :root block")];
        for (token, value) in [("--accent", THEME_COLOR), ("--bg", BACKGROUND_COLOR)] {
            let decl = format!("{token}: {};", value.to_ascii_lowercase());
            assert!(root.contains(&decl), "style.css :root has no `{decl}`");
        }
    }

    // -- the files --------------------------------------------------------

    #[test]
    fn each_png_is_the_size_its_name_says() {
        for (icon, side) in [
            (Icon::AppleTouch, 180),
            (Icon::Any192, 192),
            (Icon::Any512, 512),
            (Icon::Maskable512, 512),
        ] {
            assert_eq!(png_size(icon.bytes()), (side, side), "{icon:?}");
        }
    }

    #[test]
    fn the_ico_holds_a_32_px_image() {
        let ico = Icon::FaviconIco.bytes();
        assert_eq!(&ico[..4], &[0, 0, 1, 0], "not an ICO");
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        let sides: Vec<u8> = (0..count).map(|n| ico[6 + 16 * n]).collect();
        assert!(sides.contains(&32), "{sides:?}");
    }

    #[test]
    fn no_svg_of_the_logo_relies_on_a_font() {
        // A favicon or an <img> SVG loads no web font: a `<text>` would
        // fall back to whatever the system has.
        for (name, svg) in [
            ("favicon.svg", include_str!("brand/favicon.svg")),
            ("logo.svg", include_str!("brand/logo.svg")),
            ("logo-mono.svg", include_str!("brand/logo-mono.svg")),
            ("SIDEBAR_MARK", SIDEBAR_MARK),
        ] {
            assert!(svg.contains("<svg"), "{name}");
            assert!(!svg.contains("<text"), "{name}");
            assert!(!svg.contains("font-family"), "{name}");
        }
    }

    #[test]
    fn each_icon_has_its_own_digested_url() {
        for icon in Icon::ALL {
            assert_eq!(
                icon.href(),
                fingerprinted_url(icon.stem(), icon.ext(), icon.bytes()),
                "{icon:?}"
            );
        }
        let mut hrefs: Vec<&str> = Icon::ALL.iter().map(|i| i.href()).collect();
        hrefs.sort();
        hrefs.dedup();
        assert_eq!(hrefs.len(), Icon::ALL.len());
    }

    // -- the routes --------------------------------------------------------

    async fn get(path: &str) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
        use tower::ServiceExt;
        // No assets directory: everything here comes out of the binary.
        let missing = std::env::temp_dir().join("mhome-web-brand-has-no-directory");
        let res = crate::assets::router_at::<()>(&missing)
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("infallible router");
        let status = res.status();
        let headers = res.headers().clone();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("a body");
        (status, headers, body.to_vec())
    }

    fn header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
        headers.get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn the_manifest_is_served_as_a_manifest_and_revalidated() {
        let (status, headers, body) = get(MANIFEST_PATH).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(
            header(&headers, "content-type"),
            Some("application/manifest+json")
        );
        assert_eq!(header(&headers, "cache-control"), Some(REVALIDATE));
        let m: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(m["name"], "MHome");
    }

    #[tokio::test]
    async fn every_icon_the_manifest_cites_is_served() {
        let m: serde_json::Value = serde_json::from_str(manifest()).expect("JSON");
        for icon in m["icons"].as_array().expect("icons") {
            let src = icon["src"].as_str().expect("a src");
            let (status, headers, _) = get(src).await;
            assert_eq!(status, axum::http::StatusCode::OK, "GET {src}");
            assert_eq!(header(&headers, "content-type"), icon["type"].as_str());
        }
    }

    #[tokio::test]
    async fn each_icon_is_served_byte_for_byte_and_immutable() {
        for icon in Icon::ALL {
            let (status, headers, body) = get(icon.href()).await;
            assert_eq!(status, axum::http::StatusCode::OK, "GET {}", icon.href());
            assert_eq!(header(&headers, "content-type"), Some(icon.content_type()));
            assert_eq!(
                header(&headers, "cache-control"),
                Some("public, max-age=31536000, immutable"),
                "{icon:?}"
            );
            assert_eq!(body, icon.bytes(), "{icon:?}");
        }
    }

    #[tokio::test]
    async fn the_root_favicon_is_the_ico_revalidated() {
        let (status, headers, body) = get(FAVICON_ROOT_PATH).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(header(&headers, "content-type"), Some("image/x-icon"));
        assert_eq!(
            header(&headers, "cache-control"),
            Some(REVALIDATE),
            "an undigested name must not be promised for a year"
        );
        assert_eq!(body, Icon::FaviconIco.bytes());
    }

    #[tokio::test]
    async fn a_stale_icon_name_is_a_404() {
        for path in [
            "/assets/favicon.svg",
            "/assets/icon-512.png",
            &fingerprinted_url("icon-512", "png", b"old"),
        ] {
            let (status, ..) = get(path).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{path}");
        }
    }

    // -- the head and the sidebar -----------------------------------------

    #[test]
    fn the_head_links_both_favicons_the_touch_icon_and_the_manifest() {
        let links = head_links_with(|i| format!("/x/{i:?}"));
        for expected in [
            r#"<link rel="icon" href="/x/FaviconIco" sizes="32x32"/>"#,
            r#"<link rel="icon" href="/x/FaviconSvg" type="image/svg+xml"/>"#,
            r#"<link rel="apple-touch-icon" href="/x/AppleTouch"/>"#,
            r#"<link rel="manifest" href="/manifest.webmanifest"/>"#,
        ] {
            assert!(links.contains(expected), "{expected} missing: {links}");
        }
    }

    #[test]
    fn the_sidebar_mark_is_the_a5_drawing_coloured_by_the_sheet() {
        let mark = SIDEBAR_MARK;
        assert!(
            mark.starts_with(r#"<svg class="logo" viewBox="0 0 32 32" aria-hidden="true">"#),
            "{mark}"
        );
        // The geometry of the reference drawing…
        for d in [
            "M6.5 24.5V14.5L11.25 9L16 14.5L20.75 9L25.5 14.5V24.5",
            "M19.25 24.5V20.5a1.5 1.5 0 0 1 3 0V24.5",
            r#"x="9.5" y="17" width="3.5" height="3.5""#,
        ] {
            assert!(mark.contains(d), "{d} missing: {mark}");
        }
        // …and none of its colours: the sheet gives them, per theme and
        // per family accent.
        assert!(!mark.contains('#'), "{mark}");
        assert!(!mark.contains(" style"), "{mark}");
    }
}
