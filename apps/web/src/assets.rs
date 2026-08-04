//! The static-asset route (`/assets`) — the two self-hosted `.woff2` files
//! `style.css`'s `@font-face` rules point at (#67), and since #89 the
//! stylesheet itself.
//!
//! `apps/web` served no static file at all before #67: every page was a
//! string built by `app::shell`, stylesheet included. Fonts could not be
//! inlined into every response the way the stylesheet was (170 kB per page
//! view), and they cannot come from a CDN either — Google Fonts or Bunny
//! would hand each visitor's IP to a third party, which is a processing
//! activity to declare in `docs/registre-traitements.md`. So they are
//! served from here, from files committed next to the source.
//!
//! The stylesheet joined them for the opposite reason: at ~10 000 gzipped
//! bytes copied into every one of the eight nav routes, it had pushed
//! `/messagerie` out of DESIGN.md's 14 KiB response budget. It is served
//! **from the binary**, not from a file — see `STYLESHEET` below for why
//! that distinction is the whole of #89.
//!
//! # Provenance of `assets/fonts/`
//!
//! Both families are SIL Open Font License 1.1, and the OFL requires the
//! licence to be redistributed with the files — hence `OFL-Fraunces.txt`
//! and `OFL-SourceSans3.txt` sitting beside them. Sources, taken from the
//! `google/fonts` repository at commit `4024282`:
//!
//! - `ofl/fraunces/Fraunces[SOFT,WONK,opsz,wght].ttf`, version 1.000
//!   (Copyright 2018 The Fraunces Project Authors) →
//!   `fraunces-v1.000.woff2`
//! - `ofl/sourcesans3/SourceSans3[wght].ttf`, version 3.052
//!   (Copyright 2010-2020 Adobe, Reserved Font Name 'Source') →
//!   `source-sans-3-v3.052.woff2`
//!
//! Converted with `fonttools` (`pyftsubset … --flavor=woff2`) over the
//! union of Google Fonts' `latin` and `latin-ext` unicode ranges. Fraunces
//! additionally had its `SOFT` and `WONK` axes pinned to their defaults
//! (`fonttools varLib.instancer SOFT=drop WONK=drop`): nothing in the app
//! varies them, and dropping them takes the file from 178 kB to 99 kB.
//! `opsz` and `wght` stay live — browsers drive `opsz` from the font size
//! on their own, and `wght` is the whole point of a variable font.
//!
//! Source Sans 3 carries no `tnum` feature because it does not need one:
//! its default figures are already tabular (every digit advances 472
//! units), which is why DESIGN.md can put the budget amounts, the stock
//! quantities and the agenda hours on the body face instead of a third
//! file.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use sha2::{Digest, Sha256};
use tower_http::services::ServeDir;

/// Where the asset files live, relative to the working directory the server
/// is started from — the repo root, which is where `cargo run` and CI's e2e
/// job both put it. The container image has no repo checkout in it, so
/// `apps/web/Dockerfile` copies the directory in and points
/// `WEB_ASSETS_DIR` at the copy.
pub const DEFAULT_ASSETS_DIR: &str = "apps/web/assets";

/// The environment variable that overrides `DEFAULT_ASSETS_DIR`.
pub const ASSETS_DIR_ENV: &str = "WEB_ASSETS_DIR";

/// A year, which is what `immutable` is worth saying alongside — and a
/// promise that cannot be taken back for that long, so it is only safe on
/// a name that will never mean anything else.
///
/// Two different mechanisms earn that right here, and neither is enforced
/// by the route: the font files carry their version in the file name
/// (`fraunces-v1.000.woff2`), by convention, maintained by hand; the
/// stylesheet carries a digest of its own bytes, by construction. What
/// `ServeDir` itself guarantees is nothing — drop any file into the assets
/// directory and it will be served `immutable` for a year under whatever
/// name it happens to have. That is the pre-existing shape of #67, stated
/// rather than dressed up: the discipline lives in how files are named,
/// not in this constant.
const CACHE_FOR_A_YEAR: &str = "public, max-age=31536000, immutable";

