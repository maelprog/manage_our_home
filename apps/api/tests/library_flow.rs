//! Issue #905: the shared recipe library's schema (0066) under row-level
//! security. Every check runs as a throwaway `NOSUPERUSER NOBYPASSRLS`
//! role, as `rls.rs` does: a superuser or the tables' owner would bypass
//! every policy and prove nothing.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const TABLES: &str = "library_recipes, library_ingredients, library_recipe_publishers";

async fn insert_user(db: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, email_verified)
         VALUES ($1, 'x', $1, true) RETURNING id",
    )
    .bind(email)
    .fetch_one(db)
    .await
    .unwrap()
}

/// A family owned by `owner`, holding one recipe of theirs: `(group, recipe)`.
async fn family_with_recipe(db: &PgPool, owner: Uuid, recipe: &str) -> (Uuid, Uuid) {
    let group: Uuid =
        sqlx::query_scalar("INSERT INTO groups (name, created_by) VALUES ('F', $1) RETURNING id")
            .bind(owner)
            .fetch_one(db)
            .await
            .unwrap();
    sqlx::query("INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(group)
        .bind(owner)
        .execute(db)
        .await
        .unwrap();
    let recipe: Uuid = sqlx::query_scalar(
        "INSERT INTO recipes (group_id, created_by, name) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(group)
    .bind(owner)
    .bind(recipe)
    .fetch_one(db)
    .await
    .unwrap();
    (group, recipe)
}

async fn restricted_role(db: &PgPool) -> String {
    let role = format!("app_test_role_{}", Uuid::new_v4().simple());
    for statement in [
        format!("CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON {TABLES} TO {role}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(db)
            .await
            .unwrap();
    }
    role
}

/// A transaction as `role`, for `user` scoped to `group` as a request is
/// (`scoped_tx`); `None` sets neither variable.
async fn as_user<'a>(
    db: &'a PgPool,
    role: &str,
    scope: Option<(Uuid, Uuid)>,
) -> Transaction<'a, Postgres> {
    #[allow(clippy::disallowed_methods)]
    let mut tx = db.begin().await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut *tx)
        .await
        .unwrap();
    if let Some((user, group)) = scope {
        for (name, value) in [("app.user_id", user), ("app.group_id", group)] {
            sqlx::query("SELECT set_config($1, $2, true)")
                .bind(name)
                .bind(value.to_string())
                .execute(&mut *tx)
                .await
                .unwrap();
        }
    }
    tx
}

/// Publishes a snapshot as the caller of `tx`, the way the API will: the
/// publication, its publisher row, its ingredients. Returns its id.
async fn publish(tx: &mut Transaction<'_, Postgres>, user: Uuid, source: Uuid) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO library_recipes (name, instructions, servings, duration_minutes)
         VALUES ('Tarte aux poireaux', 'Cuire.', 4, 45) RETURNING id",
    )
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_recipe_publishers (library_recipe_id, user_id, source_recipe_id)
         VALUES ($1, $2, $3)",
    )
    .bind(id)
    .bind(user)
    .bind(source)
    .execute(&mut **tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_ingredients (library_recipe_id, position, name, quantity, unit,
                                          seasonal_months)
         VALUES ($1, 0, 'Poireau', 3, NULL, '{10,11,12}'), ($1, 1, 'Pâte brisée', 1, NULL, NULL)",
    )
    .bind(id)
    .execute(&mut **tx)
    .await
    .unwrap();
    id
}

/// The columns of `table`, in order.
async fn columns(db: &PgPool, table: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = $1 ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(db)
    .await
    .unwrap()
}

