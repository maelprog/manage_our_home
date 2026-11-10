//! No `GET` route of apps/api changes anything (#414), outside the closed
//! list the arbitrage of 2026-10-26 admits. `SameSite=Lax` sends the session
//! cookie on a cross-site navigation, so a `GET` that writes is one a third
//! site can trigger (#223, point 4).
//!
//! Every `GET` of the router is called on a household holding data in each
//! domain, and the whole database is compared before and after: a row added,
//! changed or removed in any table fails the test, naming the route and the
//! table. A `Set-Cookie` in the answer fails it too.
//!
//! The routes come from the OpenAPI spec, which `openapi_flow` holds to
//! exactly the routes `build_router` registers, plus the three it leaves
//! out. A route added later is called too, or the test stops on the path
//! parameter it cannot fill.
//!
//! Not compared, per the arbitrage: `sessions.last_seen_at` and
//! `sessions.expires_at`, which authentication refreshes.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use axum::http::{Method, StatusCode};
use common::{assert_status, call, insert_session, json_body, test_state};
use manage_our_home::api_docs::openapi;
use manage_our_home::build_router;
use sqlx::PgPool;
use uuid::Uuid;

/// The `GET` routes the arbitrage admits a side effect on (#414). Not
/// called: what they write is what they are for.
const ADMITTED: [&str; 3] = [
    // Consumes the token, marks the address verified.
    "/auth/verify-email",
    // Sets the OAuth `state` cookie.
    "/auth/google/start",
    // Opens the session, or the account (`state` + PKCE, #193).
    "/auth/google/callback",
];

/// The `GET` routes the spec leaves out (`openapi_flow::NOT_DOCUMENTED`),
/// and the documentation, which `docs_router` turns on.
const UNDOCUMENTED: [&str; 5] = [
    "/groups/{id}/messages/ws",
    "/healthz",
    "/readyz",
    "/openapi.json",
    "/docs/",
];

/// The routes this household cannot get a 2xx from, and why. Called all the
/// same: a refusal must not write either. Every other route must answer
/// 2xx, so that the comparison is made on the path that reads the data.
const NOT_REACHED: [(&str, &str); 6] = [
    // An upgrade: a plain GET is refused before the socket opens.
    ("/groups/{id}/messages/ws", "no upgrade headers"),
    // The test storage is unreachable on purpose (`common::test_state`).
    (
        "/groups/{id}/events/{event_id}/attachments/{attachment_id}/download",
        "no attachment without storage",
    ),
    ("/groups/{id}/members/{user_id}/avatar", "no profile photo"),
    ("/me/avatar", "no profile photo"),
    // No new version of the CGU is announced.
    ("/terms-of-service/announced", "nothing announced"),
    // Only a deactivated account's restricted session opens it.
    ("/account/deactivated", "a full session"),
];

/// The query string a route needs to answer 2xx.
fn query_of(route: &str) -> String {
    let today = chrono::Utc::now().date_naive();
    match route {
        "/groups/{id}/events" => {
            let from = chrono::Utc::now() - chrono::Duration::days(7);
            let to = from + chrono::Duration::days(42);
            format!(
                "?from={}&to={}",
                from.format("%Y-%m-%dT%H:%M:%SZ"),
                to.format("%Y-%m-%dT%H:%M:%SZ")
            )
        }
        "/groups/{id}/national-days" => {
            format!("?from={today}&to={}", today + chrono::Duration::days(42))
        }
        _ => String::new(),
    }
}

/// The columns left out of the comparison, by table.
const NOT_COMPARED: [(&str, &[&str]); 1] = [("sessions", &["last_seen_at", "expires_at"])];

/// The household every route is called on.
struct Household {
    group: String,
    ownerless_group: String,
    owner_cookie: String,
    member: String,
    admin_cookie: String,
    event: String,
    stock_item: String,
    recipe: String,
    grocery_item: String,
    budget_entry: String,
}

async fn insert_user(db: &PgPool, email: &str, name: &str) -> String {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, email_verified,
                            age_declared_at, terms_accepted_version, terms_accepted_at)
         VALUES ($1, 'x', $2, true, now(), $3, now())
         RETURNING id",
    )
    .bind(email)
    .bind(name)
    .bind(manage_our_home::auth::terms_acceptance::terms_in_force_now())
    .fetch_one(db)
    .await
    .unwrap();
    id.to_string()
}

