//! Issue #905: the shared recipe library's schema (0066) under row-level
//! security. Every check runs as a throwaway `NOSUPERUSER NOBYPASSRLS`
//! role, as `rls.rs` does: a superuser or the tables' owner would bypass
//! every policy and prove nothing.
//!
//! Issue #906: its routes (`recipes::library`), driven through the router.
//! The handlers run as `FLOW_TEST_RUNTIME_ROLE` when it is set (CI job
//! `test`), under the same policies.
//!
//! Issue #907: copying a publication into a family, through the router.
//!
//! Issue #908: reporting a publication, its moderation by the superadmin,
//! and the publications that end with a member's departure or by the
//! family owner's hand.

mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, insert_session, json_body, test_router};
use serde_json::{json, Value};
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

// --- #906: the routes -------------------------------------------------------

/// An account able to use the API (age declared, CGU accepted), and the
/// cookie of a session of it.
async fn api_user(db: &PgPool, email: &str) -> (Uuid, String) {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, email_verified,
                            age_declared_at, terms_accepted_version, terms_accepted_at)
         VALUES ($1, 'not-a-real-hash', $1, true, now(), $2, now())
         RETURNING id",
    )
    .bind(email)
    .bind(manage_our_home::auth::terms_acceptance::terms_in_force_now())
    .fetch_one(db)
    .await
    .unwrap();
    (id, insert_session(db, id).await)
}

/// A family member: their id and session cookie.
struct Member {
    id: Uuid,
    cookie: String,
}

/// A family of an owner, an admin and a standard member, under the default
/// `recipes_edit` threshold (`admin`).
struct Fam {
    id: Uuid,
    owner: Member,
    admin: Member,
    standard: Member,
}

async fn fam(db: &PgPool, prefix: &str) -> Fam {
    let mut members = Vec::new();
    for role in ["owner", "admin", "standard"] {
        let (id, cookie) = api_user(db, &format!("{prefix}-{role}@example.test")).await;
        members.push((role, Member { id, cookie }));
    }
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Foyer', $1) RETURNING id",
    )
    .bind(members[0].1.id)
    .fetch_one(db)
    .await
    .unwrap();
    for (role, m) in &members {
        sqlx::query(
            "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, $3::group_role)",
        )
        .bind(group)
        .bind(m.id)
        .bind(role)
        .execute(db)
        .await
        .unwrap();
    }
    let mut it = members.into_iter().map(|(_, m)| m);
    Fam {
        id: group,
        owner: it.next().unwrap(),
        admin: it.next().unwrap(),
        standard: it.next().unwrap(),
    }
}

/// `cookie`'s member writes a recipe in `group` through the API.
async fn new_recipe(router: &axum::Router, group: Uuid, cookie: &str, body: Value) -> Uuid {
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group}/recipes"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn leek_tart() -> Value {
    json!({
        "name": "Tarte aux poireaux",
        "instructions": "Cuire 45 minutes.",
        "servings": 4,
        "duration_minutes": 45,
        "source_url": "https://example.test/tarte",
        "ingredients": [
            {"name": "Poireau", "quantity": 3.0, "seasonal_months": [12, 10, 11]},
            {"name": "Pâte brisée", "quantity": 1.0, "unit": "rouleau"},
            {"name": "Sel", "is_optional": true, "seasonal_months": []}
        ]
    })
}

fn consent() -> Option<Value> {
    Some(json!({"consent": true}))
}

async fn publish_call(
    router: &axum::Router,
    group: Uuid,
    recipe: Uuid,
    cookie: &str,
    body: Option<Value>,
) -> axum::http::Response<axum::body::Body> {
    call(
        router,
        Method::POST,
        &format!("/groups/{group}/recipes/{recipe}/publish"),
        Some(cookie),
        body,
    )
    .await
}

async fn get(router: &axum::Router, uri: &str, cookie: &str) -> (StatusCode, Value) {
    let res = call(router, Method::GET, uri, Some(cookie), None).await;
    let status = res.status();
    (status, json_body(res).await)
}

async fn library_rows(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM library_recipes")
        .fetch_one(db)
        .await
        .unwrap()
}

