use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::google_calendar::can_configure;
use crate::google_calendar::parse::parse_ics;
use crate::groups::require_role;
use crate::AppState;

const MAX_FEED_BYTES: usize = 5 * 1024 * 1024;

#[derive(Deserialize)]
pub struct CreateCalendarImportRequest {
    pub label: String,
    pub feed_url: String,
}

/// Never includes the decrypted `feed_url` — it's a bearer credential, so
/// once stored it's write-only from the API's perspective (same principle
/// as never returning a password hash). Members needing to change it
/// delete and recreate the connection.
#[derive(Serialize)]
pub struct CalendarImportResponse {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by: Uuid,
    pub label: String,
    pub last_imported_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

struct CalendarImportRow {
    id: Uuid,
    group_id: Uuid,
    created_by: Uuid,
    label: String,
    last_imported_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl From<CalendarImportRow> for CalendarImportResponse {
    fn from(r: CalendarImportRow) -> Self {
        CalendarImportResponse {
            id: r.id,
            group_id: r.group_id,
            created_by: r.created_by,
            label: r.label,
            last_imported_at: r.last_imported_at,
            created_at: r.created_at,
        }
    }
}

fn validate_feed_url(url: &str) -> AppResult<&str> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("feed_url_required".into()));
    }
    // Only http(s) is accepted: Google's private ICS feed URLs are always
    // https, and accepting arbitrary schemes (file://, etc.) here would
    // turn this endpoint into a local-file-read primitive once /import is
    // triggered. http:// is allowed alongside https:// (rather than
    // https-only) so this can be exercised against a loopback test server
    // without TLS; it's not a materially larger SSRF surface than https
    // (a malicious feed_url can already point `import` at any host/port
    // the API process can reach either way) and full SSRF hardening
    // (private-IP-range blocking) is out of v1 scope for a
    // family-trusted, admin-only-configured endpoint.
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Err(AppError::BadRequest(
            "feed_url_must_be_http_or_https".into(),
        ));
    }
    Ok(trimmed)
}

/// Admin/owner only — see `can_configure`'s doc comment for why this epic
/// uses a stricter bar than the rest of the app.
pub async fn create_calendar_import(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<CreateCalendarImportRequest>,
) -> AppResult<impl IntoResponse> {
    let label = body.label.trim();
    if label.is_empty() {
        return Err(AppError::BadRequest("label_required".into()));
    }
    let feed_url = validate_feed_url(&body.feed_url)?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;
    if !can_configure(&actor_role) {
        return Err(AppError::Forbidden);
    }

    let row = sqlx::query_as!(
        CalendarImportRow,
        r#"
        INSERT INTO calendar_imports (group_id, created_by, label, feed_url)
        VALUES ($1, $2, $3, pgp_sym_encrypt($4, $5))
        RETURNING id, group_id, created_by, label, last_imported_at, created_at
        "#,
        group_id,
        auth.user_id,
        label,
        feed_url,
        state.calendar_feed_encryption_key,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(CalendarImportResponse::from(row))))
}

