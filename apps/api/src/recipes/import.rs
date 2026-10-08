//! `POST /groups/:id/recipes/import` (#405): the recipe a web page
//! publishes as schema.org JSON-LD, as a draft for the member to review.
//! It creates nothing — the reviewed recipe goes through
//! `POST /groups/:id/recipes`, with the page as `source_url`. Open to any
//! member, like creating a recipe.
//!
//! The server fetches an address the member typed, so the request is held
//! to what any visitor could reach (`public_destination`, through
//! `outbound_http::recipe_import_client`): `http`/`https` only, public
//! addresses only — checked on the address connected to, and again on each
//! redirect, at most `public_destination::MAX_REDIRECTS` of them. The page
//! must say it is HTML and weigh at most `MAX_PAGE_BYTES`; a member may
//! start `RATE_LIMIT` imports per `RATE_WINDOW`.
//!
//! Every failure is a 422 whose code says why (`ImportFailure::code`), or
//! the usual 429: none is a 500.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::Json;
use manage_our_home_shared::dto::recipes::{ImportRecipeRequest, RecipeDraft, SOURCE_URL_MAX_LEN};
use reqwest::Url;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::public_destination::{check_url, refusal_in, Refused};
use crate::recipes::{ingredient_line, jsonld};
use crate::AppState;

/// The heaviest page read, in bytes. A recipe page with its JSON-LD weighs
/// a few hundred kilobytes; past this, the import stops reading.
pub const MAX_PAGE_BYTES: usize = 3 * 1024 * 1024;

/// What the requests say they come from.
pub const USER_AGENT: &str = concat!(
    "manage_our_home/",
    env!("CARGO_PKG_VERSION"),
    " (recipe import; +https://github.com/maelprog/manage_our_home)"
);

/// Why an import brought no draft back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFailure {
    /// Not a URL, or longer than `SOURCE_URL_MAX_LEN`.
    InvalidUrl,
    /// Not `http`/`https`.
    SchemeRefused,
    /// A destination that is not public, directly or after a redirect.
    DestinationRefused,
    /// No answer within `outbound_http::REQUEST_TIMEOUT`.
    Timeout,
    /// Unreachable, too many redirects, or an answer other than a success.
    Unreachable,
    /// The answer does not say it is HTML.
    NotHtml,
    /// More than `MAX_PAGE_BYTES`.
    TooLarge,
    /// No `Recipe` node in the page's JSON-LD.
    NoRecipe,
}

impl ImportFailure {
    /// The `error` code of the 422.
    pub fn code(self) -> &'static str {
        match self {
            ImportFailure::InvalidUrl => "url_invalid",
            ImportFailure::SchemeRefused => "url_scheme_refused",
            ImportFailure::DestinationRefused => "url_destination_refused",
            ImportFailure::Timeout => "source_timeout",
            ImportFailure::Unreachable => "source_unreachable",
            ImportFailure::NotHtml => "source_not_html",
            ImportFailure::TooLarge => "source_too_large",
            ImportFailure::NoRecipe => "no_recipe_found",
        }
    }
}

impl From<ImportFailure> for AppError {
    fn from(failure: ImportFailure) -> Self {
        AppError::Unprocessable(failure.code().into())
    }
}

pub async fn import_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<ImportRecipeRequest>,
) -> AppResult<Json<RecipeDraft>> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    tx.commit().await?;

    let url = import_url(&body.url)?;
    if !state
        .recipe_import_throttle
        .admit(auth.user_id, Instant::now())
    {
        return Err(AppError::TooManyRequests);
    }
    let page = fetch_page(&state.recipe_import_client, url.clone()).await?;
    // Reading up to `MAX_PAGE_BYTES` is CPU work, linear but not free: off
    // the async workers, so one import does not stall other requests.
    let source_url = url.to_string();
    let draft = tokio::task::spawn_blocking(move || draft_of(&page, source_url))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("recipe extraction panicked: {e}")))?;
    Ok(Json(draft.ok_or(ImportFailure::NoRecipe)?))
}