/// AC: without `consent`, nothing is published; a member who may not edit
/// the recipe cannot publish it, consent or not.
#[sqlx::test]
async fn publishing_takes_the_consent_and_the_edit_bar(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "bar").await;
    let recipe = new_recipe(&router, f.id, &f.owner.cookie, leek_tart()).await;

    for body in [
        None,
        Some(json!({})),
        Some(json!({"consent": false})),
        Some(json!({"consent": "true"})),
    ] {
        let res = publish_call(&router, f.id, recipe, &f.owner.cookie, body.clone()).await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(res).await["error"],
            "consent_required",
            "{body:?}"
        );
    }
    // Not the creator, under the `admin` threshold.
    let res = publish_call(&router, f.id, recipe, &f.standard.cookie, consent()).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    // Not a member at all.
    let (_, outsider) = api_user(&db, "bar-outsider@example.test").await;
    let res = publish_call(&router, f.id, recipe, &outsider, consent()).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert_eq!(library_rows(&db).await, 0, "nothing published");

    // A recipe of another family, named under this family's path.
    let other = fam(&db, "bar-other").await;
    let foreign = new_recipe(&router, other.id, &other.owner.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, foreign, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    assert_eq!(library_rows(&db).await, 0);

    // The standard member's own recipe: its creator may.
    let own = new_recipe(&router, f.id, &f.standard.cookie, json!({"name": "Soupe"})).await;
    let res = publish_call(&router, f.id, own, &f.standard.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
    // And the admin, on the owner's recipe.
    let res = publish_call(&router, f.id, recipe, &f.admin.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
    assert_eq!(library_rows(&db).await, 2);
}

/// AC: what `GET /library/…` answers holds no id of a family or a person,
/// checked on the whole JSON, and only the publication's own fields.
#[sqlx::test]
async fn the_library_names_neither_the_family_nor_the_person(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "anon").await;
    let recipe = new_recipe(&router, f.id, &f.owner.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
    let published = json_body(res).await;
    let id = published["id"].as_str().unwrap().to_string();

    let (_, reader) = api_user(&db, "anon-reader@example.test").await;
    let (status, list) = get(&router, "/library/recipes", &reader).await;
    assert_eq!(status, StatusCode::OK);
    let (status, detail) = get(&router, &format!("/library/recipes/{id}"), &reader).await;
    assert_eq!(status, StatusCode::OK);

    let forbidden: Vec<String> = [f.id, f.owner.id, f.admin.id, f.standard.id, recipe]
        .iter()
        .flat_map(|u| [u.to_string(), u.simple().to_string()])
        .chain([
            "anon-owner@example.test".to_string(),
            "example.test/tarte".to_string(),
        ])
        .collect();
    for (what, body) in [
        ("publish", &published),
        ("list", &list),
        ("detail", &detail),
    ] {
        let text = body.to_string();
        for needle in &forbidden {
            assert!(
                !text.contains(needle.as_str()),
                "{what} holds {needle}: {text}"
            );
        }
    }

    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&list), ["next_cursor", "recipes"]);
    assert_eq!(list["recipes"].as_array().unwrap().len(), 1);
    assert_eq!(
        keys(&list["recipes"][0]),
        [
            "duration_minutes",
            "id",
            "name",
            "published_at",
            "servings",
            "updated_at"
        ]
    );
    assert_eq!(
        keys(&detail),
        [
            "duration_minutes",
            "id",
            "ingredients",
            "instructions",
            "name",
            "published_at",
            "servings",
            "updated_at"
        ]
    );
    for ingredient in detail["ingredients"].as_array().unwrap() {
        assert_eq!(
            keys(ingredient),
            ["is_optional", "name", "quantity", "seasonal_months", "unit"]
        );
    }
    assert_eq!(detail, published, "the publisher reads what everyone reads");

    // The snapshot: the recipe's order (as the family reads it, by name
    // under the database's collation), months sorted, `[]` read as « all
    // year ».
    assert_eq!(detail["name"], "Tarte aux poireaux");
    assert_eq!(detail["servings"], 4);
    assert_eq!(detail["duration_minutes"], 45);
    let names = |v: &Value| -> Vec<String> {
        v["ingredients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, source) = get(
        &router,
        &format!("/groups/{}/recipes/{recipe}", f.id),
        &f.owner.cookie,
    )
    .await;
    assert_eq!(names(&detail), names(&source));
    let ingredient = |name: &str| {
        detail["ingredients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        ingredient("Poireau")["seasonal_months"],
        json!([10, 11, 12])
    );
    assert_eq!(ingredient("Sel")["seasonal_months"], Value::Null);
    assert_eq!(ingredient("Sel")["is_optional"], true);
    assert_eq!(ingredient("Pâte brisée")["unit"], "rouleau");

    let (status, _) = get(
        &router,
        &format!("/library/recipes/{}", Uuid::new_v4()),
        &reader,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let res = call(&router, Method::GET, "/library/recipes", None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
}

/// AC: withdrawing takes the recipe out of the list at once. Only its
/// publisher withdraws it.
#[sqlx::test]
async fn unpublishing_removes_it_from_the_list_at_once(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "unpub").await;
    let recipe = new_recipe(&router, f.id, &f.owner.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
    let id = json_body(res).await["id"].as_str().unwrap().to_string();
    let (_, reader) = api_user(&db, "unpub-reader@example.test").await;
    let (_, list) = get(&router, "/library/recipes", &reader).await;
    assert_eq!(list["recipes"].as_array().unwrap().len(), 1);

    let uri = format!("/groups/{}/recipes/{recipe}/publish", f.id);
    // Another member, whatever their role, has nothing of theirs to withdraw.
    let res = call(&router, Method::DELETE, &uri, Some(&f.admin.cookie), None).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    assert_eq!(library_rows(&db).await, 1);

    let res = call(&router, Method::DELETE, &uri, Some(&f.owner.cookie), None).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let (_, list) = get(&router, "/library/recipes", &reader).await;
    assert_eq!(list["recipes"], json!([]));
    let (status, _) = get(&router, &format!("/library/recipes/{id}"), &reader).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for table in [
        "library_recipes",
        "library_ingredients",
        "library_recipe_publishers",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let res = call(&router, Method::DELETE, &uri, Some(&f.owner.cookie), None).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    // The recipe itself stays.
    let (status, _) = get(
        &router,
        &format!("/groups/{}/recipes/{recipe}", f.id),
        &f.owner.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Publishing again replaces the snapshot in place; another member's
/// publication of the same recipe is not theirs to replace.
#[sqlx::test]
async fn republishing_replaces_the_snapshot(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "repub").await;
    let recipe = new_recipe(&router, f.id, &f.owner.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
    let first = json_body(res).await;
    let detail_uri = format!("/library/recipes/{}", first["id"].as_str().unwrap());

    let res = call(
        &router,
        Method::PATCH,
        &format!("/groups/{}/recipes/{recipe}", f.id),
        Some(&f.owner.cookie),
        Some(json!({"name": "Tarte fine", "ingredients": [{"name": "Oignon"}]})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let (_, reader) = api_user(&db, "repub-reader@example.test").await;
    let (_, before) = get(&router, &detail_uri, &reader).await;
    assert_eq!(
        before["name"], "Tarte aux poireaux",
        "a snapshot does not follow its recipe"
    );

    let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::OK);
    let second = json_body(res).await;
    assert_eq!(second["id"], first["id"]);
    assert_eq!(second["published_at"], first["published_at"]);
    assert_eq!(second["name"], "Tarte fine");
    assert_eq!(
        second["ingredients"],
        json!([{"name": "Oignon", "quantity": null, "unit": null,
                "is_optional": false, "seasonal_months": null}])
    );
    let (_, after) = get(&router, &detail_uri, &reader).await;
    assert_eq!(after, second);
    assert_eq!(library_rows(&db).await, 1);

    // The admin may edit the recipe, but the publication is the owner's.
    let res = publish_call(&router, f.id, recipe, &f.admin.cookie, consent()).await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "published_by_another_member");
    assert_eq!(library_rows(&db).await, 1);
}

/// A recipe past a bound of the library is refused with a 400, nothing
/// written.
#[sqlx::test]
async fn a_recipe_past_the_library_bounds_is_refused(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "bounds").await;
    let long = new_recipe(
        &router,
        f.id,
        &f.owner.cookie,
        json!({"name": "a".repeat(201)}),
    )
    .await;
    let res = publish_call(&router, f.id, long, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "name_too_long");
    let ingredients: Vec<Value> = (0..101).map(|i| json!({"name": format!("i{i}")})).collect();
    let many = new_recipe(
        &router,
        f.id,
        &f.owner.cookie,
        json!({"name": "Bouillon", "ingredients": ingredients}),
    )
    .await;
    let res = publish_call(&router, f.id, many, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(res).await["error"], "too_many_ingredients");
    assert_eq!(library_rows(&db).await, 0);
}

/// `count` publications by `user` of fresh recipes of `group`, written
/// straight to the tables (the route publishes 10 a minute at most).
/// Returns the recipes.
async fn seed_publications(
    db: &PgPool,
    group: Uuid,
    user: Uuid,
    count: usize,
    name: &str,
) -> Vec<Uuid> {
    let mut recipes = Vec::new();
    for i in 0..count {
        #[allow(clippy::disallowed_methods)]
        let mut tx = db.begin().await.unwrap();
        let recipe: Uuid = sqlx::query_scalar(
            "INSERT INTO recipes (group_id, created_by, name) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(group)
        .bind(user)
        .bind(format!("{name} {i}"))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let id: Uuid =
            sqlx::query_scalar("INSERT INTO library_recipes (name) VALUES ($1) RETURNING id")
                .bind(format!("{name} {i}"))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO library_recipe_publishers
                 (library_recipe_id, user_id, source_recipe_id, group_id)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(user)
        .bind(recipe)
        .bind(group)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        recipes.push(recipe);
    }
    recipes
}

/// At most 50 publications per family, whichever of its members published
/// them: the 51st is refused 409, publishing one of the 50 again is not.
/// Another family's count is its own.
#[sqlx::test]
async fn a_family_holds_fifty_publications_at_most(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "quota").await;
    let seeded = seed_publications(&db, f.id, f.admin.id, 50, "Plat").await;

    let recipe = new_recipe(&router, f.id, &f.owner.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "library_quota_exceeded");
    assert_eq!(library_rows(&db).await, 50);

    let res = publish_call(&router, f.id, seeded[0], &f.admin.cookie, consent()).await;
    assert_status(&res, StatusCode::OK);

    let other = fam(&db, "quota-other").await;
    let theirs = new_recipe(&router, other.id, &other.owner.cookie, leek_tart()).await;
    let res = publish_call(&router, other.id, theirs, &other.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);

    // The count is the family's members' alone, and of `app.group_id`:
    // a number, under the runtime policies. The standard member belongs to
    // both families: scoped to one, they do not count the other.
    sqlx::query("INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'standard')")
        .bind(other.id)
        .bind(f.standard.id)
        .execute(&db)
        .await
        .unwrap();
    let role = restricted_role(&db).await;
    for (user, scope, group, expected) in [
        (f.standard.id, f.id, f.id, 50_i64),
        (other.owner.id, f.id, f.id, 0),
        (other.owner.id, other.id, f.id, 0),
        (f.standard.id, f.id, other.id, 0),
        (f.standard.id, other.id, other.id, 1),
        (other.owner.id, other.id, other.id, 1),
    ] {
        let mut tx = as_user(&db, &role, Some((user, scope))).await;
        let n: i64 = sqlx::query_scalar("SELECT group_library_publication_count($1)")
            .bind(group)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, expected, "{user} in {scope} on {group}");
    }

    // A source recipe deleted, its publication no longer counts.
    sqlx::query("DELETE FROM recipes WHERE id = $1")
        .bind(seeded[1])
        .execute(&db)
        .await
        .unwrap();
    let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
}

/// At the quota, a recipe another member already published is refused as
/// theirs, 409 `published_by_another_member`, not for the quota: the
/// member learns what would still refuse it below 50. Nothing written.
#[sqlx::test]
async fn at_the_quota_another_members_recipe_is_refused_as_theirs(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "qorder").await;
    let seeded = seed_publications(&db, f.id, f.admin.id, 50, "Plat").await;
    let res = publish_call(&router, f.id, seeded[0], &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CONFLICT);
    assert_eq!(json_body(res).await["error"], "published_by_another_member");
    assert_eq!(library_rows(&db).await, 50);
}

/// Search by name and by ingredient, case aside and literally; pages of 20
/// by default, 50 at most, chained by their cursor; a term past 100
/// characters refused.
#[sqlx::test]
async fn the_library_is_searched_and_paged(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "search").await;
    seed_publications(&db, f.id, f.owner.id, 55, "Gratin").await;
    seed_publications(&db, f.id, f.owner.id, 1, "Tarte 100%").await;
    let gratin: Uuid = sqlx::query_scalar("SELECT id FROM library_recipes WHERE name = 'Gratin 7'")
        .fetch_one(&db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO library_ingredients (library_recipe_id, position, name)
         VALUES ($1, 0, 'Poireaux')",
    )
    .bind(gratin)
    .execute(&db)
    .await
    .unwrap();
    let (_, reader) = api_user(&db, "search-reader@example.test").await;

    let (status, page) = get(&router, "/library/recipes", &reader).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["recipes"].as_array().unwrap().len(), 20);
    let (_, page) = get(&router, "/library/recipes?limit=500", &reader).await;
    assert_eq!(
        page["recipes"].as_array().unwrap().len(),
        50,
        "capped, not refused"
    );
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    let (_, rest) = get(
        &router,
        &format!("/library/recipes?limit=50&cursor={cursor}"),
        &reader,
    )
    .await;
    assert_eq!(rest["recipes"].as_array().unwrap().len(), 6);
    assert_eq!(rest["next_cursor"], Value::Null);
    let mut seen: Vec<String> = page["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .chain(rest["recipes"].as_array().unwrap())
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 56, "every publication once across the pages");

    let names = |v: &Value| -> Vec<String> {
        v["recipes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect()
    };
    let none: Vec<String> = Vec::new();
    let (_, hit) = get(&router, "/library/recipes?q=%20gRATIN%2052%20", &reader).await;
    assert_eq!(names(&hit), ["Gratin 52"]);
    let (_, hit) = get(&router, "/library/recipes?q=100%25", &reader).await;
    assert_eq!(names(&hit), ["Tarte 100% 0"], "`%` is taken literally");
    let (_, hit) = get(&router, "/library/recipes?q=_", &reader).await;
    assert_eq!(names(&hit), none, "`_` is taken literally");
    let (_, hit) = get(&router, "/library/recipes?ingredient=POIREAU", &reader).await;
    assert_eq!(names(&hit), ["Gratin 7"]);
    let (_, hit) = get(
        &router,
        "/library/recipes?q=tarte&ingredient=poireau",
        &reader,
    )
    .await;
    assert_eq!(names(&hit), none);
    let (_, all) = get(&router, "/library/recipes?q=&limit=50", &reader).await;
    assert_eq!(
        all["recipes"].as_array().unwrap().len(),
        50,
        "an empty search lists everything"
    );

    let hundred = "é".repeat(100);
    let (status, _) = get(&router, &format!("/library/recipes?q={hundred}"), &reader).await;
    assert_eq!(status, StatusCode::OK, "100 characters, not bytes");
    for query in [format!("q={hundred}a"), format!("ingredient={hundred}a")] {
        let (status, body) = get(&router, &format!("/library/recipes?{query}"), &reader).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["error"], "search_too_long");
    }
    // A cursor chrono reads but `timestamptz` cannot hold: refused, not a
    // database error.
    let far = format!("-8000000000000000000.{}", Uuid::new_v4());
    for query in ["cursor=nope".to_string(), format!("cursor={far}")] {
        let (status, body) = get(&router, &format!("/library/recipes?{query}"), &reader).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["error"], "invalid_cursor");
    }
    for query in ["limit=-1", "limit=0", "limit=abc"] {
        let (status, body) = get(&router, &format!("/library/recipes?{query}"), &reader).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["error"], "invalid_limit", "{query}");
    }
}

