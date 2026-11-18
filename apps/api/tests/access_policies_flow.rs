//! #864: a family's access policy over HTTP — `GET` for every member, `PUT`
//! for the owner alone — driven under the role apps/api/README.md
//! prescribes (`NOSUPERUSER NOBYPASSRLS`), so the reads and writes go
//! through `group_access_policies`' RLS (0062) as in production.

mod common;

use axum::http::{Method, StatusCode};
use common::{
    assert_status, call, drop_prescribed_role, json_body, prescribed_role_pool, set_cookie,
    test_router,
};
use manage_our_home::auth::session::scoped_tx;
use manage_our_home::groups::access::{min_role_for, Feature};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let password = "access-password1";
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(
            json!({"email": email, "password": password, "display_name": email,
                    "declares_minimum_age": true, "accepts_terms": true}),
        ),
    )
    .await;
    let token = common::verification_token(db, email).await;
    call(
        router,
        Method::GET,
        &format!("/auth/verify-email?token={token}"),
        None,
        None,
    )
    .await;
    let login = call(
        router,
        Method::POST,
        "/auth/login",
        None,
        Some(json!({"email": email, "password": password})),
    )
    .await;
    set_cookie(&login).unwrap()
}

async fn join(router: &axum::Router, owner: &str, member: &str, group_id: &str) {
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(owner),
        Some(json!({})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let accept = call(
        router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(member),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
}

async fn user_id_of(router: &axum::Router, cookie: &str) -> String {
    let me = call(router, Method::GET, "/auth/me", Some(cookie), None).await;
    json_body(me).await["user_id"].as_str().unwrap().to_string()
}

/// The `(feature, min_role)` pairs of a policy response, in its order.
fn pairs(body: &Value) -> Vec<(String, String)> {
    body["policies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["feature"].as_str().unwrap().to_string(),
                p["min_role"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn expected(set: &[(&str, &str)]) -> Vec<(String, String)> {
    [
        "budget",
        "google_calendar",
        "stocks_edit",
        "recipes_edit",
        "grocery_edit",
        "agenda_edit",
        "messagerie_moderate",
    ]
    .iter()
    .map(|feature| {
        let min_role = set
            .iter()
            .find(|(f, _)| f == feature)
            .map_or("admin", |(_, r)| r);
        (feature.to_string(), min_role.to_string())
    })
    .collect()
}

#[sqlx::test]
async fn access_policies_are_read_by_members_and_set_by_the_owner(db: PgPool) {
    let (role, app_db) = prescribed_role_pool(&db).await;
    let router = test_router(app_db.clone());
    let owner = register_verify_login(&router, &db, "owner@example.test").await;
    let admin = register_verify_login(&router, &db, "admin@example.test").await;
    let standard = register_verify_login(&router, &db, "standard@example.test").await;
    let outsider = register_verify_login(&router, &db, "outsider@example.test").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let group_id = json_body(create).await["id"].as_str().unwrap().to_string();
    join(&router, &owner, &admin, &group_id).await;
    join(&router, &owner, &standard, &group_id).await;
    let admin_id = user_id_of(&router, &admin).await;
    let promote = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/members/{admin_id}/role"),
        Some(&owner),
        Some(json!({"role": "admin"})),
    )
    .await;
    assert_status(&promote, StatusCode::OK);

    // An outsider's own family, to show its policy stays apart.
    let other = call(
        &router,
        Method::POST,
        "/groups",
        Some(&outsider),
        Some(json!({"name": "Ailleurs"})),
    )
    .await;
    let other_id = json_body(other).await["id"].as_str().unwrap().to_string();

    let url = format!("/groups/{group_id}/access-policies");

    // A standard member reads the seven features, all at the default.
    let read = call(&router, Method::GET, &url, Some(&standard), None).await;
    assert_status(&read, StatusCode::OK);
    assert_eq!(pairs(&json_body(read).await), expected(&[]));

    // A non-member reads nothing and writes nothing.
    let read = call(&router, Method::GET, &url, Some(&outsider), None).await;
    assert_status(&read, StatusCode::FORBIDDEN);
    let change = json!({"policies": [{"feature": "budget", "min_role": "standard"}]});
    let write = call(
        &router,
        Method::PUT,
        &url,
        Some(&outsider),
        Some(change.clone()),
    )
    .await;
    assert_status(&write, StatusCode::FORBIDDEN);

    // Neither an admin nor a standard member may write.
    for (label, cookie) in [("admin", &admin), ("standard", &standard)] {
        let write = call(
            &router,
            Method::PUT,
            &url,
            Some(cookie),
            Some(change.clone()),
        )
        .await;
        assert_eq!(write.status(), StatusCode::FORBIDDEN, "{label}");
    }

    // Thresholds and features off the lists are refused, the owner's
    // included, and change nothing.
    for (body, code) in [
        (
            json!({"policies": [{"feature": "budget", "min_role": "superuser"}]}),
            "invalid_role",
        ),
        (
            json!({"policies": [{"feature": "garage", "min_role": "owner"}]}),
            "unknown_feature",
        ),
        (json!({"policies": []}), "policies_required"),
        (
            json!({"policies": [{"feature": "budget", "min_role": "owner"},
                                {"feature": "budget", "min_role": "standard"}]}),
            "duplicate_feature",
        ),
    ] {
        let write = call(&router, Method::PUT, &url, Some(&owner), Some(body)).await;
        assert_eq!(write.status(), StatusCode::BAD_REQUEST, "{code}");
        assert_eq!(json_body(write).await["error"], code);
    }
    let read = call(&router, Method::GET, &url, Some(&standard), None).await;
    assert_eq!(pairs(&json_body(read).await), expected(&[]));

    // The owner sets two thresholds; the answer and the next read show them.
    let write = call(
        &router,
        Method::PUT,
        &url,
        Some(&owner),
        Some(
            json!({"policies": [{"feature": "budget", "min_role": "owner"},
                                 {"feature": "stocks_edit", "min_role": "standard"}]}),
        ),
    )
    .await;
    assert_status(&write, StatusCode::OK);
    let set = expected(&[("budget", "owner"), ("stocks_edit", "standard")]);
    assert_eq!(pairs(&json_body(write).await), set);
    let read = call(&router, Method::GET, &url, Some(&standard), None).await;
    assert_eq!(pairs(&json_body(read).await), set);

    // A second write changes one threshold back and keeps the other.
    let write = call(
        &router,
        Method::PUT,
        &url,
        Some(&owner),
        Some(json!({"policies": [{"feature": "budget", "min_role": "admin"}]})),
    )
    .await;
    assert_status(&write, StatusCode::OK);
    let read = call(&router, Method::GET, &url, Some(&admin), None).await;
    assert_eq!(
        pairs(&json_body(read).await),
        expected(&[("stocks_edit", "standard")])
    );

    // The other family is untouched.
    let read = call(
        &router,
        Method::GET,
        &format!("/groups/{other_id}/access-policies"),
        Some(&outsider),
        None,
    )
    .await;
    assert_eq!(pairs(&json_body(read).await), expected(&[]));

    // Each accepted write left one audit row, and the refused ones none.
    let owner_id: Uuid = user_id_of(&router, &owner).await.parse().unwrap();
    let audit: Vec<(Option<Uuid>, String, String, Value)> = sqlx::query_as(
        "SELECT actor_user_id, target_type, target_id, metadata FROM audit_log \
         WHERE action = 'access_policies_changed' ORDER BY id",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(audit.len(), 2, "{audit:?}");
    assert_eq!(audit[0].0, Some(owner_id));
    assert_eq!(audit[0].1, "group");
    assert_eq!(audit[0].2, group_id);
    assert_eq!(
        audit[0].3,
        json!({"policies": [{"feature": "budget", "min_role": "owner"},
                            {"feature": "stocks_edit", "min_role": "standard"}]})
    );
    assert_eq!(
        audit[1].3,
        json!({"policies": [{"feature": "budget", "min_role": "admin"}]})
    );

    // The helper the modules will call reads the same thresholds, under
    // the family scope.
    let group: Uuid = group_id.parse().unwrap();
    let mut tx = scoped_tx(&app_db, group, owner_id).await.unwrap();
    assert_eq!(
        min_role_for(&mut tx, group, Feature::StocksEdit)
            .await
            .unwrap(),
        "standard"
    );
    assert_eq!(
        min_role_for(&mut tx, group, Feature::Budget).await.unwrap(),
        "admin"
    );
    assert_eq!(
        min_role_for(&mut tx, group, Feature::AgendaEdit)
            .await
            .unwrap(),
        "admin",
        "no row: the default"
    );
    tx.rollback().await.unwrap();

    drop(router);
    drop_prescribed_role(&db, app_db, &role).await;
}
