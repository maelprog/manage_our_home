mod common;

use sqlx::PgPool;
use uuid::Uuid;

/// AC #15 requires RLS to hold even against an application bug (a missing
/// scoping clause). That's only a meaningful test if the connection isn't
/// a superuser/table owner, since both bypass RLS regardless of `FORCE
/// ROW LEVEL SECURITY` — which is exactly the trap a naive test suite
/// falls into by running everything as the migration-owning role. We
/// create a throwaway non-superuser role per test, grant it row access,
/// and `SET LOCAL ROLE` to it for the duration of one transaction so RLS
/// is genuinely enforced regardless of what role ran the migrations.
async fn restricted_role_tx(db: &PgPool) -> (String, sqlx::Transaction<'_, sqlx::Postgres>) {
    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON group_members, invitations, groups TO {role}"
    )))
    .execute(db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();
    (role, tx)
}

async fn drop_restricted_role(db: &PgPool, role: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON group_members, invitations, groups FROM {role}"
    )))
    .execute(db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(db)
        .await
        .unwrap();
}

#[sqlx::test]
async fn group_members_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('a@example.test', 'x', 'A', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('b@example.test', 'x', 'B', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')",
        group_a,
        owner_a
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')",
        group_b,
        owner_b
    )
    .execute(&db)
    .await
    .unwrap();

    let (role, mut tx) = restricted_role_tx(&db).await;

    // No SET LOCAL app.family_id at all: the policy's current_setting(...,
    // true) is NULL, so `group_id::text = NULL` is never true for anyone —
    // zero rows visible, group A and B both included.
    let visible_with_no_scope: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM group_members")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible_with_no_scope.is_empty(),
        "rows leaked with no app.family_id set"
    );

    // Scoped to group A: group B's rows must not appear even though the
    // query itself has no WHERE clause (simulated missing app-code scoping).
    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible_rows: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM group_members")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible_rows, vec![group_a]);

    let cross_tenant_write =
        sqlx::query("UPDATE group_members SET role = 'admin' WHERE group_id = $1 AND user_id = $2")
            .bind(group_b)
            .bind(owner_b)
            .execute(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        cross_tenant_write.rows_affected(),
        0,
        "cross-tenant write must affect zero rows"
    );
    tx.commit().await.unwrap();

    drop_restricted_role(&db, &role).await;
}

#[sqlx::test]
async fn invitations_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('c@example.test', 'x', 'C', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('d@example.test', 'x', 'D', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let expires = chrono::Utc::now() + chrono::Duration::days(7);
    sqlx::query!(
        "INSERT INTO invitations (group_id, created_by, expires_at) VALUES ($1, $2, $3)",
        group_b,
        owner_b,
        expires
    )
    .execute(&db)
    .await
    .unwrap();

    let (role, mut tx) = restricted_role_tx(&db).await;

    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM invitations")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible.is_empty(),
        "group A's scope must not see group B's invitations"
    );

    // An incorrect app.family_id (neither A nor B) must also see nothing.
    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(Uuid::new_v4().to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible_wrong_scope: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM invitations")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(visible_wrong_scope.is_empty());

    tx.commit().await.unwrap();
    drop_restricted_role(&db, &role).await;
}

/// AC #15 analog for the Agenda epic: `events` isolation must hold even
/// against a bug that forgets `WHERE group_id = ...` in application code.
#[sqlx::test]
async fn events_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('e@example.test', 'x', 'E', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('f@example.test', 'x', 'F', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let starts_at = chrono::Utc::now();
    let ends_at = starts_at + chrono::Duration::hours(1);
    sqlx::query!(
        "INSERT INTO events (group_id, created_by, title, starts_at, ends_at) VALUES ($1, $2, 'secret B event', $3, $4)",
        group_b,
        owner_b,
        starts_at,
        ends_at,
    )
    .execute(&db)
    .await
    .unwrap();

    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON events TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();

    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM events")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible.is_empty(),
        "group A's scope must not see group B's events"
    );

    tx.commit().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON events FROM {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&db)
        .await
        .unwrap();
}

/// AC #15 analog for the Stocks epic: `stock_items` isolation must hold even
/// against a bug that forgets `WHERE group_id = ...` in application code.
#[sqlx::test]
async fn stock_items_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('g@example.test', 'x', 'G', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('h@example.test', 'x', 'H', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO stock_items (group_id, created_by, name, quantity) VALUES ($1, $2, 'secret B item', 1)",
        group_b,
        owner_b,
    )
    .execute(&db)
    .await
    .unwrap();

    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON stock_items TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();

    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM stock_items")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible.is_empty(),
        "group A's scope must not see group B's stock items"
    );

    tx.commit().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON stock_items FROM {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&db)
        .await
        .unwrap();
}