/// Publications of one family arriving together at its quota: exactly the
/// quota is published, the rest refused 409. Without the family's lock,
/// they would all read the same count and all be written.
#[sqlx::test]
async fn simultaneous_publications_stop_at_the_quota(db: PgPool) {
    const AT_ONCE: usize = 8;
    const ROOM: usize = 3;
    let router = common::test_router_sized(db.clone(), AT_ONCE as u32 + 2);
    let f = fam(&db, "race").await;
    seed_publications(&db, f.id, f.admin.id, 50 - ROOM, "Plat").await;
    // Each publisher a member of their own recipe: no rate limit shared.
    let mut publishers = Vec::new();
    for i in 0..AT_ONCE {
        let (id, cookie) = api_user(&db, &format!("race-{i}@example.test")).await;
        sqlx::query(
            "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'standard')",
        )
        .bind(f.id)
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
        let recipe = new_recipe(&router, f.id, &cookie, json!({"name": format!("Race {i}")})).await;
        publishers.push((recipe, cookie));
    }

    let statuses = futures::future::join_all(publishers.iter().map(|(recipe, cookie)| {
        let router = router.clone();
        async move {
            publish_call(&router, f.id, *recipe, cookie, consent())
                .await
                .status()
        }
    }))
    .await;
    let created = statuses
        .iter()
        .filter(|s| **s == StatusCode::CREATED)
        .count();
    let refused = statuses
        .iter()
        .filter(|s| **s == StatusCode::CONFLICT)
        .count();
    assert_eq!((created, refused), (ROOM, AT_ONCE - ROOM), "{statuses:?}");
    assert_eq!(library_rows(&db).await, 50);
}