async fn created(
    router: &axum::Router,
    cookie: &str,
    path: &str,
    body: serde_json::Value,
) -> String {
    let res = call(router, Method::POST, path, Some(cookie), Some(body)).await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// An owner and a member in one family, with an item in each domain, and a
/// superadmin with no family.
async fn household(db: &PgPool, router: &axum::Router) -> Household {
    let owner = insert_user(db, "owner@example.test", "Owner").await;
    let member = insert_user(db, "member@example.test", "Member").await;
    let admin = insert_user(db, "admin@example.test", "Admin").await;
    sqlx::query("UPDATE users SET is_superadmin = true WHERE id = $1::uuid")
        .bind(&admin)
        .execute(db)
        .await
        .unwrap();
    let owner_cookie = insert_session(db, owner.parse().unwrap()).await;
    let member_cookie = insert_session(db, member.parse().unwrap()).await;
    let admin_cookie = insert_session(db, admin.parse().unwrap()).await;
    // The admin routes read their access off a session's full lifetime, as
    // a login opens it (`user_admin::admin_access_end`).
    sqlx::query(
        "UPDATE sessions SET expires_at = created_at + make_interval(days => $2)
         WHERE user_id = $1::uuid",
    )
    .bind(&admin)
    .bind(manage_our_home::auth::session::SESSION_TTL_DAYS as i32)
    .execute(db)
    .await
    .unwrap();

    let group = created(
        router,
        &owner_cookie,
        "/groups",
        serde_json::json!({"name": "Foyer"}),
    )
    .await;
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1::uuid, $2::uuid, 'standard')",
    )
    .bind(&group)
    .bind(&member)
    .execute(db)
    .await
    .unwrap();

    // A second family whose owner left: what `/admin/groups/{id}/members`
    // lists, for a superadmin to designate the next owner.
    let ownerless: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Sans titulaire', $1::uuid) RETURNING id",
    )
    .bind(&member)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2::uuid, 'standard')",
    )
    .bind(ownerless)
    .bind(&member)
    .execute(db)
    .await
    .unwrap();

    let g = format!("/groups/{group}");
    let starts_at = chrono::Utc::now() + chrono::Duration::days(1);
    let event = created(
        router,
        &owner_cookie,
        &format!("{g}/events"),
        serde_json::json!({
            "title": "Rendez-vous",
            "starts_at": starts_at,
            "ends_at": starts_at + chrono::Duration::hours(1),
        }),
    )
    .await;
    let stock_item = created(
        router,
        &owner_cookie,
        &format!("{g}/stock-items"),
        serde_json::json!({"name": "Lait", "quantity": 0.1, "reorder_threshold": 0.5}),
    )
    .await;
    let recipe = created(
        router,
        &owner_cookie,
        &format!("{g}/recipes"),
        serde_json::json!({"name": "Crêpes", "ingredients": []}),
    )
    .await;
    let res = call(
        router,
        Method::POST,
        &format!("{g}/recipes/{recipe}/selection"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let grocery_item = created(
        router,
        &owner_cookie,
        &format!("{g}/grocery-items"),
        serde_json::json!({"name": "Farine", "quantity": 1.0, "unit": "kg"}),
    )
    .await;
    created(
        router,
        &owner_cookie,
        &format!("{g}/budget-categories"),
        serde_json::json!({"name": "alimentation", "color": "blue"}),
    )
    .await;
    let budget_entry = created(
        router,
        &owner_cookie,
        &format!("{g}/budget-entries"),
        serde_json::json!({"name": "Farine", "amount": 2.5}),
    )
    .await;
    // From the member: unread for the owner, and a notification.
    created(
        router,
        &member_cookie,
        &format!("{g}/messages"),
        serde_json::json!({"content": "Bonjour"}),
    )
    .await;

    Household {
        group,
        ownerless_group: ownerless.to_string(),
        owner_cookie,
        member,
        admin_cookie,
        event,
        stock_item,
        recipe,
        grocery_item,
        budget_entry,
    }
}

/// `path` with its parameters filled from `h`. Panics on a parameter it
/// does not know: a new route needs its value here.
fn fill(path: &str, h: &Household) -> String {
    let item = if path.starts_with("/groups/{id}/grocery-items/") {
        &h.grocery_item
    } else {
        &h.stock_item
    };
    let group = if path.starts_with("/admin/") {
        &h.ownerless_group
    } else {
        &h.group
    };
    let nil = Uuid::nil().to_string();
    let values: [(&str, &str); 7] = [
        ("{id}", group),
        ("{user_id}", &h.member),
        ("{event_id}", &h.event),
        ("{attachment_id}", &nil),
        ("{item_id}", item),
        ("{recipe_id}", &h.recipe),
        ("{entry_id}", &h.budget_entry),
    ];
    let filled = values.iter().fold(path.to_string(), |p, (param, value)| {
        p.replace(param, value)
    });
    assert!(
        !filled.contains('{'),
        "{path}: a path parameter with no value in `fill`"
    );
    filled
}

/// Every row of every table, as text, by table. The comparison is made with
/// the test's own pool, which RLS does not filter.
async fn snapshot(db: &PgPool) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1",
    )
    .fetch_all(db)
    .await
    .unwrap();
    let mut out = BTreeMap::new();
    for table in tables {
        let left_out = NOT_COMPARED
            .iter()
            .find(|(t, _)| *t == table)
            .map_or(&[][..], |(_, cols)| *cols);
        let minus: String = left_out.iter().map(|c| format!(" - '{c}'")).collect();
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT (to_jsonb(t){minus})::text AS row FROM \"{table}\" t ORDER BY 1"
        )))
        .fetch_all(db)
        .await
        .unwrap();
        out.insert(table, rows);
    }
    out
}