/// Another family reads a publication, its ingredients and their seasons;
/// nothing it can read leads back to the family or the person it came
/// from. The two public tables hold no such column and no key onto a table
/// that has one but the publisher table, whose rows another user cannot
/// see. A `SELECT *` — the widest read query there is — is checked column
/// by column.
#[sqlx::test]
async fn another_family_reads_a_publication_that_names_no_one(db: PgPool) {
    let author = insert_user(&db, "author@example.test").await;
    let reader = insert_user(&db, "reader@example.test").await;
    let (author_group, recipe) = family_with_recipe(&db, author, "Tarte").await;
    let (reader_group, _) = family_with_recipe(&db, reader, "Soupe").await;
    let role = restricted_role(&db).await;

    let mut tx = as_user(&db, &role, Some((author, author_group))).await;
    let published = publish(&mut tx, author, recipe).await;
    tx.commit().await.unwrap();

    let public = [
        (
            "library_recipes",
            vec![
                "id",
                "name",
                "instructions",
                "servings",
                "duration_minutes",
                "published_at",
                "updated_at",
            ],
        ),
        (
            "library_ingredients",
            vec![
                "library_recipe_id",
                "position",
                "name",
                "quantity",
                "unit",
                "is_optional",
                "seasonal_months",
            ],
        ),
    ];
    for (table, expected) in &public {
        assert_eq!(&columns(&db, table).await, expected, "{table}");
    }
    // Their keys lead nowhere but to the library: no key onto `users`,
    // `groups`, `group_members` or `recipes`.
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT conrelid::regclass::text || ' -> ' || confrelid::regclass::text
         FROM pg_constraint
         WHERE contype = 'f'
           AND conrelid IN ('library_recipes'::regclass, 'library_ingredients'::regclass)
         ORDER BY 1",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(
        keys,
        vec![
            "library_ingredients -> library_recipes",
            "library_recipes -> library_recipe_publishers",
        ]
    );

    let mut tx = as_user(&db, &role, Some((reader, reader_group))).await;
    let rows: Vec<(Uuid, String, Option<i32>)> =
        sqlx::query_as("SELECT id, name, servings FROM library_recipes")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![(published, "Tarte aux poireaux".into(), Some(4))]
    );
    let everything = sqlx::query("SELECT * FROM library_recipes")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(everything.len(), 1);
    use sqlx::{Column, Row};
    let names: Vec<&str> = everything[0].columns().iter().map(|c| c.name()).collect();
    assert_eq!(names, public[0].1);
    let ingredients: Vec<(String, Option<Vec<i32>>)> = sqlx::query_as(
        "SELECT name, seasonal_months FROM library_ingredients
         WHERE library_recipe_id = $1 ORDER BY position",
    )
    .bind(published)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        ingredients,
        vec![
            ("Poireau".to_string(), Some(vec![10, 11, 12])),
            ("Pâte brisée".to_string(), None),
        ]
    );
    // Who published it, and from which recipe: hidden from the reader.
    let publishers: i64 = sqlx::query_scalar("SELECT count(*) FROM library_recipe_publishers")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(publishers, 0);
    tx.rollback().await.unwrap();

    // The author sees their own publisher row.
    let mut tx = as_user(&db, &role, Some((author, author_group))).await;
    let own: Vec<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT library_recipe_id, user_id, source_recipe_id FROM library_recipe_publishers",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(own, vec![(published, author, Some(recipe))]);
    tx.rollback().await.unwrap();

    // Not signed in: nothing.
    let mut tx = as_user(&db, &role, None).await;
    for table in ["library_recipes", "library_ingredients"] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table} without app.user_id");
    }
    tx.rollback().await.unwrap();
}