/// 60 reads a minute per user and 10 publications, 429 past them; another
/// user's budget is their own.
#[sqlx::test]
async fn reads_and_publications_are_rate_limited(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "rate").await;
    let (_, reader) = api_user(&db, "rate-reader@example.test").await;
    for i in 0..60 {
        let uri = if i % 2 == 0 {
            "/library/recipes".to_string()
        } else {
            format!("/library/recipes/{}", Uuid::new_v4())
        };
        let res = call(&router, Method::GET, &uri, Some(&reader), None).await;
        assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS, "read {i}");
    }
    let res = call(
        &router,
        Method::GET,
        "/library/recipes",
        Some(&reader),
        None,
    )
    .await;
    assert_status(&res, StatusCode::TOO_MANY_REQUESTS);
    let (status, _) = get(&router, "/library/recipes", &f.owner.cookie).await;
    assert_eq!(status, StatusCode::OK);

    let recipe = new_recipe(&router, f.id, &f.owner.cookie, leek_tart()).await;
    for i in 0..10 {
        let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
        assert!(
            res.status().is_success(),
            "publication {i}: {}",
            res.status()
        );
    }
    let res = publish_call(&router, f.id, recipe, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::TOO_MANY_REQUESTS);
    let res = publish_call(&router, f.id, recipe, &f.admin.cookie, consent()).await;
    assert_status(&res, StatusCode::CONFLICT);
}

// --- #907: copying a publication into a family ------------------------------

async fn copy_call(
    router: &axum::Router,
    group: Uuid,
    library_id: &str,
    cookie: &str,
) -> axum::http::Response<axum::body::Body> {
    call(
        router,
        Method::POST,
        &format!("/groups/{group}/library/recipes/{library_id}/copy"),
        Some(cookie),
        None,
    )
    .await
}

/// `f`'s owner publishes the leek tart: its recipe and the publication's id.
async fn published_leek_tart(router: &axum::Router, f: &Fam) -> (Uuid, String) {
    let recipe = new_recipe(router, f.id, &f.owner.cookie, leek_tart()).await;
    let res = publish_call(router, f.id, recipe, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
    let id = json_body(res).await["id"].as_str().unwrap().to_string();
    (recipe, id)
}

async fn library_link(db: &PgPool, recipe: &str) -> Option<Uuid> {
    sqlx::query_scalar("SELECT library_recipe_id FROM recipes WHERE id = $1::uuid")
        .bind(recipe)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn recipes_of(db: &PgPool, group: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM recipes WHERE group_id = $1")
        .bind(group)
        .fetch_one(db)
        .await
        .unwrap()
}

/// AC: any member copies a publication into their family: a recipe of its
/// own, created by them, linked to the publication, naming neither the
/// family nor the person it was published by. A second copy into the same
/// family is refused 409 `already_copied`; another family copies it too.
#[sqlx::test]
async fn a_member_copies_a_publication_into_their_family(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "src").await;
    let (source, published) = published_leek_tart(&router, &author).await;
    let reader = fam(&db, "dst").await;

    let res = copy_call(&router, reader.id, &published, &reader.standard.cookie).await;
    assert_status(&res, StatusCode::CREATED);
    let copy = json_body(res).await;
    let copy_id = copy["id"].as_str().unwrap().to_string();
    assert_eq!(copy["group_id"], reader.id.to_string());
    assert_eq!(copy["created_by"], reader.standard.id.to_string());
    assert_eq!(copy["name"], "Tarte aux poireaux");
    assert_eq!(copy["instructions"], "Cuire 45 minutes.");
    assert_eq!(copy["servings"], 4);
    assert_eq!(copy["duration_minutes"], 45);
    assert_eq!(copy["source_url"], Value::Null);
    assert_eq!(copy["required_equipment"], json!([]));
    assert_eq!(
        library_link(&db, &copy_id).await,
        Some(published.parse().unwrap())
    );

    // The ingredients, as the publication holds them, read as any recipe's.
    let (_, detail) = get(
        &router,
        &format!("/library/recipes/{published}"),
        &reader.owner.cookie,
    )
    .await;
    let strip = |ingredients: &Value| -> Vec<Value> {
        let mut v: Vec<Value> = ingredients
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                json!({
                    "name": i["name"], "quantity": i["quantity"], "unit": i["unit"],
                    "is_optional": i["is_optional"], "seasonal_months": i["seasonal_months"],
                })
            })
            .collect();
        v.sort_by_key(|i| i["name"].as_str().unwrap().to_string());
        v
    };
    assert_eq!(strip(&copy["ingredients"]), strip(&detail["ingredients"]));
    assert_eq!(copy["ingredients"].as_array().unwrap().len(), 3);
    let (status, read) = get(
        &router,
        &format!("/groups/{}/recipes/{copy_id}", reader.id),
        &reader.owner.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, copy);

    // Nothing of the author's family, its members, its recipe or its URL.
    let text = copy.to_string();
    for needle in [
        author.id,
        author.owner.id,
        author.admin.id,
        author.standard.id,
        source,
    ]
    .iter()
    .flat_map(|u| [u.to_string(), u.simple().to_string()])
    .chain([
        "src-owner@example.test".to_string(),
        "example.test/tarte".to_string(),
    ]) {
        assert!(!text.contains(&needle), "the copy holds {needle}: {text}");
    }

    // Once per family, whoever asks.
    for member in [&reader.owner, &reader.standard] {
        let res = copy_call(&router, reader.id, &published, &member.cookie).await;
        assert_status(&res, StatusCode::CONFLICT);
        assert_eq!(json_body(res).await["error"], "already_copied");
    }
    assert_eq!(recipes_of(&db, reader.id).await, 1);
    // The author's own family may copy it too: a recipe of its own.
    let res = copy_call(&router, author.id, &published, &author.standard.cookie).await;
    assert_status(&res, StatusCode::CREATED);
    assert_eq!(recipes_of(&db, author.id).await, 2);

    // Not a member: refused, nothing written.
    let (_, outsider) = api_user(&db, "dst-outsider@example.test").await;
    let res = copy_call(&router, reader.id, &published, &outsider).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    // No such publication.
    let res = copy_call(
        &router,
        reader.id,
        &Uuid::new_v4().to_string(),
        &reader.owner.cookie,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{}/library/recipes/{published}/copy", reader.id),
        None,
        None,
    )
    .await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(recipes_of(&db, reader.id).await, 1);
}

