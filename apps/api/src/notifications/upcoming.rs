//! A member's upcoming reminders (#667), `GET /account/reminders/upcoming`:
//! what the mobile application schedules on the device (Capacitor's Local
//! Notifications, `docs/architecture.md`, resolved question 4), so that a
//! reminder fires even without network when it falls due.
//!
//! Read only: the server's own sending (`jobs::scheduled_notifications`)
//! is unchanged, and nothing is stored.

use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use manage_our_home_shared::dto::notifications::UpcomingReminder;

use super::ReminderChannel;
use crate::agenda::reminders::NOTIFICATION_WINDOW_DAYS;
use crate::auth::session::{scoped_tx, user_scoped_tx, AuthUser};
use crate::error::AppResult;
use crate::AppState;

/// `GET /account/reminders/upcoming` — the next pending notifications of
/// the reminders the caller receives, [`select_upcoming`]: a reminder goes
/// to the event's creator (`jobs::scheduled_notifications`), so those of
/// the events they created, in every family they belong to now. `[]` on
/// the `email` channel.
///
/// `scheduled_notifications` and `events` are under RLS by family (0002):
/// the families are read from the caller's own `group_members` rows, then
/// each one is read in a transaction scoped to it (`scoped_tx`), as the
/// art. 15 export does. A family the caller left is not in the list, so
/// its reminders are not either, though the events stay theirs.
#[utoipa::path(
    get,
    path = "/account/reminders/upcoming",
    tag = "notifications",
    responses(
        (status = 200, description = "The caller's upcoming reminders, soonest first, 64 at most; empty on the `email` channel", body = Vec<UpcomingReminder>),
        crate::api_docs::SessionErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn list_upcoming(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<Json<Vec<UpcomingReminder>>> {
    let now = Utc::now();
    let horizon = horizon();
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let channel = sqlx::query_scalar!(
        "SELECT reminder_channel FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&mut *tx)
    .await?;
    // The column's CHECK (0020) admits nothing else.
    let channel = ReminderChannel::parse(&channel)
        .ok_or_else(|| anyhow::anyhow!("unknown reminder_channel {channel:?}"))?;
    if !channel.includes_push() {
        return Ok(Json(Vec::new()));
    }
    let groups = sqlx::query_scalar!(
        "SELECT group_id FROM group_members WHERE user_id = $1 ORDER BY group_id",
        auth.user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut candidates = Vec::new();
    for group_id in groups {
        let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
        // The same bounds as `select_upcoming`, before the `LIMIT`: without
        // the lower one, overdue notifications still pending (being
        // retried) would fill a family's 64 and hide the ones to come.
        let rows = sqlx::query!(
            r#"SELECT sn.id, sn.event_id, e.title, sn.occurrence_at, sn.fire_at, e.all_day
               FROM scheduled_notifications sn JOIN events e ON e.id = sn.event_id
               WHERE e.group_id = $1 AND e.created_by = $2 AND sn.status = 'pending'
                 AND sn.fire_at > $3 AND sn.fire_at <= $4
               ORDER BY sn.fire_at, sn.id
               LIMIT $5"#,
            group_id,
            auth.user_id,
            now,
            now + horizon,
            MAX_UPCOMING_REMINDERS as i64,
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        candidates.extend(rows.into_iter().map(|r| UpcomingReminder {
            id: r.id,
            event_id: r.event_id,
            group_id,
            title: r.title,
            occurrence_at: r.occurrence_at,
            fire_at: r.fire_at,
            all_day: r.all_day,
        }));
    }
    Ok(Json(select_upcoming(channel, now, horizon, candidates)))
}

/// The most reminders one answer lists: the 64 pending notifications iOS
/// keeps per application, the tighter of the two systems the application
/// schedules on.
pub const MAX_UPCOMING_REMINDERS: usize = 64;

/// How far ahead the answer looks: the window a recurring event's
/// occurrences are materialized over (`NOTIFICATION_WINDOW_DAYS`), so that
/// a series and a one-off event are listed alike. A one-off event's
/// notification is written whatever its date (`reminder_occurrences`), so
/// rows beyond the horizon do exist and are left out: a reminder due more
/// than 30 days ahead reaches the device only through a call made within
/// the 30 days before it is due.
pub fn horizon() -> Duration {
    Duration::days(NOTIFICATION_WINDOW_DAYS)
}

/// What the member's device is to schedule, out of the pending
/// notifications of the reminders they receive: nothing on the `email`
/// channel, which the device would only duplicate; otherwise those due
/// after `now` and at most `horizon` from it, soonest first (ties by id,
/// so two calls list the same ones), [`MAX_UPCOMING_REMINDERS`] at most.
pub fn select_upcoming(
    channel: ReminderChannel,
    now: DateTime<Utc>,
    horizon: Duration,
    candidates: Vec<UpcomingReminder>,
) -> Vec<UpcomingReminder> {
    if !channel.includes_push() {
        return Vec::new();
    }
    let until = now + horizon;
    let mut listed: Vec<_> = candidates
        .into_iter()
        .filter(|r| r.fire_at > now && r.fire_at <= until)
        .collect();
    listed.sort_by_key(|r| (r.fire_at, r.id));
    listed.truncate(MAX_UPCOMING_REMINDERS);
    listed
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap()
    }

    fn due_in(minutes: i64) -> UpcomingReminder {
        let fire_at = now() + Duration::minutes(minutes);
        UpcomingReminder {
            id: Uuid::new_v4(),
            event_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            title: format!("dans {minutes} min"),
            occurrence_at: fire_at + Duration::minutes(10),
            fire_at,
            all_day: false,
        }
    }

    fn select(
        channel: ReminderChannel,
        candidates: Vec<UpcomingReminder>,
    ) -> Vec<UpcomingReminder> {
        select_upcoming(channel, now(), horizon(), candidates)
    }

    #[test]
    fn the_email_channel_lists_nothing() {
        assert!(select(ReminderChannel::Email, vec![due_in(5), due_in(60)]).is_empty());
    }

    #[test]
    fn the_push_and_both_channels_list_the_reminders() {
        for channel in [ReminderChannel::Push, ReminderChannel::Both] {
            let one = due_in(5);
            assert_eq!(select(channel, vec![one.clone()]), vec![one], "{channel:?}");
        }
    }

    #[test]
    fn a_reminder_due_now_or_before_is_not_listed() {
        let soon = due_in(1);
        let listed = select(
            ReminderChannel::Push,
            vec![due_in(0), due_in(-1), due_in(-60 * 24), soon.clone()],
        );
        assert_eq!(listed, vec![soon]);
    }

    #[test]
    fn a_reminder_beyond_the_horizon_is_not_listed() {
        let at_the_edge = due_in(horizon().num_minutes());
        let listed = select(
            ReminderChannel::Push,
            vec![due_in(horizon().num_minutes() + 1), at_the_edge.clone()],
        );
        assert_eq!(listed, vec![at_the_edge]);
    }

    #[test]
    fn the_horizon_is_the_materialization_window() {
        assert_eq!(horizon(), Duration::days(NOTIFICATION_WINDOW_DAYS));
    }

    #[test]
    fn the_soonest_comes_first_and_ties_go_by_id() {
        let mut a = due_in(30);
        let mut b = due_in(30);
        if a.id > b.id {
            std::mem::swap(&mut a, &mut b);
        }
        let first = due_in(2);
        let last = due_in(600);
        let listed = select(
            ReminderChannel::Both,
            vec![last.clone(), b.clone(), first.clone(), a.clone()],
        );
        assert_eq!(listed, vec![first, a, b, last]);
    }

    #[test]
    fn at_most_64_are_listed_the_soonest_ones() {
        // Given latest first, so that the cap must come after the sort.
        let all: Vec<_> = (1..=100).rev().map(due_in).collect();
        let listed = select(ReminderChannel::Push, all);
        assert_eq!(listed.len(), MAX_UPCOMING_REMINDERS);
        assert_eq!(MAX_UPCOMING_REMINDERS, 64);
        let minutes: Vec<_> = listed
            .iter()
            .map(|r| (r.fire_at - now()).num_minutes())
            .collect();
        assert_eq!(minutes, (1..=64).collect::<Vec<_>>());
    }

    #[test]
    fn the_cap_counts_only_what_is_listable() {
        // 64 past ones and one to come: the one to come is listed.
        let mut all: Vec<_> = (1..=64).map(|m| due_in(-m)).collect();
        let to_come = due_in(10);
        all.push(to_come.clone());
        assert_eq!(select(ReminderChannel::Push, all), vec![to_come]);
    }
}
