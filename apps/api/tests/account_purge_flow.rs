//! End-to-end cover for the account purge (#139, #255), against a real
//! database.
//!
//! The purge anonymises the `users` row instead of deleting it, so no
//! `ON DELETE CASCADE` towards `users(id)` ever fires: every personal row
//! has to be deleted by name. What only a real database can show is that
//! each `DELETE` takes the purged account's rows, leaves another member's
//! alone, and leaves the content shared with the group in place.

mod common;

use chrono::Utc;
use common::{drop_prescribed_role, prescribed_role_pool};
use manage_our_home::jobs::account_purge::{
    purge_account, purge_due_accounts, send_deactivation_notices, DEACTIVATION_NOTICE_SUBJECT,
};
use sqlx::PgPool;
use uuid::Uuid;

/// A user whose deletion was requested `requested_days_ago` days ago
/// (`None`: no request), with an age declaration on file.
async fn insert_user(db: &PgPool, email: &str, requested_days_ago: Option<i64>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, age_declared_at,
                            deletion_requested_at)
         VALUES ($1, 'not-a-real-hash', $1, now() - interval '1 year',
                 now() - make_interval(days => $2::int))
         RETURNING id",
    )
    .bind(email)
    .bind(requested_days_ago.map(|d| d as i32))
    .fetch_one(db)
    .await
    .unwrap()
}

async fn insert_group(db: &PgPool, name: &str, owner: Uuid) -> Uuid {
    let group: Uuid =
        sqlx::query_scalar("INSERT INTO groups (name, created_by) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(owner)
            .fetch_one(db)
            .await
            .unwrap();
    add_member(db, group, owner, "owner").await;
    group
}

async fn add_member(db: &PgPool, group: Uuid, user: Uuid, role: &str) {
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, $3::group_role)",
    )
    .bind(group)
    .bind(user)
    .bind(role)
    .execute(db)
    .await
    .unwrap();
}

async fn insert_event(db: &PgPool, group: Uuid, by: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at)
         VALUES ($1, $2, 'Dîner', now(), now() + interval '1 hour') RETURNING id",
    )
    .bind(group)
    .bind(by)
    .fetch_one(db)
    .await
    .unwrap()
}

/// Every personal row the purge must remove, for `user` in `group`.
async fn seed_personal_rows(db: &PgPool, group: Uuid, event: Uuid, user: Uuid, tag: &str) {
    for sql in [
        "INSERT INTO oauth_identities (user_id, provider, provider_user_id)
         VALUES ($1, 'google', $2)",
        "INSERT INTO sessions (user_id, expires_at, token_hash)
         VALUES ($1, now() + interval '1 day', gen_random_bytes(32))",
        "INSERT INTO email_verification_tokens (user_id, expires_at)
         VALUES ($1, now() + interval '1 day')",
        "INSERT INTO password_reset_tokens (user_id, expires_at)
         VALUES ($1, now() + interval '1 hour')",
        "INSERT INTO audit_log (actor_user_id, action, target_type, target_id)
         VALUES ($1, 'account_data_exported', 'user', $1::text)",
        "INSERT INTO push_subscriptions (user_id, endpoint)
         VALUES ($1, 'https://fcm.googleapis.com/fcm/send/' || $2)",
    ] {
        let query = sqlx::query(sql).bind(user);
        let query = if sql.contains("$2") {
            query.bind(format!("google-sub-{tag}"))
        } else {
            query
        };
        query.execute(db).await.unwrap();
    }
    sqlx::query("INSERT INTO message_read_state (group_id, user_id) VALUES ($1, $2)")
        .bind(group)
        .bind(user)
        .execute(db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO event_assignees (event_id, user_id) VALUES ($1, $2)")
        .bind(event)
        .bind(user)
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO invitations (group_id, invited_email, created_by, expires_at)
         VALUES ($1, $2, $3, now() + interval '7 days')",
    )
    .bind(group)
    .bind(format!("tiers-{tag}@example.test"))
    .bind(user)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO calendar_imports (group_id, created_by, label, feed_url)
         VALUES ($1, $2, 'Agenda', '\\x00'::bytea)",
    )
    .bind(group)
    .bind(user)
    .execute(db)
    .await
    .unwrap();
}

