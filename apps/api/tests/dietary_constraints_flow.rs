//! A member's food allergies and dislikes, and the household's equipment
//! (#817), end to end against a real database.
//!
//! The allergy is health data that belongs to its holder. What the other
//! members of a family may learn of it depends on the level the holder set
//! there: `public` (who and which), `anonymous` (that someone has one),
//! `private` (nothing). These tests hold the API to that from the outside:
//! another member's answers, compared byte for byte; the export; erasing
//! everything; leaving a family. The logs are `dietary_constraints_logs_flow`'s,
//! the account purge `account_purge_flow`'s.

mod common;
mod dietary;

use axum::http::{Method, StatusCode};
use common::{call, test_router};
use dietary::{create_group, family, join, register_verify_login, send, Family, TAG};

/// The French name the warning gives `TAG` (#825).
const LABEL: &str = "Cacahuètes";
use http_body_util::BodyExt;
use manage_our_home::api_docs::openapi;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn set_share(router: &axum::Router, f: &Family, share: &str) {
    let (status, body) = send(
        router,
        Method::PUT,
        "/me/allergies/sharing",
        &f.holder,
        Some(json!({"tag": TAG, "group_id": f.group, "share": share})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["share"], share);
}

#[sqlx::test]
async fn a_member_lists_adds_and_removes_their_allergies_and_dislikes(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "crud").await;

    let (status, body) = send(&router, Method::GET, "/me/allergies", &f.holder, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"allergies": []}));

    let (status, body) = send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({"tag": format!(" {TAG} ")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["tag"], TAG);
    // Private, in the one family, until the holder says otherwise.
    assert_eq!(
        body["sharing"],
        json!([{"group_id": f.group, "group_name": "Maison", "share": "private"}])
    );

    for (tag, status, code) in [
        (TAG, StatusCode::CONFLICT, "already_listed"),
        ("en:unicorns", StatusCode::BAD_REQUEST, "unknown_tag"),
        ("peanuts", StatusCode::BAD_REQUEST, "invalid_tag"),
        ("", StatusCode::BAD_REQUEST, "tag_required"),
    ] {
        let (got, body) = send(
            &router,
            Method::POST,
            "/me/allergies",
            &f.holder,
            Some(json!({ "tag": tag })),
        )
        .await;
        assert_eq!((got, body["error"].as_str()), (status, Some(code)), "{tag}");
    }

    let (_, body) = send(&router, Method::GET, "/me/allergies", &f.holder, None).await;
    assert_eq!(body["allergies"].as_array().unwrap().len(), 1);
    assert_eq!(body["allergies"][0]["tag"], TAG);

    let (status, _) = send(
        &router,
        Method::DELETE,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &router,
        Method::DELETE,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Dislikes: the same, and no level at all.
    let (status, body) = send(
        &router,
        Method::POST,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["tag"], "en:mushrooms");
    let (status, _) = send(
        &router,
        Method::POST,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, body) = send(&router, Method::GET, "/me/dislikes", &f.holder, None).await;
    assert_eq!(body["dislikes"][0]["tag"], "en:mushrooms");
    assert_eq!(body["dislikes"].as_array().unwrap().len(), 1);
    // Another member's lists are their own.
    let (_, body) = send(&router, Method::GET, "/me/dislikes", &f.owner, None).await;
    assert_eq!(body, json!({"dislikes": []}));
    let (status, _) = send(
        &router,
        Method::DELETE,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&router, Method::GET, "/me/dislikes", &f.holder, None).await;
    assert_eq!(body, json!({"dislikes": []}));
}

#[sqlx::test]
async fn a_level_is_set_only_on_an_own_allergy_in_an_own_family(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "level").await;
    let stranger = register_verify_login(&router, &db, "level-stranger@example.test").await;
    let elsewhere = create_group(&router, &stranger, "Ailleurs").await;

    let sharing = |tag: &str, group: &str, share: &str| {
        Some(json!({"tag": tag, "group_id": group, "share": share}))
    };
    // No such allergy yet.
    let (status, _) = send(
        &router,
        Method::PUT,
        "/me/allergies/sharing",
        &f.holder,
        sharing(TAG, &f.group, "public"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;
    for (body, status, code) in [
        (
            sharing(TAG, &f.group, "secret"),
            StatusCode::BAD_REQUEST,
            Some("invalid_share"),
        ),
        // A family the holder is not in.
        (
            sharing(TAG, &elsewhere, "public"),
            StatusCode::NOT_FOUND,
            Some("not_found"),
        ),
    ] {
        let (got, answer) = send(
            &router,
            Method::PUT,
            "/me/allergies/sharing",
            &f.holder,
            body,
        )
        .await;
        assert_eq!((got, answer["error"].as_str()), (status, code));
    }
    // Another member cannot set the holder's level: the tag is looked up
    // among the caller's own allergies.
    let (status, _) = send(
        &router,
        Method::PUT,
        "/me/allergies/sharing",
        &f.owner,
        sharing(TAG, &f.group, "public"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM allergy_sharing")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    set_share(&router, &f, "anonymous").await;
    let (_, body) = send(&router, Method::GET, "/me/allergies", &f.holder, None).await;
    assert_eq!(body["allergies"][0]["sharing"][0]["share"], "anonymous");
}

/// What another member reads, as bytes: every `GET` of the API it can call
/// with the family's id alone, in the order of the spec.
async fn answers_to(router: &axum::Router, cookie: &str, group: &str) -> Vec<(String, Vec<u8>)> {
    // Left out: what writes on each call — the export records itself in
    // `audit_log`, which it then returns — and the routes that open a
    // session or consume a token.
    const LEFT_OUT: [&str; 4] = [
        "/account/export",
        "/auth/verify-email",
        "/auth/google/start",
        "/auth/google/callback",
    ];
    let spec = serde_json::to_value(openapi(false)).unwrap();
    let mut out = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        if item.get("get").is_none() || LEFT_OUT.contains(&path.as_str()) {
            continue;
        }
        let filled = path.replace("{id}", group);
        if filled.contains('{') {
            continue;
        }
        let query = match path.as_str() {
            "/groups/{id}/events" => "?from=2026-11-01T00:00:00Z&to=2026-12-13T00:00:00Z",
            "/groups/{id}/national-days" => "?from=2026-11-01&to=2026-12-13",
            _ => "",
        };
        let res = call(
            router,
            Method::GET,
            &format!("{filled}{query}"),
            Some(cookie),
            None,
        )
        .await;
        let status = res.status().as_u16().to_string();
        let mut bytes = status.into_bytes();
        bytes.extend(res.into_body().collect().await.unwrap().to_bytes());
        out.push((path.clone(), bytes));
    }
    out
}

fn assert_same(before: &[(String, Vec<u8>)], after: &[(String, Vec<u8>)], step: &str) {
    assert_eq!(before.len(), after.len());
    for ((path, b), (_, a)) in before.iter().zip(after) {
        assert!(
            a == b,
            "{step}: {path} answers another member differently\nbefore: {}\nafter:  {}",
            String::from_utf8_lossy(b),
            String::from_utf8_lossy(a)
        );
    }
}

/// The invariant of #817: a private allergy changes nothing another member
/// of the family is answered — recorded, its level set to private, removed.
#[sqlx::test]
async fn a_private_allergy_changes_no_answer_another_member_gets(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "private").await;
    // Something to read in the family: a recipe with peanuts.
    let (status, _) = send(
        &router,
        Method::POST,
        &format!("/groups/{}/recipes", f.group),
        &f.owner,
        Some(json!({"name": "Satay", "ingredients": [{"name": "Cacahuètes"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let before = answers_to(&router, &f.owner, &f.group).await;
    assert!(
        before.len() >= 35,
        "{} routes compared: the enumeration found too few",
        before.len()
    );
    assert!(before.iter().any(|(p, _)| p == "/groups/{id}/allergies"));
    // Asked twice with nothing changed, the same bytes: the comparison
    // below can only fail on a change.
    assert_same(
        &before,
        &answers_to(&router, &f.owner, &f.group).await,
        "nothing",
    );

    send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;
    assert_same(
        &before,
        &answers_to(&router, &f.owner, &f.group).await,
        "recorded",
    );

    set_share(&router, &f, "private").await;
    assert_same(
        &before,
        &answers_to(&router, &f.owner, &f.group).await,
        "set private",
    );

    send(
        &router,
        Method::POST,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;
    assert_same(
        &before,
        &answers_to(&router, &f.owner, &f.group).await,
        "a dislike",
    );

    let (status, _) = send(
        &router,
        Method::DELETE,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_same(
        &before,
        &answers_to(&router, &f.owner, &f.group).await,
        "removed",
    );

    // And no notification went out about any of it.
    let notifications: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(notifications, 0);
}

#[sqlx::test]
async fn anonymous_shows_neither_tag_nor_member_and_public_shows_both(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "levels").await;
    let uri = format!("/groups/{}/allergies", f.group);
    send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;

    let (status, body) = send(&router, Method::GET, &uri, &f.owner, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"public": [], "anonymous": false}));

    set_share(&router, &f, "anonymous").await;
    let res = call(&router, Method::GET, &uri, Some(&f.owner), None).await;
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"public": [], "anonymous": true})
    );
    for secret in [
        TAG.to_string(),
        "peanut".to_string(),
        f.holder_id.to_string(),
        "levels-holder".to_string(),
    ] {
        assert!(!text.contains(&secret), "{secret} in {text}");
    }

    set_share(&router, &f, "public").await;
    let (_, body) = send(&router, Method::GET, &uri, &f.owner, None).await;
    assert_eq!(
        body,
        json!({
            "public": [{
                "user_id": f.holder_id,
                "display_name": "levels-holder@example.test",
                "tag": TAG,
            }],
            "anonymous": false,
        })
    );

    // Back to private: gone at the next read.
    set_share(&router, &f, "private").await;
    let (_, body) = send(&router, Method::GET, &uri, &f.owner, None).await;
    assert_eq!(body, json!({"public": [], "anonymous": false}));

    // Someone outside the family reads nothing of it.
    set_share(&router, &f, "public").await;
    let stranger = register_verify_login(&router, &db, "levels-stranger@example.test").await;
    let (status, body) = send(&router, Method::GET, &uri, &stranger, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!body.to_string().contains(TAG));
}

#[sqlx::test]
async fn leaving_a_family_drops_the_levels_set_there_and_only_there(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "leave").await;
    let other = create_group(&router, &f.holder, "Chez moi").await;
    send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;
    set_share(&router, &f, "public").await;
    let (status, _) = send(
        &router,
        Method::PUT,
        "/me/allergies/sharing",
        &f.holder,
        Some(json!({"tag": TAG, "group_id": other, "share": "anonymous"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(
        &router,
        Method::POST,
        &format!("/groups/{}/leave", f.group),
        &f.holder,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let levels: Vec<(String, String)> = sqlx::query_as(
        "SELECT group_id::text, share FROM allergy_sharing WHERE user_id = $1 ORDER BY 1",
    )
    .bind(f.holder_id)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(levels, vec![(other.clone(), "anonymous".to_string())]);
    let (_, body) = send(
        &router,
        Method::GET,
        &format!("/groups/{}/allergies", f.group),
        &f.owner,
        None,
    )
    .await;
    assert_eq!(body, json!({"public": [], "anonymous": false}));
    // The allergy itself stays the holder's.
    let (_, body) = send(&router, Method::GET, "/me/allergies", &f.holder, None).await;
    assert_eq!(body["allergies"][0]["tag"], TAG);
    assert_eq!(
        body["allergies"][0]["sharing"],
        json!([{"group_id": other, "group_name": "Chez moi", "share": "anonymous"}])
    );

    // Removed by the owner: the same.
    join(&router, &f.owner, &f.holder, &f.group).await;
    set_share(&router, &f, "public").await;
    let (status, _) = send(
        &router,
        Method::DELETE,
        &format!("/groups/{}/members/{}", f.group, f.holder_id),
        &f.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let in_family: i64 =
        sqlx::query_scalar("SELECT count(*) FROM allergy_sharing WHERE group_id::text = $1")
            .bind(&f.group)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(in_family, 0);
}

#[sqlx::test]
async fn the_export_holds_the_allergies_their_levels_and_the_dislikes(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "export").await;
    send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": TAG })),
    )
    .await;
    set_share(&router, &f, "anonymous").await;
    send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({"tag": "en:eggs"})),
    )
    .await;
    send(
        &router,
        Method::POST,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;

    let (status, doc) = send(&router, Method::GET, "/account/export", &f.holder, None).await;
    assert_eq!(status, StatusCode::OK);
    let allergies = doc["allergies"].as_array().unwrap();
    assert_eq!(allergies.len(), 2);
    assert_eq!(allergies[0]["tag"], TAG);
    assert_eq!(
        allergies[0]["sharing"],
        json!([{"group_id": f.group, "share": "anonymous"}])
    );
    assert_eq!(allergies[1]["tag"], "en:eggs");
    assert_eq!(allergies[1]["sharing"], json!([]));
    assert!(allergies[1]["created_at"].is_string());
    assert_eq!(doc["dislikes"].as_array().unwrap().len(), 1);
    assert_eq!(doc["dislikes"][0]["tag"], "en:mushrooms");

    // The other member's export holds none of it.
    let (_, doc) = send(&router, Method::GET, "/account/export", &f.owner, None).await;
    assert_eq!(doc["allergies"], json!([]));
    assert_eq!(doc["dislikes"], json!([]));
    assert!(!doc.to_string().contains(TAG));
}

#[sqlx::test]
async fn erasing_everything_takes_every_allergy_level_and_dislike(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "erase").await;
    for (uri, tag) in [
        ("/me/allergies", TAG),
        ("/me/allergies", "en:eggs"),
        ("/me/dislikes", "en:mushrooms"),
    ] {
        send(
            &router,
            Method::POST,
            uri,
            &f.holder,
            Some(json!({ "tag": tag })),
        )
        .await;
    }
    set_share(&router, &f, "public").await;
    // The owner's own, which stay.
    send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.owner,
        Some(json!({ "tag": TAG })),
    )
    .await;

    let (status, _) = send(
        &router,
        Method::DELETE,
        "/me/dietary-constraints",
        &f.holder,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for table in ["user_allergies", "allergy_sharing", "user_dislikes"] {
        let left: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE user_id = $1"
        )))
        .bind(f.holder_id)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(left, 0, "{table}");
    }
    let (_, body) = send(&router, Method::GET, "/me/allergies", &f.owner, None).await;
    assert_eq!(body["allergies"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn the_household_equipment_and_what_a_recipe_needs(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "equipment").await;
    let uri = format!("/groups/{}/equipment", f.group);

    let (status, body) = send(&router, Method::GET, &uri, &f.holder, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"codes": []}));

    // The owner and the admins set it (#819), the list stored sorted; every
    // member reads it.
    let (status, body) = send(
        &router,
        Method::PUT,
        &uri,
        &f.owner,
        Some(json!({"codes": ["oven", "blender"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"codes": ["blender", "oven"]}));
    let (_, body) = send(&router, Method::GET, &uri, &f.holder, None).await;
    assert_eq!(body, json!({"codes": ["blender", "oven"]}));

    // A standard member does not change it.
    let (status, _) = send(
        &router,
        Method::PUT,
        &uri,
        &f.holder,
        Some(json!({"codes": ["oven"]})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, body) = send(&router, Method::GET, &uri, &f.owner, None).await;
    assert_eq!(body, json!({"codes": ["blender", "oven"]}));

    // An admin does.
    sqlx::query(
        "UPDATE group_members SET role = 'admin' WHERE group_id = $1::uuid AND role = 'standard'",
    )
    .bind(&f.group)
    .execute(&db)
    .await
    .unwrap();
    let (status, body) = send(
        &router,
        Method::PUT,
        &uri,
        &f.holder,
        Some(json!({"codes": ["blender", "oven", "hob"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"codes": ["blender", "hob", "oven"]}));

    for (codes, code) in [
        (json!(["oven", "teleporter"]), "unknown_equipment"),
        (json!(["oven", "oven"]), "duplicate_equipment"),
    ] {
        let (status, body) = send(
            &router,
            Method::PUT,
            &uri,
            &f.owner,
            Some(json!({ "codes": codes })),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some(code))
        );
    }
    let (_, body) = send(&router, Method::GET, &uri, &f.owner, None).await;
    assert_eq!(body, json!({"codes": ["blender", "hob", "oven"]}));

    let stranger = register_verify_login(&router, &db, "equipment-stranger@example.test").await;
    let (status, _) = send(&router, Method::GET, &uri, &stranger, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &router,
        Method::PUT,
        &uri,
        &stranger,
        Some(json!({"codes": []})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A recipe: none by default, set on creation, replaced, left as is.
    let recipes = format!("/groups/{}/recipes", f.group);
    let (status, body) = send(
        &router,
        Method::POST,
        &recipes,
        &f.owner,
        Some(json!({"name": "Pain"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["required_equipment"], json!([]));
    let (status, body) = send(
        &router,
        Method::POST,
        &recipes,
        &f.owner,
        Some(json!({"name": "Gratin", "required_equipment": ["oven", "food_processor"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body["required_equipment"],
        json!(["food_processor", "oven"])
    );
    let recipe = format!("{recipes}/{}", body["id"].as_str().unwrap());

    let (status, body) = send(
        &router,
        Method::PATCH,
        &recipe,
        &f.owner,
        Some(json!({"name": "Gratin dauphinois"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["required_equipment"],
        json!(["food_processor", "oven"])
    );
    let (_, body) = send(
        &router,
        Method::PATCH,
        &recipe,
        &f.owner,
        Some(json!({"required_equipment": ["oven"]})),
    )
    .await;
    assert_eq!(body["required_equipment"], json!(["oven"]));
    let (_, body) = send(&router, Method::GET, &recipe, &f.holder, None).await;
    assert_eq!(body["required_equipment"], json!(["oven"]));
    let (_, body) = send(&router, Method::GET, &recipes, &f.holder, None).await;
    let listed: Vec<&Value> = body["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["required_equipment"])
        .collect();
    assert_eq!(listed, vec![&json!(["oven"]), &json!([])]);

    for body in [
        json!({"name": "X", "required_equipment": ["teleporter"]}),
        json!({"name": "X", "required_equipment": ["oven", "oven"]}),
    ] {
        let (status, _) = send(&router, Method::POST, &recipes, &f.owner, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, body) = send(
        &router,
        Method::PATCH,
        &recipe,
        &f.owner,
        Some(json!({"required_equipment": ["teleporter"]})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unknown_equipment"))
    );
}

/// #819: the account page names an allergy or a dislike beyond the INCO
/// fourteen by searching the taxonomy, and shows each one by its name.
#[sqlx::test]
async fn a_member_finds_a_category_by_its_name_and_reads_it_back_by_name(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "search").await;
    let search = |query: Value| {
        let router = router.clone();
        let cookie = f.holder.clone();
        async move {
            send(
                &router,
                Method::POST,
                "/taxonomy/search",
                &cookie,
                Some(json!({ "query": query })),
            )
            .await
        }
    };

    let (status, body) = search(json!("Kiwi")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["results"][0],
        json!({"tag": "en:kiwifruits", "label": "Kiwis"})
    );
    assert!(body["results"].as_array().unwrap().len() <= 20, "{body}");

    let (status, body) = search(json!("")).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"results": []})));

    // The bound counts characters, as the page's `maxlength` does: 128 « é »
    // (256 bytes) are read, 129 are not.
    let (status, body) = search(json!("é".repeat(128))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for long in ["a".repeat(129), "é".repeat(129)] {
        let (status, body) = search(json!(long)).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("query_too_long"))
        );
    }

    // Signed in only.
    let res = call(
        &router,
        Method::POST,
        "/taxonomy/search",
        None,
        Some(json!({"query": "kiwi"})),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // What was found is what is recorded, and read back by its name.
    let (status, body) = send(
        &router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({"tag": "en:kiwifruits"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["label"], "Kiwis");
    let (_, body) = send(&router, Method::GET, "/me/allergies", &f.holder, None).await;
    assert_eq!(body["allergies"][0]["label"], "Kiwis");

    let (status, body) = send(
        &router,
        Method::POST,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["label"], "Champignons");
    let (_, body) = send(&router, Method::GET, "/me/dislikes", &f.holder, None).await;
    assert_eq!(body["dislikes"][0]["label"], "Champignons");
}

/// #827: creating a family or joining one writes no level there. Until the
/// member chooses, each of their allergies is private in it: no
/// `allergy_sharing` row, and what the family is answered — #817's
/// `GET /groups/{id}/allergies`, #818's warning on a recipe — takes the
/// absence for `private`. Nothing of an allergy goes out in the invitation
/// email or in a notification.
#[sqlx::test]
async fn arriving_in_a_family_shares_nothing_until_the_member_chooses(db: PgPool) {
    let (router, outbox) = common::test_router_with_outbox(db.clone());
    let owner = register_verify_login(&router, &db, "arrive-owner@example.test").await;
    let holder_email = "arrive-holder@example.test";
    let holder = register_verify_login(&router, &db, holder_email).await;
    for tag in [TAG, "en:eggs"] {
        let (status, body) = send(
            &router,
            Method::POST,
            "/me/allergies",
            &holder,
            Some(json!({ "tag": tag })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    // Creating: a family of the holder's own.
    let own = create_group(&router, &holder, "Chez moi").await;
    // Joining: the owner's, through the link the invitation email carries.
    let group = create_group(&router, &owner, "Maison").await;
    let (status, body) = send(
        &router,
        Method::POST,
        &format!("/groups/{group}/invitations"),
        &owner,
        Some(json!({ "invited_email": holder_email })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = common::mailed_token(
        &outbox,
        holder_email,
        "http://localhost:5173/groups/invitations/",
    );
    let (status, _) = send(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        &holder,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(sharing_rows(&db, &own).await, 0, "created");
    assert_eq!(sharing_rows(&db, &group).await, 0, "joined");
    let (_, body) = send(&router, Method::GET, "/me/allergies", &holder, None).await;
    let levels: Vec<&Value> = body["allergies"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|a| a["sharing"].as_array().unwrap())
        .map(|s| &s["share"])
        .collect();
    assert_eq!(levels, vec!["private"; 4], "{body}");

    // What the family is answered takes the absence for `private`.
    let (status, body) = send(
        &router,
        Method::GET,
        &format!("/groups/{group}/allergies"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"public": [], "anonymous": false}));
    let (status, recipe) = send(
        &router,
        Method::POST,
        &format!("/groups/{group}/recipes"),
        &owner,
        Some(json!({"name": "Satay", "ingredients": [{"name": "Cacahuètes"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{recipe}");
    let card = format!("/groups/{group}/recipes/{}", recipe["id"].as_str().unwrap());
    let (_, read) = send(&router, Method::GET, &card, &owner, None).await;
    assert_eq!(
        read["allergy_warning"],
        json!({"mine": [], "public": [], "anonymous": false, "unverifiable": []})
    );
    // The holder's own warning is theirs, whatever the level.
    let (_, read) = send(&router, Method::GET, &card, &holder, None).await;
    assert_eq!(
        read["allergy_warning"]["mine"],
        json!([{"tag": TAG, "label": LABEL}])
    );

    // Nothing of an allergy in an email or a notification.
    let notifications: String =
        sqlx::query_scalar("SELECT coalesce(string_agg(body, ' | '), '') FROM notifications")
            .fetch_one(&db)
            .await
            .unwrap();
    let emails = outbox.emails();
    assert!(emails.iter().any(|e| e.to == holder_email));
    for word in ["peanut", "Arachide", "Cacahu", "eggs", "Œuf", "allerg"] {
        assert!(!notifications.contains(word), "{word}: {notifications}");
        for e in &emails {
            assert!(
                !e.subject.contains(word) && !e.body.contains(word),
                "{word} in the email to {}: {}",
                e.to,
                e.body
            );
        }
    }

    // Once the member chooses, the level they set is the one row there.
    let (status, _) = send(
        &router,
        Method::PUT,
        "/me/allergies/sharing",
        &holder,
        Some(json!({"tag": TAG, "group_id": group, "share": "public"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sharing_rows(&db, &group).await, 1);
    assert_eq!(sharing_rows(&db, &own).await, 0);
    let (_, body) = send(
        &router,
        Method::GET,
        &format!("/groups/{group}/allergies"),
        &owner,
        None,
    )
    .await;
    assert_eq!(body["public"][0]["tag"], TAG);
}

/// The levels set in one family, read past RLS by the harness pool.
async fn sharing_rows(db: &PgPool, group: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM allergy_sharing WHERE group_id = $1::uuid")
        .bind(group)
        .fetch_one(db)
        .await
        .unwrap()
}
