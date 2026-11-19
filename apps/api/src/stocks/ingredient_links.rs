//! A family's manual links from an article's name to an ingredient
//! category (#903): « mon p'tit plaisir » → `en:fermented-creams`.
//!
//! An article typed by hand, or scanned without a useful Open Food Facts
//! category, whose name the taxonomy does not know, has no category: it
//! covers no recipe by category (`recipes::suggestions::stock_entry`). A
//! link gives it one. It is keyed on the article's name as [`link_key`]
//! reads it, so one link holds for every line of that name, present and to
//! come, and deleting an article leaves it in place.
//!
//! Any member may set or replace a link, as anyone may adjust a quantity
//! (#39); deleting one is for its creator, or an admin or the owner. A
//! family holds at most [`LINKS_MAX`] links. The candidates for the
//! category are those `POST /taxonomy/search` answers (#819): the French
//! names, by prefix, accents folded.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::recipes::taxonomy::{normalize, taxonomy, TagId, Taxonomy};
use crate::stocks::can_modify;
use crate::AppState;

/// The most links a family holds. A link is only needed for an article no
/// category recognizes, and a household's stock counts on the order of a
/// hundred distinct articles: 200 leaves room for every one of them and
/// bounds what one family stores. Replacing a link never counts.
pub const LINKS_MAX: i64 = 200;

/// The longest article name a link takes, in characters (the column's
/// `CHECK`, migration 0064).
pub const LINK_NAME_MAX: usize = 200;

/// Advisory lock class of the per-family bound (`pg_advisory_xact_lock`'s
/// first key): two links created at once take turns, so the count cannot
/// let both through past [`LINKS_MAX`].
const LINKS_LOCK: i32 = 903;

/// The key a link is stored under, and the one every stock line of the
/// family is looked up by: the name as the taxonomy reads one, accents
/// kept ([`normalize`]: case, `œ`/`æ`, plurals, `de`/`du`/`des`, a
/// leading article, every non-alphanumeric character a word break).
/// « Mon p’tit plaisir » and « mon P'tit  plaisirs » share it; « Pâtés »
/// and « Pâtes » do not. Empty when the name holds no word.
pub(crate) fn link_key(name: &str) -> String {
    normalize(name, false)
}

/// A link's name and category, checked: the name trimmed, its key, and
/// the category's tag. Codes: `name_required` (no word in the name),
/// `name_too_long` (over [`LINK_NAME_MAX`] characters), `unknown_tag` (a
/// category the taxonomy does not have).
pub(crate) fn validate_link<'a>(
    taxonomy: &Taxonomy,
    name: &'a str,
    tag: &str,
) -> Result<(&'a str, String, TagId), &'static str> {
    let name = name.trim();
    let key = link_key(name);
    if key.is_empty() {
        return Err("name_required");
    }
    if name.chars().count() > LINK_NAME_MAX {
        return Err("name_too_long");
    }
    let tag = taxonomy.tag(tag).ok_or("unknown_tag")?;
    Ok((name, key, tag))
}

/// The family's links, key → category id, for the stock loaders
/// (`suggestions::tagged_stock`).
pub(crate) async fn family_links(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<HashMap<String, String>> {
    Ok(sqlx::query!(
        "SELECT name_key, tag FROM ingredient_links WHERE group_id = $1",
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.name_key, r.tag))
    .collect())
}

/// `PUT /groups/{id}/ingredient-links/{name}` body.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct PutIngredientLinkRequest {
    /// An Open Food Facts category id the taxonomy has (`en:fermented-creams`),
    /// as `POST /taxonomy/search` answers it.
    pub tag: String,
}

/// A link as the API shows it.
#[derive(Serialize, utoipa::ToSchema)]
pub struct IngredientLink {
    /// The article's name, as the last link set for it wrote it.
    pub name: String,
    /// The category id.
    pub tag: String,
    /// The category's French name; empty when the taxonomy no longer has
    /// it (the link then gives no category).
    pub label: String,
    /// Who set the link last: the one who, with admins and the owner, may
    /// delete it.
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
}

/// `GET /groups/{id}/ingredient-links` answer.
#[derive(Serialize, utoipa::ToSchema)]
pub struct IngredientLinkList {
    /// By name.
    pub links: Vec<IngredientLink>,
}

struct LinkRow {
    name: String,
    tag: String,
    created_by: Uuid,
    updated_at: DateTime<Utc>,
}

impl From<LinkRow> for IngredientLink {
    fn from(r: LinkRow) -> Self {
        let taxonomy = taxonomy();
        IngredientLink {
            label: taxonomy
                .tag(&r.tag)
                .map(|t| taxonomy.label(t).to_string())
                .unwrap_or_default(),
            name: r.name,
            tag: r.tag,
            created_by: r.created_by,
            updated_at: r.updated_at,
        }
    }
}