/// The draft a page gives, or `None` when it publishes no recipe.
fn draft_of(page: &str, source_url: String) -> Option<RecipeDraft> {
    let recipe = jsonld::extract_recipe(page)?;
    Some(RecipeDraft {
        name: recipe.name,
        ingredients: recipe
            .ingredients
            .iter()
            .map(|line| ingredient_line::parse_line(line))
            .collect(),
        steps: recipe.steps,
        source_url,
    })
}

/// The address to fetch: a URL `valid_source_url` accepts once parsed (so
/// that the draft's `source_url` can be saved as is), which `check_url`
/// lets through.
fn import_url(raw: &str) -> Result<Url, ImportFailure> {
    let url = Url::parse(raw.trim()).map_err(|_| ImportFailure::InvalidUrl)?;
    check_url(&url).map_err(refused)?;
    valid_source_url(url.as_str()).ok_or(ImportFailure::InvalidUrl)?;
    Ok(url)
}

fn refused(refusal: Refused) -> ImportFailure {
    match refusal {
        Refused::Scheme => ImportFailure::SchemeRefused,
        Refused::Destination => ImportFailure::DestinationRefused,
        Refused::TooManyRedirects => ImportFailure::Unreachable,
    }
}

fn failed(error: reqwest::Error) -> ImportFailure {
    if let Some(refusal) = refusal_in(&error) {
        return refused(refusal);
    }
    if error.is_timeout() {
        return ImportFailure::Timeout;
    }
    tracing::info!(error = %error, "recipe page unreachable");
    ImportFailure::Unreachable
}

/// Whether a `Content-Type` value names HTML.
fn is_html(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence == "text/html" || essence == "application/xhtml+xml"
}

/// The page at `url`, as text, read up to `MAX_PAGE_BYTES`. Bytes that are
/// not UTF-8 are replaced: JSON-LD is UTF-8 by definition.
async fn fetch_page(client: &reqwest::Client, url: Url) -> Result<String, ImportFailure> {
    let mut response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await
        .map_err(failed)?;
    if !response.status().is_success() {
        return Err(ImportFailure::Unreachable);
    }
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(is_html);
    if !html {
        return Err(ImportFailure::NotHtml);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PAGE_BYTES as u64)
    {
        return Err(ImportFailure::TooLarge);
    }
    let mut page = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        if page.len() + chunk.len() > MAX_PAGE_BYTES {
            return Err(ImportFailure::TooLarge);
        }
        page.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&page).into_owned())
}

/// Imports one member may start per `RATE_WINDOW`.
pub const RATE_LIMIT: usize = 10;

/// The window `RATE_LIMIT` is counted over.
pub const RATE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// At most `RATE_LIMIT` imports per member per `RATE_WINDOW`, sliding.
#[derive(Default)]
pub struct ImportThrottle {
    admitted: Mutex<HashMap<Uuid, VecDeque<Instant>>>,
}

