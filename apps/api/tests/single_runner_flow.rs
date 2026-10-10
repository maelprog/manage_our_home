//! Background jobs run in one process at a time (#426), against a real
//! database: several replicas share it, and each one starts every loop.
//!
//! "Another replica" is a second pass on the same database, or a
//! connection of the test holding the job's lock. The reminder passes are
//! driven with recording senders, as in `push_reminders_flow`: no email
//! leaves.

mod common;

use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use manage_our_home::jobs::retention_purge::{scheduled_pass, RetentionCutoffs};
use manage_our_home::jobs::scheduled_notifications::send_pass;
use manage_our_home::jobs::single_runner::{exclusively, Job};
use manage_our_home::notifications::push::PushOutcome;
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

// -- reminders ------------------------------------------------------------------

/// A member on the email channel, with one event of theirs and its
/// reminder's notification due a minute ago. Returns the notification.
async fn due_reminder(db: &PgPool, email: &str) -> Uuid {
    let by: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, age_declared_at, reminder_channel)
         VALUES ($1, 'not-a-real-hash', $1, now() - interval '1 year', 'email')
         RETURNING id",
    )
    .bind(email)
    .fetch_one(db)
    .await
    .unwrap();
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Maison', $1) RETURNING id",
    )
    .bind(by)
    .fetch_one(db)
    .await
    .unwrap();
    let event: Uuid = sqlx::query_scalar(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at)
         VALUES ($1, $2, 'IRM', now() + interval '9 minutes', now() + interval '1 hour')
         RETURNING id",
    )
    .bind(group)
    .bind(by)
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

const MEMBERS: [&str; 3] = ["a@example.test", "b@example.test", "c@example.test"];

async fn three_due_reminders(db: &PgPool) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for member in MEMBERS {
        ids.push(due_reminder(db, member).await);
    }
    ids
}

