//! `GET /groups/:id/national-days?from&to` (#562): the public holidays, the
//! clock changes and Mother's and Father's Day of `[from, to]`, both civil
//! dates (`YYYY-MM-DD`) included. Computed on the fly by
//! `manage_our_home_shared::validation::national_days` — nothing stored,
//! nothing fetched — with the family's `alsace_moselle` setting (#561)
//! adding Good Friday and 26 December.
//!
//! With a `school_zone` (#561), the school holidays of that zone that
//! share a day with the range are added (#563), `kind = school_holiday`,
//! read from the cache `jobs::school_holidays` fills: a period, `date` its
//! first day and `end` its last. No zone, no school holidays.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use manage_our_home_shared::dto::agenda::{NationalDayList, NationalDayResponse};
use manage_our_home_shared::validation::groups::SchoolZone;
use manage_our_home_shared::validation::national_days::{
    national_days, validate_national_days_range,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::agenda::school_holidays::periods_overlapping;
use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::AppState;

/// The `kind` of a school holiday period (#563).
pub const SCHOOL_HOLIDAY: &str = "school_holiday";

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DateRangeQuery {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

/// Any member of the family; anyone else gets 404, as on `GET /groups/:id`.
/// A backwards range is 400 `to_before_from`, one wider than a leap year
/// 400 `range_too_wide` — checked before the database is touched.
#[utoipa::path(
    get,
    path = "/groups/{id}/national-days",
    tag = "agenda",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        DateRangeQuery,
    ),
    responses(
        (status = 200, description = "Public holidays, school holidays of the zone and national days between `from` and `to`", body = NationalDayList),
        (status = 400, description = "Invalid range (validation code)", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: no such group, or the caller is not a member", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_national_days(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(range): Query<DateRangeQuery>,
) -> AppResult<Json<NationalDayList>> {
    validate_national_days_range(range.from, range.to)
        .map_err(|code| AppError::BadRequest(code.into()))?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let settings = sqlx::query!(
        r#"
        SELECT g.alsace_moselle, g.school_zone
        FROM groups g
        JOIN group_members gm ON gm.group_id = g.id AND gm.user_id = $2
        WHERE g.id = $1
        "#,
        group_id,
        auth.user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    // The `CHECK` of 0046 allows nothing else: a zone that does not parse
    // shows no school holidays rather than failing the national days.
    let holidays = match settings.school_zone.as_deref().and_then(SchoolZone::parse) {
        Some(zone) => periods_overlapping(&mut tx, zone, range.from, range.to).await?,
        None => Vec::new(),
    };
    tx.commit().await?;

    let mut days: Vec<NationalDayResponse> =
        national_days(range.from, range.to, settings.alsace_moselle)
            .into_iter()
            .map(|n| NationalDayResponse {
                date: n.date,
                kind: n.kind.as_str().to_string(),
                label: n.label.to_string(),
                end: None,
            })
            .chain(holidays.into_iter().map(|h| NationalDayResponse {
                date: h.starts_on,
                kind: SCHOOL_HOLIDAY.to_string(),
                label: h.label,
                end: Some(h.ends_on),
            }))
            .collect();
    // Stable: on one date, the national days keep their order and come
    // before a period starting that day.
    days.sort_by_key(|d| d.date);
    Ok(Json(NationalDayList { days }))
}