impl ImportThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `user` may start an import at `now`; counted when it may.
    pub fn admit(&self, user: Uuid, now: Instant) -> bool {
        let mut admitted = self
            .admitted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Every member's imports that left the window are dropped, and a
        // member left with none is forgotten: the map holds only the
        // members who imported in the last `RATE_WINDOW`.
        admitted.retain(|_, times| {
            while times
                .front()
                .is_some_and(|&at| now.saturating_duration_since(at) >= RATE_WINDOW)
            {
                times.pop_front();
            }
            !times.is_empty()
        });
        let times = admitted.entry(user).or_default();
        if times.len() >= RATE_LIMIT {
            return false;
        }
        times.push_back(now);
        true
    }

    /// Members with an import still counted.
    pub fn tracked(&self) -> usize {
        self.admitted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

/// `source_url` as a recipe stores it: trimmed, `http` or `https`, at most
/// `SOURCE_URL_MAX_LEN` characters. `None` for anything else.
pub fn valid_source_url(source_url: &str) -> Option<String> {
    let source_url = source_url.trim();
    if source_url.chars().count() > SOURCE_URL_MAX_LEN {
        return None;
    }
    let parsed = reqwest::Url::parse(source_url).ok()?;
    let web = matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|h| !h.is_empty());
    web.then(|| source_url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_content_types() {
        for html in [
            "text/html",
            "text/html; charset=utf-8",
            "Text/HTML;charset=ISO-8859-1",
            "application/xhtml+xml",
        ] {
            assert!(is_html(html), "{html}");
        }
        for other in [
            "application/json",
            "text/plain",
            "image/png",
            "",
            "text/htmlx",
        ] {
            assert!(!is_html(other), "{other}");
        }
    }

    #[test]
    fn import_url_refusals() {
        assert_eq!(import_url("pas une url"), Err(ImportFailure::InvalidUrl));
        assert_eq!(
            import_url("ftp://e.fr/r"),
            Err(ImportFailure::SchemeRefused)
        );
        assert_eq!(
            import_url("http://127.0.0.1/r"),
            Err(ImportFailure::DestinationRefused)
        );
        assert_eq!(
            import_url(&format!("https://e.fr/{}", "a".repeat(SOURCE_URL_MAX_LEN))),
            Err(ImportFailure::InvalidUrl)
        );
        assert_eq!(
            import_url(" https://www.750g.com/r.htm ").map(|u| u.to_string()),
            Ok("https://www.750g.com/r.htm".to_string())
        );
    }

    #[test]
    fn a_member_gets_rate_limit_imports_per_window() {
        let throttle = ImportThrottle::new();
        let user = Uuid::new_v4();
        let t0 = Instant::now();
        for i in 0..RATE_LIMIT {
            assert!(
                throttle.admit(user, t0 + Duration::from_secs(i as u64)),
                "{i}"
            );
        }
        assert!(!throttle.admit(user, t0 + Duration::from_secs(RATE_LIMIT as u64)));
        // Refused attempts are not counted: the first one leaves the window
        // and exactly one slot opens.
        let later = t0 + RATE_WINDOW;
        assert!(throttle.admit(user, later));
        assert!(!throttle.admit(user, later));
    }

    #[test]
    fn members_are_counted_apart() {
        let throttle = ImportThrottle::new();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Instant::now();
        for _ in 0..RATE_LIMIT {
            assert!(throttle.admit(a, now));
        }
        assert!(!throttle.admit(a, now));
        assert!(throttle.admit(b, now));
    }

    #[test]
    fn members_whose_imports_all_left_the_window_are_forgotten() {
        let throttle = ImportThrottle::new();
        let now = Instant::now();
        throttle.admit(Uuid::new_v4(), now);
        throttle.admit(Uuid::new_v4(), now);
        assert_eq!(throttle.tracked(), 2);
        throttle.admit(Uuid::new_v4(), now + RATE_WINDOW);
        assert_eq!(throttle.tracked(), 1);
    }

    #[test]
    fn source_url_is_http_or_https_and_bounded() {
        assert_eq!(
            valid_source_url(" https://www.marmiton.org/recettes/x.aspx "),
            Some("https://www.marmiton.org/recettes/x.aspx".into())
        );
        assert_eq!(
            valid_source_url("http://exemple.fr/r"),
            Some("http://exemple.fr/r".into())
        );
        for refused in [
            "",
            "   ",
            "javascript:alert(1)",
            "ftp://exemple.fr/r",
            "data:text/html,x",
            "exemple.fr/recette",
            "https://",
        ] {
            assert_eq!(valid_source_url(refused), None, "{refused:?}");
        }
        let at_limit = format!("https://e.fr/{}", "a".repeat(SOURCE_URL_MAX_LEN - 13));
        assert_eq!(at_limit.chars().count(), SOURCE_URL_MAX_LEN);
        assert_eq!(valid_source_url(&at_limit), Some(at_limit.clone()));
        assert_eq!(valid_source_url(&format!("{at_limit}a")), None);
    }
}