/// Rows per personal table that still point at `user`.
async fn personal_rows(db: &PgPool, user: Uuid) -> Vec<(&'static str, i64)> {
    let mut counts = Vec::new();
    for (table, sql) in [
        (
            "oauth_identities",
            "SELECT count(*) FROM oauth_identities WHERE user_id = $1",
        ),
        (
            "sessions",
            "SELECT count(*) FROM sessions WHERE user_id = $1",
        ),
        (
            "email_verification_tokens",
            "SELECT count(*) FROM email_verification_tokens WHERE user_id = $1",
        ),
        (
            "password_reset_tokens",
            "SELECT count(*) FROM password_reset_tokens WHERE user_id = $1",
        ),
        (
            "group_members",
            "SELECT count(*) FROM group_members WHERE user_id = $1",
        ),
        (
            "message_read_state",
            "SELECT count(*) FROM message_read_state WHERE user_id = $1",
        ),
        (
            "event_assignees",
            "SELECT count(*) FROM event_assignees WHERE user_id = $1",
        ),
        (
            "audit_log",
            "SELECT count(*) FROM audit_log WHERE actor_user_id = $1",
        ),
        (
            "invitations",
            "SELECT count(*) FROM invitations WHERE created_by = $1",
        ),
        (
            "calendar_imports",
            "SELECT count(*) FROM calendar_imports WHERE created_by = $1",
        ),
        (
            "push_subscriptions",
            "SELECT count(*) FROM push_subscriptions WHERE user_id = $1",
        ),
    ] {
        let n: i64 = sqlx::query_scalar(sql)
            .bind(user)
            .fetch_one(db)
            .await
            .unwrap();
        counts.push((table, n));
    }
    counts
}

fn all(n: i64) -> Vec<(&'static str, i64)> {
    [
        "oauth_identities",
        "sessions",
        "email_verification_tokens",
        "password_reset_tokens",
        "group_members",
        "message_read_state",
        "event_assignees",
        "audit_log",
        "invitations",
        "calendar_imports",
        "push_subscriptions",
    ]
    .into_iter()
    .map(|t| (t, n))
    .collect()
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct UserRow {
    email: String,
    display_name: String,
    password_hash: Option<String>,
    age_declared_at: Option<chrono::DateTime<Utc>>,
    deleted_at: Option<chrono::DateTime<Utc>>,
}

async fn user_row(db: &PgPool, user: Uuid) -> UserRow {
    sqlx::query_as(
        "SELECT email, display_name, password_hash, age_declared_at, deleted_at
         FROM users WHERE id = $1",
    )
    .bind(user)
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test]
async fn the_purge_deletes_every_personal_row_and_keeps_the_shared_content(db: PgPool) {
    let owner = insert_user(&db, "owner@example.test", None).await;
    let leaving = insert_user(&db, "leaving@example.test", Some(31)).await;
    let staying = insert_user(&db, "staying@example.test", None).await;
    let group = insert_group(&db, "Famille", owner).await;
    add_member(&db, group, leaving, "admin").await;
    add_member(&db, group, staying, "standard").await;

    // Shared content: one event and one message by the purged account.
    let own_event = insert_event(&db, group, leaving).await;
    let message: Uuid = sqlx::query_scalar(
        "INSERT INTO messages (group_id, created_by, content)
         VALUES ($1, $2, '\\x00'::bytea) RETURNING id",
    )
    .bind(group)
    .bind(leaving)
    .fetch_one(&db)
    .await
    .unwrap();
    let shared_event = insert_event(&db, group, owner).await;

    seed_personal_rows(&db, group, shared_event, leaving, "leaving").await;
    seed_personal_rows(&db, group, shared_event, staying, "staying").await;

    purge_due_accounts(&db).await.unwrap();

    let mut expected = all(0);
    assert_eq!(personal_rows(&db, leaving).await, expected);
    // Another member's rows are untouched (one group_members row each).
    expected = all(1);
    assert_eq!(personal_rows(&db, staying).await, expected);

    let row = user_row(&db, leaving).await;
    assert_eq!(row.email, format!("deleted-{leaving}@deleted.invalid"));
    assert_eq!(row.display_name, "Utilisateur supprimé");
    assert_eq!(row.password_hash, None);
    assert_eq!(
        row.age_declared_at, None,
        "the age declaration is erased too"
    );
    assert!(row.deleted_at.is_some());

    // The content shared with the group stays, still pointing at the
    // anonymised row.
    for (table, id) in [("events", own_event), ("messages", message)] {
        let by: Uuid = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT created_by FROM {table} WHERE id = $1"
        )))
        .bind(id)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(by, leaving, "{table}");
    }

    // The purge itself leaves one trace, with no actor.
    let purged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log
         WHERE action = 'account_purged' AND target_id = $1 AND actor_user_id IS NULL",
    )
    .bind(leaving.to_string())
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(purged, 1);
}

