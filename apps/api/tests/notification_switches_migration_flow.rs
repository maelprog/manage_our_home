//! #853 — `0063_notification_switches.sql` against a real Postgres.
//!
//! The database is migrated up to the version before 0063, two accounts
//! are written as an older build would have — one told of the agenda
//! changes, one not — then the rest is applied through `LoggingMigrate`, as
//! `migrate` does.

use manage_our_home::migrations::{LoggingMigrate, MIGRATOR};
use sqlx::PgPool;

const NOTIFICATION_SWITCHES: i64 = 63;

async fn migrate_to(db: &PgPool, target: Option<i64>) {
    let mut conn = db.acquire().await.unwrap();
    let mut logging = LoggingMigrate(&mut conn);
    MIGRATOR
        .run_direct(target, &mut logging, false)
        .await
        .unwrap();
}

/// Each account keeps for the three switches what it had chosen for the one
/// setting they replace; the channel and the recap's hour are untouched,
/// the recap is on, and the channel may then be `none` and the recap off
/// with its hour kept.
#[sqlx::test(migrations = false)]
async fn each_account_keeps_its_choice_for_the_three_switches(db: PgPool) {
    migrate_to(&db, Some(NOTIFICATION_SWITCHES - 1)).await;
    for (email, told) in [("on@example.test", true), ("off@example.test", false)] {
        sqlx::query(
            "INSERT INTO users (email, password_hash, display_name, notify_agenda_changes,
                                reminder_channel, expiry_digest_hour)
             VALUES ($1, 'x', $1, $2, 'both', 18)",
        )
        .bind(email)
        .bind(told)
        .execute(&db)
        .await
        .unwrap();
    }

    migrate_to(&db, None).await;

    let rows: Vec<(String, bool, bool, bool, String, i16, bool)> = sqlx::query_as(
        "SELECT email, notify_agenda_added, notify_agenda_changed, notify_agenda_deleted,
                reminder_channel, expiry_digest_hour, expiry_digest_enabled
         FROM users ORDER BY email",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            (
                "off@example.test".into(),
                false,
                false,
                false,
                "both".into(),
                18,
                true
            ),
            (
                "on@example.test".into(),
                true,
                true,
                true,
                "both".into(),
                18,
                true
            ),
        ]
    );
    // A new account: the three on, the recap on at 9.
    let new: (bool, bool, bool, bool, i16) = sqlx::query_as(
        "INSERT INTO users (email, password_hash, display_name)
         VALUES ('new@example.test', 'x', 'New')
         RETURNING notify_agenda_added, notify_agenda_changed, notify_agenda_deleted,
                   expiry_digest_enabled, expiry_digest_hour",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(new, (true, true, true, true, 9));
    sqlx::query(
        "UPDATE users SET reminder_channel = 'none', expiry_digest_enabled = false
         WHERE email = 'on@example.test'",
    )
    .execute(&db)
    .await
    .unwrap();
    // The hour stays one of the day's, recap on or off.
    let refused = sqlx::query("UPDATE users SET expiry_digest_hour = NULL")
        .execute(&db)
        .await;
    assert!(refused.is_err());
}
