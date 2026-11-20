//! The shared recipe library (#906), over the schema of #905
//! (`migrations/0066_recipe_library.sql`): a member publishes one of their
//! family's recipes, withdraws it, and every signed-in user browses what
//! was published.
//!
//! - `POST /groups/{id}/recipes/{recipe_id}/publish` takes a snapshot of
//!   the recipe — name, method, servings, duration, ingredients with their
//!   seasons — once the caller says `{"consent": true}`. Who may: whoever
//!   may edit the recipe (`recipes::can_modify`, the family's
//!   `recipes_edit` threshold of #866). Publishing again replaces the
//!   snapshot, in place.
//! - `DELETE` on the same path withdraws the caller's publication of that
//!   recipe.
//! - `GET /library/recipes` (search by name and by ingredient, keyset
//!   pages) and `GET /library/recipes/{id}` answer what was published and
//!   nothing about who published it: no family, no person, no source recipe.
//!   Their JSON is built from `library_recipes` and `library_ingredients`
//!   alone, which hold none of these.
//! - `POST /groups/{id}/library/recipes/{library_id}/copy` (#907): any
//!   member copies a publication into their family, a recipe of its own
//!   (`created_by` the caller, `library_recipe_id` the publication) that
//!   does not follow it afterwards (user's decision of 2026-11-17). Once
//!   per family: a second copy is refused 409 `already_copied` rather
//!   than answered with the first, so the client knows nothing was made.
//!
//! **The source recipe is never taken from the client.** It is the recipe
//! of the path, read in the caller's family under its row-level policy
//! (`recipes`, `app.group_id`): `library_recipe_publishers.source_recipe_id`
//! is unique and its key ignores RLS, so a client-given id would let one
//! family probe another's recipes through the unique violation.
//!
//! Limits (user's decisions of 2026-11-20): at most [`FAMILY_QUOTA`]
//! publications per family, 409 `library_quota_exceeded` past it; reads at
//! [`READ_RATE`] and publications at [`PUBLISH_RATE`] per user, 429 past
//! them (`rate_limit::LIMITS`); a search term of [`SEARCH_MAX_CHARS`]
//! characters at most, 400 `search_too_long` past it; pages of
//! [`PAGE_DEFAULT`], [`PAGE_MAX`] at most.

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, user_scoped_tx, AuthUser};
use crate::auth::throttle::Pseudonyms;
use crate::error::{AppError, AppResult};
use crate::groups::access::{min_role_for, Feature};
use crate::groups::require_role;
use crate::rate_limit::{SlidingLimiter, SlidingWindow};
use crate::recipes::can_modify;
use crate::recipes::crud::{read_recipe, DURATION_MAX};
use crate::recipes::selections::SERVINGS_MAX;
use crate::AppState;

/// Publications a family may hold at once (user's decision of 2026-11-20).
/// Counted over the publications whose source recipe is one of the
/// family's (`group_library_publication_count`, 0067): one taken from a
/// recipe since deleted no longer counts.
pub const FAMILY_QUOTA: i64 = 50;

/// Entries of a page when the client asks for no size.
pub const PAGE_DEFAULT: i64 = 20;

/// Entries of a page at most; a larger request is served this many.
pub const PAGE_MAX: i64 = 50;

/// The longest search term, in characters, once trimmed.
pub const SEARCH_MAX_CHARS: usize = 100;

/// Reads of the library (list, detail and copies) per user.
pub const READ_RATE: SlidingWindow = SlidingWindow {
    limit: 60,
    window: Duration::from_secs(60),
};

/// Publications (first or again) per user.
pub const PUBLISH_RATE: SlidingWindow = SlidingWindow {
    limit: 10,
    window: Duration::from_secs(60),
};

/// `rate_limit_log.kind` of [`READ_RATE`].
pub const READ_KIND: &str = "library_read";

/// `rate_limit_log.kind` of [`PUBLISH_RATE`].
pub const PUBLISH_KIND: &str = "library_publish";

/// The bounds of `library_recipes` and `library_ingredients` (0066), which
/// `recipes` does not have: a recipe past them is refused with a 400 here
/// rather than with a failed `CHECK`.
const NAME_MAX_CHARS: usize = 200;
const INSTRUCTIONS_MAX_CHARS: usize = 20_000;
const UNIT_MAX_CHARS: usize = 50;
const INGREDIENTS_MAX: usize = 100;

/// Advisory lock class of a family's publications: two publications of one
/// family are decided one after the other, so they cannot both read « 49 ».
/// The second key is `hashtext` of the family's id; two families that
/// collide only wait on each other.
const QUOTA_LOCK: i32 = i32::from_be_bytes(*b"mlbq");

/// Advisory lock class of a family's copies: two copies of one publication
/// cannot both find none in the family. Second key as [`QUOTA_LOCK`]'s.
const COPY_LOCK: i32 = i32::from_be_bytes(*b"mlbc");