/// AC #15 analog for the Recipes epic: `recipes` isolation must hold even
/// against a bug that forgets `WHERE group_id = ...` in application code.
#[sqlx::test]
async fn recipes_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('i@example.test', 'x', 'I', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('j@example.test', 'x', 'J', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO recipes (group_id, created_by, name) VALUES ($1, $2, 'secret B recipe')",
        group_b,
        owner_b,
    )
    .execute(&db)
    .await
    .unwrap();

    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON recipes TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();

    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM recipes")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible.is_empty(),
        "group A's scope must not see group B's recipes"
    );

    tx.commit().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON recipes FROM {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&db)
        .await
        .unwrap();
}

/// AC #15 analog for the Grocery-list epic: `grocery_items` isolation must
/// hold even against a bug that forgets `WHERE group_id = ...` in
/// application code.
#[sqlx::test]
async fn grocery_items_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('k@example.test', 'x', 'K', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('l@example.test', 'x', 'L', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_items (group_id, created_by, name) VALUES ($1, $2, 'secret B item')",
        group_b,
        owner_b,
    )
    .execute(&db)
    .await
    .unwrap();

    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON grocery_items TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();

    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM grocery_items")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible.is_empty(),
        "group A's scope must not see group B's grocery items"
    );

    tx.commit().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON grocery_items FROM {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&db)
        .await
        .unwrap();
}

/// AC #5 for Messagerie: `messages` isolation must hold even against a bug
/// that forgets `WHERE group_id = ...` in application code, same pattern as
/// every other RLS-scoped table.
#[sqlx::test]
async fn messages_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('msg-rls-a@example.test', 'x', 'A', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('msg-rls-b@example.test', 'x', 'B', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO messages (group_id, created_by, content) VALUES ($1, $2, pgp_sym_encrypt('secret B message', 'test-key'))",
        group_b,
        owner_b,
    )
    .execute(&db)
    .await
    .unwrap();

    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON messages TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();

    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM messages")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible.is_empty(),
        "group A's scope must not see group B's messages"
    );

    tx.commit().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON messages FROM {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&db)
        .await
        .unwrap();
}

/// AC #15 analog for the Budget epic: `budget_entries` isolation must hold
/// even against a bug that forgets `WHERE group_id = ...` in application code.
#[sqlx::test]
async fn budget_entries_isolated_without_scoping(db: PgPool) {
    let owner_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('m@example.test', 'x', 'M', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let owner_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('n@example.test', 'x', 'N', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_a: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('A', $1) RETURNING id",
        owner_a
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_b: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('B', $1) RETURNING id",
        owner_b
    )
    .fetch_one(&db)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO budget_entries (group_id, created_by, name, amount_cents) VALUES ($1, $2, 'secret B entry', 100)",
        group_b,
        owner_b,
    )
    .execute(&db)
    .await
    .unwrap();

    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON budget_entries TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();

    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();

    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let visible: Vec<Uuid> = sqlx::query_scalar("SELECT group_id FROM budget_entries")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(
        visible.is_empty(),
        "group A's scope must not see group B's budget entries"
    );

    tx.commit().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON budget_entries FROM {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {role}")))
        .execute(&db)
        .await
        .unwrap();
}
