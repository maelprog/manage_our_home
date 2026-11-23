//! Reporting a publication of the shared library and its moderation by the
//! superadmin (#908), over the schema of
//! `migrations/0068_recipe_library_moderation.sql`.
//!
//! - `POST /library/recipes/{id}/report`: any signed-in user reports a
//!   publication, a reason from a closed list ([`ReportReason`]) and a
//!   comment of [`COMMENT_MAX_CHARS`] characters at most, required with
//!   `other`. Once per user and
//!   per publication: a second report is answered as the first, and changes
//!   nothing. Counted at `library::REPORT_RATE` per user.
//! - `GET /admin/library/reports`: the superadmin lists the reports, open
//!   ones first, each with its reason, comment and state, and the name of
//!   the publication while it exists. Never who reported it: the API shows
//!   the reporter to no one, the publisher least of all.
//! - `POST /admin/library/reports/{id}/dismiss`: the superadmin closes a
//!   report, the publication left as it is.
//! - `POST /admin/library/recipes/{id}/withdraw`: the superadmin withdraws a
//!   publication. It leaves the library (list, detail, copy: 404), its
//!   ingredients and publisher row with it; the families' copies stay. Its
//!   open reports are closed, and its source recipe, while it exists, is
//!   blocked: publishing it again is refused 409 `withdrawn_by_moderation`.
//! - `DELETE /admin/library/recipes/{id}/withdrawal`: the superadmin lifts
//!   that block.
//!
//! Every superadmin route runs on `AppState.admin_db` behind
//! `SuperAdminUser` (recent session included); each of its writes is traced in
//! `audit_log` in the same transaction (`audit::record`).

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit;
use crate::auth::session::{user_scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::recipes::library::LibraryThrottle;
use crate::user_admin::SuperAdminUser;
use crate::AppState;

/// Why a publication is reported: a closed list (user's decision of
/// 2026-11-20), the CHECK of `library_reports.reason` (0068).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportReason {
    Illegal,
    Copyright,
    Hateful,
    Spam,
    HealthHazard,
    Other,
}

impl ReportReason {
    const ALL: [ReportReason; 6] = [
        ReportReason::Illegal,
        ReportReason::Copyright,
        ReportReason::Hateful,
        ReportReason::Spam,
        ReportReason::HealthHazard,
        ReportReason::Other,
    ];

    /// The reason the client named, exactly; 400 `invalid_reason` otherwise.
    pub(crate) fn parse(raw: &str) -> AppResult<ReportReason> {
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == raw)
            .ok_or_else(|| AppError::BadRequest("invalid_reason".into()))
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReportReason::Illegal => "illegal",
            ReportReason::Copyright => "copyright",
            ReportReason::Hateful => "hateful",
            ReportReason::Spam => "spam",
            ReportReason::HealthHazard => "health_hazard",
            ReportReason::Other => "other",
        }
    }
}

/// The longest comment, in characters, once trimmed.
pub const COMMENT_MAX_CHARS: usize = 500;