/// The unique key of `library_recipe_publishers.source_recipe_id`.
const SOURCE_KEY: &str = "library_recipe_publishers_source_recipe_id_key";

fn bad(code: &str) -> AppError {
    AppError::BadRequest(code.into())
}

/// Who may publish a recipe: whoever may edit it (#866), and only with the
/// consent ticked. The bar comes first, so a member who may not edit the
/// recipe is told so whatever the body says.
pub(crate) fn publish_bar(
    actor_role: &str,
    min_role: &str,
    actor_is_creator: bool,
    consent: bool,
) -> AppResult<()> {
    if !can_modify(actor_role, min_role, actor_is_creator) {
        return Err(AppError::Forbidden);
    }
    if !consent {
        return Err(bad("consent_required"));
    }
    Ok(())
}

/// Whether a family holding `published` publications may publish: always
/// when it replaces one of them, under [`FAMILY_QUOTA`] otherwise.
pub(crate) fn within_quota(published: i64, republishing: bool) -> bool {
    republishing || published < FAMILY_QUOTA
}

/// A search term as the client sent it: trimmed, `None` when blank (no
/// filter), refused past [`SEARCH_MAX_CHARS`] characters.
pub(crate) fn search_term(raw: Option<&str>) -> AppResult<Option<String>> {
    let term = raw.map(str::trim).unwrap_or("");
    if term.is_empty() {
        return Ok(None);
    }
    if term.chars().count() > SEARCH_MAX_CHARS {
        return Err(bad("search_too_long"));
    }
    Ok(Some(term.to_string()))
}

/// A `LIKE … ESCAPE '\'` pattern matching `term` anywhere, its `%`, `_`
/// and `\` taken literally.
pub(crate) fn contains_pattern(term: &str) -> String {
    let mut pattern = String::with_capacity(term.len() + 2);
    pattern.push('%');
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// The page size served for `requested`: [`PAGE_DEFAULT`] when absent or
/// empty, [`PAGE_MAX`] at most; 400 `invalid_limit` when it is no whole
/// number of at least 1.
///
/// Read from the query string as text, so that a value that is no positive
/// whole number gets the API's own 400 `invalid_limit` rather than the
/// extractor's plain-text one. A run of digits too long for an integer is
/// still a number, past the cap.
pub(crate) fn page_size(requested: Option<&str>) -> AppResult<i64> {
    let raw = match requested {
        None | Some("") => return Ok(PAGE_DEFAULT),
        Some(raw) => raw,
    };
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("invalid_limit"));
    }
    match raw.parse::<i64>() {
        Ok(0) => Err(bad("invalid_limit")),
        Ok(n) => Ok(n.min(PAGE_MAX)),
        Err(_) => Ok(PAGE_MAX),
    }
}

/// The latest instant a cursor may name, 9999-12-31T23:59:59.999999Z. A
/// cursor is a `published_at`, which is never before 1970 nor after it;
/// outside, chrono may still read the value where `timestamptz` cannot.
const CURSOR_MAX_MICROS: i64 = 253_402_300_799_999_999;

/// Where a page ends: the newest-first order is `(published_at, id)`
/// descending, and the next page starts strictly after this pair. Sent to
/// the client as `<microseconds>.<uuid>`, which it hands back unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub published_at: DateTime<Utc>,
    pub id: Uuid,
}

impl Cursor {
    pub(crate) fn encode(&self) -> String {
        format!("{}.{}", self.published_at.timestamp_micros(), self.id)
    }

    pub(crate) fn parse(raw: &str) -> AppResult<Cursor> {
        let invalid = || bad("invalid_cursor");
        let (micros, id) = raw.split_once('.').ok_or_else(invalid)?;
        let published_at = micros
            .parse::<i64>()
            .ok()
            .filter(|m| (0..=CURSOR_MAX_MICROS).contains(m))
            .and_then(DateTime::from_timestamp_micros)
            .ok_or_else(invalid)?;
        let id = Uuid::parse_str(id).map_err(|_| invalid())?;
        Ok(Cursor { published_at, id })
    }
}

/// The recipe being published, as the family holds it.
pub(crate) struct SourceRecipe {
    pub name: String,
    pub instructions: Option<String>,
    pub servings: Option<i32>,
    pub duration_minutes: Option<i32>,
}

/// One of its ingredients, as `recipe_ingredients` holds it.
pub(crate) struct SourceIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub is_optional: bool,
    pub seasonal_months: Option<Vec<i32>>,
}

/// What is written to the library: within its bounds, normalised.
#[derive(Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub name: String,
    pub instructions: Option<String>,
    pub servings: Option<i32>,
    pub duration_minutes: Option<i32>,
    pub ingredients: Vec<SnapshotIngredient>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct SnapshotIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub is_optional: bool,
    pub seasonal_months: Option<Vec<i32>>,
}