#[utoipa::path(
    get,
    path = "/groups/{id}/ingredient-links",
    tag = "stocks",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    responses(
        (status = 200, description = "The family's links, by name", body = IngredientLinkList),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_ingredient_links(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let links = sqlx::query_as!(
        LinkRow,
        r#"SELECT name, tag, created_by, updated_at FROM ingredient_links
           WHERE group_id = $1 ORDER BY name, name_key"#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(IngredientLink::from)
    .collect();
    tx.commit().await?;
    Ok(Json(IngredientLinkList { links }))
}

/// Links every stock line of the family named `name` (as [`link_key`]
/// reads it) to a category, or replaces the category it had. Open to any
/// member; whoever sets it becomes its creator.
#[utoipa::path(
    put,
    path = "/groups/{id}/ingredient-links/{name}",
    tag = "stocks",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("name" = String, Path, description = "The article's name, percent-encoded"),
    ),
    request_body = PutIngredientLinkRequest,
    responses(
        (status = 200, description = "The link, as stored", body = IngredientLink),
        (status = 400, description = "`name_required`, `name_too_long` or `unknown_tag`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 409, description = "`link_limit_reached`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn put_ingredient_link(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, name)): Path<(Uuid, String)>,
    Json(body): Json<PutIngredientLinkRequest>,
) -> AppResult<impl IntoResponse> {
    let taxonomy = taxonomy();
    let (name, key, tag) = validate_link(taxonomy, &name, &body.tag)
        .map_err(|code| AppError::BadRequest(code.into()))?;
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    sqlx::query!(
        "SELECT pg_advisory_xact_lock($1, hashtext($2::text))",
        LINKS_LOCK,
        group_id.to_string(),
    )
    .execute(&mut *tx)
    .await?;
    let held = sqlx::query!(
        r#"SELECT count(*) AS "count!", coalesce(bool_or(name_key = $2), false) AS "replaces!"
           FROM ingredient_links WHERE group_id = $1"#,
        group_id,
        key,
    )
    .fetch_one(&mut *tx)
    .await?;
    if !held.replaces && held.count >= LINKS_MAX {
        return Err(AppError::Conflict("link_limit_reached".into()));
    }

    let link = sqlx::query_as!(
        LinkRow,
        r#"INSERT INTO ingredient_links (group_id, name_key, name, tag, created_by)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (group_id, name_key) DO UPDATE
           SET name = EXCLUDED.name, tag = EXCLUDED.tag,
               created_by = EXCLUDED.created_by, updated_at = now()
           RETURNING name, tag, created_by, updated_at"#,
        group_id,
        key,
        name,
        taxonomy.id(tag),
        auth.user_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(IngredientLink::from(link)))
}

/// Deletes the link of `name` (as [`link_key`] reads it): its creator, an
/// admin or the owner. The stock lines of that name keep their rows and
/// fall back to their own categories.
#[utoipa::path(
    delete,
    path = "/groups/{id}/ingredient-links/{name}",
    tag = "stocks",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        ("name" = String, Path, description = "The article's name, percent-encoded"),
    ),
    responses(
        (status = 204, description = "Link deleted"),
        (status = 404, description = "`not_found`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn delete_ingredient_link(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, name)): Path<(Uuid, String)>,
) -> AppResult<impl IntoResponse> {
    let key = link_key(&name);
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;
    let created_by = sqlx::query_scalar!(
        "SELECT created_by FROM ingredient_links WHERE group_id = $1 AND name_key = $2",
        group_id,
        key,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    // The issue's bar, creator or admin/owner: not the family's
    // `stocks_edit` threshold, which is about the articles themselves.
    if !can_modify(&role, "admin", created_by == auth.user_id) {
        return Err(AppError::Forbidden);
    }
    sqlx::query!(
        "DELETE FROM ingredient_links WHERE group_id = $1 AND name_key = $2",
        group_id,
        key,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_folds_case_spaces_apostrophes_and_plurals() {
        let key = link_key("Mon p'tit plaisir");
        assert_eq!(key, "mon p tit plaisir");
        for same in [
            "  mon P’tit   plaisir ",
            "Mon p'tit plaisirs",
            "MON P-TIT PLAISIR",
        ] {
            assert_eq!(link_key(same), key, "{same:?}");
        }
    }

    #[test]
    fn the_key_keeps_accents_and_words() {
        // Accents kept: « pâtés » is not « pâtes ».
        assert_ne!(link_key("Pâtés"), link_key("Pâtes"));
        assert_eq!(link_key("Crème"), "crème");
        assert_ne!(link_key("Crème"), link_key("Creme"));
        // A different word is a different article.
        assert_ne!(link_key("Mon p'tit plaisir"), link_key("Mon gros plaisir"));
        // Ligatures and a leading article.
        assert_eq!(link_key("L'Œuf du jour"), link_key("oeuf jour"));
    }

    #[test]
    fn the_key_of_a_name_without_a_word_is_empty() {
        assert_eq!(link_key(""), "");
        assert_eq!(link_key("  - ' "), "");
        assert_eq!(link_key("de"), "");
    }

    fn checked(name: &str, tag: &str) -> Result<(String, String, String), &'static str> {
        let t = taxonomy();
        validate_link(t, name, tag).map(|(n, k, tag)| (n.to_string(), k, t.id(tag).to_string()))
    }

    #[test]
    fn a_link_to_a_known_category_is_valid() {
        assert_eq!(
            checked("  Mon p'tit plaisir ", "en:fermented-creams"),
            Ok((
                "Mon p'tit plaisir".to_string(),
                "mon p tit plaisir".to_string(),
                "en:fermented-creams".to_string()
            ))
        );
    }

    #[test]
    fn a_category_the_taxonomy_lacks_is_refused() {
        assert_eq!(
            checked("Mon p'tit plaisir", "en:not-a-category"),
            Err("unknown_tag")
        );
        assert_eq!(checked("Mon p'tit plaisir", ""), Err("unknown_tag"));
        // The id as written: no case folding.
        assert_eq!(
            checked("Mon p'tit plaisir", "EN:FERMENTED-CREAMS"),
            Err("unknown_tag")
        );
    }

    #[test]
    fn a_name_without_a_word_or_too_long_is_refused() {
        assert_eq!(checked("   ", "en:fermented-creams"), Err("name_required"));
        assert_eq!(checked("de", "en:fermented-creams"), Err("name_required"));
        let long = "é".repeat(LINK_NAME_MAX);
        assert!(checked(&long, "en:fermented-creams").is_ok());
        let longer = "é".repeat(LINK_NAME_MAX + 1);
        assert_eq!(
            checked(&longer, "en:fermented-creams"),
            Err("name_too_long")
        );
    }
}
