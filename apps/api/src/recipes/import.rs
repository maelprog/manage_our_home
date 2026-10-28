//! `POST /groups/:id/recipes/import` (#405): the recipe a web page
//! publishes, as a draft for the member to review. Read from the page's
//! schema.org JSON-LD (`jsonld`), else its schema.org microdata
//! (`microdata`), else its prose (`prose`, #475): an « Ingrédients »
//! heading over a list. The draft's `source_url` is the address without
//! its tracking parameters (`without_tracking`).
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
//! the usual 429. The 500s are the database failing (the limit's
//! included) and an extraction that panics. An import the limit cannot
//! count is not started (#428).

use std::time::Duration;

use axum::extract::{Path, State};
use axum::Json;
use manage_our_home_shared::dto::recipes::{ImportRecipeRequest, RecipeDraft, SOURCE_URL_MAX_LEN};
use reqwest::Url;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::auth::throttle::Pseudonyms;
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::public_destination::{check_url, refusal_in, Refused};
use crate::rate_limit::{SlidingLimiter, SlidingWindow};
use crate::recipes::{ingredient_line, jsonld, microdata, prose};
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
    /// No recipe in the page: no `Recipe` in its JSON-LD or microdata, no
    /// ingredient under an « Ingrédients » heading.
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
    if !state.recipe_import_throttle.admit(auth.user_id).await? {
        return Err(AppError::TooManyRequests);
    }
    let page = fetch_page(&state.recipe_import_client, url.clone()).await?;
    // Reading up to `MAX_PAGE_BYTES` is CPU work, linear but not free: off
    // the async workers, so one import does not stall other requests.
    let source_url = without_tracking(&url).to_string();
    let draft = tokio::task::spawn_blocking(move || draft_of(&page, source_url))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("recipe extraction panicked: {e}")))?;
    Ok(Json(draft.ok_or(ImportFailure::NoRecipe)?))
}