/// AC: the copy is edited and deleted under the family's usual rules, does
/// not follow its publication, and outlives it.
#[sqlx::test]
async fn a_copy_is_the_familys_own_recipe(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "own-src").await;
    let (source, published) = published_leek_tart(&router, &author).await;
    let f = fam(&db, "own").await;

    let res = copy_call(&router, f.id, &published, &f.admin.cookie).await;
    assert_status(&res, StatusCode::CREATED);
    let copy_id = json_body(res).await["id"].as_str().unwrap().to_string();
    let uri = format!("/groups/{}/recipes/{copy_id}", f.id);

    // Not its creator, under the `admin` threshold: refused.
    let rename = Some(json!({"name": "Tarte de mamie"}));
    let res = call(
        &router,
        Method::PATCH,
        &uri,
        Some(&f.standard.cookie),
        rename.clone(),
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    let res = call(
        &router,
        Method::DELETE,
        &uri,
        Some(&f.standard.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    // Its creator may.
    let res = call(&router, Method::PATCH, &uri, Some(&f.admin.cookie), rename).await;
    assert_status(&res, StatusCode::OK);

    // The publication published again: the copy does not follow.
    let res = call(
        &router,
        Method::PATCH,
        &format!("/groups/{}/recipes/{source}", author.id),
        Some(&author.owner.cookie),
        Some(json!({"name": "Tarte fine", "ingredients": [{"name": "Oignon"}]})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let res = publish_call(&router, author.id, source, &author.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::OK);
    let (_, copy) = get(&router, &uri, &f.owner.cookie).await;
    assert_eq!(copy["name"], "Tarte de mamie");
    assert_eq!(copy["ingredients"].as_array().unwrap().len(), 3);

    // Deleted, the family may copy the publication again.
    let res = call(&router, Method::DELETE, &uri, Some(&f.admin.cookie), None).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let res = copy_call(&router, f.id, &published, &f.standard.cookie).await;
    assert_status(&res, StatusCode::CREATED);
    let again = json_body(res).await;
    assert_eq!(
        again["name"], "Tarte fine",
        "a copy takes the snapshot of now"
    );
    let again_id = again["id"].as_str().unwrap().to_string();

    // The publication withdrawn by its publisher: the copy stays, its link
    // gone, and it can no longer be copied.
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{}/recipes/{source}/publish", author.id),
        Some(&author.owner.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let (status, kept) = get(
        &router,
        &format!("/groups/{}/recipes/{again_id}", f.id),
        &f.standard.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kept, again);
    assert_eq!(library_link(&db, &again_id).await, None);
    let res = copy_call(&router, f.id, &published, &f.owner.cookie).await;
    assert_status(&res, StatusCode::NOT_FOUND);
}

/// AC: a publication withdrawn by moderation (#908: the superadmin's
/// route) can no longer be copied: 404, nothing written.
#[sqlx::test]
async fn a_withdrawn_publication_cannot_be_copied(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "mod-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let admin = superadmin(&db, "mod-admin").await;
    let res = withdraw_call(&router, &published, &admin).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let f = fam(&db, "mod").await;
    let res = copy_call(&router, f.id, &published, &f.owner.cookie).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    assert_eq!(json_body(res).await["error"], "not_found");
    assert_eq!(recipes_of(&db, f.id).await, 0);
}

/// Copies of one publication into one family arriving together: exactly
/// one is made, the others refused 409. Each waits on the family's lock
/// (`COPY_LOCK` of `recipes::library`) before it looks for a copy: held
/// here while they arrive, none writes anything until it is released.
/// Without that lock, each could find no copy and write one.
#[sqlx::test]
async fn simultaneous_copies_make_one(db: PgPool) {
    const AT_ONCE: usize = 8;
    let router = common::test_router_sized(db.clone(), AT_ONCE as u32 + 2);
    let author = fam(&db, "crace-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let f = fam(&db, "crace").await;
    let mut cookies = Vec::new();
    for i in 0..AT_ONCE {
        let (id, cookie) = api_user(&db, &format!("crace-{i}@example.test")).await;
        sqlx::query(
            "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'standard')",
        )
        .bind(f.id)
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
        cookies.push(cookie);
    }

    #[allow(clippy::disallowed_methods)]
    let mut lock = db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(i32::from_be_bytes(*b"mlbc"))
        .bind(f.id.to_string())
        .execute(&mut *lock)
        .await
        .unwrap();
    let group = f.id;
    let copies = tokio::spawn(futures::future::join_all(cookies.into_iter().map(
        move |cookie| {
            let router = router.clone();
            let published = published.clone();
            async move {
                copy_call(&router, group, &published, &cookie)
                    .await
                    .status()
            }
        },
    )));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(recipes_of(&db, f.id).await, 0, "written under the lock");
    assert!(
        !copies.is_finished(),
        "the copies wait on the family's lock"
    );
    lock.commit().await.unwrap();

    let statuses = copies.await.unwrap();
    let created = statuses
        .iter()
        .filter(|s| **s == StatusCode::CREATED)
        .count();
    let refused = statuses
        .iter()
        .filter(|s| **s == StatusCode::CONFLICT)
        .count();
    assert_eq!((created, refused), (1, AT_ONCE - 1), "{statuses:?}");
    assert_eq!(recipes_of(&db, f.id).await, 1);
}

/// A copy counts against the caller's reads of the library: past 60 a
/// minute, refused 429, nothing written; another member's budget is theirs.
#[sqlx::test]
async fn copies_count_with_the_reads(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "crate-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let f = fam(&db, "crate").await;
    for i in 0..60 {
        let (status, _) = get(&router, "/library/recipes", &f.owner.cookie).await;
        assert_eq!(status, StatusCode::OK, "read {i}");
    }
    let res = copy_call(&router, f.id, &published, &f.owner.cookie).await;
    assert_status(&res, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(recipes_of(&db, f.id).await, 0);
    let res = copy_call(&router, f.id, &published, &f.admin.cookie).await;
    assert_status(&res, StatusCode::CREATED);
}

// --- #908: reports, moderation, departures -----------------------------------

/// A superadmin's cookie, its session just opened as a login opens one —
/// the full lifetime ahead, which the `/admin/*` freshness reads.
async fn superadmin(db: &PgPool, prefix: &str) -> String {
    let (id, _) = api_user(db, &format!("{prefix}-moderator@example.test")).await;
    sqlx::query("UPDATE users SET is_superadmin = true WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    let token = manage_our_home::auth::token::new_token();
    sqlx::query(
        "INSERT INTO sessions (user_id, expires_at, token_hash)
         VALUES ($1, now() + make_interval(days => $2), $3)",
    )
    .bind(id)
    .bind(manage_our_home::auth::session::SESSION_TTL_DAYS as i32)
    .bind(&token.hash()[..])
    .execute(db)
    .await
    .unwrap();
    format!("session_id={}", token.value())
}

async fn report_call(
    router: &axum::Router,
    library_id: &str,
    cookie: &str,
    body: Option<Value>,
) -> axum::http::Response<axum::body::Body> {
    call(
        router,
        Method::POST,
        &format!("/library/recipes/{library_id}/report"),
        Some(cookie),
        body,
    )
    .await
}

async fn withdraw_call(
    router: &axum::Router,
    library_id: &str,
    cookie: &str,
) -> axum::http::Response<axum::body::Body> {
    call(
        router,
        Method::POST,
        &format!("/admin/library/recipes/{library_id}/withdraw"),
        Some(cookie),
        None,
    )
    .await
}

/// `(reason, comment, resolution)` of the reports on `library_id`, oldest
/// first.
async fn reports_on(
    db: &PgPool,
    library_id: &str,
) -> Vec<(String, Option<String>, Option<String>)> {
    sqlx::query_as(
        "SELECT reason, comment, resolution FROM library_reports
         WHERE library_recipe_id = $1::uuid ORDER BY created_at, id",
    )
    .bind(library_id)
    .fetch_all(db)
    .await
    .unwrap()
}

async fn audit_actions(db: &PgPool, target: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE target_id = $1 ORDER BY occurred_at, id")
        .bind(target)
        .fetch_all(db)
        .await
        .unwrap()
}

/// AC: a report takes a reason from the closed list and a comment of 500
/// characters at most; the same user reporting the same publication again
/// is answered as the first time and changes nothing; an unknown
/// publication is a 404.
#[sqlx::test]
async fn a_report_is_made_once_per_user(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "rep-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let (_, reader) = api_user(&db, "rep-reader@example.test").await;

    for (body, code) in [
        (None, "invalid_reason"),
        (Some(json!({})), "invalid_reason"),
        (Some(json!({"reason": "abuse"})), "invalid_reason"),
        (Some(json!({"reason": "Spam"})), "invalid_reason"),
        (
            Some(json!({"reason": "spam", "comment": "x".repeat(501)})),
            "comment_too_long",
        ),
    ] {
        let res = report_call(&router, &published, &reader, body.clone()).await;
        assert_status(&res, StatusCode::BAD_REQUEST);
        assert_eq!(json_body(res).await["error"], code, "{body:?}");
    }
    assert_eq!(reports_on(&db, &published).await, vec![]);

    let res = report_call(
        &router,
        &published,
        &reader,
        Some(json!({"reason": "copyright", "comment": "  Recopiée d'un livre.  "})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let res = report_call(
        &router,
        &published,
        &reader,
        Some(json!({"reason": "spam", "comment": "é".repeat(500)})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        reports_on(&db, &published).await,
        vec![(
            "copyright".to_string(),
            Some("Recopiée d'un livre.".to_string()),
            None
        )]
    );

    // Another user's report is their own.
    let res = report_call(
        &router,
        &published,
        &author.standard.cookie,
        Some(json!({"reason": "other", "comment": "   "})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(reports_on(&db, &published).await.len(), 2);
    assert_eq!(reports_on(&db, &published).await[1].1, None);

    let res = report_call(
        &router,
        &Uuid::new_v4().to_string(),
        &reader,
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
}

/// AC: the reporter is revealed to no one. Under the runtime policies, the
/// publisher reads no report of their publication and their export holds
/// none; the reporter reads and exports their own; the superadmin's list
/// names no reporter.
#[sqlx::test]
async fn the_reporter_is_revealed_to_no_one(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "anon-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let (reader_id, reader) = api_user(&db, "anon-reader@example.test").await;
    let res = report_call(
        &router,
        &published,
        &reader,
        Some(json!({"reason": "hateful", "comment": "Propos injurieux."})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);

    let role = restricted_role(&db).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON library_reports TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    for (user, expected) in [(author.owner.id, 0_i64), (reader_id, 1)] {
        let mut tx = as_user(&db, &role, Some((user, author.id))).await;
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM library_reports")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, expected, "{user}");
        // Nor does the runtime role close or delete a report.
        let touched = sqlx::query("UPDATE library_reports SET resolution = 'dismissed'")
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected()
            + sqlx::query("DELETE FROM library_reports")
                .execute(&mut *tx)
                .await
                .unwrap()
                .rows_affected();
        assert_eq!(touched, 0, "{user}");
    }

    let (status, export) = get(&router, "/account/export", &author.owner.cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(export["library_reports"], json!([]));
    assert!(!export.to_string().contains("Propos injurieux"));
    let (status, export) = get(&router, "/account/export", &reader).await;
    assert_eq!(status, StatusCode::OK);
    let mine = export["library_reports"].as_array().unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["reason"], "hateful");
    assert_eq!(mine[0]["library_recipe_id"], published.as_str());

    let admin = superadmin(&db, "anon-admin").await;
    let (status, list) = get(&router, "/admin/library/reports", &admin).await;
    assert_eq!(status, StatusCode::OK);
    let reports = list["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["library_recipe_name"], "Tarte aux poireaux");
    assert_eq!(reports[0]["comment"], "Propos injurieux.");
    assert_eq!(reports[0]["resolution"], Value::Null);
    let mut keys: Vec<&String> = reports[0].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "closed_at",
            "comment",
            "created_at",
            "id",
            "library_recipe_id",
            "library_recipe_name",
            "reason",
            "resolution"
        ]
    );
    assert!(!list.to_string().contains(&reader_id.to_string()));
}

/// AC: a user who is not the superadmin is refused every
/// `/admin/library/*` route, 403, and nothing changes.
#[sqlx::test]
async fn the_moderation_routes_are_the_superadmins(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "gate-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let res = report_call(
        &router,
        &published,
        &author.admin.cookie,
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let report: Uuid = sqlx::query_scalar("SELECT id FROM library_reports")
        .fetch_one(&db)
        .await
        .unwrap();

    for cookie in [&author.owner.cookie, &author.admin.cookie] {
        for (method, uri) in [
            (Method::GET, "/admin/library/reports".to_string()),
            (
                Method::POST,
                format!("/admin/library/reports/{report}/dismiss"),
            ),
            (
                Method::POST,
                format!("/admin/library/recipes/{published}/withdraw"),
            ),
            (
                Method::DELETE,
                format!("/admin/library/recipes/{published}/withdrawal"),
            ),
        ] {
            let res = call(&router, method.clone(), &uri, Some(cookie), None).await;
            assert_status(&res, StatusCode::FORBIDDEN);
        }
    }
    let res = call(&router, Method::GET, "/admin/library/reports", None, None).await;
    assert_status(&res, StatusCode::UNAUTHORIZED);
    assert_eq!(library_rows(&db).await, 1);
    assert_eq!(
        reports_on(&db, &published).await,
        vec![("spam".to_string(), None, None)]
    );
}

/// AC: dismissing a report closes it, the publication left in the library;
/// a report closed already, or unknown, is a 404. Traced in `audit_log`.
#[sqlx::test]
async fn a_dismissed_report_leaves_the_publication(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "dis-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let (_, reader) = api_user(&db, "dis-reader@example.test").await;
    let res = report_call(
        &router,
        &published,
        &reader,
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let report: Uuid = sqlx::query_scalar("SELECT id FROM library_reports")
        .fetch_one(&db)
        .await
        .unwrap();
    let admin = superadmin(&db, "dis-admin").await;
    let uri = format!("/admin/library/reports/{report}/dismiss");

    let res = call(&router, Method::POST, &uri, Some(&admin), None).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        reports_on(&db, &published).await,
        vec![("spam".to_string(), None, Some("dismissed".to_string()))]
    );
    assert_eq!(library_rows(&db).await, 1);
    let (status, _) = get(&router, &format!("/library/recipes/{published}"), &reader).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        audit_actions(&db, &report.to_string()).await,
        vec!["admin.library.report.dismiss"]
    );

    let res = call(&router, Method::POST, &uri, Some(&admin), None).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    let res = call(
        &router,
        Method::POST,
        &format!("/admin/library/reports/{}/dismiss", Uuid::new_v4()),
        Some(&admin),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    // Reporting again changes nothing: the closed report stands.
    let res = report_call(
        &router,
        &published,
        &reader,
        Some(json!({"reason": "other"})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(reports_on(&db, &published).await.len(), 1);
}

/// AC: withdrawing a publication takes it out of the library — the list,
/// the detail and the copy — and closes its open reports; the copies
/// families made stay. Its source recipe cannot be published again, by
/// any member, until the superadmin lifts the block. Each step traced in
/// `audit_log`.
#[sqlx::test]
async fn a_withdrawn_publication_leaves_the_library_and_blocks_its_recipe(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "wd-src").await;
    let (recipe, published) = published_leek_tart(&router, &author).await;
    let other = fam(&db, "wd").await;
    let res = copy_call(&router, other.id, &published, &other.owner.cookie).await;
    assert_status(&res, StatusCode::CREATED);
    let copy = json_body(res).await["id"].as_str().unwrap().to_string();
    for cookie in [&other.owner.cookie, &other.admin.cookie] {
        let res = report_call(
            &router,
            &published,
            cookie,
            Some(json!({"reason": "illegal"})),
        )
        .await;
        assert_status(&res, StatusCode::NO_CONTENT);
    }
    let admin = superadmin(&db, "wd-admin").await;

    let res = withdraw_call(&router, &published, &admin).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let (_, list) = get(&router, "/library/recipes", &other.standard.cookie).await;
    assert_eq!(list["recipes"], json!([]));
    let (status, _) = get(
        &router,
        &format!("/library/recipes/{published}"),
        &other.standard.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let third = fam(&db, "wd-third").await;
    let res = copy_call(&router, third.id, &published, &third.owner.cookie).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    assert_eq!(recipes_of(&db, third.id).await, 0);
    assert_eq!(library_rows(&db).await, 0);
    // The copy stays, its link dropped.
    let (status, _) = get(
        &router,
        &format!("/groups/{}/recipes/{copy}", other.id),
        &other.owner.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(library_link(&db, &copy).await, None);
    assert_eq!(
        reports_on(&db, &published)
            .await
            .into_iter()
            .map(|r| r.2)
            .collect::<Vec<_>>(),
        vec![Some("withdrawn".to_string()), Some("withdrawn".to_string())]
    );
    // Reporting what is gone is a 404.
    let res = report_call(
        &router,
        &published,
        &third.owner.cookie,
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);

    // Blocked, whoever publishes it.
    for cookie in [&author.owner.cookie, &author.admin.cookie] {
        let res = publish_call(&router, author.id, recipe, cookie, consent()).await;
        assert_status(&res, StatusCode::CONFLICT);
        assert_eq!(json_body(res).await["error"], "withdrawn_by_moderation");
    }
    assert_eq!(library_rows(&db).await, 0);
    // Withdrawn already: nothing left to withdraw.
    let res = withdraw_call(&router, &published, &admin).await;
    assert_status(&res, StatusCode::NOT_FOUND);

    let lift = format!("/admin/library/recipes/{published}/withdrawal");
    let res = call(&router, Method::DELETE, &lift, Some(&admin), None).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let res = call(&router, Method::DELETE, &lift, Some(&admin), None).await;
    assert_status(&res, StatusCode::NOT_FOUND);
    let res = publish_call(&router, author.id, recipe, &author.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
    assert_eq!(
        audit_actions(&db, &published).await,
        vec!["admin.library.withdraw", "admin.library.withdrawal.lift"]
    );
    let metadata: Value = sqlx::query_scalar(
        "SELECT metadata FROM audit_log WHERE action = 'admin.library.withdraw'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        metadata,
        json!({"reports_closed": 2, "source_blocked": true})
    );
}

/// The block is the family's to read, no one else's, and the runtime role
/// does not lift it.
#[sqlx::test]
async fn a_block_is_read_by_its_family_alone(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "blk-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let admin = superadmin(&db, "blk-admin").await;
    let res = withdraw_call(&router, &published, &admin).await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let other = fam(&db, "blk").await;

    let role = restricted_role(&db).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON library_withdrawn_sources, recipes TO {role}"
    )))
    .execute(&db)
    .await
    .unwrap();
    for (user, group, expected) in [
        (author.standard.id, author.id, 1_i64),
        (other.owner.id, other.id, 0),
    ] {
        let mut tx = as_user(&db, &role, Some((user, group))).await;
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM library_withdrawn_sources")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, expected, "{user} in {group}");
        let lifted = sqlx::query("DELETE FROM library_withdrawn_sources")
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(lifted, 0);
    }
}

/// Reports are counted per user: 10 an hour, a repeated one included.
#[sqlx::test]
async fn reports_are_rate_limited(db: PgPool) {
    let router = test_router(db.clone());
    let author = fam(&db, "rrate-src").await;
    let (_, published) = published_leek_tart(&router, &author).await;
    let (_, reader) = api_user(&db, "rrate-reader@example.test").await;
    for i in 0..10 {
        let res = report_call(
            &router,
            &published,
            &reader,
            Some(json!({"reason": "spam"})),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT, "report {i}");
    }
    let res = report_call(
        &router,
        &published,
        &reader,
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert_status(&res, StatusCode::TOO_MANY_REQUESTS);
    let res = report_call(
        &router,
        &published,
        &author.owner.cookie,
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
}

/// User's decision of 2026-11-20: a member's publications go when they
/// leave the family or are removed from it — the copies stay, the quota is
/// freed. The other members' publications stay.
#[sqlx::test]
async fn a_departing_members_publications_leave_the_library(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "dep").await;
    let mut published = Vec::new();
    for m in [&f.owner, &f.admin, &f.standard] {
        // The standard member publishes the recipe they wrote.
        let recipe = new_recipe(&router, f.id, &m.cookie, leek_tart()).await;
        let res = publish_call(&router, f.id, recipe, &m.cookie, consent()).await;
        assert_status(&res, StatusCode::CREATED);
        published.push(json_body(res).await["id"].as_str().unwrap().to_string());
    }
    let other = fam(&db, "dep-copy").await;
    let res = copy_call(&router, other.id, &published[1], &other.owner.cookie).await;
    assert_status(&res, StatusCode::CREATED);
    let copy = json_body(res).await["id"].as_str().unwrap().to_string();
    // The admin also publishes in a family of their own: that one stays.
    let (own_group, own_recipe) = family_with_recipe(&db, f.admin.id, "Soupe").await;
    let res = publish_call(&router, own_group, own_recipe, &f.admin.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);

    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{}/members/{}", f.id, f.admin.id),
        Some(&f.owner.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let (status, _) = get(
        &router,
        &format!("/library/recipes/{}", published[1]),
        &other.owner.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "removed: theirs goes");
    assert_eq!(library_rows(&db).await, 3, "the others' stay");
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{}/leave", f.id),
        Some(&f.standard.cookie),
        Some(json!({})),
    )
    .await;
    assert_status(&res, StatusCode::OK);

    let (_, list) = get(&router, "/library/recipes", &other.owner.cookie).await;
    let names: Vec<&str> = list["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&published[0].as_str()));
    assert!(!names.contains(&published[1].as_str()));
    assert!(!names.contains(&published[2].as_str()));
    assert_eq!(library_rows(&db).await, 2);
    assert_eq!(library_link(&db, &copy).await, None);
    let (status, _) = get(
        &router,
        &format!("/groups/{}/recipes/{copy}", other.id),
        &other.owner.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The family's count holds the owner's alone.
    let role = restricted_role(&db).await;
    let mut tx = as_user(&db, &role, Some((f.owner.id, f.id))).await;
    let n: i64 = sqlx::query_scalar("SELECT group_library_publication_count($1)")
        .bind(f.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

/// User's decision of 2026-11-20: the family's owner withdraws any
/// publication of one of its recipes, whoever published it — another
/// member, or a former one. No one else withdraws another's.
#[sqlx::test]
async fn the_owner_withdraws_any_publication_of_the_family(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "own").await;
    let recipe = new_recipe(&router, f.id, &f.admin.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, recipe, &f.admin.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
    let mine = new_recipe(&router, f.id, &f.owner.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, mine, &f.owner.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);

    let uri = |r: Uuid| format!("/groups/{}/recipes/{r}/publish", f.id);
    let res = call(
        &router,
        Method::DELETE,
        &uri(recipe),
        Some(&f.standard.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    // The admin withdraws not the owner's.
    let res = call(
        &router,
        Method::DELETE,
        &uri(mine),
        Some(&f.admin.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    assert_eq!(library_rows(&db).await, 2);
    let res = call(
        &router,
        Method::DELETE,
        &uri(recipe),
        Some(&f.owner.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(library_rows(&db).await, 1);

    // A former member's publication, left by a departure that predates the
    // automatic withdrawal: written straight to the tables.
    let (gone, _) = api_user(&db, "own-gone@example.test").await;
    let seeded = seed_publications(&db, f.id, gone, 1, "Ancien").await;
    assert_eq!(library_rows(&db).await, 2);
    let res = call(
        &router,
        Method::DELETE,
        &uri(seeded[0]),
        Some(&f.owner.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(library_rows(&db).await, 1);
    let res = call(
        &router,
        Method::DELETE,
        &uri(seeded[0]),
        Some(&f.owner.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);

    // The functions of 0068, under the runtime policies: the owner's only
    // for the owner of `app.group_id`, the departure's only for what no
    // member stands behind.
    let again = new_recipe(&router, f.id, &f.admin.cookie, leek_tart()).await;
    let res = publish_call(&router, f.id, again, &f.admin.cookie, consent()).await;
    assert_status(&res, StatusCode::CREATED);
    let role = restricted_role(&db).await;
    let other = fam(&db, "own-other").await;
    for (user, scope) in [
        (f.admin.id, f.id),
        (f.standard.id, f.id),
        (other.owner.id, other.id),
        (f.owner.id, other.id),
    ] {
        let mut tx = as_user(&db, &role, Some((user, scope))).await;
        let n: i64 = sqlx::query_scalar("SELECT group_library_unpublish_as_owner($1, $2, NULL)")
            .bind(f.id)
            .bind(again)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 0, "{user} in {scope}");
        let n: i64 = sqlx::query_scalar("SELECT group_library_unpublish_departed($1)")
            .bind(f.id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(n, 0, "{user} in {scope}");
    }
    assert_eq!(library_rows(&db).await, 2);
}

/// User's decision of 2026-11-20: the publication keeps its family. Its
/// source recipe deleted, the owner still withdraws it — by its own id —
/// and its publisher's departure still takes it.
#[sqlx::test]
async fn a_publication_follows_its_family_once_its_source_is_deleted(db: PgPool) {
    let router = test_router(db.clone());
    let f = fam(&db, "orph").await;
    let mut published = Vec::new();
    for m in [&f.admin, &f.standard, &f.standard] {
        let recipe = new_recipe(&router, f.id, &m.cookie, leek_tart()).await;
        let res = publish_call(&router, f.id, recipe, &m.cookie, consent()).await;
        assert_status(&res, StatusCode::CREATED);
        let id = json_body(res).await["id"].as_str().unwrap().to_string();
        let res = call(
            &router,
            Method::DELETE,
            &format!("/groups/{}/recipes/{recipe}", f.id),
            Some(&m.cookie),
            None,
        )
        .await;
        assert_status(&res, StatusCode::NO_CONTENT);
        published.push((recipe, id));
    }
    assert_eq!(
        library_rows(&db).await,
        3,
        "the publications outlive their source"
    );

    // By its source recipe, gone: nothing to name it by.
    let (recipe, _) = &published[1];
    let res = call(
        &router,
        Method::DELETE,
        &format!("/groups/{}/recipes/{recipe}/publish", f.id),
        Some(&f.owner.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);
    // By its own id: not another member's, nor from another family.
    let by_id = |group: Uuid, id: &str| format!("/groups/{group}/library/recipes/{id}");
    let other = fam(&db, "orph-other").await;
    for (group, cookie) in [(f.id, &f.admin.cookie), (other.id, &other.owner.cookie)] {
        let res = call(
            &router,
            Method::DELETE,
            &by_id(group, &published[1].1),
            Some(cookie),
            None,
        )
        .await;
        assert_status(&res, StatusCode::NOT_FOUND);
    }
    assert_eq!(library_rows(&db).await, 3);
    // The owner withdraws a member's; the publisher their own.
    let res = call(
        &router,
        Method::DELETE,
        &by_id(f.id, &published[1].1),
        Some(&f.owner.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    let res = call(
        &router,
        Method::DELETE,
        &by_id(f.id, &published[2].1),
        Some(&f.standard.cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(library_rows(&db).await, 1);

    // The admin leaves: their publication goes, its source gone or not.
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{}/leave", f.id),
        Some(&f.admin.cookie),
        Some(json!({})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
    assert_eq!(library_rows(&db).await, 0);
    let (status, _) = get(
        &router,
        &format!("/library/recipes/{}", published[0].1),
        &other.owner.cookie,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
