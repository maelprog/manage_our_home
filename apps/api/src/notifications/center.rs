//! The notification centre (#534): what a member reads in the application,
//! whatever reached their devices or their inbox.
//!
//! Every event of [`Kind`] writes one row of `notifications` per recipient
//! (migration 0036), through [`insert_for`] or [`insert_for_other_members`]
//! — the two writers every feature calls, budget alerts (#536) included.
//! The member lists them, counts the unread ones and marks them read here;
//! the rows go 30 days after they were written, read or not
//! (`jobs::retention_purge::NOTIFICATION_RETENTION_DAYS`).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use manage_our_home_shared::dto::notifications::{
    NotificationPage, NotificationResponse, UnreadNotificationCount,
};
use manage_our_home_shared::validation::rgpd::sanitize_email_line;
use serde::Deserialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::auth::session::{user_scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::AppState;

/// The value of `notifications.kind`, as the `CHECK` of migration 0036
/// spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    AgendaReminder,
    StockExpiry,
    Message,
    Invitation,
    GroceryItem,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::AgendaReminder,
        Kind::StockExpiry,
        Kind::Message,
        Kind::Invitation,
        Kind::GroceryItem,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        Kind::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::AgendaReminder => "agenda_reminder",
            Kind::StockExpiry => "stock_expiry",
            Kind::Message => "message",
            Kind::Invitation => "invitation",
            Kind::GroceryItem => "grocery_item",
        }
    }
}

/// What happened, with what the text of its notification names.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// A reminder of an agenda event came due. `all_day` drops the hour,
    /// which for such an event is only the Paris midnight it opens on.
    AgendaReminder {
        title: &'a str,
        occurrence_at: DateTime<Utc>,
        all_day: bool,
    },
    /// A stock item reaches its « à consommer jusqu'au » date soon.
    StockExpiry {
        item: &'a str,
        expires_on: NaiveDate,
    },
    /// A member posted a message. Its content is never copied here: it is
    /// encrypted at rest, and the notification is not.
    Message { author: &'a str },
    /// A member invited the recipient's address into a family.
    Invitation { inviter: &'a str, group: &'a str },
    /// A member added an item to the grocery list.
    GroceryItem { author: &'a str, item: &'a str },
}

impl Event<'_> {
    pub fn kind(&self) -> Kind {
        match self {
            Event::AgendaReminder { .. } => Kind::AgendaReminder,
            Event::StockExpiry { .. } => Kind::StockExpiry,
            Event::Message { .. } => Kind::Message,
            Event::Invitation { .. } => Kind::Invitation,
            Event::GroceryItem { .. } => Kind::GroceryItem,
        }
    }
}

/// The short text of the notification, in French, on one line.
///
/// Every part a member typed (a title, a name) goes through
/// `sanitize_email_line`: whitespace runs flattened to one space, invisible
/// characters dropped, cut at 80 characters with an ellipsis. The body thus
/// stays within the 300 characters migration 0036 allows whatever was typed,
/// and no title can open a second line in the list. Dates and hours are
/// those of Europe/Paris, the fixed v1 display timezone.
pub fn body(event: &Event<'_>) -> String {
    let typed = sanitize_email_line;
    match *event {
        Event::AgendaReminder {
            title,
            occurrence_at,
            all_day,
        } => {
            let local = occurrence_at.with_timezone(&PARIS);
            let when = if all_day {
                local.format("%d/%m/%Y")
            } else {
                local.format("%d/%m/%Y à %H:%M")
            };
            format!("Rappel : « {} » le {when}.", typed(title))
        }
        Event::StockExpiry { item, expires_on } => format!(
            "« {} » est à consommer jusqu'au {}.",
            typed(item),
            expires_on.format("%d/%m/%Y")
        ),
        Event::Message { author } => format!("Nouveau message de {}.", typed(author)),
        Event::Invitation { inviter, group } => format!(
            "{} vous invite à rejoindre « {} ».",
            typed(inviter),
            typed(group)
        ),
        Event::GroceryItem { author, item } => format!(
            "{} a ajouté « {} » à la liste de courses.",
            typed(author),
            typed(item)
        ),
    }
}

