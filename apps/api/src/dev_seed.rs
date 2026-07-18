//! Dev-only database seed, run at startup when `DEV_SEED_USERS=true`
//! (see `main.rs`). Local stacks point SMTP at Mailpit rather than a real
//! relay, so these pre-verified accounts let you log in without completing
//! the email-verification flow after each `docker compose down -v`.
//!
//! Deliberately uses runtime-checked queries (`sqlx::query`, not the
//! `query!` macros): this path never runs in production and keeping it
//! macro-free means no `.sqlx` offline-data regeneration when it changes.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

pub const DEV_USERS: &[(&str, &str)] = &[
    ("alice.dev@example.test", "Alice Dev"),
    ("bob.dev@example.test", "Bob Dev"),
];
pub const DEV_PASSWORD: &str = "devpassword";
pub const DEV_GROUP_NAME: &str = "Foyer Dev";

/// Inserts the dev users (already `email_verified`) plus a shared group —
/// Alice owner, Bob standard. Idempotent: if the first dev user already
/// exists, the whole seed is skipped, so it never duplicates the group or
/// fights the one-owner-per-group index on re-boot.
pub async fn seed_dev_users(db: &PgPool) -> Result<()> {
    let already_seeded: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(DEV_USERS[0].0)
        .fetch_optional(db)
        .await?;
    if already_seeded.is_some() {
        tracing::info!("dev seed: users already present, skipping");
        return Ok(());
    }

    let mut tx = db.begin().await?;

    let mut user_ids = Vec::with_capacity(DEV_USERS.len());
    for (email, display_name) in DEV_USERS {
        // Hashed per user: reusing one hash string would make the shared
        // dev password obvious from a DB dump diff, and hashing twice at
        // boot costs nothing.
        let password_hash = crate::crypto::hash_password(DEV_PASSWORD)?;
        let id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO users (email, password_hash, display_name, email_verified)
            VALUES ($1, $2, $3, true)
            RETURNING id
            "#,
        )
        .bind(email)
        .bind(password_hash)
        .bind(display_name)
        .fetch_one(&mut *tx)
        .await?;
        user_ids.push(id);
    }

    // `groups`/`group_members` are under FORCE RLS; their policies check
    // `app.family_id`/`app.user_id` (see migration 0001 and
    // auth/session.rs). Generating the group id client-side lets us satisfy
    // the WITH CHECK on the very INSERT that creates the group.
    let group_id = Uuid::new_v4();
    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_id.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(user_ids[0].to_string())
        .execute(&mut *tx)
        .await?;

    sqlx::query("INSERT INTO groups (id, name, created_by) VALUES ($1, $2, $3)")
        .bind(group_id)
        .bind(DEV_GROUP_NAME)
        .bind(user_ids[0])
        .execute(&mut *tx)
        .await?;

    sqlx::query("INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(group_id)
        .bind(user_ids[0])
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'standard')")
        .bind(group_id)
        .bind(user_ids[1])
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    tracing::info!(
        "dev seed: created {} users (password: {DEV_PASSWORD}) and group '{DEV_GROUP_NAME}'",
        DEV_USERS.len()
    );
    Ok(())
}