/// Another user can neither edit nor delete a publication, nor touch its
/// ingredients, nor claim it as theirs; its publisher can do all of it.
#[sqlx::test]
async fn only_the_publisher_edits_or_deletes_a_publication(db: PgPool) {
    let author = insert_user(&db, "author@example.test").await;
    let other = insert_user(&db, "other@example.test").await;
    let (author_group, recipe) = family_with_recipe(&db, author, "Tarte").await;
    let (other_group, _) = family_with_recipe(&db, other, "Soupe").await;
    let role = restricted_role(&db).await;

    let mut tx = as_user(&db, &role, Some((author, author_group))).await;
    let published = publish(&mut tx, author, recipe).await;
    tx.commit().await.unwrap();

    // Statements that touch nothing: RLS hides the rows from the write.
    for statement in [
        "UPDATE library_recipes SET name = 'Volée' WHERE id = $1",
        "DELETE FROM library_recipes WHERE id = $1",
        "UPDATE library_ingredients SET name = 'Volé' WHERE library_recipe_id = $1",
        "DELETE FROM library_ingredients WHERE library_recipe_id = $1",
        "DELETE FROM library_recipe_publishers WHERE library_recipe_id = $1",
    ] {
        let mut tx = as_user(&db, &role, Some((other, other_group))).await;
        let done = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(published)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(done.rows_affected(), 0, "{statement}");
        tx.commit().await.unwrap();
    }
    // Statements that are refused outright.
    for (statement, binds) in [
        (
            "INSERT INTO library_ingredients (library_recipe_id, position, name)
             VALUES ($1, 2, 'Sel')",
            vec![published],
        ),
        // Its publisher row holds the key already.
        (
            "INSERT INTO library_recipe_publishers (library_recipe_id, user_id)
             VALUES ($1, $2)",
            vec![published, other],
        ),
        // Nor can a publisher row name someone else.
        (
            "WITH l AS (INSERT INTO library_recipes (name) VALUES ('Autre') RETURNING id)
             INSERT INTO library_recipe_publishers (library_recipe_id, user_id)
             SELECT id, $1 FROM l",
            vec![author],
        ),
    ] {
        let mut tx = as_user(&db, &role, Some((other, other_group))).await;
        let mut query = sqlx::query(sqlx::AssertSqlSafe(statement));
        for bind in binds {
            query = query.bind(bind);
        }
        let refused = query.execute(&mut *tx).await.unwrap_err();
        // Refused by a policy or a key, not for a malformed statement.
        let code = refused
            .as_database_error()
            .and_then(|e| e.code())
            .map(|c| c.into_owned());
        assert!(
            matches!(code.as_deref(), Some("42501" | "23505")),
            "{statement}: {refused}"
        );
        tx.rollback().await.unwrap();
    }
    let untouched: (String, i64) = sqlx::query_as(
        "SELECT name, (SELECT count(*) FROM library_ingredients WHERE library_recipe_id = l.id)
         FROM library_recipes l WHERE id = $1",
    )
    .bind(published)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(untouched, ("Tarte aux poireaux".to_string(), 2));

    // The publisher edits, then withdraws: the ingredients and the
    // publisher row go with it.
    let mut tx = as_user(&db, &role, Some((author, author_group))).await;
    for (statement, affected) in [
        ("UPDATE library_recipes SET name = 'Tarte' WHERE id = $1", 1),
        (
            "UPDATE library_ingredients SET quantity = 4 WHERE library_recipe_id = $1 AND position = 0",
            1,
        ),
        ("DELETE FROM library_recipes WHERE id = $1", 1),
    ] {
        let done = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(published)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(done.rows_affected(), affected, "{statement}");
    }
    tx.commit().await.unwrap();
    for table in TABLES.split(", ") {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}

/// A publication without its publisher row cannot be committed: nobody
/// could edit it, and the account purge, which finds publications through
/// that row, would miss it.
#[sqlx::test]
async fn a_publication_without_a_publisher_is_refused_at_commit(db: PgPool) {
    let author = insert_user(&db, "author@example.test").await;
    let (group, _) = family_with_recipe(&db, author, "Tarte").await;
    let role = restricted_role(&db).await;

    let mut tx = as_user(&db, &role, Some((author, group))).await;
    sqlx::query("INSERT INTO library_recipes (name) VALUES ('Orpheline')")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(tx.commit().await.is_err());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM library_recipes")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// A family's copy of a publication keeps its recipe when the publication
/// goes — withdrawn by its publisher, or by the superadmin through the
/// admin pool, which bypasses RLS (here, the harness's own role) — its
/// link dropping to NULL. A publication outlives its source recipe.
#[sqlx::test]
async fn a_copy_and_a_publication_outlive_each_others_source(db: PgPool) {
    let author = insert_user(&db, "author@example.test").await;
    let reader = insert_user(&db, "reader@example.test").await;
    let (author_group, recipe) = family_with_recipe(&db, author, "Tarte").await;
    let (_, copy) = family_with_recipe(&db, reader, "Tarte aux poireaux").await;
    let role = restricted_role(&db).await;

    let mut tx = as_user(&db, &role, Some((author, author_group))).await;
    let published = publish(&mut tx, author, recipe).await;
    tx.commit().await.unwrap();
    sqlx::query("UPDATE recipes SET library_recipe_id = $1 WHERE id = $2")
        .bind(published)
        .bind(copy)
        .execute(&db)
        .await
        .unwrap();

    // The source recipe goes: the publication stays, its link to it gone.
    sqlx::query("DELETE FROM recipes WHERE id = $1")
        .bind(recipe)
        .execute(&db)
        .await
        .unwrap();
    let source: Option<Uuid> = sqlx::query_scalar(
        "SELECT source_recipe_id FROM library_recipe_publishers WHERE library_recipe_id = $1",
    )
    .bind(published)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(source, None);

    // The superadmin withdraws it: the copy stays.
    sqlx::query("DELETE FROM library_recipes WHERE id = $1")
        .bind(published)
        .execute(&db)
        .await
        .unwrap();
    let link: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT library_recipe_id FROM recipes WHERE id = $1")
            .bind(copy)
            .fetch_optional(&db)
            .await
            .unwrap();
    assert_eq!(link, Some(None));
}