/// Attach the cache header to what was actually served, and to nothing
/// else. A 404 must stay uncached: `immutable` on a miss would have every
/// browser that asked during a bad deploy remember the file as absent for
/// a year, unfixable from the server side.
async fn cache_what_was_served(mut response: Response) -> Response {
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(CACHE_FOR_A_YEAR));
    }
    response
}

/// The stylesheet, sealed into the binary — the one copy of it that
/// exists at runtime (#89).
///
/// Everything about the sheet is derived from *this constant*: the digest
/// that names it, the URL `app::document` puts in every `<link>`, and the
/// bytes this module's route answers with. That is the property the switch
/// out of inlining had to preserve. Inlining made a stale sheet
/// structurally impossible (DESIGN.md → Livraison du CSS, benefit n°2);
/// an external sheet reopens that window, and the only thing that closes
/// it again is a name that no two different contents can share.
///
/// Note what is *not* the source: a file under `assets/`. `ServeDir` reads
/// the working directory, the container image copies `apps/web/assets` in
/// at build time, and the two can be a deploy apart — serving the sheet
/// from disk would have reintroduced exactly the drift this is avoiding,
/// only with a hash on top to make it look safe.
pub const STYLESHEET: &str = include_str!("style.css");

/// How many hex characters of the digest name the file.
///
/// 16 — 64 bits. The digest is not defending against an adversary (nobody
/// can choose the bytes; they are compiled in), only against two versions
/// of the sheet colliding, which at 64 bits does not happen. The URL is
/// paid on every page view, in every document, so the rest of the digest
/// would be 48 bytes of nothing.
const FINGERPRINT_LEN: usize = 16;

/// `/assets/style-<digest>.css` for a given sheet.
///
/// Takes the CSS rather than reading `STYLESHEET` so the naming rule can
/// be tested on inputs of its own — the one interesting property being
/// that two different sheets never get the same name.
fn stylesheet_url(css: &str) -> String {
    let digest = Sha256::digest(css.as_bytes());
    let mut hex = String::with_capacity(FINGERPRINT_LEN);
    for byte in digest.iter().take(FINGERPRINT_LEN.div_ceil(2)) {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex.truncate(FINGERPRINT_LEN);
    format!("/assets/style-{hex}.css")
}

/// The URL *the* stylesheet is served under, computed once per process.
///
/// Once, because the answer cannot change while the process lives: the
/// input is a `const`. A `LazyLock` rather than a `const fn` only because
/// SHA-256 is not one; the guarantee is the same.
pub fn stylesheet_href() -> &'static str {
    static HREF: LazyLock<String> = LazyLock::new(|| stylesheet_url(STYLESHEET));
    &HREF
}

/// `GET /assets/style-<digest>.css` — the sheet, straight out of the
/// binary, no filesystem in the path.
///
/// It needs no `ETag` and answers no conditional request: the URL *is* the
/// content, so a browser holding this name already holds these bytes and
/// `immutable` tells it not to ask again.
async fn serve_stylesheet() -> Response {
    (
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        STYLESHEET,
    )
        .into_response()
}

/// Resolve the directory `ServeDir` is rooted at. Split out from
/// `router` so the fallback is testable without a filesystem.
pub fn resolve_assets_dir(configured: Option<String>) -> PathBuf {
    match configured {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
        _ => PathBuf::from(DEFAULT_ASSETS_DIR),
    }
}

/// `GET /assets/*` — the stylesheet out of the binary, the fonts off
/// `dir`, every served response carrying the immutable cache header.
/// Generic over the state so it merges into the application router before
/// `with_state`; it needs no state of its own.
///
/// The two live under one prefix because they are one thing from the
/// browser's side: the content-addressed files a page loads and never
/// re-validates. The sheet is a literal route rather than a file in `dir`,
/// so it takes precedence over the `ServeDir` fallback for that one path.
pub fn router_at<S>(dir: &Path) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(stylesheet_href(), get(serve_stylesheet))
        .nest_service("/assets", ServeDir::new(dir))
        .layer(axum::middleware::map_response(cache_what_was_served))
}