pub async fn list_calendar_imports(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let rows = sqlx::query_as!(
        CalendarImportRow,
        r#"
        SELECT id, group_id, created_by, label, last_imported_at, created_at
        FROM calendar_imports
        WHERE group_id = $1
        ORDER BY created_at
        "#,
        group_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let imports: Vec<CalendarImportResponse> =
        rows.into_iter().map(CalendarImportResponse::from).collect();
    Ok(Json(json!({ "imports": imports })))
}

/// Admin/owner only, same reasoning as `create_calendar_import`.
pub async fn delete_calendar_import(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, import_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;
    if !can_configure(&actor_role) {
        return Err(AppError::Forbidden);
    }

    let result = sqlx::query!(
        "DELETE FROM calendar_imports WHERE id = $1 AND group_id = $2",
        import_id,
        group_id,
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct ImportRunResponse {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
}

/// Any member may trigger a manual on-demand import (it's a read of the
/// feed + write to shared Agenda data, not a credential change) — same
/// "any member" bar Stocks/Recipes/Grocery list/Budget use for their
/// create/read actions. Fetches the feed fresh every call: v1 is
/// pull-on-demand only, no background polling (see migration comment for
/// the OAuth-vs-ICS tradeoff).
pub async fn trigger_calendar_import(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, import_id)): Path<(Uuid, Uuid)>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let import = sqlx::query!(
        r#"SELECT pgp_sym_decrypt(feed_url, $3) as "feed_url!" FROM calendar_imports WHERE id = $1 AND group_id = $2"#,
        import_id,
        group_id,
        state.calendar_feed_encryption_key,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    tx.commit().await?;

    let response = reqwest::Client::new()
        .get(&import.feed_url)
        .send()
        .await
        .map_err(|e| AppError::Unprocessable(format!("feed_fetch_failed: {e}")))?;
    if !response.status().is_success() {
        return Err(AppError::Unprocessable(format!(
            "feed_fetch_failed: status {}",
            response.status()
        )));
    }
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Unprocessable(format!("feed_fetch_failed: {e}")))?;
    if body.len() > MAX_FEED_BYTES {
        return Err(AppError::Unprocessable("feed_too_large".into()));
    }

    let parsed = parse_ics(&body).map_err(|_| AppError::Unprocessable("invalid_ics".into()))?;

    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;

    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;

    for event in parsed {
        let existing = sqlx::query!(
            r#"SELECT event_id, external_updated_at FROM calendar_import_events
               WHERE calendar_import_id = $1 AND external_uid = $2"#,
            import_id,
            event.external_uid,
        )
        .fetch_optional(&mut *tx)
        .await?;

        match existing {
            Some(existing)
                if existing.external_updated_at.is_some()
                    && existing.external_updated_at == event.external_updated_at =>
            {
                // Upstream version unchanged since last import: nothing to do.
                skipped += 1;
            }
            Some(existing) => {
                sqlx::query!(
                    r#"
                    UPDATE events SET
                        title = $3, description = $4, location = $5,
                        starts_at = $6, ends_at = $7, all_day = $8, updated_at = now()
                    WHERE id = $1 AND group_id = $2
                    "#,
                    existing.event_id,
                    group_id,
                    event.title,
                    event.description,
                    event.location,
                    event.starts_at,
                    event.ends_at,
                    event.all_day,
                )
                .execute(&mut *tx)
                .await?;
                sqlx::query!(
                    "UPDATE calendar_import_events SET external_updated_at = $2 WHERE calendar_import_id = $1 AND external_uid = $3",
                    import_id,
                    event.external_updated_at,
                    event.external_uid,
                )
                .execute(&mut *tx)
                .await?;
                updated += 1;
            }
            None => {
                let new_event_id = sqlx::query_scalar!(
                    r#"
                    INSERT INTO events (group_id, created_by, title, description, location, starts_at, ends_at, all_day)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    RETURNING id
                    "#,
                    group_id,
                    auth.user_id,
                    event.title,
                    event.description,
                    event.location,
                    event.starts_at,
                    event.ends_at,
                    event.all_day,
                )
                .fetch_one(&mut *tx)
                .await?;
                sqlx::query!(
                    r#"
                    INSERT INTO calendar_import_events (calendar_import_id, event_id, external_uid, external_updated_at)
                    VALUES ($1, $2, $3, $4)
                    "#,
                    import_id,
                    new_event_id,
                    event.external_uid,
                    event.external_updated_at,
                )
                .execute(&mut *tx)
                .await?;
                imported += 1;
            }
        }
    }

    sqlx::query!(
        "UPDATE calendar_imports SET last_imported_at = now(), updated_at = now() WHERE id = $1 AND group_id = $2",
        import_id,
        group_id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(ImportRunResponse {
        imported,
        updated,
        skipped,
    }))
}

#[cfg(test)]
mod tests {
    use super::validate_feed_url;

    #[test]
    fn accepts_https_url() {
        assert_eq!(
            validate_feed_url(" https://calendar.google.com/calendar/ical/x/basic.ics ").unwrap(),
            "https://calendar.google.com/calendar/ical/x/basic.ics"
        );
    }

    #[test]
    fn rejects_empty_url() {
        assert!(validate_feed_url("   ").is_err());
    }

    #[test]
    fn accepts_http_url() {
        // Allowed alongside https:// so tests can hit a loopback server
        // without TLS — see validate_feed_url's doc comment.
        assert!(validate_feed_url("http://127.0.0.1:9999/basic.ics").is_ok());
    }

    #[test]
    fn rejects_unsupported_schemes() {
        assert!(validate_feed_url("file:///etc/passwd").is_err());
        assert!(validate_feed_url("ftp://example.com/x.ics").is_err());
    }
}
