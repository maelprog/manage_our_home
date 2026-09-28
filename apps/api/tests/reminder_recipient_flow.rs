//! End-to-end cover for who an event reminder goes to (#291), against a
//! real database.
//!
//! A reminder goes to the event's creator. A creator support deactivated
//! keeps its address for the two years its account is kept, and a purged
//! one has it rewritten to `deleted-<id>@deleted.invalid`: neither must be
//! written to. Their due reminders are retired instead of left pending, so
//! a reactivation does not bring back a backlog of stale reminders and a
//! purged creator's are not read again on every pass.

use manage_our_home::jobs::account_purge::purge_account;
use manage_our_home::jobs::scheduled_notifications::send_due_notifications;
use sqlx::PgPool;
use std::sync::Mutex;
use uuid::Uuid;

async fn insert_user(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, age_declared_at)
         VALUES ($1, 'not-a-real-hash', $1, now() - interval '1 year')
         RETURNING id",
    )
    .bind(email)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn insert_group(db: &PgPool, owner: Uuid) -> Uuid {
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Maison', $1) RETURNING id",
    )
    .bind(owner)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner'::group_role)",
    )
    .bind(group)
    .bind(owner)
    .execute(db)
    .await
    .unwrap();
    group
}

/// An event created by `by`, with one reminder whose notification is due
/// (its `fire_at` a minute in the past). Returns the notification's id.
async fn due_reminder(db: &PgPool, group: Uuid, by: Uuid, title: &str) -> Uuid {
    let event: Uuid = sqlx::query_scalar(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at)
         VALUES ($1, $2, $3, now() + interval '9 minutes', now() + interval '1 hour')
         RETURNING id",
    )
    .bind(group)
    .bind(by)
    .bind(title)
    .fetch_one(db)
    .await
    .unwrap();
    let reminder: Uuid = sqlx::query_scalar(
        "INSERT INTO event_reminders (event_id, offset_minutes) VALUES ($1, 10) RETURNING id",
    )
    .bind(event)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO scheduled_notifications (event_reminder_id, event_id, occurrence_at, fire_at)
         VALUES ($1, $2, now() + interval '9 minutes', now() - interval '1 minute')
         RETURNING id",
    )
    .bind(reminder)
    .bind(event)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn notification(db: &PgPool, id: Uuid) -> (String, i32) {
    sqlx::query_as("SELECT status, attempts FROM scheduled_notifications WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

/// One pass, recording every email instead of sending it.
async fn pass(db: &PgPool) -> Vec<(String, String)> {
    let sent = Mutex::new(Vec::<(String, String)>::new());
    let record = |to: String, subject: String, _body: String| {
        sent.lock().unwrap().push((to, subject));
        async { Ok::<(), anyhow::Error>(()) }
    };
    send_due_notifications(db, &record).await.unwrap();
    sent.into_inner().unwrap()
}

/// The control: an active creator gets its due reminder, once.
#[sqlx::test]
async fn an_active_creator_gets_its_due_reminder(db: PgPool) {
    let active = insert_user(&db, "active@example.test").await;
    let group = insert_group(&db, active).await;
    let due = due_reminder(&db, group, active, "Dîner").await;

    let sent = pass(&db).await;
    assert_eq!(
        sent,
        vec![("active@example.test".into(), "Rappel : Dîner".into())]
    );
    assert_eq!(notification(&db, due).await, ("sent".into(), 0));
    assert!(pass(&db).await.is_empty());
}

/// A deactivated creator gets nothing, and its due reminder is retired
/// without an attempt: reactivating the account does not send it late.
/// A reminder not yet due stays pending, to go if the account is back by
/// then.
#[sqlx::test]
async fn a_deactivated_creator_gets_no_reminder(db: PgPool) {
    let active = insert_user(&db, "active@example.test").await;
    let group = insert_group(&db, active).await;
    let deactivated = insert_user(&db, "deactivated@example.test").await;
    let theirs = due_reminder(&db, group, deactivated, "Piscine").await;
    let later = due_reminder(&db, group, deactivated, "Plus tard").await;
    sqlx::query(
        "UPDATE scheduled_notifications SET fire_at = now() + interval '1 day' WHERE id = $1",
    )
    .bind(later)
    .execute(&db)
    .await
    .unwrap();
    let mine = due_reminder(&db, group, active, "Dîner").await;
    sqlx::query("UPDATE users SET deactivated_at = now() WHERE id = $1")
        .bind(deactivated)
        .execute(&db)
        .await
        .unwrap();

    let sent = pass(&db).await;
    assert_eq!(
        sent,
        vec![("active@example.test".into(), "Rappel : Dîner".into())]
    );
    assert_eq!(notification(&db, theirs).await, ("failed".into(), 0));
    assert_eq!(notification(&db, later).await, ("pending".into(), 0));
    assert_eq!(notification(&db, mine).await, ("sent".into(), 0));

    sqlx::query("UPDATE users SET deactivated_at = NULL WHERE id = $1")
        .bind(deactivated)
        .execute(&db)
        .await
        .unwrap();
    assert!(pass(&db).await.is_empty());
}

/// A purged creator's event stays with the group, and so do its reminders:
/// none goes to the `deleted-<id>@deleted.invalid` address the purge left.
#[sqlx::test]
async fn a_purged_creator_gets_no_reminder(db: PgPool) {
    let active = insert_user(&db, "active@example.test").await;
    let group = insert_group(&db, active).await;
    let purged = insert_user(&db, "purged@example.test").await;
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'standard'::group_role)",
    )
    .bind(group)
    .bind(purged)
    .execute(&db)
    .await
    .unwrap();
    let theirs = due_reminder(&db, group, purged, "Piscine").await;
    let mine = due_reminder(&db, group, active, "Dîner").await;
    sqlx::query(
        "UPDATE users SET deletion_requested_at = now() - interval '31 days' WHERE id = $1",
    )
    .bind(purged)
    .execute(&db)
    .await
    .unwrap();
    purge_account(&db, purged).await.unwrap();
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(purged)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(email, format!("deleted-{purged}@deleted.invalid"));

    let sent = pass(&db).await;
    assert_eq!(
        sent,
        vec![("active@example.test".into(), "Rappel : Dîner".into())]
    );
    assert_eq!(notification(&db, theirs).await, ("failed".into(), 0));
    assert_eq!(notification(&db, mine).await, ("sent".into(), 0));
}