/// The draft a page gives, or `None` when it publishes no recipe: its
/// JSON-LD recipe, else its microdata recipe, else the recipe its prose
/// gives.
fn draft_of(page: &str, source_url: String) -> Option<RecipeDraft> {
    let recipe = jsonld::extract_recipe(page)
        .or_else(|| microdata::extract_recipe(page))
        .or_else(|| prose::extract_recipe(page))?;
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
    // Without the URL: the address a member typed is kept with the recipe
    // they save, nowhere else (privacy policy).
    tracing::info!(error = %error.without_url(), "recipe page unreachable");
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
/// not UTF-8 are replaced: JSON-LD is UTF-8 by definition, and so are
/// nearly all recipe pages.
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
pub const RATE: SlidingWindow = SlidingWindow {
    limit: RATE_LIMIT,
    window: RATE_WINDOW,
};

/// [`RATE`], counted per member in `rate_limit_log` (#428): every replica
/// counts against the same budget. See `rate_limit` for the mechanism; the
/// member's id is stored as a keyed pseudonym.
#[derive(Debug, Clone)]
pub struct ImportThrottle(SlidingLimiter);

impl ImportThrottle {
    pub fn new(db: PgPool, pseudonyms: Pseudonyms) -> Self {
        Self(SlidingLimiter::new(db, pseudonyms, "recipe_import", RATE))
    }

    /// Whether `user` may start an import now; counted when it may.
    pub async fn admit(&self, user: Uuid) -> Result<bool, sqlx::Error> {
        self.0.admit(user.as_bytes()).await
    }
}

/// `url` without its tracking parameters: `utm_*`, `fbclid`, `gclid`
/// (names in any case). The other parameters stay as written, in their
/// order; a query left empty is dropped with its `?`.
fn without_tracking(url: &Url) -> Url {
    let Some(query) = url.query().filter(|q| !q.is_empty()) else {
        return url.clone();
    };
    let tracking = |pair: &&str| {
        let name = pair
            .split('=')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        name.starts_with("utm_") || name == "fbclid" || name == "gclid"
    };
    let kept: Vec<&str> = query.split('&').filter(|p| !tracking(p)).collect();
    let mut cleaned = url.clone();
    cleaned.set_query((!kept.is_empty()).then(|| kept.join("&")).as_deref());
    cleaned
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
    use std::time::SystemTime;

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

    /// The per-member map the process used to hold before #428, on the
    /// same pure rule the store applies to each member's row: a member
    /// whose imports all left the window is forgotten, as the store
    /// deletes such rows on every admission.
    #[derive(Default)]
    struct ImportThrottle {
        admitted: std::collections::HashMap<Uuid, Vec<SystemTime>>,
    }

    impl ImportThrottle {
        fn new() -> Self {
            Self::default()
        }

        fn admit(&mut self, user: Uuid, now: SystemTime) -> bool {
            self.admitted.retain(|_, log| !RATE.forgets(log, now));
            RATE.admit(self.admitted.entry(user).or_default(), now)
        }

        fn tracked(&self) -> usize {
            self.admitted.len()
        }
    }

    #[test]
    fn a_member_gets_rate_limit_imports_per_window() {
        let mut throttle = ImportThrottle::new();
        let user = Uuid::new_v4();
        let t0 = SystemTime::now();
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
        let mut throttle = ImportThrottle::new();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let now = SystemTime::now();
        for _ in 0..RATE_LIMIT {
            assert!(throttle.admit(a, now));
        }
        assert!(!throttle.admit(a, now));
        assert!(throttle.admit(b, now));
    }

    #[test]
    fn members_whose_imports_all_left_the_window_are_forgotten() {
        let mut throttle = ImportThrottle::new();
        let now = SystemTime::now();
        throttle.admit(Uuid::new_v4(), now);
        throttle.admit(Uuid::new_v4(), now);
        assert_eq!(throttle.tracked(), 2);
        throttle.admit(Uuid::new_v4(), now + RATE_WINDOW);
        assert_eq!(throttle.tracked(), 1);
    }

    fn cleaned(url: &str) -> String {
        without_tracking(&Url::parse(url).unwrap()).to_string()
    }

    #[test]
    fn tracking_parameters_are_dropped_from_the_source_url() {
        assert_eq!(
            cleaned("https://recipescomfortfood.woochiens.com/tajine/?utm_source=Pinterest&utm_medium=organic"),
            "https://recipescomfortfood.woochiens.com/tajine/"
        );
        assert_eq!(
            cleaned(
                "https://e.fr/r?id=3&utm_campaign=x&page=2&fbclid=Ab1&gclid=Z9&UTM_TERM=y#etape-2"
            ),
            "https://e.fr/r?id=3&page=2#etape-2"
        );
        // Other parameters stay as written, in their order and encoding.
        for kept in [
            "https://e.fr/r?q=cr%C3%AApe+sucr%C3%A9e&tri=note",
            "https://e.fr/r?utm=1&utmsource=2&xfbclid=3",
            "https://e.fr/r?a=1&&b",
            "https://e.fr/r?",
            "https://e.fr/r",
        ] {
            assert_eq!(cleaned(kept), kept);
        }
    }

    const WOOCHIENS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/recipe_pages/woochiens_prose.html"
    ));

    fn draft(page: &str) -> Option<RecipeDraft> {
        draft_of(page, "https://e.fr/r".into())
    }

    /// The page of #475: neither JSON-LD nor microdata, a recipe in prose.
    #[test]
    fn the_woochiens_prose_page_gives_a_draft() {
        let draft = draft(WOOCHIENS).unwrap();
        assert!(draft.name.starts_with("Tajine de poulet"), "{}", draft.name);
        assert!(draft.ingredients.len() >= 15, "{}", draft.ingredients.len());
        let named = |name: &str| {
            draft
                .ingredients
                .iter()
                .find(|i| i.name == name)
                .unwrap_or_else(|| panic!("no {name}: {:#?}", draft.ingredients))
                .clone()
        };
        let poulet = named("Poulet entier ou morceaux");
        assert_eq!(
            (poulet.quantity, poulet.unit.as_deref()),
            (Some(1.2), Some("kg"))
        );
        let bouillon = named("Bouillon de volaille");
        assert_eq!(
            (bouillon.quantity, bouillon.unit.as_deref()),
            (Some(250.0), Some("ml"))
        );
        assert!(!draft.steps.is_empty());
    }

    /// JSON-LD first, then microdata, then prose: a page carrying the three
    /// gives its JSON-LD recipe, one without JSON-LD its microdata recipe.
    #[test]
    fn extractors_are_tried_in_order() {
        let jsonld =
            r#"<script type="application/ld+json">{"@type": "Recipe", "name": "JSON-LD"}</script>"#;
        let microdata = r#"<div itemscope itemtype="https://schema.org/Recipe"><h2 itemprop="name">Microdata</h2></div>"#;
        let prose = "<h1>Prose</h1><h2>Ingrédients</h2><ul><li>1 poireau</li></ul>";
        let name = |page: &str| draft(page).map(|d| d.name);
        assert_eq!(
            name(&format!("{jsonld}{microdata}{prose}")).as_deref(),
            Some("JSON-LD")
        );
        assert_eq!(
            name(&format!("{prose}{microdata}")).as_deref(),
            Some("Microdata")
        );
        assert_eq!(name(prose).as_deref(), Some("Prose"));
        assert_eq!(name("<h1>Rien</h1><p>Pas de recette.</p>"), None);
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
