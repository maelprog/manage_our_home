//! A member's food allergies and dislikes, and a household's kitchen
//! equipment (#817). Schema and access rules: migration 0057.
//!
//! An allergy is health data and belongs to the member: `/me/allergies`
//! reads and writes the caller's own, under `user_scoped_tx` (the policies
//! of 0057 key on `app.user_id`). For each family they belong to, the member
//! chooses who learns of each allergy — `public`, `anonymous` or `private`,
//! `private` when they did not choose. What the other members are shown
//! comes from `GET /groups/{id}/allergies`, read at each request through the
//! `SECURITY DEFINER` functions of 0057, the only path across members.
//!
//! Nothing here leaves a trace of an allergy: no `audit_log` row, no log
//! line, no notification. A database error is logged with its detail
//! (`error::AppError::Sqlx`), which would carry the tag, so every value is
//! validated before it is written and the writes raise no constraint error
//! (`ON CONFLICT DO NOTHING`, the allergy locked before its level is set).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use manage_our_home_shared::validation::dietary::{
    validate_equipment, validate_tag, DietaryError, ShareLevel,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, user_scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::groups::require_role;
use crate::recipes::taxonomy::{taxonomy, Taxonomy};
use crate::AppState;

impl From<DietaryError> for AppError {
    fn from(error: DietaryError) -> Self {
        AppError::BadRequest(error.code().into())
    }
}

/// A tag the member may record: well-formed (`validate_tag`) and a category
/// of the taxonomy, so that it means something to the recipes it will be
/// matched against (#818). Returns the error code otherwise.
pub(crate) fn checked_tag(taxonomy: &Taxonomy, tag: &str) -> Result<String, &'static str> {
    let tag = validate_tag(tag).map_err(|e| e.code())?;
    match taxonomy.tag(tag) {
        Some(_) => Ok(tag.to_string()),
        None => Err("unknown_tag"),
    }
}

/// What a tag is called (#819): its first French name in the taxonomy, or
/// the tag itself when the taxonomy no longer knows it.
pub(crate) fn tag_label(taxonomy: &Taxonomy, tag: &str) -> String {
    taxonomy
        .tag(tag)
        .map_or_else(|| tag.to_string(), |id| taxonomy.label(id).to_string())
}

/// One family the member belongs to.
pub(crate) struct Membership {
    pub group_id: Uuid,
    pub group_name: String,
}

/// A level the member set, for one allergy in one family.
pub(crate) struct SharingRow {
    pub tag: String,
    pub group_id: Uuid,
    pub share: String,
}

/// The level of one allergy in one family.
#[derive(Debug, PartialEq, Serialize, utoipa::ToSchema)]
pub struct AllergySharing {
    pub group_id: Uuid,
    pub group_name: String,
    /// `public`, `anonymous` or `private`.
    pub share: String,
}

/// One of the caller's allergies, with its level in each of their families.
#[derive(Debug, PartialEq, Serialize, utoipa::ToSchema)]
pub struct AllergyEntry {
    pub tag: String,
    /// The tag's French name (#819), the tag itself if the taxonomy does
    /// not know it.
    pub label: String,
    pub created_at: DateTime<Utc>,
    /// One entry per family the caller belongs to, in the order given.
    pub sharing: Vec<AllergySharing>,
}

/// Puts each allergy with its level in every family the member belongs to:
/// the level they set, `private` where they set none. A level set for a
/// family they no longer belong to is not listed (the schema deletes those
/// rows; this does not rely on it).
pub(crate) fn allergy_entries(
    allergies: Vec<(String, DateTime<Utc>)>,
    memberships: &[Membership],
    sharing: &[SharingRow],
) -> Vec<AllergyEntry> {
    allergies
        .into_iter()
        .map(|(tag, created_at)| {
            let sharing = memberships
                .iter()
                .map(|m| AllergySharing {
                    group_id: m.group_id,
                    group_name: m.group_name.clone(),
                    share: sharing
                        .iter()
                        .find(|s| s.tag == tag && s.group_id == m.group_id)
                        .map_or(ShareLevel::default().as_str(), |s| s.share.as_str())
                        .to_string(),
                })
                .collect();
            AllergyEntry {
                label: tag_label(taxonomy(), &tag),
                tag,
                created_at,
                sharing,
            }
        })
        .collect()
}