/// The grace period is 30 days: a request one day short of it is not due.
#[sqlx::test]
async fn an_account_still_in_its_grace_period_is_left_alone(db: PgPool) {
    let owner = insert_user(&db, "owner@example.test", None).await;
    let pending = insert_user(&db, "pending@example.test", Some(29)).await;
    let group = insert_group(&db, "Famille", owner).await;
    add_member(&db, group, pending, "standard").await;
    let event = insert_event(&db, group, owner).await;
    seed_personal_rows(&db, group, event, pending, "pending").await;

    purge_due_accounts(&db).await.unwrap();

    assert_eq!(personal_rows(&db, pending).await, all(1));
    assert_eq!(user_row(&db, pending).await.deleted_at, None);
}

async fn add_member_joined(db: &PgPool, group: Uuid, user: Uuid, role: &str, days_ago: i32) {
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role, joined_at)
         VALUES ($1, $2, $3::group_role, now() - make_interval(days => $4))",
    )
    .bind(group)
    .bind(user)
    .bind(role)
    .bind(days_ago)
    .execute(db)
    .await
    .unwrap();
}

/// `(user_id, role)` of every member of `group`, ordered by user id.
async fn members(db: &PgPool, group: Uuid) -> Vec<(Uuid, String)> {
    sqlx::query_as(
        "SELECT user_id, role::text FROM group_members WHERE group_id = $1 ORDER BY user_id",
    )
    .bind(group)
    .fetch_all(db)
    .await
    .unwrap()
}

fn sorted(mut v: Vec<(Uuid, String)>) -> Vec<(Uuid, String)> {
    v.sort();
    v
}

/// `delete_account` refuses an owner, but an account can still become one
/// during its grace period, by creating a group or being handed one. The
/// purge hands the group to its longest-standing admin — here the admin who
/// joined after an older standard member — and purges the account.
#[sqlx::test]
async fn a_purged_owner_hands_the_group_to_its_longest_standing_admin(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let elder = insert_user(&db, "elder@example.test", None).await;
    let admin_old = insert_user(&db, "admin-old@example.test", None).await;
    let admin_new = insert_user(&db, "admin-new@example.test", None).await;
    let group = insert_group(&db, "Famille", owning).await;
    add_member_joined(&db, group, elder, "standard", 90).await;
    add_member_joined(&db, group, admin_old, "admin", 60).await;
    add_member_joined(&db, group, admin_new, "admin", 30).await;
    let event = insert_event(&db, group, owning).await;
    seed_personal_rows(&db, group, event, owning, "owning").await;

    purge_due_accounts(&db).await.unwrap();

    assert_eq!(personal_rows(&db, owning).await, all(0));
    assert!(user_row(&db, owning).await.deleted_at.is_some());
    assert_eq!(
        members(&db, group).await,
        sorted(vec![
            (elder, "standard".into()),
            (admin_old, "owner".into()),
            (admin_new, "admin".into()),
        ])
    );
    let transfers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log
         WHERE action = 'ownership_transferred' AND target_id = $1
           AND actor_user_id IS NULL AND metadata->>'new_owner_id' = $2",
    )
    .bind(group.to_string())
    .bind(admin_old.to_string())
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(transfers, 1);
}

/// With no admin left, the longest-standing member inherits.
#[sqlx::test]
async fn without_an_admin_the_longest_standing_member_inherits(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let recent = insert_user(&db, "recent@example.test", None).await;
    let elder = insert_user(&db, "elder@example.test", None).await;
    let group = insert_group(&db, "Famille", owning).await;
    add_member_joined(&db, group, recent, "standard", 10).await;
    add_member_joined(&db, group, elder, "standard", 40).await;

    purge_due_accounts(&db).await.unwrap();

    assert!(user_row(&db, owning).await.deleted_at.is_some());
    assert_eq!(
        members(&db, group).await,
        sorted(vec![(recent, "standard".into()), (elder, "owner".into())])
    );
}

async fn deactivate(db: &PgPool, user: Uuid) {
    deactivate_since(db, user, "0 days", None).await;
}