/// The route as `main` mounts it, reading `WEB_ASSETS_DIR` from the
/// environment.
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router_at(&resolve_assets_dir(std::env::var(ASSETS_DIR_ENV).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// The repo root, derived from this crate's manifest — the working
    /// directory `DEFAULT_ASSETS_DIR` is written relative to.
    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root resolves")
    }

    // -- resolve_assets_dir --------------------------------------------

    #[test]
    fn falls_back_to_the_repo_relative_default() {
        assert_eq!(
            resolve_assets_dir(None),
            PathBuf::from("apps/web/assets"),
            "with no override the server must find the committed assets \
             from the repo root"
        );
    }

    #[test]
    fn an_override_wins_over_the_default() {
        assert_eq!(
            resolve_assets_dir(Some("/srv/assets".to_string())),
            PathBuf::from("/srv/assets")
        );
    }

    #[test]
    fn a_blank_override_is_not_an_override() {
        // An unset-but-declared `WEB_ASSETS_DIR=` in a compose file would
        // otherwise root ServeDir at "", where nothing resolves and every
        // font silently 404s into the fallback stack.
        for blank in ["", "   "] {
            assert_eq!(
                resolve_assets_dir(Some(blank.to_string())),
                PathBuf::from(DEFAULT_ASSETS_DIR)
            );
        }
    }

    #[test]
    fn the_default_directory_exists_in_the_repo() {
        // The pairing that actually has to hold: the constant and the
        // committed directory. Moving one without the other 404s every
        // font, and a 404 font is invisible — the page just renders in
        // Georgia and nothing reports it.
        let dir = workspace_root().join(DEFAULT_ASSETS_DIR);
        assert!(dir.is_dir(), "{} is not a directory", dir.display());
    }

    // -- the stylesheet fingerprint (#89) -------------------------------
    //
    // Pure logic, so it is written test-first (`.claude/CLAUDE.md` →
    // Development process). What it has to guarantee is narrow: the name
    // moves whenever the bytes move, and it is derived from the bytes
    // alone — never from a file on disk, a build step or a version string,
    // any of which could disagree with what the binary actually serves.

    #[test]
    fn the_same_sheet_always_gets_the_same_name() {
        // A name that varied between two calls (a random suffix, a build
        // timestamp) would put a different URL in the page than the one the
        // route answers on, and no cache would ever hit.
        assert_eq!(stylesheet_url(STYLESHEET), stylesheet_url(STYLESHEET));
    }

    #[test]
    fn one_byte_of_difference_is_a_different_name() {
        // The whole point: a deployed browser holding the old sheet under
        // the old name can never be handed new HTML that points at it.
        assert_ne!(
            stylesheet_url("body { color: red }"),
            stylesheet_url("body { color: red;}")
        );
        // Including a change that only adds a comment: prose is served to
        // the visitor too, so it is part of what the name addresses.
        assert_ne!(
            stylesheet_url("body { color: red }"),
            stylesheet_url("/* why */ body { color: red }")
        );
    }

    #[test]
    fn the_name_is_a_fixed_run_of_lowercase_hex_under_the_assets_route() {
        // `immutable` is a promise made on a URL, so the URL has to be
        // stable in shape as well as in content — and safe in a path.
        for css in ["", "body{}", STYLESHEET] {
            let url = stylesheet_url(css);
            let hex = url
                .strip_prefix("/assets/style-")
                .and_then(|rest| rest.strip_suffix(".css"))
                .unwrap_or_else(|| panic!("unexpected shape: {url}"));
            assert_eq!(hex.len(), FINGERPRINT_LEN, "{url}");
            assert!(
                hex.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "{url}"
            );
        }
    }

    #[test]
    fn the_digest_is_sha_256_of_the_bytes_and_nothing_else() {
        // A known answer, so that swapping the hash function is a visible,
        // deliberate edit rather than a silent one. These are the first 16
        // hex characters of SHA-256 over the empty string and over "a".
        assert_eq!(stylesheet_url(""), "/assets/style-e3b0c44298fc1c14.css");
        assert_eq!(stylesheet_url("a"), "/assets/style-ca978112ca1bbdca.css");
    }

    #[test]
    fn the_page_links_the_url_this_module_serves() {
        // The pairing the whole issue is about. `app::document` builds the
        // `<link>` from `stylesheet_href`, and the route below answers on
        // the same string — both out of the one constant, so there is no
        // arrangement of deploys in which they disagree.
        let html = crate::app::shell(crate::app::Width::Form, "Titre", "<h1>x</h1>");
        assert!(
            html.contains(&format!(
                r#"<link rel="stylesheet" href="{}"/>"#,
                stylesheet_href()
            )),
            "{html}"
        );
        assert!(
            !html.contains("<style>"),
            "the sheet must not travel inside the document any more: {html}"
        );
    }

    // -- the route ------------------------------------------------------

    async fn get(path: &str) -> axum::http::Response<Body> {
        router_at::<()>(&workspace_root().join(DEFAULT_ASSETS_DIR))
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("infallible router")
    }

    #[tokio::test]
    async fn serves_a_font_with_an_immutable_cache_header() {
        for file in crate::app::stylesheet_font_files() {
            let res = get(&format!("/assets/fonts/{file}")).await;
            assert_eq!(res.status(), StatusCode::OK, "GET /assets/fonts/{file}");
            assert_eq!(
                res.headers()
                    .get(axum::http::header::CACHE_CONTROL)
                    .and_then(|v| v.to_str().ok()),
                Some("public, max-age=31536000, immutable"),
                "a font re-fetched on every page view defeats the point of \
                 serving it ourselves"
            );
        }
    }

    #[tokio::test]
    async fn serves_the_stylesheet_out_of_the_binary_byte_for_byte() {
        let res = get(stylesheet_href()).await;
        assert_eq!(res.status(), StatusCode::OK, "GET {}", stylesheet_href());
        assert_eq!(
            res.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/css; charset=utf-8"),
            "a stylesheet served as anything else is ignored by the browser"
        );
        assert_eq!(
            res.headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some(CACHE_FOR_A_YEAR),
            "the whole reason to take the sheet out of the document is that \
             it can then be cached; without this header it is re-fetched on \
             every navigation and we have kept the cost and lost the benefit"
        );
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("a body");
        assert_eq!(
            body.as_ref(),
            STYLESHEET.as_bytes(),
            "the bytes served must be the bytes the name was computed from"
        );
    }

    #[tokio::test]
    async fn no_other_stylesheet_name_is_served() {
        // A stale name is a miss, not a fallback to the current sheet: the
        // browser must be told to come back for the new URL rather than be
        // handed today's bytes under yesterday's `immutable` name.
        for path in [
            "/assets/style.css",
            "/assets/style-0000000000000000.css",
            &stylesheet_url("body{}"),
        ] {
            assert_eq!(get(path).await.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn the_stylesheet_route_wins_over_the_directory_below_it() {
        // Both live under `/assets`: the sheet comes from the binary, the
        // fonts from `ServeDir`. A change that let the directory shadow the
        // route would 404 the sheet on a machine with no `assets/` checkout
        // — and an unstyled page is not something a test suite notices.
        assert_eq!(get(stylesheet_href()).await.status(), StatusCode::OK);
        for file in crate::app::stylesheet_font_files() {
            assert_eq!(
                get(&format!("/assets/fonts/{file}")).await.status(),
                StatusCode::OK
            );
        }
    }

    /// Serve `path` off a throwaway directory, and hand back the status and
    /// the body. Used by the two tests below, which are about what the
    /// route does when the *filesystem* disagrees with the binary.
    async fn get_from(dir: &Path, path: &str) -> (StatusCode, Vec<u8>) {
        let res = router_at::<()>(dir)
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("infallible router");
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("a body");
        (status, body.to_vec())
    }

    /// A directory of our own under the system temp dir, named after this
    /// test so two runs never collide. No `tempfile` dev-dependency for two
    /// tests.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mom-web-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[tokio::test]
    async fn a_decoy_of_the_same_name_on_disk_does_not_shadow_the_binary() {
        // This is the failure mode the whole issue exists to prevent,
        // staged rather than argued: a file carrying the *exact* hashed
        // name, sitting in the assets directory, with different bytes
        // inside. That is what a container image whose `apps/web/assets`
        // copy is one deploy behind the binary looks like — and it is
        // precisely why the sheet is served from `include_str!` and not
        // from a file. If `ServeDir` ever won this race, every visitor
        // would get stale CSS under an `immutable` name for a year, with
        // the page still rendering and nothing reporting it.
        let dir = scratch_dir("decoy");
        let name = stylesheet_href()
            .strip_prefix("/assets/")
            .expect("the sheet is served under /assets");
        std::fs::write(dir.join(name), "body{background:#f00}").expect("write the decoy");

        // First prove the decoy is genuinely reachable — that this
        // directory really is the one `ServeDir` is rooted at — so that the
        // assertion below cannot pass merely because nothing was wired up.
        std::fs::write(dir.join("probe.txt"), "reachable").expect("write the probe");
        let (probe_status, probe_body) = get_from(&dir, "/assets/probe.txt").await;
        assert_eq!(probe_status, StatusCode::OK);
        assert_eq!(probe_body, b"reachable");

        let (status, body) = get_from(&dir, stylesheet_href()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            STYLESHEET.as_bytes(),
            "a file of the same name on disk shadowed the binary: the URL \
             would then no longer address the bytes its digest was computed \
             from, which is the one guarantee #89 buys"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_stylesheet_is_served_even_with_no_assets_directory_at_all() {
        // The other half of "from the binary": `WEB_ASSETS_DIR` pointing at
        // nothing — a mis-set env var, an image built without the COPY —
        // costs the fonts, which fall back to the declared stacks, but must
        // not cost the stylesheet.
        let missing = std::env::temp_dir().join("mom-web-there-is-no-such-directory");
        let _ = std::fs::remove_dir_all(&missing);

        let (status, body) = get_from(&missing, stylesheet_href()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.len(), STYLESHEET.len());

        // …and the fonts really are gone in that configuration, so this
        // test is not passing because it accidentally found a real dir.
        let (font_status, _) = get_from(&missing, "/assets/fonts/anything.woff2").await;
        assert_eq!(font_status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_missing_asset_is_a_404_not_a_panic() {
        assert_eq!(
            get("/assets/fonts/does-not-exist.woff2").await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_404_is_not_cached_for_a_year() {
        // `immutable` on a miss is a trap: a font left out of one deploy
        // would be remembered as absent by every browser that asked for it,
        // for a year, with no way to invalidate it from the server.
        let res = get("/assets/fonts/does-not-exist.woff2").await;
        assert_eq!(res.headers().get(axum::http::header::CACHE_CONTROL), None);
    }

    #[tokio::test]
    async fn the_route_does_not_escape_its_directory() {
        // `../src/style.css` sits one level above the asset root and must
        // stay unreachable.
        for path in [
            "/assets/../src/style.css",
            "/assets/%2e%2e/src/style.css",
            "/assets/fonts/../../src/style.css",
        ] {
            let status = get(path).await.status();
            assert_ne!(status, StatusCode::OK, "{path} should not be servable");
        }
    }
}