/// The fixed v1 display timezone, as `agenda::recurrence` names it.
const PARIS: rrule::Tz = rrule::Tz::Europe__Paris;

/// A notification to write, for one recipient or several.
#[derive(Debug, Clone)]
pub struct NewNotification<'a> {
    /// The family it happened in (for an invitation, the inviting one).
    pub group_id: Uuid,
    /// What happened: gives the kind and the text.
    pub event: Event<'a>,
    /// The row it is about, per kind.
    pub object_id: Option<Uuid>,
    /// The member whose action caused it, `None` for a job. Their own
    /// notification is never written by [`insert_for_members`].
    pub actor_id: Option<Uuid>,
    /// Set by a writer that may see the same event twice (a job's next
    /// pass): a recipient who already holds a notification with this key
    /// gets no second one. `None` writes unconditionally.
    pub dedup_key: Option<String>,
}

/// Writes `notification` for each of `recipients`. Returns how many rows
/// were written — fewer than `recipients` when `dedup_key` already exists
/// for some.
///
/// On a runtime connection the transaction must be scoped to
/// `notification.group_id` (`scoped_tx`): the INSERT policy of 0036 checks
/// the family, not the recipient.
pub async fn insert_for(
    conn: &mut PgConnection,
    recipients: &[Uuid],
    notification: &NewNotification<'_>,
) -> Result<u64, sqlx::Error> {
    let written = sqlx::query!(
        r#"INSERT INTO notifications (user_id, group_id, kind, object_id, actor_id, body, dedup_key)
           SELECT r, $2, $3, $4, $5, $6, $7 FROM unnest($1::uuid[]) AS r
           ON CONFLICT DO NOTHING"#,
        recipients,
        notification.group_id,
        notification.event.kind().as_str(),
        notification.object_id,
        notification.actor_id,
        body(&notification.event),
        notification.dedup_key,
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(written)
}

/// Writes `notification` for every member of its family but its actor,
/// whose account is neither deactivated nor purged. Returns how many rows
/// were written. Same scoping as [`insert_for`].
pub async fn insert_for_members(
    conn: &mut PgConnection,
    notification: &NewNotification<'_>,
) -> Result<u64, sqlx::Error> {
    let written = sqlx::query!(
        r#"INSERT INTO notifications (user_id, group_id, kind, object_id, actor_id, body, dedup_key)
           SELECT gm.user_id, $1, $2, $3, $4, $5, $6
           FROM group_members gm
           JOIN users u ON u.id = gm.user_id
           WHERE gm.group_id = $1
             AND gm.user_id IS DISTINCT FROM $4
             AND u.deactivated_at IS NULL AND u.deleted_at IS NULL
           ON CONFLICT DO NOTHING"#,
        notification.group_id,
        notification.event.kind().as_str(),
        notification.object_id,
        notification.actor_id,
        body(&notification.event),
        notification.dedup_key,
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(written)
}

const DEFAULT_PAGE_LIMIT: i64 = 20;
const MAX_PAGE_LIMIT: i64 = 100;

/// `GET /notifications` query parameters.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// One kind only (`Kind::as_str`); every kind when absent.
    pub kind: Option<String>,
    /// The cursor: the `created_at` and `id` of the last entry of the
    /// previous page. Both or neither.
    pub before_created_at: Option<DateTime<Utc>>,
    pub before_id: Option<Uuid>,
    /// 20 by default, 1 to 100.
    pub limit: Option<i64>,
}

/// `GET /notifications` — the caller's notifications from every family,
/// newest first, by pages. 400 `invalid_notification_kind` for a `kind`
/// that is none, 400 `invalid_cursor` for half a cursor.
///
/// Every query of this module filters on the caller's id itself, and runs
/// under `app.user_id` too, so the recipient policy of 0036 applies the
/// same filter a second time.
pub async fn list_notifications(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<ListQuery>,
) -> AppResult<Json<NotificationPage>> {
    let kind = match query.kind.as_deref() {
        None => None,
        Some(k) => Some(
            Kind::parse(k)
                .ok_or_else(|| AppError::BadRequest("invalid_notification_kind".into()))?,
        ),
    };
    if query.before_created_at.is_some() != query.before_id.is_some() {
        return Err(AppError::BadRequest("invalid_cursor".into()));
    }
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);

    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let rows = sqlx::query!(
        r#"SELECT id, group_id, kind, object_id, body, read_at, created_at
           FROM notifications
           WHERE user_id = $1
             AND ($2::text IS NULL OR kind = $2)
             AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
           ORDER BY created_at DESC, id DESC
           LIMIT $5"#,
        auth.user_id,
        kind.map(Kind::as_str),
        query.before_created_at,
        query.before_id,
        limit + 1,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let has_more = rows.len() as i64 > limit;
    let notifications = rows
        .into_iter()
        .take(limit as usize)
        .map(|r| NotificationResponse {
            id: r.id,
            group_id: r.group_id,
            kind: r.kind,
            object_id: r.object_id,
            body: r.body,
            read_at: r.read_at,
            created_at: r.created_at,
        })
        .collect();
    Ok(Json(NotificationPage {
        notifications,
        has_more,
    }))
}

/// `GET /notifications/unread_count` — one `count(*)` on the partial index
/// of 0036, for the badge every page of the application shows.
pub async fn unread_count(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<Json<UnreadNotificationCount>> {
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let unread_count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM notifications
           WHERE user_id = $1 AND read_at IS NULL"#,
        auth.user_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(UnreadNotificationCount { unread_count }))
}

/// `POST /notifications/:id/read` — 204, also when it was read already (its
/// `read_at` is kept). 404 for an id that is not one of the caller's: the
/// answer is the same whether it belongs to another member or to nobody.
pub async fn mark_read(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let found = sqlx::query!(
        r#"UPDATE notifications SET read_at = COALESCE(read_at, now())
           WHERE id = $1 AND user_id = $2"#,
        id,
        auth.user_id,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    if found == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /notifications/read` — marks every unread notification of the
/// caller read: 204.
pub async fn mark_all_read(State(state): State<AppState>, auth: AuthUser) -> AppResult<StatusCode> {
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    sqlx::query!(
        "UPDATE notifications SET read_at = now() WHERE user_id = $1 AND read_at IS NULL",
        auth.user_id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_five_kinds_round_trip() {
        for (text, kind) in [
            ("agenda_reminder", Kind::AgendaReminder),
            ("stock_expiry", Kind::StockExpiry),
            ("message", Kind::Message),
            ("invitation", Kind::Invitation),
            ("grocery_item", Kind::GroceryItem),
        ] {
            assert_eq!(Kind::parse(text), Some(kind));
            assert_eq!(kind.as_str(), text);
        }
        assert_eq!(Kind::ALL.len(), 5);
    }

    #[test]
    fn anything_else_is_not_a_kind() {
        for text in ["", "Message", "MESSAGE", " message", "budget", "agenda"] {
            assert_eq!(Kind::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn each_event_has_its_kind() {
        let at = Utc.with_ymd_and_hms(2026, 10, 3, 14, 30, 0).unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        for (event, kind) in [
            (
                Event::AgendaReminder {
                    title: "t",
                    occurrence_at: at,
                    all_day: false,
                },
                Kind::AgendaReminder,
            ),
            (
                Event::StockExpiry {
                    item: "i",
                    expires_on: day,
                },
                Kind::StockExpiry,
            ),
            (Event::Message { author: "a" }, Kind::Message),
            (
                Event::Invitation {
                    inviter: "a",
                    group: "g",
                },
                Kind::Invitation,
            ),
            (
                Event::GroceryItem {
                    author: "a",
                    item: "i",
                },
                Kind::GroceryItem,
            ),
        ] {
            assert_eq!(event.kind(), kind, "{event:?}");
        }
    }

    /// 14:30 UTC on 3 October is 16:30 in Paris (summer time).
    #[test]
    fn a_reminder_names_the_event_and_its_paris_hour() {
        let at = Utc.with_ymd_and_hms(2026, 10, 3, 14, 30, 0).unwrap();
        assert_eq!(
            body(&Event::AgendaReminder {
                title: "Dîner",
                occurrence_at: at,
                all_day: false,
            }),
            "Rappel : « Dîner » le 03/10/2026 à 16:30."
        );
    }

    /// In winter Paris is UTC+1; 23:30 UTC on 31 December is already the
    /// next day there.
    #[test]
    fn a_reminder_reads_its_day_in_paris_not_in_utc() {
        let at = Utc.with_ymd_and_hms(2026, 12, 31, 23, 30, 0).unwrap();
        assert_eq!(
            body(&Event::AgendaReminder {
                title: "Réveillon",
                occurrence_at: at,
                all_day: false,
            }),
            "Rappel : « Réveillon » le 01/01/2027 à 00:30."
        );
    }

    /// An all-day occurrence starts at the Paris midnight opening its day
    /// (22:00 UTC the eve, in summer): the day, without « à 00:00 ».
    #[test]
    fn an_all_day_reminder_names_the_day_only() {
        let at = Utc.with_ymd_and_hms(2026, 10, 2, 22, 0, 0).unwrap();
        assert_eq!(
            body(&Event::AgendaReminder {
                title: "Vacances",
                occurrence_at: at,
                all_day: true,
            }),
            "Rappel : « Vacances » le 03/10/2026."
        );
    }

    #[test]
    fn an_expiry_names_the_item_and_its_date() {
        assert_eq!(
            body(&Event::StockExpiry {
                item: "Lait",
                expires_on: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            }),
            "« Lait » est à consommer jusqu'au 02/11/2026."
        );
    }

    #[test]
    fn a_message_names_its_author_and_not_its_content() {
        assert_eq!(
            body(&Event::Message { author: "Alice" }),
            "Nouveau message de Alice."
        );
    }

    #[test]
    fn an_invitation_names_the_inviter_and_the_family() {
        assert_eq!(
            body(&Event::Invitation {
                inviter: "Alice",
                group: "Famille Martin",
            }),
            "Alice vous invite à rejoindre « Famille Martin »."
        );
    }

    #[test]
    fn a_grocery_item_names_its_author_and_the_item() {
        assert_eq!(
            body(&Event::GroceryItem {
                author: "Alice",
                item: "Lait",
            }),
            "Alice a ajouté « Lait » à la liste de courses."
        );
    }

    /// A typed part cannot open a second line, nor hide characters.
    #[test]
    fn typed_parts_are_flattened_to_one_line() {
        let text = body(&Event::GroceryItem {
            author: "Ali\u{202e}ce\n",
            item: "Lait\r\n\r\nentier\u{200b}",
        });
        assert_eq!(
            text,
            "Alice a ajouté « Lait entier » à la liste de courses."
        );
    }

    /// Whatever was typed, the body stays within the 300 characters the
    /// column's CHECK allows (0036): each typed part is cut at 80.
    #[test]
    fn the_longest_body_fits_the_column() {
        let long = "x".repeat(10_000);
        let at = Utc.with_ymd_and_hms(2026, 10, 3, 14, 30, 0).unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        for event in [
            Event::AgendaReminder {
                title: &long,
                occurrence_at: at,
                all_day: false,
            },
            Event::StockExpiry {
                item: &long,
                expires_on: day,
            },
            Event::Message { author: &long },
            Event::Invitation {
                inviter: &long,
                group: &long,
            },
            Event::GroceryItem {
                author: &long,
                item: &long,
            },
        ] {
            let n = body(&event).chars().count();
            assert!((1..=300).contains(&n), "{n} characters for {event:?}");
        }
    }

    /// An empty part still leaves a non-empty body (the CHECK's lower bound).
    #[test]
    fn an_empty_part_still_gives_a_body() {
        assert!(!body(&Event::Message { author: "" }).is_empty());
    }
}