/// Support deactivation, `ago` in the past (a Postgres interval), with the
/// purge warning sent `notice_days_ago` days ago (`None`: not sent).
async fn deactivate_since(db: &PgPool, user: Uuid, ago: &str, notice_days_ago: Option<i32>) {
    sqlx::query(
        "UPDATE users SET deactivated_at = now() - $2::interval,
                          deactivation_notice_sent_at = now() - make_interval(days => $3)
         WHERE id = $1",
    )
    .bind(user)
    .bind(ago)
    .bind(notice_days_ago)
    .execute(db)
    .await
    .unwrap();
}

/// An admin support deactivated never inherits, and an admin whose own
/// deletion is pending comes after every active member: here the active
/// admin inherits, ahead of both and of an older standard member.
#[sqlx::test]
async fn an_active_admin_inherits_ahead_of_deactivated_and_pending_admins(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let disabled = insert_user(&db, "disabled@example.test", None).await;
    let pending = insert_user(&db, "pending@example.test", Some(5)).await;
    let elder = insert_user(&db, "elder@example.test", None).await;
    let heir = insert_user(&db, "heir@example.test", None).await;
    deactivate(&db, disabled).await;
    let group = insert_group(&db, "Famille", owning).await;
    add_member_joined(&db, group, disabled, "admin", 90).await;
    add_member_joined(&db, group, pending, "admin", 80).await;
    add_member_joined(&db, group, elder, "standard", 70).await;
    add_member_joined(&db, group, heir, "admin", 10).await;

    purge_due_accounts(&db).await.unwrap();

    assert_eq!(
        members(&db, group).await,
        sorted(vec![
            (disabled, "admin".into()),
            (pending, "admin".into()),
            (elder, "standard".into()),
            (heir, "owner".into()),
        ])
    );
}

/// With only deactivated members left, nobody inherits. A deactivated
/// account keeps its membership (the purge only removes the purged
/// account's own rows), and the group is left without an owner. A group
/// made only of accounts purged in the same pass ends with no member.
#[sqlx::test]
async fn with_no_eligible_member_nobody_inherits(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let disabled = insert_user(&db, "disabled@example.test", None).await;
    deactivate(&db, disabled).await;
    let group = insert_group(&db, "Famille", owning).await;
    add_member_joined(&db, group, disabled, "admin", 90).await;

    let owning_too = insert_user(&db, "owning-too@example.test", Some(31)).await;
    let leaving = insert_user(&db, "leaving@example.test", Some(40)).await;
    let emptied = insert_group(&db, "Départ", owning_too).await;
    add_member_joined(&db, emptied, leaving, "admin", 90).await;

    purge_due_accounts(&db).await.unwrap();

    assert!(user_row(&db, owning).await.deleted_at.is_some());
    assert_eq!(members(&db, group).await, vec![(disabled, "admin".into())]);
    assert!(user_row(&db, owning_too).await.deleted_at.is_some());
    assert!(user_row(&db, leaving).await.deleted_at.is_some());
    assert_eq!(members(&db, emptied).await, vec![]);
}

/// The only admin awaiting its own deletion — it can still cancel it —
/// inherits ahead of a member also awaiting deletion, even an older one;
/// after cancelling, it is a live owner.
#[sqlx::test]
async fn an_admin_awaiting_deletion_inherits_and_stays_owner_once_it_cancels(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let admin = insert_user(&db, "admin@example.test", Some(5)).await;
    let member = insert_user(&db, "member@example.test", Some(5)).await;
    let group = insert_group(&db, "Famille", owning).await;
    add_member_joined(&db, group, member, "standard", 90).await;
    add_member_joined(&db, group, admin, "admin", 30).await;

    purge_due_accounts(&db).await.unwrap();

    assert_eq!(
        members(&db, group).await,
        sorted(vec![(admin, "owner".into()), (member, "standard".into())])
    );

    // The admin cancels its request through the real endpoint.
    let session = common::insert_session(&db, admin).await;
    let router = common::test_router(db.clone());
    let cancel = common::call(
        &router,
        axum::http::Method::POST,
        "/account/delete/cancel",
        Some(&session),
        None,
    )
    .await;
    assert!(cancel.status().is_success(), "{}", cancel.status());
    let pending: bool =
        sqlx::query_scalar("SELECT deletion_requested_at IS NOT NULL FROM users WHERE id = $1")
            .bind(admin)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(!pending);
    assert_eq!(
        members(&db, group).await,
        sorted(vec![(admin, "owner".into()), (member, "standard".into())])
    );
}