/// A report's comment as the client sent it: trimmed, `None` when absent
/// or blank, refused past [`COMMENT_MAX_CHARS`] characters. `other` says
/// nothing by itself: its comment is required, 400 `comment_required`
/// when absent or blank (user's decision of 2026-11-20, #911).
pub(crate) fn report_comment(reason: ReportReason, raw: Option<&str>) -> AppResult<Option<String>> {
    let comment = raw.map(str::trim).unwrap_or("");
    if comment.is_empty() {
        return match reason {
            ReportReason::Other => Err(AppError::BadRequest("comment_required".into())),
            _ => Ok(None),
        };
    }
    if comment.chars().count() > COMMENT_MAX_CHARS {
        return Err(AppError::BadRequest("comment_too_long".into()));
    }
    Ok(Some(comment.to_string()))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReportRequest {
    /// `illegal`, `copyright`, `hateful`, `spam`, `health_hazard` or
    /// `other`.
    pub reason: String,
    /// At most 500 characters once trimmed; blank, none. Required with
    /// `other`.
    pub comment: Option<String>,
}

#[utoipa::path(
    post,
    path = "/library/recipes/{id}/report",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Publication id"),
    ),
    request_body = ReportRequest,
    responses(
        (status = 204, description = "Reported; a second report of the same publication by the same user is answered the same, and changes nothing"),
        (status = 400, description = "`invalid_reason` (absent, not one of the six, or a body that is not JSON), `comment_required` (`other` without a comment) or `comment_too_long`", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 404, description = "`not_found`: no such publication", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 429, description = "`too_many_attempts`: 10 reports per user per hour", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn report_library_recipe(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    body: Result<Json<ReportRequest>, JsonRejection>,
) -> AppResult<impl IntoResponse> {
    let Ok(Json(body)) = body else {
        return Err(AppError::BadRequest("invalid_reason".into()));
    };
    let reason = ReportReason::parse(&body.reason)?;
    let comment = report_comment(reason, body.comment.as_deref())?;
    // Before any transaction opens, as the library's other limits.
    LibraryThrottle::admit(&state.library_throttle.report, auth.user_id).await?;

    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    // Written only if the publication exists; the unique key (0068) makes a
    // second report of it by the same user a no-op.
    let written = sqlx::query!(
        r#"INSERT INTO library_reports (library_recipe_id, reporter_id, reason, comment)
           SELECT id, $2, $3, $4 FROM library_recipes WHERE id = $1
           ON CONFLICT (library_recipe_id, reporter_id) DO NOTHING"#,
        id,
        auth.user_id,
        reason.as_str(),
        comment,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if written == 0 {
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM library_recipes WHERE id = $1) AS "exists!""#,
            id
        )
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(AppError::NotFound);
        }
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A report, as the superadmin reads it: never its reporter.
#[derive(Serialize, utoipa::ToSchema)]
pub struct AdminLibraryReport {
    pub id: Uuid,
    pub library_recipe_id: Uuid,
    /// The publication's name; absent once it is gone.
    pub library_recipe_name: Option<String>,
    pub reason: String,
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Absent while open; `dismissed`, `withdrawn` (the publication
    /// withdrawn by the superadmin) or `lapsed` (the publication gone
    /// otherwise).
    pub resolution: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AdminLibraryReportList {
    pub reports: Vec<AdminLibraryReport>,
}

#[utoipa::path(
    get,
    path = "/admin/library/reports",
    tag = "admin",
    responses(
        (status = 200, description = "Every report kept: the open ones first, oldest first, then the closed ones, latest closed first", body = AdminLibraryReportList),
        crate::api_docs::AdminErrors,
    ),
    security(("session_cookie" = [])),
)]
/// A read with no side effect (`get_side_effects_flow`): it names no
/// account, so unlike the reads of `/admin/users` and `/admin/groups` it
/// writes nothing to `audit_log`. The decisions it leads to are traced.
pub async fn list_reports(
    State(state): State<AppState>,
    _actor: SuperAdminUser,
) -> AppResult<impl IntoResponse> {
    let reports = sqlx::query_as!(
        AdminLibraryReport,
        r#"SELECT r.id, r.library_recipe_id, l.name AS "library_recipe_name?", r.reason,
                  r.comment, r.created_at, r.resolution, r.closed_at
           FROM library_reports r
           LEFT JOIN library_recipes l ON l.id = r.library_recipe_id
           ORDER BY r.closed_at DESC NULLS FIRST, r.created_at, r.id"#
    )
    .fetch_all(&state.admin_db)
    .await?;
    Ok(Json(AdminLibraryReportList { reports }))
}

#[utoipa::path(
    post,
    path = "/admin/library/reports/{id}/dismiss",
    tag = "admin",
    params(
        ("id" = uuid::Uuid, Path, description = "Report id"),
    ),
    responses(
        (status = 204, description = "Report closed, the publication left as it is"),
        (status = 404, description = "`not_found`: no such report open", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::AdminErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn dismiss_report(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;
    let library_recipe_id = sqlx::query_scalar!(
        r#"UPDATE library_reports SET resolution = 'dismissed', closed_at = now()
           WHERE id = $1 AND resolution IS NULL
           RETURNING library_recipe_id"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.library.report.dismiss",
        "library_report",
        &id.to_string(),
        json!({ "library_recipe_id": library_recipe_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/admin/library/recipes/{id}/withdraw",
    tag = "admin",
    params(
        ("id" = uuid::Uuid, Path, description = "Publication id"),
    ),
    responses(
        (status = 204, description = "Withdrawn from the library, its open reports closed, its source recipe blocked from being published again"),
        (status = 404, description = "`not_found`: no such publication", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::AdminErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn withdraw_publication(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;
    // Read before the delete takes the publisher row with it.
    let publication = sqlx::query!(
        r#"SELECT l.id, p.source_recipe_id AS "source_recipe_id?"
           FROM library_recipes l
           LEFT JOIN library_recipe_publishers p ON p.library_recipe_id = l.id
           WHERE l.id = $1
           FOR UPDATE OF l"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    // Its ingredients and publisher row go with it (`ON DELETE CASCADE`,
    // 0066); the families' copies keep their recipe, their
    // `library_recipe_id` dropping to NULL.
    sqlx::query!("DELETE FROM library_recipes WHERE id = $1", publication.id)
        .execute(&mut *tx)
        .await?;
    // Its source recipe, unless deleted since: not published again until
    // the block is lifted.
    if let Some(source) = publication.source_recipe_id {
        sqlx::query!(
            r#"INSERT INTO library_withdrawn_sources (source_recipe_id, library_recipe_id)
               VALUES ($1, $2)
               ON CONFLICT (source_recipe_id) DO UPDATE
                   SET library_recipe_id = EXCLUDED.library_recipe_id,
                       withdrawn_at = now()"#,
            source,
            id,
        )
        .execute(&mut *tx)
        .await?;
    }
    let reports_closed = sqlx::query!(
        r#"UPDATE library_reports SET resolution = 'withdrawn', closed_at = now()
           WHERE library_recipe_id = $1 AND resolution IS NULL"#,
        id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.library.withdraw",
        "library_recipe",
        &id.to_string(),
        json!({
            "reports_closed": reports_closed,
            "source_blocked": publication.source_recipe_id.is_some(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/admin/library/recipes/{id}/withdrawal",
    tag = "admin",
    params(
        ("id" = uuid::Uuid, Path, description = "Id of the withdrawn publication"),
    ),
    responses(
        (status = 204, description = "Block lifted: its source recipe may be published again"),
        (status = 404, description = "`not_found`: no block held by this publication", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::AdminErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn lift_withdrawal(
    State(state): State<AppState>,
    actor: SuperAdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.admin_db).await?;
    let lifted = sqlx::query!(
        "DELETE FROM library_withdrawn_sources WHERE library_recipe_id = $1",
        id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if lifted == 0 {
        return Err(AppError::NotFound);
    }
    audit::record(
        &mut tx,
        Some(actor.user_id),
        "admin.library.withdrawal.lift",
        "library_recipe",
        &id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(result: AppResult<impl std::fmt::Debug>) -> String {
        match result {
            Err(AppError::BadRequest(m)) => m,
            other => panic!("expected a 400, got {other:?}"),
        }
    }

    #[test]
    fn the_six_reasons_are_read_and_written_back() {
        for (raw, reason) in [
            ("illegal", ReportReason::Illegal),
            ("copyright", ReportReason::Copyright),
            ("hateful", ReportReason::Hateful),
            ("spam", ReportReason::Spam),
            ("health_hazard", ReportReason::HealthHazard),
            ("other", ReportReason::Other),
        ] {
            assert_eq!(ReportReason::parse(raw).unwrap(), reason, "{raw}");
            assert_eq!(reason.as_str(), raw);
        }
    }

    #[test]
    fn any_other_reason_is_refused() {
        for raw in [
            "",
            " spam",
            "Spam",
            "SPAM",
            "abuse",
            "health-hazard",
            "autre",
        ] {
            assert_eq!(code(ReportReason::parse(raw)), "invalid_reason", "{raw:?}");
        }
    }

    #[test]
    fn an_absent_or_blank_comment_is_none() {
        assert_eq!(report_comment(ReportReason::Spam, None).unwrap(), None);
        assert_eq!(report_comment(ReportReason::Spam, Some("")).unwrap(), None);
        assert_eq!(
            report_comment(ReportReason::Spam, Some(" \n\t ")).unwrap(),
            None
        );
    }

    #[test]
    fn other_requires_a_comment() {
        for raw in [None, Some(""), Some(" \n\t ")] {
            assert_eq!(
                code(report_comment(ReportReason::Other, raw)),
                "comment_required",
                "{raw:?}"
            );
        }
        assert_eq!(
            report_comment(ReportReason::Other, Some(" Recette en double. "))
                .unwrap()
                .as_deref(),
            Some("Recette en double.")
        );
        assert_eq!(
            code(report_comment(ReportReason::Other, Some(&"é".repeat(501)))),
            "comment_too_long"
        );
        // Only `other`: the five others still go without one.
        for reason in [
            ReportReason::Illegal,
            ReportReason::Copyright,
            ReportReason::Hateful,
            ReportReason::Spam,
            ReportReason::HealthHazard,
        ] {
            assert_eq!(report_comment(reason, None).unwrap(), None, "{reason:?}");
        }
    }

    #[test]
    fn a_comment_is_trimmed_and_bounded_at_500_characters() {
        assert_eq!(
            report_comment(ReportReason::Spam, Some("  Copié d'un livre. "))
                .unwrap()
                .as_deref(),
            Some("Copié d'un livre.")
        );
        let full = "é".repeat(500);
        assert_eq!(
            report_comment(ReportReason::Spam, Some(&full)).unwrap(),
            Some(full.clone())
        );
        let padded = format!(" {full} ");
        assert_eq!(
            report_comment(ReportReason::Spam, Some(&padded)).unwrap(),
            Some(full.clone())
        );
        assert_eq!(
            code(report_comment(ReportReason::Spam, Some(&"é".repeat(501)))),
            "comment_too_long"
        );
        assert_eq!(COMMENT_MAX_CHARS, 500);
    }
}
