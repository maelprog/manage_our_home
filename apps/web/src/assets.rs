//! The static-asset route (`/assets`) — today the two self-hosted `.woff2`
//! files `style.css`'s `@font-face` rules point at (#67).
//!
//! `apps/web` served no static file at all before this: every page is a
//! string built by `app::shell`, stylesheet included. Fonts cannot be
//! inlined into every response the way the stylesheet is (170 kB per page
//! view), and they cannot come from a CDN either — Google Fonts or Bunny
//! would hand each visitor's IP to a third party, which is a processing
//! activity to declare in `docs/registre-traitements.md`. So they are
//! served from here, from files committed next to the source.
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

use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;
use tower_http::services::ServeDir;

/// Where the asset files live, relative to the working directory the server
/// is started from — the repo root, which is where `cargo run` and CI's e2e
/// job both put it. The container image has no repo checkout in it, so
/// `apps/web/Dockerfile` copies the directory in and points
/// `WEB_ASSETS_DIR` at the copy.
pub const DEFAULT_ASSETS_DIR: &str = "apps/web/assets";

/// The environment variable that overrides `DEFAULT_ASSETS_DIR`.
pub const ASSETS_DIR_ENV: &str = "WEB_ASSETS_DIR";

/// A year, which is what `immutable` is worth saying alongside. Safe only
/// because the file names carry the font version: a font upgrade is a new
/// name and therefore a new URL, never a stale hit on an old one.
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

/// Resolve the directory `ServeDir` is rooted at. Split out from
/// `router` so the fallback is testable without a filesystem.
pub fn resolve_assets_dir(configured: Option<String>) -> PathBuf {
    match configured {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
        _ => PathBuf::from(DEFAULT_ASSETS_DIR),
    }
}

/// `GET /assets/*` served off `dir`, every response carrying the immutable
/// cache header. Generic over the state so it merges into the application
/// router before `with_state`; it needs no state of its own.
pub fn router_at<S>(dir: &Path) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
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