/// The pass reads the due accounts before purging each one in its own
/// transaction: an account that cancels its request in between, or that
/// the superadmin reactivates in between, is left alone — memberships,
/// rows and identity intact — and the call is not an error.
#[sqlx::test]
async fn an_account_no_longer_due_when_its_turn_comes_is_left_alone(db: PgPool) {
    let owner = insert_user(&db, "owner@example.test", None).await;
    let group = insert_group(&db, "Famille", owner).await;
    let event = insert_event(&db, group, owner).await;
    let cancelled = insert_user(&db, "cancelled@example.test", Some(31)).await;
    let reactivated = insert_user(&db, "reactivated@example.test", None).await;
    deactivate_since(&db, reactivated, "3 years", Some(40)).await;
    for (user, tag) in [(cancelled, "cancelled"), (reactivated, "reactivated")] {
        add_member(&db, group, user, "standard").await;
        seed_personal_rows(&db, group, event, user, tag).await;
    }
    // Selected as due, then: one cancels, the other is reactivated.
    sqlx::query("UPDATE users SET deletion_requested_at = NULL WHERE id = $1")
        .bind(cancelled)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE users SET deactivated_at = NULL, deactivation_notice_sent_at = NULL
         WHERE id = $1",
    )
    .bind(reactivated)
    .execute(&db)
    .await
    .unwrap();

    purge_account(&db, cancelled).await.unwrap();
    purge_account(&db, reactivated).await.unwrap();

    for user in [cancelled, reactivated] {
        assert_eq!(personal_rows(&db, user).await, all(1));
        let row = user_row(&db, user).await;
        assert_ne!(row.display_name, "Utilisateur supprimé");
        assert!(row.age_declared_at.is_some());
        assert_eq!(row.deleted_at, None);
    }
}

/// #139: an account that requested its deletion, then was deactivated by
/// support, is purged at the end of its 30 days like any other — the
/// deactivation used to exclude it for good.
#[sqlx::test]
async fn a_deletion_request_followed_by_a_deactivation_is_purged_at_30_days(db: PgPool) {
    let owner = insert_user(&db, "owner@example.test", None).await;
    let group = insert_group(&db, "Famille", owner).await;
    let event = insert_event(&db, group, owner).await;
    let leaving = insert_user(&db, "leaving@example.test", Some(31)).await;
    deactivate(&db, leaving).await;
    add_member(&db, group, leaving, "standard").await;
    seed_personal_rows(&db, group, event, leaving, "leaving").await;

    purge_due_accounts(&db).await.unwrap();

    assert_eq!(personal_rows(&db, leaving).await, all(0));
    let row = user_row(&db, leaving).await;
    assert_eq!(row.display_name, "Utilisateur supprimé");
    assert!(row.deleted_at.is_some());
}

/// #256: an account left deactivated is purged 2 years after its
/// deactivation once its holder was warned 30 days before — not while the
/// 2 years run, not less than 30 days after a late warning, and, without
/// any warning, not before the 2 years and 30 days.
#[sqlx::test]
async fn a_deactivated_account_is_purged_after_two_years_once_warned(db: PgPool) {
    let owner = insert_user(&db, "owner@example.test", None).await;
    let group = insert_group(&db, "Famille", owner).await;
    let event = insert_event(&db, group, owner).await;
    let cases = [
        ("due", "2 years 1 day", Some(31), true),
        ("young", "1 year 11 months", None, false),
        ("late-warned", "2 years 1 day", Some(10), false),
        ("never-warned", "2 years 10 days", None, false),
        ("never-warned-capped", "2 years 31 days", None, true),
    ];
    let mut users = Vec::new();
    for (tag, ago, notice, _) in cases {
        let user = insert_user(&db, &format!("{tag}@example.test"), None).await;
        deactivate_since(&db, user, ago, notice).await;
        add_member(&db, group, user, "standard").await;
        seed_personal_rows(&db, group, event, user, tag).await;
        users.push(user);
    }

    purge_due_accounts(&db).await.unwrap();

    for ((tag, _, _, purged), user) in cases.iter().zip(users) {
        let row = user_row(&db, user).await;
        assert_eq!(row.deleted_at.is_some(), *purged, "{tag}");
        let expected = if *purged { all(0) } else { all(1) };
        assert_eq!(personal_rows(&db, user).await, expected, "{tag}");
    }
}

