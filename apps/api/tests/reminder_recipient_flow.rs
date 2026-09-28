//! End-to-end cover for who an event reminder goes to (#291), against a
//! real database.
//!
//! A reminder goes to the event's creator. A creator support deactivated
//! keeps its address for the two years its account is kept, and a purged
//! one has it rewritten to `deleted-<id>@deleted.invalid`: neither must be
//! written to. Their due reminders are retired instead of left pending, so
//! a reactivation does not bring back a backlog of stale reminders and a
//! purged creator's are not read again on every pass.
//!
//! The pass reads `scheduled_notifications` and `events` across every
//! family, both under forced RLS: it runs on the `BYPASSRLS` admin pool,
//! and refuses any other role rather than find nothing due (#293).

mod common;

use common::{drop_prescribed_role, prescribed_role_pool};
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

/// On the runtime role (`NOSUPERUSER NOBYPASSRLS`), forced RLS hides every
/// due notification from the pass: it would find nothing, send nothing and
/// report success. It refuses instead, and leaves the queue as it was
/// (#293).
#[sqlx::test]
async fn the_pass_refuses_a_role_that_does_not_bypass_rls(db: PgPool) {
    let active = insert_user(&db, "active@example.test").await;
    let group = insert_group(&db, active).await;
    let due = due_reminder(&db, group, active, "Dîner").await;
    let (role, app_db) = prescribed_role_pool(&db).await;

    let sent = Mutex::new(Vec::<String>::new());
    let record = |to: String, _subject: String, _body: String| {
        sent.lock().unwrap().push(to);
        async { Ok::<(), anyhow::Error>(()) }
    };
    let result = send_due_notifications(&app_db, &record).await;
    drop_prescribed_role(&db, app_db, &role).await;

    // The whole chain names this pass's tables, not another guard's.
    let error = format!("{:#}", result.expect_err("the pass must refuse"));
    assert!(error.contains("scheduled_notifications"), "{error}");
    assert!(!error.contains("event_attachments"), "{error}");
    assert!(sent.into_inner().unwrap().is_empty());
    assert_eq!(notification(&db, due).await, ("pending".into(), 0));
    // The same queue, on the harness role, which bypasses RLS: the
    // reminder was there to send.
    assert_eq!(pass(&db).await.len(), 1);
}

/// A creator deactivated while the pass is sending is not written to: its
/// account is read again right before its own reminder goes, not once for
/// the whole pass. Its reminder is retired without an attempt.
#[sqlx::test]
async fn a_creator_deactivated_during_the_pass_gets_no_reminder(db: PgPool) {
    let first = insert_user(&db, "first@example.test").await;
    let group = insert_group(&db, first).await;
    let second = insert_user(&db, "second@example.test").await;
    let firsts = due_reminder(&db, group, first, "Dîner").await;
    let seconds = due_reminder(&db, group, second, "Piscine").await;

    // Whichever creator is sent to first, the other is deactivated before
    // the pass reaches its reminder.
    let sent = Mutex::new(Vec::<String>::new());
    let record = |to: String, _subject: String, _body: String| {
        let other = if to == "first@example.test" {
            second
        } else {
            first
        };
        sent.lock().unwrap().push(to);
        let db = db.clone();
        async move {
            sqlx::query("UPDATE users SET deactivated_at = now() WHERE id = $1")
                .bind(other)
                .execute(&db)
                .await?;
            Ok::<(), anyhow::Error>(())
        }
    };
    send_due_notifications(&db, &record).await.unwrap();
    let sent = sent.into_inner().unwrap();

    assert_eq!(sent.len(), 1, "{sent:?}");
    let (gone, kept) = if sent[0] == "first@example.test" {
        (seconds, firsts)
    } else {
        (firsts, seconds)
    };
    assert_eq!(notification(&db, kept).await, ("sent".into(), 0));
    assert_eq!(notification(&db, gone).await, ("failed".into(), 0));
    let last_error: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM scheduled_notifications WHERE id = $1")
            .bind(gone)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        last_error.as_deref(),
        Some("recipient account deactivated or purged")
    );
}
