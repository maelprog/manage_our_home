//! `GET /groups/:id/national-days?from&to` (#562): the public holidays, the
//! clock changes and Mother's and Father's Day of `[from, to]`, both civil
//! dates (`YYYY-MM-DD`) included. Computed on the fly by
//! `manage_our_home_shared::validation::national_days` — nothing stored,
//! nothing fetched — with the family's `alsace_moselle` setting (#561)
//! adding Good Friday and 26 December.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use manage_our_home_shared::dto::agenda::{NationalDayList, NationalDayResponse};
use manage_our_home_shared::validation::national_days::{
    national_days, validate_national_days_range,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::AppState;

#[derive(Deserialize)]
pub struct DateRangeQuery {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

/// Any member of the family; anyone else gets 404, as on `GET /groups/:id`.
/// A backwards range is 400 `to_before_from`, one wider than a leap year
/// 400 `range_too_wide` — checked before the database is touched.
pub async fn list_national_days(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(range): Query<DateRangeQuery>,
) -> AppResult<Json<NationalDayList>> {
    validate_national_days_range(range.from, range.to)
        .map_err(|code| AppError::BadRequest(code.into()))?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let alsace_moselle = sqlx::query_scalar!(
        r#"
        SELECT g.alsace_moselle
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
    tx.commit().await?;

    let days = national_days(range.from, range.to, alsace_moselle)
        .into_iter()
        .map(|n| NationalDayResponse {
            date: n.date,
            kind: n.kind.as_str().to_string(),
            label: n.label.to_string(),
        })
        .collect();
    Ok(Json(NationalDayList { days }))
}