/// #256: the warning goes once, to the account's own address, 30 days
/// before the 2 years are up, and only once it has gone is it stamped. An
/// account whose deletion is pending, or that is not yet in its last 30
/// days, gets nothing; a refused email leaves the account to be tried
/// again.
#[sqlx::test]
async fn the_purge_warning_goes_once_to_the_deactivated_account(db: PgPool) {
    let due = insert_user(&db, "due@example.test", None).await;
    deactivate_since(&db, due, "2 years -29 days", None).await;
    let young = insert_user(&db, "young@example.test", None).await;
    deactivate_since(&db, young, "2 years -31 days", None).await;
    let leaving = insert_user(&db, "leaving@example.test", Some(5)).await;
    deactivate_since(&db, leaving, "2 years -29 days", None).await;

    let sent = std::sync::Mutex::new(Vec::<(String, String, String)>::new());
    let record = |to: String, subject: String, body: String| {
        sent.lock().unwrap().push((to, subject, body));
        async { Ok::<(), anyhow::Error>(()) }
    };
    send_deactivation_notices(&db, POLICY_URL, &record)
        .await
        .unwrap();
    send_deactivation_notices(&db, POLICY_URL, &record)
        .await
        .unwrap();

    let sent = sent.into_inner().unwrap();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (to, subject, body) = &sent[0];
    assert_eq!(to, "due@example.test");
    assert_eq!(subject, DEACTIVATION_NOTICE_SUBJECT);
    assert!(body.contains(POLICY_URL), "{body}");
    for (user, stamped) in [(due, true), (young, false), (leaving, false)] {
        let noticed: bool = sqlx::query_scalar(
            "SELECT deactivation_notice_sent_at IS NOT NULL FROM users WHERE id = $1",
        )
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(noticed, stamped);
    }

    // The relay refuses: nothing is stamped, the next pass tries again.
    let refused = insert_user(&db, "refused@example.test", None).await;
    deactivate_since(&db, refused, "2 years -29 days", None).await;
    let refuse = |_: String, _: String, _: String| async { Err(anyhow::anyhow!("relay down")) };
    send_deactivation_notices(&db, POLICY_URL, refuse)
        .await
        .unwrap();
    let noticed: bool = sqlx::query_scalar(
        "SELECT deactivation_notice_sent_at IS NOT NULL FROM users WHERE id = $1",
    )
    .bind(refused)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!noticed);
}

const POLICY_URL: &str = "https://maison.example.org/privacy-policy";

/// A group whose only member is purged stays, with its content, and no
/// member — like the rest of the content shared under it.
#[sqlx::test]
async fn a_sole_member_group_stays_without_members(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let group = insert_group(&db, "Solo", owning).await;
    let event = insert_event(&db, group, owning).await;

    purge_due_accounts(&db).await.unwrap();

    assert!(user_row(&db, owning).await.deleted_at.is_some());
    assert_eq!(members(&db, group).await, vec![]);
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events e JOIN groups g ON g.id = e.group_id WHERE e.id = $1",
    )
    .bind(event)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(kept, 1);
}

/// `group_members`, `message_read_state`, `event_assignees`, `invitations`
/// and `calendar_imports` are under forced RLS policies: on a role that
/// does not bypass them their `DELETE`s would match no row while the
/// account is stamped purged. The pass refuses instead, touching nothing.
#[sqlx::test]
async fn the_purge_refuses_a_role_that_does_not_bypass_rls(db: PgPool) {
    let owner = insert_user(&db, "owner@example.test", None).await;
    let leaving = insert_user(&db, "leaving@example.test", Some(31)).await;
    let group = insert_group(&db, "Famille", owner).await;
    add_member(&db, group, leaving, "standard").await;
    let event = insert_event(&db, group, owner).await;
    seed_personal_rows(&db, group, event, leaving, "leaving").await;
    let (role, app_db) = prescribed_role_pool(&db).await;

    let result = purge_due_accounts(&app_db).await;
    drop_prescribed_role(&db, app_db, &role).await;

    assert!(result.is_err(), "{result:?}");
    assert_eq!(personal_rows(&db, leaving).await, all(1));
    assert_eq!(user_row(&db, leaving).await.deleted_at, None);
}