fn known_tag(tag: &str) -> AppResult<String> {
    checked_tag(taxonomy(), tag).map_err(|code| AppError::BadRequest(code.into()))
}

/// `POST` / `DELETE` body naming one tag.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct TagRequest {
    /// An Open Food Facts category, e.g. `en:peanuts`.
    pub tag: String,
}

/// `GET /me/allergies`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct AllergyList {
    pub allergies: Vec<AllergyEntry>,
}

/// One of the caller's dislikes.
#[derive(Serialize, utoipa::ToSchema)]
pub struct DislikeEntry {
    pub tag: String,
    /// The tag's French name (#819), as for an allergy.
    pub label: String,
    pub created_at: DateTime<Utc>,
}

impl DislikeEntry {
    fn new(tag: String, created_at: DateTime<Utc>) -> Self {
        DislikeEntry {
            label: tag_label(taxonomy(), &tag),
            tag,
            created_at,
        }
    }
}

/// `GET /me/dislikes`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct DislikeList {
    pub dislikes: Vec<DislikeEntry>,
}

/// `PUT /me/allergies/sharing` body.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct SharingRequest {
    pub tag: String,
    pub group_id: Uuid,
    /// `public`, `anonymous` or `private`.
    pub share: String,
}

/// `PUT /me/allergies/sharing` answer: the level now in force.
#[derive(Serialize, utoipa::ToSchema)]
pub struct SharingResponse {
    pub tag: String,
    pub group_id: Uuid,
    pub share: String,
}

/// An allergy a member shares publicly with the family.
#[derive(Serialize, utoipa::ToSchema)]
pub struct PublicAllergy {
    pub user_id: Uuid,
    pub display_name: String,
    pub tag: String,
}

/// `GET /groups/{id}/allergies`: what the family may know of its members'
/// allergies, and nothing else.
#[derive(Serialize, utoipa::ToSchema)]
pub struct GroupAllergies {
    /// The allergies shared publicly, with their holder.
    pub public: Vec<PublicAllergy>,
    /// Whether at least one member shares an allergy anonymously here. Who,
    /// and which, are not said.
    pub anonymous: bool,
}

/// `GET` / `PUT /groups/{id}/equipment`.
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct Equipment {
    /// Codes of `validation::dietary::EQUIPMENT`, sorted.
    pub codes: Vec<String>,
}

