//! What a member's food constraints do to the recipes (#818), end to end:
//! the hard filter on the suggestions and on the replacements of a planned
//! recipe (#815), and the allergy warning on every recipe a member reads.
//!
//! The family is `dietary::family`'s: `owner`, and `holder`, the member
//! whose allergies and dislikes the tests are about.

mod common;
mod dietary;

use axum::http::{Method, StatusCode};
use common::{call, test_router};
use dietary::{family, send, Family, TAG};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn recipe(router: &axum::Router, f: &Family, body: Value) -> String {
    let (status, body) = send(
        router,
        Method::POST,
        &format!("/groups/{}/recipes", f.group),
        &f.owner,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

async fn allergy(router: &axum::Router, f: &Family, tag: &str, share: &str) {
    let (status, body) = send(
        router,
        Method::POST,
        "/me/allergies",
        &f.holder,
        Some(json!({ "tag": tag })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = send(
        router,
        Method::PUT,
        "/me/allergies/sharing",
        &f.holder,
        Some(json!({"tag": tag, "group_id": f.group, "share": share})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn share(router: &axum::Router, f: &Family, tag: &str, share: &str) {
    let (status, body) = send(
        router,
        Method::PUT,
        "/me/allergies/sharing",
        &f.holder,
        Some(json!({"tag": tag, "group_id": f.group, "share": share})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The suggestions `cookie` is given, by name, with the query `query`.
async fn suggested(router: &axum::Router, f: &Family, cookie: &str, query: &str) -> Vec<Value> {
    let (status, body) = send(
        router,
        Method::GET,
        &format!("/groups/{}/recipes/suggestions{query}", f.group),
        cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut list = body["suggestions"].as_array().unwrap().clone();
    list.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    list
}

fn names(list: &[Value]) -> Vec<&str> {
    list.iter().map(|s| s["name"].as_str().unwrap()).collect()
}

fn named<'a>(list: &'a [Value], name: &str) -> &'a Value {
    list.iter().find(|s| s["name"] == name).unwrap()
}

fn no_warning() -> Value {
    json!({"mine": [], "public": [], "anonymous": false, "unverifiable": false})
}

#[sqlx::test]
async fn suggestions_leave_out_what_the_reader_may_not_be_proposed(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "filter").await;
    recipe(
        &router,
        &f,
        json!({"name": "Satay", "ingredients": [{"name": "Cacahuètes"}, {"name": "Riz"}]}),
    )
    .await;
    recipe(
        &router,
        &f,
        json!({"name": "Poêlée", "ingredients": [{"name": "Champignons"}]}),
    )
    .await;
    recipe(
        &router,
        &f,
        json!({"name": "Gâteau", "ingredients": [{"name": "Farine"}], "required_equipment": ["oven"]}),
    )
    .await;
    recipe(
        &router,
        &f,
        json!({"name": "Riz garni", "ingredients": [{"name": "Riz"}, {"name": "Cacahuètes", "is_optional": true}]}),
    )
    .await;

    // Before any constraint: the four recipes, nothing to warn of, except
    // the cake, which needs an oven the household has not said it has.
    let before = suggested(&router, &f, &f.owner, "").await;
    assert_eq!(names(&before), ["Poêlée", "Riz garni", "Satay"]);
    for s in &before {
        assert_eq!(s["allergy_warning"], no_warning(), "{s}");
    }
    let (status, _) = send(
        &router,
        Method::PUT,
        &format!("/groups/{}/equipment", f.group),
        &f.owner,
        Some(json!({"codes": ["oven"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let before = suggested(&router, &f, &f.owner, "").await;
    assert_eq!(names(&before), ["Gâteau", "Poêlée", "Riz garni", "Satay"]);

    // The holder: allergic to peanuts (public), dislikes mushrooms.
    allergy(&router, &f, TAG, "public").await;
    let (status, _) = send(
        &router,
        Method::POST,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The holder is proposed neither the satay nor the mushrooms; the
    // optional peanuts of the rice do not exclude it, they warn.
    let holder = suggested(&router, &f, &f.holder, "").await;
    assert_eq!(names(&holder), ["Gâteau", "Riz garni"]);
    assert_eq!(
        named(&holder, "Riz garni")["allergy_warning"],
        json!({"mine": [TAG], "public": [], "anonymous": false, "unverifiable": false})
    );
    // Unticked: the allergy is lifted, the dislike is not.
    let holder = suggested(&router, &f, &f.holder, "?avoid_allergens=false").await;
    assert_eq!(names(&holder), ["Gâteau", "Riz garni", "Satay"]);
    assert_eq!(
        named(&holder, "Satay")["allergy_warning"],
        json!({"mine": [TAG], "public": [], "anonymous": false, "unverifiable": false})
    );

    // The owner: the holder's public allergy excludes the satay, the
    // holder's dislike does not touch the mushrooms.
    let owner = suggested(&router, &f, &f.owner, "").await;
    assert_eq!(names(&owner), ["Gâteau", "Poêlée", "Riz garni"]);
    let public = json!({
        "mine": [],
        "public": [{"member": "filter-holder@example.test", "tag": TAG}],
        "anonymous": false,
        "unverifiable": false,
    });
    assert_eq!(named(&owner, "Riz garni")["allergy_warning"], public);
    let owner = suggested(&router, &f, &f.owner, "?avoid_allergens=true").await;
    assert_eq!(names(&owner), ["Gâteau", "Poêlée", "Riz garni"]);

    // Unticked, the owner is proposed all four again, each scored exactly
    // as before any constraint.
    let owner = suggested(&router, &f, &f.owner, "?avoid_allergens=false").await;
    assert_eq!(names(&owner), names(&before));
    for (now, then) in owner.iter().zip(&before) {
        let (mut now, mut then) = (now.clone(), then.clone());
        now.as_object_mut().unwrap().remove("allergy_warning");
        then.as_object_mut().unwrap().remove("allergy_warning");
        assert_eq!(now, then);
    }
    assert_eq!(named(&owner, "Satay")["allergy_warning"], public);

    // Anonymous: the owner learns that a member's allergy matches, and
    // nothing else.
    share(&router, &f, TAG, "anonymous").await;
    let res = call(
        &router,
        Method::GET,
        &format!(
            "/groups/{}/recipes/suggestions?avoid_allergens=false",
            f.group
        ),
        Some(&f.owner),
        None,
    )
    .await;
    let text =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    for secret in [TAG, "filter-holder", &f.holder_id.to_string()] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    let owner = suggested(&router, &f, &f.owner, "?avoid_allergens=false").await;
    assert_eq!(
        named(&owner, "Satay")["allergy_warning"],
        json!({"mine": [], "public": [], "anonymous": true, "unverifiable": false})
    );
    assert_eq!(
        names(&suggested(&router, &f, &f.owner, "").await),
        ["Gâteau", "Poêlée", "Riz garni"]
    );
    // The holder's own anonymous allergy is theirs, not another member's.
    let holder = suggested(&router, &f, &f.holder, "?avoid_allergens=false").await;
    assert_eq!(
        named(&holder, "Satay")["allergy_warning"],
        json!({"mine": [TAG], "public": [], "anonymous": false, "unverifiable": false})
    );

    // An ingredient the taxonomy does not know: unverifiable, for a reader
    // who may see an allergy.
    recipe(
        &router,
        &f,
        json!({"name": "Sauce", "ingredients": [{"name": "Sauce maison du chef"}]}),
    )
    .await;
    let owner = suggested(&router, &f, &f.owner, "").await;
    assert_eq!(
        named(&owner, "Sauce")["allergy_warning"],
        json!({"mine": [], "public": [], "anonymous": false, "unverifiable": true})
    );
}

/// The replacements of a planned recipe (#815) go through the same filter.
#[sqlx::test]
async fn replacements_leave_out_the_same_recipes(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "swap").await;
    let gratin = recipe(
        &router,
        &f,
        json!({"name": "Gratin", "ingredients": [{"name": "Pommes de terre"}]}),
    )
    .await;
    recipe(
        &router,
        &f,
        json!({"name": "Satay", "ingredients": [{"name": "Cacahuètes"}]}),
    )
    .await;
    recipe(
        &router,
        &f,
        json!({"name": "Poêlée", "ingredients": [{"name": "Champignons"}]}),
    )
    .await;
    let (status, _) = send(
        &router,
        Method::POST,
        &format!("/groups/{}/recipes/{gratin}/selection", f.group),
        &f.owner,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    allergy(&router, &f, TAG, "private").await;
    let (status, _) = send(
        &router,
        Method::POST,
        "/me/dislikes",
        &f.holder,
        Some(json!({"tag": "en:mushrooms"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let offered = |cookie: String, query: &'static str| {
        let router = router.clone();
        let uri = format!(
            "/groups/{}/recipes/{gratin}/selection/replacements{query}",
            f.group
        );
        async move {
            let (status, body) = send(&router, Method::GET, &uri, &cookie, None).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let mut names: Vec<String> = body["replacements"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["name"].as_str().unwrap().to_string())
                .collect();
            names.sort();
            names
        }
    };
    // The holder: their private allergy and their dislike exclude.
    assert_eq!(offered(f.holder.clone(), "").await, Vec::<String>::new());
    assert_eq!(
        offered(f.holder.clone(), "?avoid_allergens=false").await,
        ["Satay"]
    );
    // The owner: neither the holder's private allergy nor their dislike.
    assert_eq!(offered(f.owner.clone(), "").await, ["Poêlée", "Satay"]);
}

/// Every answer holding a recipe carries its warning for the reader: the
/// card, the list, a selection made, changed or replaced, and the planned
/// list.
#[sqlx::test]
async fn every_recipe_a_member_reads_carries_its_warning(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "card").await;
    allergy(&router, &f, TAG, "public").await;
    let satay = recipe(
        &router,
        &f,
        json!({"name": "Satay", "ingredients": [{"name": "Cacahuètes"}]}),
    )
    .await;
    let riz = recipe(
        &router,
        &f,
        json!({"name": "Riz", "ingredients": [{"name": "Riz"}]}),
    )
    .await;
    let warned = json!({"mine": [TAG], "public": [], "anonymous": false, "unverifiable": false});
    let g = &f.group;

    let (_, card) = send(
        &router,
        Method::GET,
        &format!("/groups/{g}/recipes/{satay}"),
        &f.holder,
        None,
    )
    .await;
    assert_eq!(card["allergy_warning"], warned);
    let (_, list) = send(
        &router,
        Method::GET,
        &format!("/groups/{g}/recipes"),
        &f.holder,
        None,
    )
    .await;
    assert_eq!(list["recipes"][0]["name"], "Riz");
    assert_eq!(list["recipes"][0]["allergy_warning"], no_warning());
    assert_eq!(list["recipes"][1]["allergy_warning"], warned);

    let (status, planned) = send(
        &router,
        Method::POST,
        &format!("/groups/{g}/recipes/{riz}/selection"),
        &f.holder,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(planned["allergy_warning"], no_warning());
    let (status, replaced) = send(
        &router,
        Method::POST,
        &format!("/groups/{g}/recipes/{riz}/selection/replacement"),
        &f.holder,
        Some(json!({ "recipe_id": satay })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(replaced["selection"]["allergy_warning"], warned);
    let (status, changed) = send(
        &router,
        Method::PATCH,
        &format!("/groups/{g}/recipes/{satay}/selection"),
        &f.holder,
        Some(json!({"servings": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["allergy_warning"], warned);
    let (_, selections) = send(
        &router,
        Method::GET,
        &format!("/groups/{g}/recipes/selections"),
        &f.holder,
        None,
    )
    .await;
    assert_eq!(selections["selections"][0]["allergy_warning"], warned);

    // The owner reads the same recipe with the holder's public allergy.
    let (_, card) = send(
        &router,
        Method::GET,
        &format!("/groups/{g}/recipes/{satay}"),
        &f.owner,
        None,
    )
    .await;
    assert_eq!(
        card["allergy_warning"],
        json!({
            "mine": [],
            "public": [{"member": "card-holder@example.test", "tag": TAG}],
            "anonymous": false,
            "unverifiable": false,
        })
    );
}

/// The answers of `routes`, as bytes, to `cookie`.
async fn bytes_of(router: &axum::Router, cookie: &str, routes: &[String]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for uri in routes {
        let res = call(router, Method::GET, uri, Some(cookie), None).await;
        let mut bytes = res.status().as_u16().to_string().into_bytes();
        bytes.extend(res.into_body().collect().await.unwrap().to_bytes());
        out.push(bytes);
    }
    out
}

/// The invariant of #818: what another member is answered — suggestions
/// and replacements with `avoid_allergens` either way, the card, the list,
/// the planned list — is the same with or without a
/// private allergy of the holder, and with or without one shared in
/// another family.
#[sqlx::test]
async fn a_private_allergy_changes_nothing_another_member_is_proposed(db: PgPool) {
    let router = test_router(db.clone());
    let f = family(&router, &db, "invariant").await;
    let satay = recipe(
        &router,
        &f,
        json!({"name": "Satay", "ingredients": [{"name": "Cacahuètes"}, {"name": "Sauce maison"}]}),
    )
    .await;
    let riz = recipe(
        &router,
        &f,
        json!({"name": "Riz", "ingredients": [{"name": "Riz"}]}),
    )
    .await;
    let (status, _) = send(
        &router,
        Method::POST,
        &format!("/groups/{}/recipes/{riz}/selection", f.group),
        &f.owner,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let g = &f.group;
    let routes: Vec<String> = [
        format!("/groups/{g}/recipes/suggestions"),
        format!("/groups/{g}/recipes/suggestions?avoid_allergens=true"),
        format!("/groups/{g}/recipes/suggestions?avoid_allergens=false"),
        format!("/groups/{g}/recipes/{riz}/selection/replacements"),
        format!("/groups/{g}/recipes/{riz}/selection/replacements?avoid_allergens=false"),
        format!("/groups/{g}/recipes/{satay}"),
        format!("/groups/{g}/recipes"),
        format!("/groups/{g}/recipes/selections"),
    ]
    .into();

    let before = bytes_of(&router, &f.owner, &routes).await;
    // Satay is proposed, with nothing to warn of.
    assert!(String::from_utf8_lossy(&before[0]).contains("Satay"));
    assert_eq!(
        bytes_of(&router, &f.owner, &routes).await,
        before,
        "the same twice"
    );

    // Private here; then public in another family the owner is not in.
    allergy(&router, &f, TAG, "private").await;
    assert_eq!(bytes_of(&router, &f.owner, &routes).await, before);
    let other = dietary::create_group(&router, &f.holder, "Ailleurs").await;
    let (status, _) = send(
        &router,
        Method::PUT,
        "/me/allergies/sharing",
        &f.holder,
        Some(json!({"tag": TAG, "group_id": other, "share": "public"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes_of(&router, &f.owner, &routes).await, before);
    // Shared publicly here, it does change them: the comparison can fail.
    share(&router, &f, TAG, "public").await;
    assert_ne!(bytes_of(&router, &f.owner, &routes).await, before);
}
