use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use manage_our_home_shared::validation::agenda::{normalize_all_day, paris_start_of_day};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::agenda::{attachments, recurrence};
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

/// Query of `DELETE /groups/:gid/calendar-imports/:import_id`.
///
/// Absent — the default — keeps the events the import created, which is
/// what removing a connection has always done: once imported they are
/// ordinary family events and may carry local work with no Google
/// counterpart (a reminder and its queued notifications, an attachment, a
/// per-occurrence completion), all of which cascades from `events`.
/// Destroying that silently would be worse than leaving them behind, so
/// this is a choice offered, never a new default.
#[derive(Deserialize, Default)]
pub struct DeleteCalendarImportQuery {
    #[serde(default)]
    pub delete_events: bool,
}

/// What the delete removed. Reported rather than left to the caller to
/// work out: the front tells the user how many events left the agenda,
/// and it cannot count them itself once they are gone.
#[derive(Serialize)]
pub struct DeleteCalendarImportResponse {
    pub deleted_events: usize,
}

/// Admin/owner only, same reasoning as `create_calendar_import` — and the
/// same bar for the `delete_events` branch, which only widens the radius
/// of a delete this role could already perform.
pub async fn delete_calendar_import(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((group_id, import_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<DeleteCalendarImportQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    let actor_role = require_role(&mut tx, group_id, auth.user_id).await?;
    if !can_configure(&actor_role) {
        return Err(AppError::Forbidden);
    }

    let mut deleted_events = 0usize;
    if query.delete_events {
        // `calendar_import_events` cascades from *both* sides, so dropping
        // the connection takes the UID→event mapping with it and there is
        // no second chance to find these ids. Read them while the mapping
        // is still there.
        let event_ids: Vec<Uuid> = sqlx::query_scalar!(
            "SELECT event_id FROM calendar_import_events WHERE calendar_import_id = $1",
            import_id,
        )
        .fetch_all(&mut *tx)
        .await?;

        // Objects before rows, the order the per-event and per-group
        // deletes use and for the same reason (see
        // `attachments::delete_objects`): a storage failure aborts the
        // whole delete instead of leaving bytes nothing points at. At feed
        // scale this is exactly the multiplication of #54 the issue warns
        // about, so it reuses that fix rather than re-deriving it.
        let storage_keys = attachments::storage_keys_for_events(&mut tx, &event_ids).await?;
        attachments::delete_objects(&state.storage, &storage_keys).await?;

        // `group_id` is redundant under the group-scoped transaction and
        // kept as a belt-and-braces bound on a delete this wide.
        let result = sqlx::query!(
            "DELETE FROM events WHERE id = ANY($1) AND group_id = $2",
            &event_ids,
            group_id,
        )
        .execute(&mut *tx)
        .await?;
        deleted_events = result.rows_affected() as usize;
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

    Ok(Json(DeleteCalendarImportResponse { deleted_events }))
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
        // The mapped row's current shape comes back with the mapping: the
        // `skipped` arm below needs it to decide whether this row predates
        // #106/#118 and has to be repaired. The join is total — the mapping's
        // `event_id` is a NOT NULL FK with ON DELETE CASCADE
        // (`0010_google_calendar_import.sql`), so a mapping cannot outlive its
        // event.
        let existing = sqlx::query!(
            r#"SELECT cie.event_id, cie.external_updated_at,
                      e.all_day, e.starts_at, e.ends_at, e.rrule,
                      EXISTS (
                          SELECT 1 FROM event_assignees ea WHERE ea.event_id = e.id
                      ) AS "has_assignee!"
               FROM calendar_import_events cie
               JOIN events e ON e.id = cie.event_id
               WHERE cie.calendar_import_id = $1 AND cie.external_uid = $2"#,
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
                // Upstream version unchanged since last import: nothing to
                // pull. The row may still predate #106/#118 though, and
                // nothing else will ever come back for it — this arm is the
                // only one that runs on an event Google never touches again.
                let repair = plan_row_repair(
                    existing.all_day,
                    existing.starts_at,
                    existing.ends_at,
                    existing.has_assignee,
                );
                if repair.is_needed() {
                    if let Some((starts_at, ends_at)) = repair.bounds {
                        sqlx::query!(
                            r#"
                            UPDATE events SET starts_at = $3, ends_at = $4, updated_at = now()
                            WHERE id = $1 AND group_id = $2
                            "#,
                            existing.event_id,
                            group_id,
                            starts_at,
                            ends_at,
                        )
                        .execute(&mut *tx)
                        .await?;
                    }
                    if repair.missing_assignee {
                        ensure_assignee(&mut tx, existing.event_id, auth.user_id).await?;
                    }
                }
                // Counted `skipped` either way: the counters report what the
                // *feed* did — `import_run_summary` renders this one as
                // "inchangé", and upstream is indeed unchanged. A repair is
                // this deployment catching up with itself, not news from
                // Google.
                skipped += 1;
            }
            Some(existing) => {
                let (starts_at, ends_at) =
                    import_bounds(event.all_day, event.starts_at, event.ends_at);
                // The feed decides `all_day`, and nothing here goes back
                // through `validate`: a rule this row took while hour-bound
                // is dropped rather than stored on an all-day row that
                // refuses it (#175). The rule is only ever cleared here,
                // never rewritten.
                let drop_rule = drops_rule_on_reimport(event.all_day, existing.rrule.as_deref());
                if drop_rule {
                    tracing::warn!(
                        event_id = %existing.event_id,
                        group_id = %group_id,
                        calendar_import_id = %import_id,
                        rrule = existing.rrule.as_deref(),
                        was_all_day = existing.all_day,
                        "calendar re-import turned the event all-day, dropping a rule that steps by less than a day (#175)"
                    );
                }
                sqlx::query!(
                    r#"
                    UPDATE events SET
                        title = $3, description = $4, location = $5,
                        starts_at = $6, ends_at = $7, all_day = $8,
                        rrule = CASE WHEN $9 THEN NULL ELSE rrule END,
                        updated_at = now()
                    WHERE id = $1 AND group_id = $2
                    "#,
                    existing.event_id,
                    group_id,
                    event.title,
                    event.description,
                    event.location,
                    starts_at,
                    ends_at,
                    event.all_day,
                    drop_rule,
                )
                .execute(&mut *tx)
                .await?;
                ensure_assignee(&mut tx, existing.event_id, auth.user_id).await?;
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
                let (starts_at, ends_at) =
                    import_bounds(event.all_day, event.starts_at, event.ends_at);
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
                    starts_at,
                    ends_at,
                    event.all_day,
                )
                .fetch_one(&mut *tx)
                .await?;
                ensure_assignee(&mut tx, new_event_id, auth.user_id).await?;
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

// ---------------------------------------------------------------------------
// What the mirror stores (issues #106, #118)
// ---------------------------------------------------------------------------

/// The bounds to store for a feed event, upholding #101's invariant — an
/// `all_day` event covers whole **Paris** civil days — on the mirror's two
/// write paths, which bypass `create_event`/`update_event` (and therefore
/// their `normalized_bounds`) entirely. That gap was issue #118.
///
/// A timed event is stored as the feed states it: its DTSTART/DTEND are
/// absolute instants and #101's invariant says nothing about them. That
/// includes the zero-length row `parse.rs:76-80` produces for a VEVENT with
/// no DTEND — giving it a duration here would be inventing one.
///
/// **The all-day case is not just `normalize_all_day(starts_at, ends_at)`,
/// and this is the subtle part.** `parse.rs:37-38` anchors a
/// `DTSTART;VALUE=DATE` on midnight **UTC**, not midnight Paris. So a feed
/// saying "1 June, all day" (DTSTART:20260601, DTEND:20260602 — RFC 5545's
/// end is exclusive) reaches us as 00:00Z → 00:00Z, which in Paris reads
/// 02:00 on the 1st → 02:00 on the 2nd. `normalize_all_day` would take that
/// end for an instant *inside* 2 June, include that day too, and hand back
/// a **two-day** event: every all-day event in the mirror would silently
/// grow by a day.
///
/// What those two 00:00Z instants really carry is a pair of civil *dates*,
/// losslessly (`date_naive()` in UTC recovers exactly the DATE the feed
/// wrote). So they are re-anchored onto Paris first, and `normalize_all_day`
/// then applies its own two rules to a pair it can read correctly: an end
/// already sitting on Paris midnight names the first day past the event, and
/// an event covers at least one whole day — which is what turns the
/// no-DTEND `ends_at == starts_at` row into a real civil day.
fn import_bounds(
    all_day: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    if !all_day {
        return (starts_at, ends_at);
    }
    normalize_all_day(
        paris_start_of_day(starts_at.date_naive()),
        paris_start_of_day(ends_at.date_naive()),
    )
}

/// Gives an imported event an assignee when it has none — issue #106.
///
/// The mirror writes `events` directly and never wrote `event_assignees` at
/// all, so every imported event arrived with `assignee_ids = []` and
/// degraded to "? —" in `--accent` instead of carrying a member's pastille.
/// It was the only route to an unassigned event that held on every
/// deployment and did not go away on its own. The others listed on
/// `EventResponse::assignee_ids` (`apps/shared/src/dto/agenda.rs`) all
/// concern events predating migration `0011`, around `0013`'s backfill: one
/// closed by #105 before any database reached it, and one that would need
/// two binaries serving the same database and closes when `0013`'s pass
/// finishes.
///
/// `user_id` is the account that ran the sync — the same one the INSERT
/// already stores in `created_by`, so a freshly imported event ends up
/// assigned to its creator exactly as `resolve_assignees`
/// (`agenda/events.rs`) guarantees for every applicative write.
///
/// **Fills only when empty, never replaces.** Three reasons, and the first
/// is the one that matters: an imported event may since have picked up local
/// work with no Google counterpart (`apps/web/src/routes/agenda/imports.rs`
/// says so where it refuses to cascade-delete them), and a member who
/// assigned the school run to one child must not have that undone by the
/// next sync. Second, it makes this callable from all three arms — new
/// event, upstream change, and the `skipped` repair — with one meaning.
/// Third, it is the same rule `0013_backfill_event_assignees.sql` used
/// (`WHERE NOT EXISTS`), which is the shape a catch-up wants: idempotent,
/// and silent when there is nothing to do.
async fn ensure_assignee(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO event_assignees (event_id, user_id)
        SELECT $1, $2
        WHERE NOT EXISTS (SELECT 1 FROM event_assignees WHERE event_id = $1)
        ON CONFLICT DO NOTHING
        "#,
        event_id,
        user_id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// What a row already in `events` is missing relative to what the import
/// writes today — the `skipped` path's business (#106, #118).
///
/// Fixing the two write paths repairs nothing already in the database: when
/// a feed's `LAST-MODIFIED` hasn't moved, the sync counts the row `skipped`
/// and never touches it, so a row written wrong stays wrong until Google
/// happens to edit that event. The arbitration on #106 put the catch-up
/// here rather than in a backfill migration: #105 — whether a migration's
/// DML can see its own source at all — was open at the time, so nothing new
/// was allowed to depend on it. #105 is settled since, and not by relaxing
/// anything: migrations run on their own connection
/// (`MIGRATION_DATABASE_URL`) as an owner role carrying `BYPASSRLS`, and the
/// pass refuses to start unless that connection provably bypasses RLS
/// (`apps/api/src/migrations.rs`). That constraint no longer binds anyone;
/// the catch-up stays here on #106's own arbitration, on the path that
/// already visits these rows.
///
/// The bounds test is `normalize_all_day`'s own fixed point: it is
/// idempotent, so a conforming row compares equal and is left alone. A row
/// that fails it can only have come from the pre-fix import — this path
/// visits none but rows mapped in `calendar_import_events` — so it is
/// UTC-date-carrier shaped and goes back through `import_bounds`, the same
/// transform the write paths use. `import_bounds`' output is itself a fixed
/// point of `normalize_all_day` (unit-tested), so the repair converges on
/// the first sync and never fires again.
#[derive(Debug, PartialEq, Eq)]
struct RowRepair {
    bounds: Option<(DateTime<Utc>, DateTime<Utc>)>,
    missing_assignee: bool,
}

impl RowRepair {
    fn is_needed(&self) -> bool {
        self.bounds.is_some() || self.missing_assignee
    }
}

fn plan_row_repair(
    all_day: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    has_assignee: bool,
) -> RowRepair {
    let conforming = !all_day || normalize_all_day(starts_at, ends_at) == (starts_at, ends_at);
    RowRepair {
        bounds: (!conforming).then(|| import_bounds(all_day, starts_at, ends_at)),
        missing_assignee: !has_assignee,
    }
}

/// Whether a re-import has to drop the `rrule` of the row it rewrites
/// (#175): the feed makes the row all-day, and its rule steps by less than a
/// day or names a time of day.
///
/// A re-import rewrites `all_day` from the feed and does not go back through
/// `validate`, which refuses such a rule on an all-day row (#171). An import
/// cannot refuse what the feed says, so the rule goes: kept, an hour-bound
/// event given `FREQ=HOURLY;COUNT=5` by `PATCH` and turned into a
/// `VALUE=DATE` event by the feed listed five entries on its day for one
/// reminder, and took a 400 on any `PATCH` keeping both the rule and the
/// flag. The feed carries no rule of its own (`parse.rs`), so every rule on
/// a mapped row was added locally.
///
/// **The whole rule, not its sub-daily parts.** `FREQ=HOURLY` has no daily
/// counterpart to fall back on, and taking `BYHOUR` out of
/// `FREQ=DAILY;BYHOUR=9,17;COUNT=4` would write a rule nobody wrote — four
/// days instead of two — on a path `validate` does not guard. A rule that
/// steps by days is kept, whatever the row was before; so is a rule on a row
/// that stays or becomes hour-bound.
///
/// The test is `validate`'s own (`recurrence::steps_by_less_than_a_day`),
/// so among the rules an hour-bound row takes, the re-import drops exactly
/// those `validate` refuses on the all-day row for that reason. It drops
/// nothing for any other reason: a rule moved past its `UNTIL` by the feed
/// stays, and `list_events` renders it on its own (#161).
fn drops_rule_on_reimport(all_day: bool, rrule: Option<&str>) -> bool {
    all_day && rrule.is_some_and(recurrence::steps_by_less_than_a_day)
}

#[cfg(test)]
mod tests {
    use super::{
        drops_rule_on_reimport, import_bounds, plan_row_repair, recurrence, validate_feed_url,
    };
    use chrono::{DateTime, Utc};
    use manage_our_home_shared::validation::agenda::normalize_all_day;

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

    // -- import_bounds (#118) -----------------------------------------------

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_timed_event_keeps_the_bounds_the_feed_gave_it() {
        // Not an all-day event: #101's invariant says nothing about it, and
        // the feed's instants are already absolute.
        let starts = utc("2026-06-01T14:00:00Z");
        let ends = utc("2026-06-01T15:00:00Z");
        assert_eq!(import_bounds(false, starts, ends), (starts, ends));
    }

    #[test]
    fn a_zero_duration_timed_event_is_left_alone_too() {
        // `parse.rs:76-80` yields `ends_at == starts_at` when a VEVENT has no
        // DTEND. For a *timed* event that stays a zero-length row: widening it
        // would invent a duration the feed never stated, and it is not what
        // #118 is about.
        let at = utc("2026-06-01T14:00:00Z");
        assert_eq!(import_bounds(false, at, at), (at, at));
    }

    #[test]
    fn an_all_day_event_covers_the_paris_day_its_feed_date_names() {
        // DTSTART;VALUE=DATE:20260601 + DTEND;VALUE=DATE:20260602 — RFC 5545's
        // exclusive end, so the event covers 1 June and nothing else.
        //
        // `parse.rs:37-38` anchors both dates on **UTC** midnight, so what
        // reaches us is 00:00Z, which is 02:00 *Paris*. Feeding that straight
        // to `normalize_all_day` would read the end as "inside 2 June" and
        // hand back a two-day event; the dates are re-anchored on Paris first.
        assert_eq!(
            import_bounds(
                true,
                utc("2026-06-01T00:00:00Z"),
                utc("2026-06-02T00:00:00Z"),
            ),
            (utc("2026-05-31T22:00:00Z"), utc("2026-06-01T22:00:00Z")),
        );
    }

    #[test]
    fn an_all_day_event_without_a_dtend_still_covers_one_whole_day() {
        // The `ends_at == starts_at` case of `parse.rs:76-80`, all-day side:
        // a zero-length row, invisible on the dashboard all day.
        assert_eq!(
            import_bounds(
                true,
                utc("2026-09-05T00:00:00Z"),
                utc("2026-09-05T00:00:00Z"),
            ),
            (utc("2026-09-04T22:00:00Z"), utc("2026-09-05T22:00:00Z")),
        );
    }

    #[test]
    fn a_winter_all_day_event_uses_the_winter_offset() {
        // Paris midnight is 23:00Z in winter and 22:00Z in summer — the reason
        // this is a timezone conversion and not a fixed subtraction.
        assert_eq!(
            import_bounds(
                true,
                utc("2026-01-01T00:00:00Z"),
                utc("2026-01-02T00:00:00Z"),
            ),
            (utc("2025-12-31T23:00:00Z"), utc("2026-01-01T23:00:00Z")),
        );
    }

    #[test]
    fn a_multi_day_all_day_event_keeps_its_span() {
        // DTSTART:20260601, DTEND:20260604 — three civil days.
        assert_eq!(
            import_bounds(
                true,
                utc("2026-06-01T00:00:00Z"),
                utc("2026-06-04T00:00:00Z"),
            ),
            (utc("2026-05-31T22:00:00Z"), utc("2026-06-03T22:00:00Z")),
        );
    }

    #[test]
    fn what_import_bounds_returns_is_already_normalized() {
        // The property the repair path leans on: what the write paths store is
        // a fixed point of `normalize_all_day`, so `plan_row_repair` reads it
        // back as conforming and never rewrites it a second time.
        let (starts, ends) = import_bounds(
            true,
            utc("2026-06-01T00:00:00Z"),
            utc("2026-06-02T00:00:00Z"),
        );
        assert_eq!(normalize_all_day(starts, ends), (starts, ends));
    }

    // -- plan_row_repair (#106 + #118, the `skipped` path) ------------------

    #[test]
    fn a_conforming_row_with_an_assignee_needs_nothing() {
        let repair = plan_row_repair(
            true,
            utc("2026-05-31T22:00:00Z"),
            utc("2026-06-01T22:00:00Z"),
            true,
        );
        assert_eq!(repair.bounds, None);
        assert!(!repair.missing_assignee);
        assert!(!repair.is_needed());
    }

    #[test]
    fn a_row_written_by_the_old_import_is_rewritten_onto_paris_days() {
        // Exactly what every all-day row imported before this fix looks like:
        // UTC midnight to UTC midnight.
        let repair = plan_row_repair(
            true,
            utc("2026-06-01T00:00:00Z"),
            utc("2026-06-02T00:00:00Z"),
            true,
        );
        assert_eq!(
            repair.bounds,
            Some((utc("2026-05-31T22:00:00Z"), utc("2026-06-01T22:00:00Z"))),
        );
        assert!(repair.is_needed());
    }

    #[test]
    fn repairing_the_same_row_twice_changes_nothing() {
        // The repair runs on every sync of an unchanged event, so it has to
        // converge — a transform drifting by a day per run would walk an event
        // off the calendar.
        let (starts, ends) = plan_row_repair(
            true,
            utc("2026-06-01T00:00:00Z"),
            utc("2026-06-02T00:00:00Z"),
            true,
        )
        .bounds
        .expect("first pass repairs");
        assert_eq!(plan_row_repair(true, starts, ends, true).bounds, None);
    }

    #[test]
    fn a_timed_row_is_never_rewritten() {
        let repair = plan_row_repair(
            false,
            utc("2026-06-01T14:00:00Z"),
            utc("2026-06-01T15:00:00Z"),
            true,
        );
        assert_eq!(repair.bounds, None);
    }

    #[test]
    fn a_row_with_no_assignee_is_flagged_whatever_its_bounds() {
        // #106: the mirror never wrote `event_assignees` at all, so a row with
        // conforming bounds can still be missing its assignee.
        let repair = plan_row_repair(
            true,
            utc("2026-05-31T22:00:00Z"),
            utc("2026-06-01T22:00:00Z"),
            false,
        );
        assert_eq!(repair.bounds, None);
        assert!(repair.missing_assignee);
        assert!(repair.is_needed());
    }

    #[test]
    fn a_row_can_need_both_repairs_at_once() {
        let repair = plan_row_repair(
            true,
            utc("2026-06-01T00:00:00Z"),
            utc("2026-06-02T00:00:00Z"),
            false,
        );
        assert!(repair.bounds.is_some());
        assert!(repair.missing_assignee);
    }

    // -- drops_rule_on_reimport (#175) ----------------------------------------
    //
    // A re-import rewrites `all_day` from the feed without going through
    // `validate`. A rule a `PATCH` gave an hour-bound row — `FREQ=HOURLY;
    // COUNT=5` — stayed on the row once the feed turned the event into a
    // `VALUE=DATE` one: five entries listed on its day, one reminder stored,
    // and a 400 on any `PATCH` keeping both the rule and the flag.

    /// Rules an hour-bound row takes and an all-day row refuses (#171).
    const SUB_DAILY_RULES: [&str; 8] = [
        "FREQ=HOURLY;COUNT=5",
        "FREQ=MINUTELY",
        "FREQ=SECONDLY;COUNT=3",
        "freq=hourly",
        "FREQ=DAILY;BYHOUR=12;COUNT=3",
        "FREQ=WEEKLY;BYDAY=SA;BYMINUTE=30",
        "FREQ=MONTHLY;BYSECOND=0",
        "FREQ=DAILY;byhour=9,17",
    ];

    #[test]
    fn a_rule_stepping_by_less_than_a_day_is_dropped_when_the_row_turns_all_day() {
        for rule in SUB_DAILY_RULES {
            assert!(
                drops_rule_on_reimport(true, Some(rule)),
                "{rule:?} kept on an all-day row"
            );
        }
    }

    #[test]
    fn an_hour_bound_row_keeps_its_rule_whatever_it_steps_by() {
        for rule in SUB_DAILY_RULES {
            assert!(
                !drops_rule_on_reimport(false, Some(rule)),
                "{rule:?} dropped from an hour-bound row"
            );
        }
    }

    #[test]
    fn an_all_day_row_keeps_a_rule_stepping_by_days() {
        for rule in [
            "FREQ=DAILY",
            "FREQ=DAILY;COUNT=3",
            "FREQ=WEEKLY;BYDAY=MO,SA",
            "FREQ=MONTHLY;BYMONTHDAY=5",
            "FREQ=YEARLY;BYMONTH=9;BYMONTHDAY=5",
            "FREQ=DAILY;UNTIL=20261010T235959Z",
            "FREQ=DAILY;BYHOUR=",
        ] {
            assert!(
                !drops_rule_on_reimport(true, Some(rule)),
                "{rule:?} dropped from an all-day row"
            );
        }
    }

    #[test]
    fn a_row_without_a_rule_has_nothing_to_drop() {
        assert!(!drops_rule_on_reimport(true, None));
        assert!(!drops_rule_on_reimport(false, None));
    }

    /// Among the rules a `PATCH` gives an hour-bound row, the re-import drops
    /// exactly those `validate` refuses on the all-day row it writes: what it
    /// keeps, a `PATCH` would take; what it drops, a `PATCH` would refuse.
    #[test]
    fn a_rule_is_dropped_on_reimport_exactly_when_the_all_day_row_would_refuse_it() {
        let frequencies = [
            "YEARLY", "MONTHLY", "WEEKLY", "DAILY", "HOURLY", "MINUTELY", "SECONDLY",
        ];
        let parts = [
            "",
            ";BYHOUR=12",
            ";BYHOUR=9,17",
            ";BYMINUTE=0,30",
            ";BYSECOND=0,15",
            ";BYHOUR=",
            ";BYDAY=SA",
            ";BYMONTHDAY=5,6",
        ];
        let ends = ["", ";COUNT=5", ";UNTIL=20261020T235959Z"];
        // The feed's hour-bound event, then the same day as a `VALUE=DATE`.
        let timed = utc("2026-09-05T09:00:00Z");
        let date = utc("2026-09-05T00:00:00Z");
        let (all_day_start, _) = import_bounds(true, date, date);

        let (mut kept, mut dropped) = (0, 0);
        for frequency in frequencies {
            for part in parts {
                for end in ends {
                    let rule = format!("FREQ={frequency}{part}{end}");
                    if recurrence::validate(&rule, timed, false).is_err() {
                        continue;
                    }
                    let refused = recurrence::validate(&rule, all_day_start, true).is_err();
                    assert_eq!(
                        drops_rule_on_reimport(true, Some(&rule)),
                        refused,
                        "{rule:?}: dropped on re-import, refused on the all-day row"
                    );
                    if refused {
                        dropped += 1;
                    } else {
                        kept += 1;
                    }
                }
            }
        }
        assert!(kept > 0 && dropped > 0, "kept {kept}, dropped {dropped}");
    }
}