/// The caller's allergies, each with its level in each of their families.
#[utoipa::path(
    get,
    path = "/me/allergies",
    tag = "account",
    responses(
        (status = 200, description = "The caller's allergies", body = AllergyList),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_allergies(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let allergies = sqlx::query!(
        "SELECT tag, created_at FROM user_allergies WHERE user_id = $1 ORDER BY created_at, tag",
        auth.user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let memberships = sqlx::query_as!(
        Membership,
        r#"SELECT g.id AS group_id, g.name AS group_name
           FROM group_members gm JOIN groups g ON g.id = gm.group_id
           WHERE gm.user_id = $1
           ORDER BY g.name, g.id"#,
        auth.user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let sharing = sqlx::query_as!(
        SharingRow,
        "SELECT tag, group_id, share FROM allergy_sharing WHERE user_id = $1",
        auth.user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let allergies = allergies
        .into_iter()
        .map(|a| (a.tag, a.created_at))
        .collect();
    Ok(Json(AllergyList {
        allergies: allergy_entries(allergies, &memberships, &sharing),
    }))
}

/// Records an allergy, `private` in every family until the caller says
/// otherwise.
#[utoipa::path(
    post,
    path = "/me/allergies",
    tag = "account",
    request_body = TagRequest,
    responses(
        (status = 201, description = "Allergy recorded", body = AllergyEntry),
        (status = 400, description = "`tag_required`, `tag_too_long`, `invalid_tag` or `unknown_tag`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 409, description = "`already_listed`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn add_allergy(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TagRequest>,
) -> AppResult<impl IntoResponse> {
    let tag = known_tag(&body.tag)?;
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let created_at = sqlx::query_scalar!(
        r#"INSERT INTO user_allergies (user_id, tag) VALUES ($1, $2)
           ON CONFLICT (user_id, tag) DO NOTHING
           RETURNING created_at"#,
        auth.user_id,
        tag
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Conflict("already_listed".into()))?;
    let memberships = sqlx::query_as!(
        Membership,
        r#"SELECT g.id AS group_id, g.name AS group_name
           FROM group_members gm JOIN groups g ON g.id = gm.group_id
           WHERE gm.user_id = $1
           ORDER BY g.name, g.id"#,
        auth.user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let entry = allergy_entries(vec![(tag, created_at)], &memberships, &[])
        .pop()
        .expect("one allergy in, one entry out");
    Ok((StatusCode::CREATED, Json(entry)))
}

/// Removes one of the caller's allergies, and its levels with it. The tag
/// is in the body, not in the path: a path ends up in access logs.
#[utoipa::path(
    delete,
    path = "/me/allergies",
    tag = "account",
    request_body = TagRequest,
    responses(
        (status = 204, description = "Allergy removed"),
        (status = 400, description = "`tag_required`, `tag_too_long` or `invalid_tag`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: not among the caller's allergies", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn remove_allergy(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TagRequest>,
) -> AppResult<impl IntoResponse> {
    let tag = validate_tag(&body.tag)?;
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let removed = sqlx::query!(
        "DELETE FROM user_allergies WHERE user_id = $1 AND tag = $2",
        auth.user_id,
        tag
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if removed == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sets who, in one of the caller's families, learns of one of their
/// allergies. In force from the next read: nothing is copied.
#[utoipa::path(
    put,
    path = "/me/allergies/sharing",
    tag = "account",
    request_body = SharingRequest,
    responses(
        (status = 200, description = "Level set", body = SharingResponse),
        (status = 400, description = "`tag_required`, `tag_too_long`, `invalid_tag` or `invalid_share`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: not among the caller's allergies, or not one of their families", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn set_allergy_sharing(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SharingRequest>,
) -> AppResult<impl IntoResponse> {
    let tag = validate_tag(&body.tag)?;
    let share = ShareLevel::parse(&body.share)?;
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    // The allergy is locked, so that it does not go away before its level is
    // written: the foreign-key error that would follow names the tag, and
    // errors are logged. The membership cannot be locked — the caller only
    // reads it (`group_members_self_read`, 0014) — and an error on its key
    // names the family and the member, not the allergy.
    let allergy = sqlx::query_scalar!(
        "SELECT tag FROM user_allergies WHERE user_id = $1 AND tag = $2 FOR UPDATE",
        auth.user_id,
        tag
    )
    .fetch_optional(&mut *tx)
    .await?;
    let member = sqlx::query_scalar!(
        "SELECT group_id FROM group_members WHERE group_id = $1 AND user_id = $2",
        body.group_id,
        auth.user_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if allergy.is_none() || member.is_none() {
        return Err(AppError::NotFound);
    }
    sqlx::query!(
        r#"INSERT INTO allergy_sharing (user_id, tag, group_id, share)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (user_id, tag, group_id) DO UPDATE SET share = EXCLUDED.share"#,
        auth.user_id,
        tag,
        body.group_id,
        share.as_str()
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(SharingResponse {
        tag: tag.to_string(),
        group_id: body.group_id,
        share: share.as_str().to_string(),
    }))
}

/// The caller's dislikes. Never shared.
#[utoipa::path(
    get,
    path = "/me/dislikes",
    tag = "account",
    responses(
        (status = 200, description = "The caller's dislikes", body = DislikeList),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_dislikes(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let dislikes = sqlx::query!(
        "SELECT tag, created_at FROM user_dislikes WHERE user_id = $1 ORDER BY created_at, tag",
        auth.user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let dislikes = dislikes
        .into_iter()
        .map(|d| DislikeEntry::new(d.tag, d.created_at))
        .collect();
    Ok(Json(DislikeList { dislikes }))
}

/// Records a dislike.
#[utoipa::path(
    post,
    path = "/me/dislikes",
    tag = "account",
    request_body = TagRequest,
    responses(
        (status = 201, description = "Dislike recorded", body = DislikeEntry),
        (status = 400, description = "`tag_required`, `tag_too_long`, `invalid_tag` or `unknown_tag`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 409, description = "`already_listed`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn add_dislike(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TagRequest>,
) -> AppResult<impl IntoResponse> {
    let tag = known_tag(&body.tag)?;
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let entry = sqlx::query!(
        r#"INSERT INTO user_dislikes (user_id, tag) VALUES ($1, $2)
           ON CONFLICT (user_id, tag) DO NOTHING
           RETURNING tag, created_at"#,
        auth.user_id,
        tag
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Conflict("already_listed".into()))?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(DislikeEntry::new(entry.tag, entry.created_at)),
    ))
}

/// Removes a dislike. The tag is in the body, like the allergies'.
#[utoipa::path(
    delete,
    path = "/me/dislikes",
    tag = "account",
    request_body = TagRequest,
    responses(
        (status = 204, description = "Dislike removed"),
        (status = 400, description = "`tag_required`, `tag_too_long` or `invalid_tag`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: not among the caller's dislikes", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn remove_dislike(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TagRequest>,
) -> AppResult<impl IntoResponse> {
    let tag = validate_tag(&body.tag)?;
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let removed = sqlx::query!(
        "DELETE FROM user_dislikes WHERE user_id = $1 AND tag = $2",
        auth.user_id,
        tag
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if removed == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Erases every allergy, every level and every dislike of the caller in one
/// call: withdrawing consent is as simple as giving it (RGPD art. 7(3)).
#[utoipa::path(
    delete,
    path = "/me/dietary-constraints",
    tag = "account",
    responses(
        (status = 204, description = "Everything erased"),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn erase_constraints(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    // The levels go with their allergy (ON DELETE CASCADE); named anyway,
    // so that this does not rest on the key alone.
    sqlx::query!(
        "DELETE FROM allergy_sharing WHERE user_id = $1",
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM user_allergies WHERE user_id = $1",
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM user_dislikes WHERE user_id = $1", auth.user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What the family may know of its members' allergies: those shared
/// publicly, with their holder, and whether any is shared anonymously.
/// Read at each request through the functions of 0057.
#[utoipa::path(
    get,
    path = "/groups/{id}/allergies",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    responses(
        (status = 200, description = "The family's shared allergies", body = GroupAllergies),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn group_allergies(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let public = sqlx::query_as!(
        PublicAllergy,
        r#"SELECT user_id AS "user_id!", display_name AS "display_name!", tag AS "tag!"
           FROM group_public_allergies($1)"#,
        group_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let anonymous = sqlx::query_scalar!(
        r#"SELECT group_has_anonymous_allergy($1) AS "anonymous!""#,
        group_id
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(GroupAllergies { public, anonymous }))
}

/// The household's kitchen equipment.
#[utoipa::path(
    get,
    path = "/groups/{id}/equipment",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    responses(
        (status = 200, description = "The household's equipment", body = Equipment),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn get_equipment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let codes = sqlx::query_scalar!(
        "SELECT code FROM group_equipment WHERE group_id = $1 ORDER BY code",
        group_id
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(Equipment { codes }))
}

/// The longest query `POST /taxonomy/search` reads, in bytes: the longest
/// tag's bound, which no French name of the taxonomy exceeds by much.
pub const SEARCH_QUERY_MAX_LEN: usize = 128;

/// The most categories `POST /taxonomy/search` answers.
pub const SEARCH_LIMIT: usize = 20;

/// `POST /taxonomy/search` body.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct TagSearchRequest {
    /// A food's name, in French: « kiwi », « coriandre ».
    pub query: String,
}

/// A category of the taxonomy and its French name.
#[derive(Serialize, utoipa::ToSchema)]
pub struct TagMatch {
    pub tag: String,
    pub label: String,
}

/// `POST /taxonomy/search` answer.
#[derive(Serialize, utoipa::ToSchema)]
pub struct TagSearchResults {
    /// At most `SEARCH_LIMIT`, best match first (`Taxonomy::search`).
    pub results: Vec<TagMatch>,
}

/// The categories of the taxonomy whose French names match a query: how
/// the account page offers an allergy or a dislike beyond the fourteen
/// INCO allergens (#819). A `POST` with the query in the body, like the
/// tags of `/me/allergies`: the name of a food a member looks up to say
/// they are allergic to it stays out of the access logs. Reads nothing
/// stored, writes nothing.
#[utoipa::path(
    post,
    path = "/taxonomy/search",
    tag = "recipes",
    request_body = TagSearchRequest,
    responses(
        (status = 200, description = "The matching categories, best first", body = TagSearchResults),
        (status = 400, description = "`query_too_long`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn search_tags(
    _auth: AuthUser,
    Json(body): Json<TagSearchRequest>,
) -> AppResult<impl IntoResponse> {
    if body.query.len() > SEARCH_QUERY_MAX_LEN {
        return Err(AppError::BadRequest("query_too_long".into()));
    }
    let taxonomy = taxonomy();
    let results = taxonomy
        .search(&body.query, SEARCH_LIMIT)
        .into_iter()
        .map(|tag| TagMatch {
            tag: taxonomy.id(tag).to_string(),
            label: taxonomy.label(tag).to_string(),
        })
        .collect();
    Ok(Json(TagSearchResults { results }))
}

/// Replaces the household's equipment with the list given. Owner and
/// admins only, like the family's colour and calendar (arbitrated
/// 2026-11-16, #819): until the per-family access policy lets the owner
/// set the bar, a standard member reads the list and does not change it.
#[utoipa::path(
    put,
    path = "/groups/{id}/equipment",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
    ),
    request_body = Equipment,
    responses(
        (status = 200, description = "The household's equipment, as stored", body = Equipment),
        (status = 400, description = "`unknown_equipment` or `duplicate_equipment`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn put_equipment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<Equipment>,
) -> AppResult<impl IntoResponse> {
    let codes = validate_equipment(&body.codes)?;
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let role = require_role(&mut tx, group_id, auth.user_id).await?;
    if role != "owner" && role != "admin" {
        return Err(AppError::Forbidden);
    }
    sqlx::query!("DELETE FROM group_equipment WHERE group_id = $1", group_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "INSERT INTO group_equipment (group_id, code) SELECT $1, unnest($2::text[]) ON CONFLICT DO NOTHING",
        group_id,
        &codes
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(Equipment { codes }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use manage_our_home_shared::validation::dietary::INCO_ALLERGENS;

    fn small_taxonomy() -> Taxonomy {
        Taxonomy::parse("en:nuts\t\tFruits à coque\nen:peanuts\ten:nuts\tCacahuètes\n")
    }

    #[test]
    fn a_known_category_is_a_tag() {
        let t = small_taxonomy();
        assert_eq!(checked_tag(&t, "en:peanuts"), Ok("en:peanuts".to_string()));
        assert_eq!(checked_tag(&t, " en:nuts "), Ok("en:nuts".to_string()));
    }

    #[test]
    fn a_well_formed_tag_outside_the_taxonomy_is_unknown() {
        let t = small_taxonomy();
        assert_eq!(checked_tag(&t, "en:unicorns"), Err("unknown_tag"));
    }

    #[test]
    fn a_malformed_tag_is_refused_before_the_lookup() {
        let t = small_taxonomy();
        assert_eq!(checked_tag(&t, ""), Err("tag_required"));
        assert_eq!(checked_tag(&t, "peanuts"), Err("invalid_tag"));
        assert_eq!(checked_tag(&t, "EN:peanuts"), Err("invalid_tag"));
    }

    #[test]
    fn every_inco_allergen_the_account_page_offers_is_a_known_category() {
        for (tag, label) in INCO_ALLERGENS {
            assert_eq!(checked_tag(taxonomy(), tag), Ok(tag.to_string()), "{label}");
        }
    }

    #[test]
    fn a_tag_is_labelled_by_its_first_french_name_or_by_itself() {
        assert_eq!(tag_label(taxonomy(), "en:peanuts"), "Cacahuètes");
        assert_eq!(tag_label(taxonomy(), "en:unicorns"), "en:unicorns");
    }

    #[test]
    fn the_embedded_taxonomy_knows_the_common_allergens_categories() {
        for tag in [
            "en:peanuts",
            "en:nuts",
            "en:eggs",
            "en:milks",
            "en:crustaceans",
        ] {
            assert_eq!(checked_tag(taxonomy(), tag), Ok(tag.to_string()), "{tag}");
        }
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 12, 0, 0).unwrap()
    }

    fn families() -> Vec<Membership> {
        vec![
            Membership {
                group_id: id(1),
                group_name: "Maison".into(),
            },
            Membership {
                group_id: id(2),
                group_name: "Grands-parents".into(),
            },
        ]
    }

    fn row(tag: &str, group: u128, share: &str) -> SharingRow {
        SharingRow {
            tag: tag.into(),
            group_id: id(group),
            share: share.into(),
        }
    }

    fn levels(entry: &AllergyEntry) -> Vec<(Uuid, &str)> {
        entry
            .sharing
            .iter()
            .map(|s| (s.group_id, s.share.as_str()))
            .collect()
    }

    #[test]
    fn an_allergy_without_a_level_is_private_in_every_family() {
        let entries = allergy_entries(vec![("en:peanuts".into(), at(1))], &families(), &[]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tag, "en:peanuts");
        assert_eq!(entries[0].created_at, at(1));
        assert_eq!(
            levels(&entries[0]),
            vec![(id(1), "private"), (id(2), "private")]
        );
        assert_eq!(entries[0].sharing[1].group_name, "Grands-parents");
    }

    #[test]
    fn each_allergy_carries_the_levels_set_for_it_and_no_other() {
        let entries = allergy_entries(
            vec![("en:peanuts".into(), at(1)), ("en:eggs".into(), at(2))],
            &families(),
            &[
                row("en:peanuts", 1, "public"),
                row("en:eggs", 2, "anonymous"),
            ],
        );
        assert_eq!(entries[0].tag, "en:peanuts");
        assert_eq!(
            levels(&entries[0]),
            vec![(id(1), "public"), (id(2), "private")]
        );
        assert_eq!(entries[1].tag, "en:eggs");
        assert_eq!(
            levels(&entries[1]),
            vec![(id(1), "private"), (id(2), "anonymous")]
        );
    }

    #[test]
    fn a_level_for_a_family_left_is_not_listed() {
        let entries = allergy_entries(
            vec![("en:peanuts".into(), at(1))],
            &families(),
            &[row("en:peanuts", 3, "public")],
        );
        assert_eq!(
            levels(&entries[0]),
            vec![(id(1), "private"), (id(2), "private")]
        );
    }

    #[test]
    fn without_a_family_an_allergy_has_no_level_to_show() {
        let entries = allergy_entries(vec![("en:peanuts".into(), at(1))], &[], &[]);
        assert_eq!(entries[0].sharing, Vec::new());
    }

    #[test]
    fn no_allergy_no_entry() {
        assert_eq!(allergy_entries(Vec::new(), &families(), &[]), Vec::new());
    }
}
