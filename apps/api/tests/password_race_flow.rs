//! #988: a password reset racing a password change, a password set or
//! another reset of the same account. Each of them writes `users` and
//! deletes the account's `password_reset_tokens`; taken in different orders,
//! those locks deadlocked and one of the two requests answered 500.
//!
//! The race is not left to chance. The test holds a lock on a third reset
//! link of the account, the first in the table, which every one of these
//! writes deletes: the first request parks on it, the second is let in
//! behind it, and the link is released with both in flight — the window in
//! which their locks used to cross.

mod common;

use axum::http::{Method, StatusCode};
use common::{call, insert_session, lock_waiters, test_router, wait_for_lock_waiters};
use manage_our_home::auth::terms_acceptance::terms_in_force_now;
use manage_our_home::auth::token::new_token;
use sqlx::PgPool;
use uuid::Uuid;

/// Replays of each scenario, a fresh account each time.
const REPLAYS: usize = 10;

const CURRENT_PASSWORD: &str = "current-password-988";

/// An account, with a password or Google-only, and a session of it.
async fn account(db: &PgPool, email: &str, with_password: bool) -> (Uuid, String) {
    let password_hash =
        with_password.then(|| manage_our_home::crypto::hash_password(CURRENT_PASSWORD).unwrap());
    // Google-only: the identity goes in the same transaction, since `users`
    // refuses a row with no auth method (deferred trigger).
    #[allow(clippy::disallowed_methods)]
    let mut tx = db.begin().await.unwrap();
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, display_name, email_verified, password_hash,
                            age_declared_at, terms_accepted_version, terms_accepted_at)
         VALUES ($1, 'Race', true, $2, now(), $3, now())
         RETURNING id",
    )
    .bind(email)
    .bind(password_hash)
    .bind(terms_in_force_now())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    if !with_password {
        sqlx::query(
            "INSERT INTO oauth_identities (user_id, provider, provider_user_id)
             VALUES ($1, 'google', $2)",
        )
        .bind(user_id)
        .bind(format!("google-{email}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let cookie = insert_session(db, user_id).await;
    (user_id, cookie)
}

/// A live reset link of `user_id`; its token.
async fn reset_link(db: &PgPool, user_id: Uuid) -> String {
    let token = new_token();
    sqlx::query(
        "INSERT INTO password_reset_tokens (user_id, expires_at, token_hash)
         VALUES ($1, now() + interval '1 hour', $2)",
    )
    .bind(user_id)
    .bind(&token.hash()[..])
    .execute(db)
    .await
    .unwrap();
    token.value().to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Write {
    Reset,
    Change,
    Set,
}

/// One write of the account: `token` is the link a reset follows.
async fn send(router: axum::Router, write: Write, cookie: String, token: String) -> StatusCode {
    let (path, cookie, body) = match write {
        Write::Reset => (
            "/auth/password/reset",
            None,
            serde_json::json!({"token": token, "new_password": "reset-password-988"}),
        ),
        Write::Change => (
            "/settings/password/change",
            Some(cookie),
            serde_json::json!({
                "current_password": CURRENT_PASSWORD,
                "new_password": "changed-password-988",
            }),
        ),
        Write::Set => (
            "/settings/password/set",
            Some(cookie),
            serde_json::json!({"new_password": "set-password-988"}),
        ),
    };
    call(&router, Method::POST, path, cookie.as_deref(), Some(body))
        .await
        .status()
}

/// What a write that lost the race answers: its refusal, never a 500.
fn refusal(write: Write) -> StatusCode {
    match write {
        // The winner deleted every link of the account.
        Write::Reset => StatusCode::NOT_FOUND,
        // The current password is no longer the one the request gave.
        Write::Change => StatusCode::UNAUTHORIZED,
        // A password is set now.
        Write::Set => StatusCode::CONFLICT,
    }
}

/// `first` then `second` on one account, crossing as described at the top.
async fn race(db: &PgPool, first: Write, second: Write) {
    for replay in 0..REPLAYS {
        let router = test_router(db.clone());
        let email = format!("race-{first:?}-{second:?}-{replay}@example.test").to_lowercase();
        let with_password = first != Write::Set && second != Write::Set;
        let (user_id, cookie) = account(db, &email, with_password).await;
        // First in the table, so that each write's delete reaches it before
        // the links the resets follow.
        let blocker = reset_link(db, user_id).await;
        let first_token = reset_link(db, user_id).await;
        let second_token = reset_link(db, user_id).await;

        #[allow(clippy::disallowed_methods)]
        let mut holder = db.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM password_reset_tokens WHERE token_hash = $1 FOR UPDATE")
            .bind(&manage_our_home::auth::token::token_hash(&blocker).unwrap()[..])
            .execute(&mut *holder)
            .await
            .unwrap();
        assert_eq!(lock_waiters(db, None).await, 0);

        let a = tokio::spawn(send(router.clone(), first, cookie.clone(), first_token));
        wait_for_lock_waiters(db, None, 1).await;
        let b = tokio::spawn(send(router.clone(), second, cookie.clone(), second_token));
        wait_for_lock_waiters(db, None, 2).await;
        holder.rollback().await.unwrap();

        let (a, b) = (a.await.unwrap(), b.await.unwrap());
        let outcome = format!("{first:?} -> {a}, {second:?} -> {b} (replay {replay})");
        assert!(
            (a == StatusCode::OK && b == refusal(second))
                || (b == StatusCode::OK && a == refusal(first)),
            "{outcome}"
        );
    }
}

#[sqlx::test]
async fn a_change_then_a_reset_do_not_deadlock(db: PgPool) {
    race(&db, Write::Change, Write::Reset).await;
}

#[sqlx::test]
async fn a_reset_then_a_change_do_not_deadlock(db: PgPool) {
    race(&db, Write::Reset, Write::Change).await;
}

#[sqlx::test]
async fn a_set_then_a_reset_do_not_deadlock(db: PgPool) {
    race(&db, Write::Set, Write::Reset).await;
}

#[sqlx::test]
async fn a_reset_then_a_set_do_not_deadlock(db: PgPool) {
    race(&db, Write::Reset, Write::Set).await;
}

#[sqlx::test]
async fn two_resets_with_two_links_do_not_deadlock(db: PgPool) {
    race(&db, Write::Reset, Write::Reset).await;
}