fn non_blank(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty())
}

/// The snapshot of a recipe, or the 400 naming the first bound it passes.
/// `recipes` holds no bound on lengths nor on the number of ingredients,
/// and `recipe_ingredients` none on the months; the library holds them
/// all (0066). Normalised: names trimmed, a blank method or unit absent,
/// and the months sorted, each once — an empty list becomes NULL, « all
/// year » as in `recipe_ingredients`, where `{}` would read « never ».
pub(crate) fn snapshot(
    recipe: SourceRecipe,
    ingredients: Vec<SourceIngredient>,
) -> AppResult<Snapshot> {
    let name = recipe.name.trim().to_string();
    if name.is_empty() {
        return Err(bad("name_required"));
    }
    if name.chars().count() > NAME_MAX_CHARS {
        return Err(bad("name_too_long"));
    }
    let instructions = non_blank(recipe.instructions);
    if instructions
        .as_ref()
        .is_some_and(|i| i.chars().count() > INSTRUCTIONS_MAX_CHARS)
    {
        return Err(bad("instructions_too_long"));
    }
    if recipe
        .servings
        .is_some_and(|s| !(1..=SERVINGS_MAX).contains(&s))
    {
        return Err(bad("invalid_servings"));
    }
    if recipe
        .duration_minutes
        .is_some_and(|d| !(1..=DURATION_MAX).contains(&d))
    {
        return Err(bad("invalid_duration"));
    }
    if ingredients.len() > INGREDIENTS_MAX {
        return Err(bad("too_many_ingredients"));
    }
    let ingredients = ingredients
        .into_iter()
        .map(|i| {
            let name = i.name.trim().to_string();
            if name.is_empty() {
                return Err(bad("ingredient_name_required"));
            }
            if name.chars().count() > NAME_MAX_CHARS {
                return Err(bad("ingredient_name_too_long"));
            }
            let unit = non_blank(i.unit).map(|u| u.trim().to_string());
            if unit
                .as_ref()
                .is_some_and(|u| u.chars().count() > UNIT_MAX_CHARS)
            {
                return Err(bad("ingredient_unit_too_long"));
            }
            if i.quantity.is_some_and(|q| !q.is_finite() || q < 0.0) {
                return Err(bad("ingredient_quantity_must_be_non_negative"));
            }
            let seasonal_months = match i.seasonal_months {
                None => None,
                Some(mut months) => {
                    if months.iter().any(|m| !(1..=12).contains(m)) {
                        return Err(bad("invalid_seasonal_month"));
                    }
                    months.sort_unstable();
                    months.dedup();
                    (!months.is_empty()).then_some(months)
                }
            };
            Ok(SnapshotIngredient {
                name,
                quantity: i.quantity,
                unit,
                is_optional: i.is_optional,
                seasonal_months,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(Snapshot {
        name,
        instructions,
        servings: recipe.servings,
        duration_minutes: recipe.duration_minutes,
        ingredients,
    })
}

/// [`READ_RATE`] and [`PUBLISH_RATE`], counted per user in
/// `rate_limit_log` (#428): every replica counts against the same budget.
/// The user's id is stored as a keyed pseudonym.
#[derive(Debug, Clone)]
pub struct LibraryThrottle {
    read: SlidingLimiter,
    publish: SlidingLimiter,
}

impl LibraryThrottle {
    /// `db` is the limits' own pool (`rate_limit::POOL_SIZE`).
    pub fn new(db: PgPool, pseudonyms: Pseudonyms) -> Self {
        Self {
            read: SlidingLimiter::new(db.clone(), pseudonyms.clone(), READ_KIND, READ_RATE),
            publish: SlidingLimiter::new(db, pseudonyms, PUBLISH_KIND, PUBLISH_RATE),
        }
    }

    async fn admit(limiter: &SlidingLimiter, user: Uuid) -> AppResult<()> {
        if limiter.admit(user.as_bytes()).await? {
            Ok(())
        } else {
            Err(AppError::TooManyRequests)
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PublishRequest {
    /// The member's agreement to publish the recipe to every user of the
    /// application, anonymously. Anything but `true` publishes nothing.
    #[serde(default)]
    pub consent: bool,
}

/// One ingredient of a publication, in the recipe's order.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LibraryIngredientResponse {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub is_optional: bool,
    /// Months (1-12) in season, sorted; absent, all year.
    pub seasonal_months: Option<Vec<i32>>,
}

/// A publication, whole. Names neither the family nor the person it comes
/// from, nor the recipe it was taken from.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LibraryRecipeResponse {
    pub id: Uuid,
    pub name: String,
    pub instructions: Option<String>,
    pub servings: Option<i32>,
    pub duration_minutes: Option<i32>,
    pub ingredients: Vec<LibraryIngredientResponse>,
    pub published_at: DateTime<Utc>,
    /// When it was last published again; `published_at` on a first one.
    pub updated_at: DateTime<Utc>,
}

/// A publication in a list.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LibraryRecipeSummary {
    pub id: Uuid,
    pub name: String,
    pub servings: Option<i32>,
    pub duration_minutes: Option<i32>,
    pub published_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One page of the library, newest first.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LibraryPage {
    pub recipes: Vec<LibraryRecipeSummary>,
    /// To pass as `cursor` for the next page; absent on the last one.
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryQuery {
    /// Publications whose name holds this text, case aside. Blank: no
    /// filter. At most 100 characters (`search_too_long`).
    pub q: Option<String>,
    /// Publications with an ingredient whose name holds this text, case
    /// aside. Same bounds as `q`.
    pub ingredient: Option<String>,
    /// The `next_cursor` of the previous page (`invalid_cursor`).
    pub cursor: Option<String>,
    /// Entries per page: 20 when absent, 50 at most (a larger value is
    /// served 50). Anything but a whole number of at least 1 is refused
    /// (`invalid_limit`).
    #[param(value_type = Option<i64>, minimum = 1)]
    pub limit: Option<String>,
}

struct LibraryRow {
    id: Uuid,
    name: String,
    instructions: Option<String>,
    servings: Option<i32>,
    duration_minutes: Option<i32>,
    published_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// A publication and its ingredients, read in `tx` (`app.user_id` set).
async fn fetch_publication(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> AppResult<Option<LibraryRecipeResponse>> {
    let Some(row) = sqlx::query_as!(
        LibraryRow,
        r#"SELECT id, name, instructions, servings, duration_minutes, published_at, updated_at
           FROM library_recipes WHERE id = $1"#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let ingredients = sqlx::query_as!(
        LibraryIngredientResponse,
        r#"SELECT name, quantity, unit, is_optional, seasonal_months
           FROM library_ingredients WHERE library_recipe_id = $1 ORDER BY position"#,
        id
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(Some(LibraryRecipeResponse {
        id: row.id,
        name: row.name,
        instructions: row.instructions,
        servings: row.servings,
        duration_minutes: row.duration_minutes,
        ingredients,
        published_at: row.published_at,
        updated_at: row.updated_at,
    }))
}

#[utoipa::path(
    post,
    path = "/groups/{id}/recipes/{recipe_id}/publish",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe id"),
    ),
    request_body = PublishRequest,
    responses(
        (status = 201, description = "Published: the publication, as every user reads it", body = LibraryRecipeResponse),
        (status = 200, description = "Published again: the snapshot replaced", body = LibraryRecipeResponse),
        (status = 400, description = "`consent_required` (absent or not `true`, or a body that is not JSON); the recipe past a bound of the library: `name_required`, `name_too_long`, `instructions_too_long`, `invalid_servings`, `invalid_duration`, `too_many_ingredients`, `ingredient_name_required`, `ingredient_name_too_long`, `ingredient_unit_too_long`, `ingredient_quantity_must_be_non_negative` or `invalid_seasonal_month`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: no such recipe in the family", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 409, description = "`library_quota_exceeded`: the family holds 50 publications; `published_by_another_member`: another member already published this recipe", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 429, description = "`too_many_attempts`: 10 publications per user per minute", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn publish_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
    body: Result<Json<PublishRequest>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    // Counted before any transaction opens: the admission takes a
    // connection of the limits' own pool, never while holding one of the
    // handlers'.
    LibraryThrottle::admit(&state.library_throttle.publish, auth.user_id).await?;
    let consent = matches!(body, Ok(Json(PublishRequest { consent: true })));
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;

    // The source: a recipe of this family, read under its policy — never
    // an id the client chose (see the module's note).
    let source = sqlx::query!(
        r#"SELECT created_by, name, instructions, servings, duration_minutes
           FROM recipes WHERE id = $1 AND group_id = $2"#,
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    let min_role = min_role_for(&mut tx, group_id, Feature::RecipesEdit).await?;
    publish_bar(
        &actor_role,
        &min_role,
        source.created_by == auth.user_id,
        consent,
    )?;

    let ingredients = sqlx::query_as!(
        SourceIngredient,
        r#"SELECT name, quantity, unit, is_optional, seasonal_months
           FROM recipe_ingredients WHERE recipe_id = $1 ORDER BY name, id"#,
        recipe_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let snap = snapshot(
        SourceRecipe {
            name: source.name,
            instructions: source.instructions,
            servings: source.servings,
            duration_minutes: source.duration_minutes,
        },
        ingredients,
    )?;

    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(QUOTA_LOCK)
        .bind(group_id.to_string())
        .execute(&mut *tx)
        .await?;
    // The caller's own publication of this recipe; the publishers' policy
    // shows no other, and the `user_id` says so again.
    let existing = sqlx::query_scalar!(
        r#"SELECT library_recipe_id FROM library_recipe_publishers
           WHERE source_recipe_id = $1 AND user_id = $2"#,
        recipe_id,
        auth.user_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let published = sqlx::query_scalar!(
        r#"SELECT group_library_publication_count($1) AS "n!""#,
        group_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if !within_quota(published, existing.is_some()) {
        return Err(AppError::Conflict("library_quota_exceeded".into()));
    }

    let id = match existing {
        Some(id) => {
            sqlx::query!(
                r#"UPDATE library_recipes
                   SET name = $2, instructions = $3, servings = $4, duration_minutes = $5,
                       updated_at = now()
                   WHERE id = $1"#,
                id,
                snap.name,
                snap.instructions,
                snap.servings,
                snap.duration_minutes,
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!(
                "DELETE FROM library_ingredients WHERE library_recipe_id = $1",
                id
            )
            .execute(&mut *tx)
            .await?;
            id
        }
        None => {
            let id = sqlx::query_scalar!(
                r#"INSERT INTO library_recipes (name, instructions, servings, duration_minutes)
                   VALUES ($1, $2, $3, $4) RETURNING id"#,
                snap.name,
                snap.instructions,
                snap.servings,
                snap.duration_minutes,
            )
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query!(
                r#"INSERT INTO library_recipe_publishers (library_recipe_id, user_id, source_recipe_id)
                   VALUES ($1, $2, $3)"#,
                id,
                auth.user_id,
                recipe_id,
            )
            .execute(&mut *tx)
            .await
            .map_err(|e| match e {
                // Not the caller's (looked up above): another member's,
                // of this family's recipe — nothing another family learns.
                sqlx::Error::Database(db) if db.constraint() == Some(SOURCE_KEY) => {
                    AppError::Conflict("published_by_another_member".into())
                }
                e => e.into(),
            })?;
            id
        }
    };
    for (position, ing) in snap.ingredients.iter().enumerate() {
        // At most `INGREDIENTS_MAX` (100): fits the `SMALLINT`.
        let position = position as i16;
        sqlx::query!(
            r#"INSERT INTO library_ingredients
                   (library_recipe_id, position, name, quantity, unit, is_optional, seasonal_months)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            id,
            position,
            ing.name,
            ing.quantity,
            ing.unit,
            ing.is_optional,
            ing.seasonal_months.as_deref(),
        )
        .execute(&mut *tx)
        .await?;
    }
    let publication = fetch_publication(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("publication {id} not read back")))?;
    tx.commit().await?;

    let status = if existing.is_some() {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(publication)))
}

#[utoipa::path(
    delete,
    path = "/groups/{id}/recipes/{recipe_id}/publish",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("recipe_id" = uuid::Uuid, Path, description = "Recipe id"),
    ),
    responses(
        (status = 204, description = "The caller's publication of the recipe withdrawn"),
        (status = 404, description = "`not_found`: no such recipe in the family, or the caller has not published it", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn unpublish_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, recipe_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    sqlx::query_scalar!(
        "SELECT id FROM recipes WHERE id = $1 AND group_id = $2",
        recipe_id,
        group_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    // Its publisher withdraws it, whatever their role now: the policies of
    // 0066 let no one else. The ingredients and the publisher row go with
    // it (`ON DELETE CASCADE`).
    let withdrawn = sqlx::query!(
        r#"DELETE FROM library_recipes WHERE id = (
               SELECT library_recipe_id FROM library_recipe_publishers
               WHERE source_recipe_id = $1 AND user_id = $2)"#,
        recipe_id,
        auth.user_id,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if withdrawn == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/library/recipes",
    tag = "recipes",
    params(LibraryQuery),
    responses(
        (status = 200, description = "One page of the library, newest first", body = LibraryPage),
        (status = 400, description = "`search_too_long`, `invalid_cursor` or `invalid_limit`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 429, description = "`too_many_attempts`: 60 reads per user per minute", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_library(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<LibraryQuery>,
) -> AppResult<impl IntoResponse> {
    let name = search_term(query.q.as_deref())?.map(|t| contains_pattern(&t));
    let ingredient = search_term(query.ingredient.as_deref())?.map(|t| contains_pattern(&t));
    let after = query.cursor.as_deref().map(Cursor::parse).transpose()?;
    let size = page_size(query.limit.as_deref())?;
    LibraryThrottle::admit(&state.library_throttle.read, auth.user_id).await?;

    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    // One more than the page: whether a next one exists.
    let mut recipes = sqlx::query_as!(
        LibraryRecipeSummary,
        r#"SELECT l.id, l.name, l.servings, l.duration_minutes, l.published_at, l.updated_at
           FROM library_recipes l
           WHERE ($1::text IS NULL OR lower(l.name) LIKE lower($1) ESCAPE '\')
             AND ($2::text IS NULL OR EXISTS (
                     SELECT 1 FROM library_ingredients i
                     WHERE i.library_recipe_id = l.id
                       AND lower(i.name) LIKE lower($2) ESCAPE '\'))
             AND ($3::timestamptz IS NULL OR (l.published_at, l.id) < ($3, $4::uuid))
           ORDER BY l.published_at DESC, l.id DESC
           LIMIT $5"#,
        name,
        ingredient,
        after.map(|c| c.published_at),
        after.map(|c| c.id),
        size + 1,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let next_cursor = if recipes.len() as i64 > size {
        recipes.truncate(size as usize);
        recipes.last().map(|r| {
            Cursor {
                published_at: r.published_at,
                id: r.id,
            }
            .encode()
        })
    } else {
        None
    };
    Ok(Json(LibraryPage {
        recipes,
        next_cursor,
    }))
}

#[utoipa::path(
    get,
    path = "/library/recipes/{id}",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Publication id"),
    ),
    responses(
        (status = 200, description = "The publication", body = LibraryRecipeResponse),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 429, description = "`too_many_attempts`: 60 reads per user per minute", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_library_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    LibraryThrottle::admit(&state.library_throttle.read, auth.user_id).await?;
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let publication = fetch_publication(&mut tx, id)
        .await?
        .ok_or(AppError::NotFound)?;
    tx.commit().await?;
    Ok(Json(publication))
}

#[utoipa::path(
    post,
    path = "/groups/{id}/library/recipes/{library_id}/copy",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("library_id" = uuid::Uuid, Path, description = "Publication id"),
    ),
    responses(
        (status = 201, description = "The family's copy: a recipe of its own, created by the caller", body = crate::recipes::crud::RecipeResponse),
        (status = 404, description = "`not_found`: no such publication (never published, withdrawn by its publisher or by moderation)", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 409, description = "`already_copied`: the family already holds a copy of this publication", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 429, description = "`too_many_attempts`: counted with the reads of the library, 60 per user per minute", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn copy_library_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, library_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    // A copy reads a publication whole, as the detail does: same budget.
    LibraryThrottle::admit(&state.library_throttle.read, auth.user_id).await?;
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(COPY_LOCK)
        .bind(group_id.to_string())
        .execute(&mut *tx)
        .await?;
    let copied = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM recipes
                          WHERE group_id = $1 AND library_recipe_id = $2) AS "copied!""#,
        group_id,
        library_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if copied {
        return Err(AppError::Conflict("already_copied".into()));
    }

    // Only the publication's own columns: nothing of who published it, nor
    // of the recipe it was taken from, reaches the family.
    let recipe_id = sqlx::query_scalar!(
        r#"INSERT INTO recipes (group_id, created_by, name, instructions, servings,
                                duration_minutes, library_recipe_id)
           SELECT $1, $2, name, instructions, servings, duration_minutes, id
           FROM library_recipes WHERE id = $3
           RETURNING id"#,
        group_id,
        auth.user_id,
        library_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    sqlx::query!(
        r#"INSERT INTO recipe_ingredients
               (recipe_id, group_id, name, quantity, unit, is_optional, seasonal_months)
           SELECT $1, $2, name, quantity, unit, is_optional, seasonal_months
           FROM library_ingredients WHERE library_recipe_id = $3
           ORDER BY position"#,
        recipe_id,
        group_id,
        library_id,
    )
    .execute(&mut *tx)
    .await?;

    let copy = read_recipe(&mut tx, group_id, auth.user_id, recipe_id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("copy {recipe_id} not read back")))?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(copy)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(result: AppResult<impl std::fmt::Debug>) -> String {
        match result {
            Err(AppError::BadRequest(m)) => m,
            Err(AppError::Forbidden) => "forbidden".into(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // --- Who may publish ---------------------------------------------------

    /// The bar of `crud::update_recipe` (#866), then the consent: a member
    /// who may not edit the recipe is told so whatever the body says.
    const BAR: [(&str, &str, bool, bool, Option<&str>); 12] = [
        ("standard", "admin", false, true, Some("forbidden")),
        ("standard", "admin", false, false, Some("forbidden")),
        ("admin", "owner", false, true, Some("forbidden")),
        ("standard", "admin", true, true, None),
        ("standard", "admin", true, false, Some("consent_required")),
        ("standard", "standard", false, true, None),
        ("admin", "admin", false, true, None),
        ("admin", "admin", false, false, Some("consent_required")),
        ("owner", "owner", false, true, None),
        ("owner", "admin", false, false, Some("consent_required")),
        ("admin", "owner", true, true, None),
        ("owner", "standard", true, true, None),
    ];

    #[test]
    fn publishing_takes_the_edit_bar_then_the_consent() {
        for (role, min_role, creator, consent, expected) in BAR {
            let got = publish_bar(role, min_role, creator, consent);
            match expected {
                None => assert!(got.is_ok(), "{role} under {min_role}, creator {creator}"),
                Some(c) => assert_eq!(code(got), c, "{role} under {min_role}, creator {creator}"),
            }
        }
    }

    #[test]
    fn the_quota_counts_new_publications_only() {
        assert!(within_quota(0, false));
        assert!(within_quota(FAMILY_QUOTA - 1, false));
        assert!(!within_quota(FAMILY_QUOTA, false));
        assert!(!within_quota(FAMILY_QUOTA + 3, false));
        assert!(
            within_quota(FAMILY_QUOTA, true),
            "republishing adds nothing"
        );
        assert_eq!(FAMILY_QUOTA, 50);
    }

    // --- Search -----------------------------------------------------------

    #[test]
    fn an_absent_or_blank_search_is_no_filter() {
        assert_eq!(search_term(None).unwrap(), None);
        assert_eq!(search_term(Some("")).unwrap(), None);
        assert_eq!(search_term(Some("  \t ")).unwrap(), None);
    }

    #[test]
    fn a_search_is_trimmed_and_bounded_at_100_characters() {
        assert_eq!(
            search_term(Some("  tarte ")).unwrap().as_deref(),
            Some("tarte")
        );
        let hundred = "é".repeat(100);
        assert_eq!(search_term(Some(&hundred)).unwrap(), Some(hundred.clone()));
        let padded = format!("  {hundred}  ");
        assert_eq!(search_term(Some(&padded)).unwrap(), Some(hundred.clone()));
        assert_eq!(code(search_term(Some(&"é".repeat(101)))), "search_too_long");
        assert_eq!(SEARCH_MAX_CHARS, 100);
    }

    #[test]
    fn the_pattern_matches_the_term_anywhere_and_literally() {
        assert_eq!(contains_pattern("poireau"), "%poireau%");
        assert_eq!(contains_pattern("100%"), r"%100\%%");
        assert_eq!(contains_pattern("a_b"), r"%a\_b%");
        assert_eq!(contains_pattern(r"a\b"), r"%a\\b%");
    }

    // --- Pages ------------------------------------------------------------

    #[test]
    fn a_page_holds_20_by_default_and_50_at_most() {
        assert_eq!(page_size(None).unwrap(), 20);
        assert_eq!(page_size(Some("")).unwrap(), 20);
        assert_eq!(page_size(Some("1")).unwrap(), 1);
        assert_eq!(page_size(Some("35")).unwrap(), 35);
        assert_eq!(page_size(Some("50")).unwrap(), 50);
        assert_eq!(page_size(Some("51")).unwrap(), 50);
        assert_eq!(page_size(Some("4294967295")).unwrap(), 50);
        assert_eq!(
            page_size(Some("99999999999999999999999")).unwrap(),
            50,
            "past i64: still a number, capped"
        );
    }

    #[test]
    fn a_page_size_that_is_not_a_positive_number_is_refused() {
        for raw in [
            "0",
            "-1",
            "-99999999999999999999999",
            "abc",
            "1.5",
            " 5",
            "+",
        ] {
            assert_eq!(code(page_size(Some(raw))), "invalid_limit", "{raw:?}");
        }
    }

    #[test]
    fn a_cursor_round_trips_to_the_microsecond() {
        let cursor = Cursor {
            published_at: DateTime::from_timestamp_micros(1_763_600_000_123_456).unwrap(),
            id: Uuid::new_v4(),
        };
        assert_eq!(Cursor::parse(&cursor.encode()).unwrap(), cursor);
        for micros in [0, 253_402_300_799_999_999] {
            let edge = Cursor {
                published_at: DateTime::from_timestamp_micros(micros).unwrap(),
                id: cursor.id,
            };
            assert_eq!(Cursor::parse(&edge.encode()).unwrap(), edge, "{micros}");
        }
    }

    #[test]
    fn a_cursor_that_is_not_one_is_refused() {
        let id = Uuid::new_v4();
        for raw in [
            String::new(),
            "abc".into(),
            id.to_string(),
            format!("x.{id}"),
            "1763600000123456.not-a-uuid".into(),
            format!("1763600000123456.{id}.1"),
            format!("99999999999999999999.{id}"),
            // Within chrono's range, outside `timestamptz`'s: a 500 there.
            format!("-8000000000000000000.{id}"),
            format!("8000000000000000000.{id}"),
            format!("-1.{id}"),
            format!("253402300800000000.{id}"),
        ] {
            assert_eq!(code(Cursor::parse(&raw)), "invalid_cursor", "{raw:?}");
        }
    }

    // --- Snapshot ---------------------------------------------------------

    fn recipe() -> SourceRecipe {
        SourceRecipe {
            name: "Tarte aux poireaux".into(),
            instructions: Some("Cuire.".into()),
            servings: Some(4),
            duration_minutes: Some(45),
        }
    }

    fn ingredient(name: &str) -> SourceIngredient {
        SourceIngredient {
            name: name.into(),
            quantity: Some(3.0),
            unit: Some("g".into()),
            is_optional: false,
            seasonal_months: None,
        }
    }

    #[test]
    fn a_recipe_within_the_bounds_is_taken_as_it_is() {
        let mut leek = ingredient("Poireau");
        leek.seasonal_months = Some(vec![10, 11, 12]);
        let got = snapshot(recipe(), vec![leek, ingredient("Pâte")]).unwrap();
        assert_eq!(
            got,
            Snapshot {
                name: "Tarte aux poireaux".into(),
                instructions: Some("Cuire.".into()),
                servings: Some(4),
                duration_minutes: Some(45),
                ingredients: vec![
                    SnapshotIngredient {
                        name: "Poireau".into(),
                        quantity: Some(3.0),
                        unit: Some("g".into()),
                        is_optional: false,
                        seasonal_months: Some(vec![10, 11, 12]),
                    },
                    SnapshotIngredient {
                        name: "Pâte".into(),
                        quantity: Some(3.0),
                        unit: Some("g".into()),
                        is_optional: false,
                        seasonal_months: None,
                    },
                ],
            }
        );
    }

    /// `{}` would read « never in season » where the recipe means « all
    /// year » (NULL, 0005); months come sorted, each once.
    #[test]
    fn seasons_blanks_and_spaces_are_normalised() {
        let mut empty = ingredient(" Sel ");
        empty.seasonal_months = Some(vec![]);
        empty.unit = Some("  ".into());
        let mut shuffled = ingredient("Poireau");
        shuffled.seasonal_months = Some(vec![12, 10, 12, 11]);
        let mut r = recipe();
        r.name = "  Tarte  ".into();
        r.instructions = Some(" \n ".into());
        let got = snapshot(r, vec![empty, shuffled]).unwrap();
        assert_eq!(got.name, "Tarte");
        assert_eq!(got.instructions, None);
        assert_eq!(got.ingredients[0].name, "Sel");
        assert_eq!(got.ingredients[0].unit, None);
        assert_eq!(got.ingredients[0].seasonal_months, None);
        assert_eq!(got.ingredients[1].seasonal_months, Some(vec![10, 11, 12]));
    }

    #[test]
    fn the_bounds_of_the_library_are_checked_before_writing() {
        let refused = |r: SourceRecipe, i: Vec<SourceIngredient>| code(snapshot(r, i));

        let mut r = recipe();
        r.name = "a".repeat(201);
        assert_eq!(refused(r, vec![]), "name_too_long");
        let mut r = recipe();
        r.name = "é".repeat(200);
        assert!(snapshot(r, vec![]).is_ok(), "200 characters, not bytes");
        let mut r = recipe();
        r.name = "  ".into();
        assert_eq!(refused(r, vec![]), "name_required");

        let mut r = recipe();
        r.instructions = Some("a".repeat(20_001));
        assert_eq!(refused(r, vec![]), "instructions_too_long");
        let mut r = recipe();
        r.instructions = Some("é".repeat(20_000));
        assert!(snapshot(r, vec![]).is_ok());

        let mut r = recipe();
        r.servings = Some(51);
        assert_eq!(refused(r, vec![]), "invalid_servings");
        let mut r = recipe();
        r.duration_minutes = Some(0);
        assert_eq!(refused(r, vec![]), "invalid_duration");

        let hundred: Vec<_> = (0..100).map(|i| ingredient(&format!("i{i}"))).collect();
        assert_eq!(snapshot(recipe(), hundred).unwrap().ingredients.len(), 100);
        let too_many: Vec<_> = (0..101).map(|i| ingredient(&format!("i{i}"))).collect();
        assert_eq!(refused(recipe(), too_many), "too_many_ingredients");

        assert_eq!(
            refused(recipe(), vec![ingredient(&"a".repeat(201))]),
            "ingredient_name_too_long"
        );
        assert_eq!(
            refused(recipe(), vec![ingredient(" ")]),
            "ingredient_name_required"
        );
        let mut long_unit = ingredient("Sel");
        long_unit.unit = Some("u".repeat(51));
        assert_eq!(
            refused(recipe(), vec![long_unit]),
            "ingredient_unit_too_long"
        );
        for q in [-1.0, f64::NAN, f64::INFINITY] {
            let mut bad = ingredient("Sel");
            bad.quantity = Some(q);
            assert_eq!(
                refused(recipe(), vec![bad]),
                "ingredient_quantity_must_be_non_negative",
                "{q}"
            );
        }
        for months in [vec![0], vec![13], vec![1, -1]] {
            let mut bad = ingredient("Sel");
            bad.seasonal_months = Some(months);
            assert_eq!(refused(recipe(), vec![bad]), "invalid_seasonal_month");
        }
    }
}