/// The tables whose rows differ between `before` and `after`.
fn changed_tables(
    before: &BTreeMap<String, Vec<String>>,
    after: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let tables: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    tables
        .into_iter()
        .filter(|t| before.get(*t) != after.get(*t))
        .cloned()
        .collect()
}

/// Every `GET` route: the spec's, then the undocumented ones.
fn get_routes() -> Vec<String> {
    let spec = serde_json::to_value(openapi(false)).unwrap();
    let mut routes: Vec<String> = spec["paths"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, item)| item.get("get").is_some())
        .map(|(path, _)| path.clone())
        .collect();
    routes.extend(UNDOCUMENTED.iter().map(|p| p.to_string()));
    routes
}

#[sqlx::test]
async fn no_get_route_outside_the_admitted_list_changes_anything(db: PgPool) {
    let mut state = test_state(db.clone());
    state.api_docs_enabled = true;
    let router = build_router(state);
    let h = household(&db, &router).await;

    let routes = get_routes();
    // A sanity bound on the enumeration itself.
    assert!(routes.len() > 40, "{} GET routes found", routes.len());
    for admitted in ADMITTED {
        assert!(
            routes.iter().any(|r| r == admitted),
            "{admitted} is admitted but is no GET route"
        );
    }
    for (route, _) in NOT_REACHED {
        assert!(
            routes.iter().any(|r| r == route),
            "{route} is listed as not reached but is no GET route"
        );
    }

    let mut called = 0;
    for route in routes.iter().filter(|r| !ADMITTED.contains(&r.as_str())) {
        let cookie = if route.starts_with("/admin/") {
            &h.admin_cookie
        } else {
            &h.owner_cookie
        };
        let uri = fill(route, &h) + &query_of(route);
        let before = snapshot(&db).await;
        let res = call(&router, Method::GET, &uri, Some(cookie), None).await;
        let status = res.status();
        let set_cookie = res.headers().get(axum::http::header::SET_COOKIE).cloned();
        let after = snapshot(&db).await;

        let changed = changed_tables(&before, &after);
        assert!(
            changed.is_empty(),
            "GET {route} ({status}) changed {changed:?}"
        );
        assert!(
            set_cookie.is_none(),
            "GET {route} ({status}) sets a cookie: {set_cookie:?}"
        );
        let reached = !NOT_REACHED.iter().any(|(r, _)| r == route);
        assert_eq!(
            status.is_success(),
            reached,
            "GET {route}: {status}, expected {}",
            if reached {
                "2xx"
            } else {
                "a refusal (NOT_REACHED)"
            }
        );
        called += 1;
    }
    assert_eq!(called, routes.len() - ADMITTED.len());
}

/// The comparison sees a write: a `POST` that adds a row, and one that only
/// changes a session's `revoked_at`, beside the columns it leaves out.
#[sqlx::test]
async fn the_comparison_sees_a_write(db: PgPool) {
    let router = build_router(test_state(db.clone()));
    let h = household(&db, &router).await;

    let before = snapshot(&db).await;
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{}/messages", h.group),
        Some(&h.owner_cookie),
        Some(serde_json::json!({"content": "Re"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let after = snapshot(&db).await;
    assert!(changed_tables(&before, &after).contains(&"messages".to_string()));

    let before = snapshot(&db).await;
    let res = call(
        &router,
        Method::POST,
        "/auth/logout",
        Some(&h.owner_cookie),
        None,
    )
    .await;
    assert!(res.status().is_success(), "{}", res.status());
    let after = snapshot(&db).await;
    assert_eq!(changed_tables(&before, &after), ["sessions"]);
}