async fn statuses(db: &PgPool, ids: &[Uuid]) -> Vec<String> {
    sqlx::query_scalar("SELECT status FROM scheduled_notifications WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(db)
        .await
        .unwrap()
}

fn sorted(emails: &Mutex<Vec<String>>) -> Vec<String> {
    let mut emails = emails.lock().unwrap().clone();
    emails.sort();
    emails
}

/// Nobody here is on the push channel.
async fn no_push(_endpoint: String, _ttl: i64) -> PushOutcome {
    PushOutcome::Failed("no push expected".into())
}

/// The second pass starts while the first one is sending its first email,
/// with every reminder still pending: before #426 it read them all again
/// and sent each one a second time.
#[sqlx::test]
async fn a_reminder_pass_started_during_another_sends_nothing(db: PgPool) {
    let ids = three_due_reminders(&db).await;
    let emails = Mutex::new(Vec::new());
    let entered = Notify::new();
    let gate = Semaphore::new(0);

    let held = |to: String, _subject: String, _body: String| {
        emails.lock().unwrap().push(to);
        entered.notify_one();
        let gate = &gate;
        async move {
            gate.acquire().await.unwrap().forget();
            Ok::<(), anyhow::Error>(())
        }
    };
    let free = |to: String, _subject: String, _body: String| {
        emails.lock().unwrap().push(to);
        async { Ok::<(), anyhow::Error>(()) }
    };

    let (first, second) = tokio::join!(send_pass(&db, held, no_push), async {
        entered.notified().await;
        let second = send_pass(&db, free, no_push).await;
        gate.add_permits(MEMBERS.len());
        second
    });

    assert_eq!(first.unwrap(), Some(()));
    assert_eq!(second.unwrap(), None, "the second pass ran");
    assert_eq!(sorted(&emails), MEMBERS);
    assert_eq!(statuses(&db, &ids).await, ["sent", "sent", "sent"]);
}

/// Two loops on one database, as two replicas run them: each reminder goes
/// out exactly once, whichever loop sends it.
#[sqlx::test]
async fn two_reminder_loops_on_one_database_send_each_reminder_once(db: PgPool) {
    let ids = three_due_reminders(&db).await;
    let emails = Mutex::new(Vec::new());
    let send = |to: String, _subject: String, _body: String| {
        emails.lock().unwrap().push(to);
        // Long enough for both loops' first passes to read the due rows
        // before either marks one: at 20 ms, the time each takes to open
        // its connections kept them apart and the test passed without any
        // lock.
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<(), anyhow::Error>(())
        }
    };
    let replica = || async {
        for _ in 0..5 {
            send_pass(&db, send, no_push).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };

    tokio::join!(replica(), replica());

    assert_eq!(sorted(&emails), MEMBERS);
    assert_eq!(statuses(&db, &ids).await, ["sent", "sent", "sent"]);
}

// -- purges ---------------------------------------------------------------------

/// A connection of the test holding `job`'s lock, as another replica in
/// the middle of its pass does. Returns it with its backend pid.
async fn hold(db: &PgPool, job: Job) -> (PoolConnection<Postgres>, i32) {
    let mut conn = db.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(job.lock_key())
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert!(held, "{job:?} was already held");
    (conn, pid)
}

async fn release(mut conn: PoolConnection<Postgres>, job: Job) {
    let released: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(job.lock_key())
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert!(released);
}

/// An audit line past its six months: the retention pass deletes it.
async fn expired_audit_line(db: &PgPool) {
    sqlx::query(
        "INSERT INTO audit_log (occurred_at, action, target_type, target_id)
         VALUES (now() - interval '7 months', 'test', 'test', 'old')",
    )
    .execute(db)
    .await
    .unwrap();
}

async fn audit_lines(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE target_id = 'old'")
        .fetch_one(db)
        .await
        .unwrap()
}

/// While another replica runs the retention purge, this one skips its
/// pass and deletes nothing; once that one is done, the next tick runs.
#[sqlx::test]
async fn a_purge_another_process_is_running_is_skipped(db: PgPool) {
    expired_audit_line(&db).await;
    let (other, _) = hold(&db, Job::RetentionPurge).await;

    let skipped = scheduled_pass(&db, RetentionCutoffs::at(Utc::now())).await;
    assert_eq!(skipped.unwrap(), None);
    assert_eq!(audit_lines(&db).await, 1, "the skipped pass deleted");

    release(other, Job::RetentionPurge).await;
    let ran = scheduled_pass(&db, RetentionCutoffs::at(Utc::now())).await;
    assert_eq!(ran.unwrap().unwrap().audit_log, 1);
    assert_eq!(audit_lines(&db).await, 0);
}

/// A replica whose lock connection is cut — the process killed, the
/// network gone — does not hold the job back: Postgres releases the lock
/// with the session, and the next tick of another replica takes it.
#[sqlx::test]
async fn a_lock_whose_connection_was_cut_is_taken_on_the_next_tick(db: PgPool) {
    expired_audit_line(&db).await;
    let (mut other, pid) = hold(&db, Job::RetentionPurge).await;
    assert_eq!(
        scheduled_pass(&db, RetentionCutoffs::at(Utc::now()))
            .await
            .unwrap(),
        None
    );

    let cut: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(cut);
    // The pool must not hand the dead connection out again.
    other.close_on_drop();
    drop(other);
    wait_for_backend_gone(&db, pid).await;

    let ran = scheduled_pass(&db, RetentionCutoffs::at(Utc::now())).await;
    assert_eq!(ran.unwrap().unwrap().audit_log, 1);
    assert_eq!(audit_lines(&db).await, 0);
}

/// `pg_terminate_backend` only signals: the session and its locks go a
/// moment later.
async fn wait_for_backend_gone(db: &PgPool, pid: i32) {
    for _ in 0..100 {
        let alive: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)")
                .bind(pid)
                .fetch_one(db)
                .await
                .unwrap();
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("backend {pid} still alive");
}

// -- every job ------------------------------------------------------------------

/// Each job is held back by its own lock only, and leaves it free once its
/// pass is done: the next pass of any process runs.
#[sqlx::test]
async fn each_job_waits_on_its_own_lock_and_frees_it(db: PgPool) {
    for job in Job::ALL {
        let (other, _) = hold(&db, job).await;
        for each in Job::ALL {
            let ran = exclusively(&db, each, async { Ok(()) }).await.unwrap();
            assert_eq!(ran.is_some(), each != job, "{each:?} while {job:?} is held");
        }
        release(other, job).await;
        for _ in 0..2 {
            let ran = exclusively(&db, job, async { Ok(()) }).await.unwrap();
            assert_eq!(ran, Some(()), "{job:?} after its holder released it");
        }
    }
}

/// A pass that fails still frees its lock.
#[sqlx::test]
async fn a_failed_pass_frees_its_lock(db: PgPool) {
    let failed = exclusively(&db, Job::AccountPurge, async {
        Err::<(), _>(anyhow::anyhow!("pass failed"))
    })
    .await;
    assert!(failed.is_err());
    let (other, _) = hold(&db, Job::AccountPurge).await;
    release(other, Job::AccountPurge).await;
}
