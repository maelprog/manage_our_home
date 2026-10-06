mod common;

use axum::http::{Method, StatusCode};
use common::{
    assert_status, call, drop_prescribed_role, json_body, prescribed_role_pool, set_cookie,
    test_router,
};
use sqlx::PgPool;

async fn register_verify_login(
    router: &axum::Router,
    db: &PgPool,
    email: &str,
    password: &str,
) -> String {
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true, "accepts_terms": true})),
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
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await;
    set_cookie(&login).unwrap()
}

async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(serde_json::json!({"name": name})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// The owner invites, the member accepts: returns the invitation token.
async fn join_group(
    router: &axum::Router,
    owner_cookie: &str,
    member_cookie: &str,
    group_id: &str,
) -> String {
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(owner_cookie),
        Some(serde_json::json!({})),
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
        Some(member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
    token
}

/// AC: `GET /account/export` returns every category of data the requesting
/// user authored, in the requesting user's own group — and the raw
/// `audit_log` gains an `account_data_exported` entry (architecture.md's
/// RGPD security requirement to audit-log export/deletion actions).
#[sqlx::test]
async fn export_returns_owned_data_across_categories(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "export-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer Export").await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Farine", "quantity": 2.0, "unit": "kg"})),
    )
    .await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/budget-entries"),
        Some(&cookie),
        Some(serde_json::json!({"name": "Courses", "amount": 12.5})),
    )
    .await;

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&cookie),
        Some(serde_json::json!({"content": "Bonjour la famille"})),
    )
    .await;

    // #346: registration writes `age_declared_at`, `terms_accepted_at` and
    // `created_at` with the same `now()` of one transaction, so they are equal
    // and an export that handed back one under another's name would still
    // match. Move each to an instant of its own before exporting.
    let (declared_at, accepted_at, created_at) = sqlx::query_as::<
        _,
        (
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "UPDATE users SET age_declared_at = created_at - interval '3 days 7 hours',
                          terms_accepted_at = created_at - interval '1 day 2 hours'
         WHERE email = 'export-owner@example.test'
         RETURNING age_declared_at, terms_accepted_at, created_at",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let declared_at = declared_at.expect("the declaration was just set");
    let accepted_at = accepted_at.expect("the acceptance was just set");
    assert_ne!(declared_at, created_at);
    assert_ne!(accepted_at, created_at);
    assert_ne!(accepted_at, declared_at);

    let export = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;

    assert_eq!(doc["profile"]["email"], "export-owner@example.test");
    // #137, #317: the art. 8 GDPR age declaration is data held about the
    // person, so art. 15 hands it back — the very instant on file, not just a
    // non-null field.
    let exported_at: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(doc["profile"]["age_declared_at"].clone())
            .expect("the export carries the age declaration as a timestamp");
    assert_eq!(exported_at, declared_at);
    // #319: so is the acceptance of the CGU, version and instant on file.
    let terms = sqlx::query!(
        "SELECT terms_accepted_version, terms_accepted_at FROM users
         WHERE email = 'export-owner@example.test'"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        doc["profile"]["terms_accepted_version"],
        terms
            .terms_accepted_version
            .expect("registration records the acceptance")
    );
    let exported_accepted_at: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(doc["profile"]["terms_accepted_at"].clone())
            .expect("the export carries the acceptance as a timestamp");
    assert_eq!(Some(exported_accepted_at), terms.terms_accepted_at);
    assert_eq!(exported_accepted_at, accepted_at);
    assert_eq!(doc["group_memberships"][0]["name"], "Foyer Export");
    assert_eq!(doc["group_memberships"][0]["role"], "owner");
    assert_eq!(doc["stock_items"][0]["name"], "Farine");
    assert_eq!(doc["budget_entries"][0]["name"], "Courses");
    assert_eq!(doc["budget_entries"][0]["amount"], 12.5);
    assert_eq!(doc["messages"][0]["content"], "Bonjour la famille");

    let audit_action: Option<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action = 'account_data_exported' LIMIT 1",
    )
    .fetch_optional(&db)
    .await
    .unwrap();
    assert_eq!(audit_action.as_deref(), Some("account_data_exported"));
}

/// AC: export is strictly self-scoped — a second member's content in the
/// same group must never appear in the first member's export.
#[sqlx::test]
async fn export_never_leaks_another_members_content(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "leak-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer Leak").await;

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let member_cookie =
        register_verify_login(&router, &db, "leak-member@example.test", "member-password1").await;
    let accept = call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);

    call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/messages"),
        Some(&owner_cookie),
        Some(serde_json::json!({"content": "Secret du owner"})),
    )
    .await;

    let export = call(
        &router,
        Method::GET,
        "/account/export",
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;
    assert!(doc["messages"].as_array().unwrap().is_empty());
    assert_eq!(doc["group_memberships"][0]["role"], "standard");
}

/// Issue #209, then #140. The export reads the caller's memberships from
/// their own `group_members` rows, not from every group the connection can
/// see — a test pool connects as a superuser, which bypasses RLS. But
/// leaving a group deletes that row and nothing else: what the caller wrote
/// there is still held, so it is still theirs to export (#140). It comes out
/// under `former_groups`, never as a membership, and never with another
/// member's content. The superuser and the `NOBYPASSRLS` role the README
/// prescribes must agree.
#[sqlx::test]
async fn export_covers_the_groups_the_caller_left(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "scope-owner@example.test", "owner-password1").await;
    let left_group = create_group(&router, &owner_cookie, "Foyer Quitte").await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "scope-member@example.test",
        "member-password1",
    )
    .await;
    join_group(&router, &owner_cookie, &member_cookie, &left_group).await;
    let own_group = create_group(&router, &member_cookie, "Foyer Garde").await;

    for (group_id, name, cookie) in [
        (&left_group, "Sel", &member_cookie),
        (&own_group, "Riz", &member_cookie),
        (&left_group, "Poivre", &owner_cookie),
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/stock-items"),
            Some(cookie),
            Some(serde_json::json!({"name": name, "quantity": 1.0, "unit": "kg"})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{left_group}/messages"),
        Some(&member_cookie),
        Some(serde_json::json!({"content": "Écrit avant de partir"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);

    let leave = call(
        &router,
        Method::POST,
        &format!("/groups/{left_group}/leave"),
        Some(&member_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&leave, StatusCode::OK);

    let (role, app_db) = prescribed_role_pool(&db).await;
    let scoped_router = test_router(app_db.clone());
    for (label, r) in [("superuser", &router), ("prescribed role", &scoped_router)] {
        let export = call(
            r,
            Method::GET,
            "/account/export",
            Some(&member_cookie),
            None,
        )
        .await;
        assert_status(&export, StatusCode::OK);
        let doc = json_body(export).await;

        let memberships: Vec<&str> = doc["group_memberships"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["group_id"].as_str().unwrap())
            .collect();
        assert_eq!(memberships, [own_group.as_str()], "{label}: memberships");
        assert_eq!(doc["group_memberships"][0]["role"], "owner", "{label}");
        assert_eq!(
            doc["former_groups"],
            serde_json::json!([
                {"group_id": left_group, "name": "Foyer Quitte", "created_by_me": false}
            ]),
            "{label}: former groups"
        );
        let stocks: Vec<(&str, &str)> = doc["stock_items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["group_id"].as_str().unwrap(), s["name"].as_str().unwrap()))
            .collect();
        assert_eq!(
            stocks,
            [(own_group.as_str(), "Riz"), (left_group.as_str(), "Sel")],
            "{label}: stock items — the caller's own, in both groups"
        );
        assert_eq!(
            doc["messages"][0]["content"], "Écrit avant de partir",
            "{label}"
        );
        assert_eq!(
            doc["messages"][0]["group_id"],
            left_group.as_str(),
            "{label}"
        );
    }
    drop(scoped_router);
    drop_prescribed_role(&db, app_db, &role).await;
}

/// Issue #140: art. 15 covers everything held that concerns the person, not
/// only what they typed. An event someone else assigned them to, a role
/// someone else gave them, their reminders, attachments, completions, read
/// marker, sessions, invitations, email checks — each has its own category.
/// Bearer secrets stay out: the session id (it is the cookie), the
/// invitation token, the storage key.
#[sqlx::test]
async fn export_covers_what_concerns_the_caller_beyond_what_they_wrote(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "art15-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer Acces").await;
    let member_cookie = register_verify_login(
        &router,
        &db,
        "art15-member@example.test",
        "member-password1",
    )
    .await;
    let token = join_group(&router, &owner_cookie, &member_cookie, &group_id).await;
    let member_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'art15-member@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();

    let role = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/members/{member_id}/role"),
        Some(&owner_cookie),
        Some(serde_json::json!({"role": "admin"})),
    )
    .await;
    assert_status(&role, StatusCode::OK);

    let create_event = |cookie: String, title: &'static str, assignees: Option<uuid::Uuid>| {
        let router = router.clone();
        let group_id = group_id.clone();
        async move {
            let mut body = serde_json::json!({
                "title": title,
                "starts_at": "2026-10-01T10:00:00Z",
                "ends_at": "2026-10-01T11:00:00Z",
                "is_task": true,
            });
            if let Some(a) = assignees {
                body["assignee_ids"] = serde_json::json!([a]);
            }
            let res = call(
                &router,
                Method::POST,
                &format!("/groups/{group_id}/events"),
                Some(&cookie),
                Some(body),
            )
            .await;
            assert_status(&res, StatusCode::CREATED);
            json_body(res).await["id"].as_str().unwrap().to_string()
        }
    };
    let owners_event =
        create_event(owner_cookie.clone(), "Rendez-vous confie", Some(member_id)).await;
    let members_event = create_event(member_cookie.clone(), "Tache du membre", None).await;

    let reminder = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events/{members_event}/reminders"),
        Some(&member_cookie),
        Some(serde_json::json!({"offset_minutes": 30})),
    )
    .await;
    assert_status(&reminder, StatusCode::CREATED);

    // No route to drive for these three without real bytes or a rendered
    // page: the rows are written as the handlers would write them.
    for statement in [
        format!(
            "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
             VALUES ('{members_event}', '{member_id}', 'attachments/export-140-key', 'facture.pdf', 'application/pdf', 1234)"
        ),
        format!(
            "INSERT INTO event_occurrence_completions (event_id, occurrence_at, completed_by)
             VALUES ('{owners_event}', '2026-10-01T10:00:00Z', '{member_id}')"
        ),
        format!(
            "INSERT INTO message_read_state (group_id, user_id) VALUES ('{group_id}', '{member_id}')"
        ),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&db)
            .await
            .unwrap();
    }

    let (role, app_db) = prescribed_role_pool(&db).await;
    let scoped_router = test_router(app_db.clone());
    for (label, r) in [("superuser", &router), ("prescribed role", &scoped_router)] {
        let export = call(
            r,
            Method::GET,
            "/account/export",
            Some(&member_cookie),
            None,
        )
        .await;
        assert_status(&export, StatusCode::OK);
        let doc = json_body(export).await;
        let text = doc.to_string();

        let titles = |key: &str| -> Vec<String> {
            doc[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["title"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(titles("agenda_events"), ["Tache du membre"], "{label}");
        let mut assigned = titles("event_assignments");
        assigned.sort();
        assert_eq!(
            assigned,
            ["Rendez-vous confie", "Tache du membre"],
            "{label}: assigned by someone else, and by default to oneself"
        );

        assert_eq!(doc["event_reminders"][0]["offset_minutes"], 30, "{label}");
        assert_eq!(
            doc["event_reminders"][0]["event_id"],
            members_event.as_str()
        );

        let attachment = &doc["event_attachments"][0];
        assert_eq!(attachment["filename"], "facture.pdf", "{label}");
        assert_eq!(attachment["mime_type"], "application/pdf", "{label}");
        assert_eq!(attachment["size_bytes"], 1234, "{label}");
        let url = attachment["download_url"].as_str().unwrap();
        assert!(
            url.contains("X-Amz-Signature="),
            "{label}: presigned: {url}"
        );
        assert!(
            url.contains("X-Amz-Expires=300"),
            "{label}: short-lived: {url}"
        );
        assert!(attachment.get("storage_key").is_none(), "{label}");

        assert_eq!(
            doc["event_completions"][0]["event_id"],
            owners_event.as_str()
        );
        assert_eq!(doc["message_read_state"][0]["group_id"], group_id.as_str());
        assert!(doc["invitations_sent"].as_array().unwrap().is_empty());
        assert!(!text.contains(&token), "{label}: invitation token exported");

        let sessions = doc["sessions"].as_array().unwrap();
        assert!(!sessions.is_empty(), "{label}");
        let session_id = member_cookie
            .split(';')
            .next()
            .unwrap()
            .split('=')
            .nth(1)
            .unwrap();
        assert!(!text.contains(session_id), "{label}: session id exported");

        assert!(!doc["email_verifications"][0]["consumed_at"].is_null());
        assert!(doc["oauth_identities"].as_array().unwrap().is_empty());
        assert_eq!(doc["profile"]["has_password"], true, "{label}");
        assert_eq!(doc["profile"]["is_superadmin"], false, "{label}");

        let role_change = doc["audit_log"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["action"] == "role_change")
            .expect("the role someone else gave the caller");
        assert_eq!(role_change["by_me"], false, "{label}");
        assert_eq!(role_change["metadata"]["new_role"], "admin", "{label}");
        assert!(role_change.get("actor_user_id").is_none(), "{label}");
    }
    drop(scoped_router);
    drop_prescribed_role(&db, app_db, &role).await;

    // The owner's side: a pending invitation they sent, without its token.
    // (The accepted one is gone: accepting deletes it.)
    let pending = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({"invited_email": "art15-invitee@example.test"})),
    )
    .await;
    assert_status(&pending, StatusCode::CREATED);
    let pending_token = json_body(pending).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let export = call(
        &router,
        Method::GET,
        "/account/export",
        Some(&owner_cookie),
        None,
    )
    .await;
    let doc = json_body(export).await;
    assert_eq!(doc["invitations_sent"][0]["group_id"], group_id.as_str());
    assert_eq!(
        doc["invitations_sent"][0]["invited_email"],
        "art15-invitee@example.test"
    );
    assert_eq!(doc["invitations_sent"].as_array().unwrap().len(), 1);
    assert!(!doc.to_string().contains(&pending_token));
    assert!(doc["event_attachments"].as_array().unwrap().is_empty());
}

/// Front epic F10 prerequisite: `GET /auth/me` reports `has_password` and the
/// pending `deletion_requested_at`, so `apps/web` can decide whether the
/// deletion form must ask for the current password and whether to render the
/// grace-period banner + cancel action instead of the request form. Read live on
/// every request, so requesting/cancelling shows up on the next call with no
/// re-login.
#[sqlx::test]
async fn auth_me_reports_password_and_pending_deletion(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "me-rgpd@example.test", "owner-password1").await;

    let before = json_body(call(&router, Method::GET, "/auth/me", Some(&cookie), None).await).await;
    assert_eq!(before["has_password"], true);
    assert_eq!(before["deletion_requested_at"], serde_json::Value::Null);

    let delete = call(
        &router,
        Method::POST,
        "/account/delete",
        Some(&cookie),
        Some(serde_json::json!({"current_password": "owner-password1"})),
    )
    .await;
    assert_status(&delete, StatusCode::OK);

    let after = json_body(call(&router, Method::GET, "/auth/me", Some(&cookie), None).await).await;
    assert!(after["deletion_requested_at"].is_string());

    let cancel = call(
        &router,
        Method::POST,
        "/account/delete/cancel",
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&cancel, StatusCode::OK);

    let cancelled =
        json_body(call(&router, Method::GET, "/auth/me", Some(&cookie), None).await).await;
    assert_eq!(cancelled["deletion_requested_at"], serde_json::Value::Null);
}

/// AC: `/account/export` and `/privacy-policy` reject/serve as documented —
/// export requires a session, the privacy policy is public.
#[sqlx::test]
async fn export_requires_auth_and_privacy_policy_is_public(db: PgPool) {
    let router = test_router(db.clone());

    let unauthenticated = call(&router, Method::GET, "/account/export", None, None).await;
    assert_status(&unauthenticated, StatusCode::UNAUTHORIZED);

    let policy = call(&router, Method::GET, "/privacy-policy", None, None).await;
    assert_status(&policy, StatusCode::OK);
}

/// The legal notice (LCEN art. 1-1) and the CGU are public documents, served
/// exactly like the privacy policy: no session, `text/markdown`, non-empty
/// (#132). A visitor must be able to read what they are agreeing to *before*
/// registering, so a session requirement creeping onto these routes has to
/// turn a test red.
#[sqlx::test]
async fn the_legal_notice_and_the_terms_are_public_markdown(db: PgPool) {
    let router = test_router(db.clone());

    for path in ["/legal-notice", "/terms-of-service"] {
        let response = call(&router, Method::GET, path, None, None).await;
        assert_status(&response, StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8"),
            "{path} is not served as markdown"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            body.starts_with(b"# "),
            "{path} does not serve the document verbatim"
        );
    }
}

/// #367: the CGU behind `/terms-of-service` change on a date, not on a
/// deploy, so no cache may reuse them unchecked across that date — the text
/// in force and the announced one alike. Nothing announced, or its date come,
/// `/terms-of-service/announced` is a 404 rather than a stale text.
#[sqlx::test]
async fn the_terms_are_never_reused_from_a_cache(db: PgPool) {
    use manage_our_home_shared::validation::auth::{paris_day, terms_announced_on};
    let router = test_router(db.clone());
    let announced = terms_announced_on(paris_day(chrono::Utc::now()));

    for (path, status) in [
        ("/terms-of-service", StatusCode::OK),
        (
            "/terms-of-service/announced",
            if announced.is_some() {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
        ),
    ] {
        let response = call(&router, Method::GET, path, None, None).await;
        assert_status(&response, status);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-cache"),
            "{path} may be reused from a cache"
        );
    }
}

/// Issue #140: a recipe is exported with its ingredients, nested under it.
#[sqlx::test]
async fn export_nests_the_ingredients_under_their_recipe(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "recipe-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer Cuisine").await;
    for (name, ingredients) in [
        (
            "Crepes",
            serde_json::json!([
                {"name": "Lait", "quantity": 0.5, "unit": "l"},
                {"name": "Farine", "quantity": 250.0, "unit": "g", "seasonal_months": [1, 2]},
                {"name": "Sucre", "is_optional": true},
            ]),
        ),
        ("Eau", serde_json::json!([])),
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/recipes"),
            Some(&cookie),
            Some(serde_json::json!({"name": name, "ingredients": ingredients})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    let export = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;
    let recipes = doc["recipes"].as_array().unwrap();
    let crepes = recipes.iter().find(|r| r["name"] == "Crepes").unwrap();
    assert_eq!(
        crepes["ingredients"],
        serde_json::json!([
            {"name": "Farine", "quantity": 250.0, "unit": "g", "is_optional": false, "seasonal_months": [1, 2]},
            {"name": "Lait", "quantity": 0.5, "unit": "l", "is_optional": false, "seasonal_months": null},
            {"name": "Sucre", "quantity": null, "unit": null, "is_optional": true, "seasonal_months": null},
        ])
    );
    let eau = recipes.iter().find(|r| r["name"] == "Eau").unwrap();
    assert_eq!(eau["ingredients"], serde_json::json!([]));

    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM recipe_ingredients")
        .fetch_one(&db)
        .await
        .unwrap();
    let exported: usize = recipes
        .iter()
        .map(|r| r["ingredients"].as_array().unwrap().len())
        .sum();
    assert_eq!(exported as i64, held, "every ingredient held is exported");
}

/// Issue #140: an invitation another member sent to the caller's address is
/// data about them, even from a group they never joined. It comes out with
/// its group and its sender, as the invitation email states them — not its
/// token, not the group as a former membership, nothing else of the group.
/// Addresses compare exactly, as registration and login compare them: other
/// capitals are another address.
#[sqlx::test]
async fn export_lists_the_invitations_addressed_to_the_caller(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "inviter@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer Invitant").await;
    let invitee_cookie =
        register_verify_login(&router, &db, "invitee@example.test", "member-password1").await;

    let mut tokens = Vec::new();
    for email in [
        "invitee@example.test",
        "Invitee@Example.test",
        "someone-else@example.test",
    ] {
        let res = call(
            &router,
            Method::POST,
            &format!("/groups/{group_id}/invitations"),
            Some(&owner_cookie),
            Some(serde_json::json!({"invited_email": email})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
        tokens.push(json_body(res).await["token"].as_str().unwrap().to_string());
    }
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/stock-items"),
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Contenu du groupe", "quantity": 1.0, "unit": "kg"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);

    let (role, app_db) = prescribed_role_pool(&db).await;
    let scoped_router = test_router(app_db.clone());
    for (label, r) in [("superuser", &router), ("prescribed role", &scoped_router)] {
        let export = call(
            r,
            Method::GET,
            "/account/export",
            Some(&invitee_cookie),
            None,
        )
        .await;
        assert_status(&export, StatusCode::OK);
        let doc = json_body(export).await;
        let received = doc["invitations_received"].as_array().unwrap();
        assert_eq!(received.len(), 1, "{label}: only the one to my address");
        assert_eq!(received[0]["group_id"], group_id.as_str(), "{label}");
        assert_eq!(received[0]["group_name"], "Foyer Invitant", "{label}");
        assert_eq!(received[0]["invited_by"], "inviter@example.test", "{label}");
        assert_eq!(
            received[0]["invited_email"], "invitee@example.test",
            "{label}"
        );
        assert!(received[0].get("token").is_none(), "{label}");

        let text = doc.to_string();
        for token in &tokens {
            assert!(!text.contains(token.as_str()), "{label}: token exported");
        }
        assert!(
            !text.contains("Contenu du groupe"),
            "{label}: group content"
        );
        assert!(
            !text.contains("someone-else"),
            "{label}: another invitation"
        );
        assert!(
            !text.contains("Invitee@Example.test"),
            "{label}: other capitals"
        );
        assert!(
            doc["former_groups"].as_array().unwrap().is_empty(),
            "{label}"
        );
        assert!(
            doc["group_memberships"].as_array().unwrap().is_empty(),
            "{label}"
        );
    }
    drop(scoped_router);
    drop_prescribed_role(&db, app_db, &role).await;
}

/// A credentials provider that always fails, as a storage whose secret
/// cannot be loaded would. (A client with no provider at all does not fail:
/// it hands out an unsigned URL.)
#[derive(Debug)]
struct NoCredentials;

impl aws_credential_types::provider::ProvideCredentials for NoCredentials {
    fn provide_credentials<'a>(
        &'a self,
    ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        aws_credential_types::provider::future::ProvideCredentials::ready(Err(
            aws_credential_types::provider::error::CredentialsError::not_loaded("test: none"),
        ))
    }
}

/// Issue #140: an attachment whose link cannot be signed keeps its metadata,
/// with a `null` link, and does not take the rest of the export down.
#[sqlx::test]
async fn an_attachment_that_cannot_be_signed_keeps_its_metadata(db: PgPool) {
    let unsigned = aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::config::Builder::new()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .endpoint_url("http://127.0.0.1:1")
            .credentials_provider(NoCredentials)
            .force_path_style(true)
            .build(),
    );
    let router = common::test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(unsigned, "manage-our-home".into()),
    );
    let cookie =
        register_verify_login(&router, &db, "unsigned@example.test", "owner-password1").await;
    let group_id = create_group(&router, &cookie, "Foyer Sans Signature").await;
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&cookie),
        Some(serde_json::json!({
            "title": "Avec piece jointe",
            "starts_at": "2026-10-01T10:00:00Z",
            "ends_at": "2026-10-01T11:00:00Z",
        })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let event_id = json_body(res).await["id"].as_str().unwrap().to_string();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
         SELECT '{event_id}', id, 'attachments/unsigned-140', 'scan.png', 'image/png', 42
         FROM users WHERE email = 'unsigned@example.test'"
    )))
    .execute(&db)
    .await
    .unwrap();

    let export = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;
    let attachment = &doc["event_attachments"][0];
    assert_eq!(attachment["filename"], "scan.png");
    assert_eq!(attachment["size_bytes"], 42);
    assert!(attachment["download_url"].is_null(), "{attachment}");
    assert_eq!(doc["agenda_events"][0]["title"], "Avec piece jointe");
}

/// Issue #140: every table of the schema is accounted for — exported under a
/// key of the document, or left out for a stated reason. A table added
/// without deciding which fails here.
#[sqlx::test]
async fn export_accounts_for_every_table(db: PgPool) {
    const EXPORTED: &[(&str, &str)] = &[
        ("users", "profile"),
        ("groups", "group_memberships / former_groups"),
        ("group_members", "group_memberships"),
        ("events", "agenda_events / event_assignments"),
        ("event_assignees", "event_assignments"),
        ("event_reminders", "event_reminders"),
        ("scheduled_notifications", "reminder_notifications"),
        ("event_attachments", "event_attachments"),
        ("event_occurrence_completions", "event_completions"),
        ("stock_items", "stock_items"),
        ("recipes", "recipes"),
        ("recipe_ingredients", "recipes"),
        ("meal_history", "meal_history"),
        ("grocery_items", "grocery_items"),
        ("budget_entries", "budget_entries"),
        ("messages", "messages"),
        ("message_read_state", "message_read_state"),
        ("calendar_imports", "calendar_imports"),
        ("invitations", "invitations_sent / invitations_received"),
        ("sessions", "sessions"),
        ("oauth_identities", "oauth_identities"),
        ("email_verification_tokens", "email_verifications"),
        ("password_reset_tokens", "password_resets"),
        ("audit_log", "audit_log"),
        ("push_subscriptions", "push_subscriptions"),
    ];
    const LEFT_OUT: &[(&str, &str)] = &[
        (
            "account_reactivation_requests",
            "exists only while the account is deactivated, when AuthUser refuses the export",
        ),
        (
            "calendar_import_events",
            "sync bookkeeping (feed UID -> event); the events are exported themselves",
        ),
        ("_sqlx_migrations", "schema history, no personal data"),
    ];

    let mut tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    tables.sort();
    let mut accounted: Vec<String> = EXPORTED
        .iter()
        .chain(LEFT_OUT)
        .map(|(t, _)| t.to_string())
        .collect();
    accounted.sort();
    assert_eq!(tables, accounted);

    // And each key named above is really in the document.
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "tables@example.test", "owner-password1").await;
    let export = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    let doc = json_body(export).await;
    for (table, keys) in EXPORTED {
        for key in keys.split(" / ") {
            assert!(doc.get(key).is_some(), "{table}: no `{key}` in the export");
        }
    }
}

/// Issue #299: the same bookkeeping, one level down. The table-level guard
/// above passes as soon as a table is listed, so a new column referencing
/// `users(id)` in a table it already covers would slip through it — and
/// through `account_export_group_ids()` (0019), which lists its columns by
/// hand. Every such column is accounted for here: read by the export, or
/// left out for a stated reason. A column added without deciding which
/// fails here.
#[sqlx::test]
async fn export_accounts_for_every_column_referencing_users(db: PgPool) {
    // Family-scoped columns: `account_export_group_ids()` (0019) must name
    // each of them, or the rows a person wrote in a group they have left
    // drop out of the export. Adding one here means adding it there, in a
    // new migration.
    const FAMILY_SCOPED: &[(&str, &str)] = &[
        ("groups", "created_by"),
        ("group_members", "user_id"),
        ("events", "created_by"),
        ("event_assignees", "user_id"),
        ("event_attachments", "uploaded_by"),
        ("event_occurrence_completions", "completed_by"),
        ("stock_items", "created_by"),
        ("recipes", "created_by"),
        ("meal_history", "created_by"),
        ("grocery_items", "created_by"),
        ("budget_entries", "created_by"),
        ("messages", "created_by"),
        ("message_read_state", "user_id"),
        ("calendar_imports", "created_by"),
        ("invitations", "created_by"),
    ];
    // Account-scoped columns: read directly on the user's id, no group.
    const ACCOUNT_SCOPED: &[(&str, &str)] = &[
        ("sessions", "user_id"),
        ("oauth_identities", "user_id"),
        ("email_verification_tokens", "user_id"),
        ("password_reset_tokens", "user_id"),
        ("audit_log", "actor_user_id"),
        ("push_subscriptions", "user_id"),
    ];
    const LEFT_OUT: &[(&str, &str, &str)] = &[
        (
            "account_reactivation_requests",
            "user_id",
            "exists only while the account is deactivated, when AuthUser refuses the export",
        ),
        // The comment of 0019 says its list follows "every column that
        // references users(id)"; this one is not in it, and need not be.
        (
            "invitations",
            "consumed_by",
            "never written: accepting an invitation deletes the row",
        ),
    ];

    let mut columns: Vec<String> = sqlx::query_scalar(
        "SELECT c.conrelid::regclass::text || '.' || a.attname::text
         FROM pg_constraint c
         CROSS JOIN LATERAL unnest(c.conkey) AS k(attnum)
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum
         WHERE c.contype = 'f' AND c.confrelid = 'public.users'::regclass",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    columns.sort();
    let mut accounted: Vec<String> = FAMILY_SCOPED
        .iter()
        .chain(ACCOUNT_SCOPED)
        .copied()
        .chain(LEFT_OUT.iter().map(|(t, c, _)| (*t, *c)))
        .map(|(t, c)| format!("{t}.{c}"))
        .collect();
    accounted.sort();
    assert_eq!(columns, accounted);

    // And each family-scoped column is matched against the caller in the
    // function itself, branch by branch. Each `UNION` branch reads
    // `FROM|JOIN <table> <alias>` and compares `<alias>.<column> = me.id`;
    // a column passes only if one branch does both for it, so a removed
    // branch, or one that keeps its table but compares another column,
    // fails here even when other branches join the same table or compare a
    // column of the same name. The check reads the text, SQL comments
    // included: a branch commented out with `--` still satisfies it. Nor
    // does it prove that the branch returns the right group id, or that no
    // extra condition empties it.
    let definition: String =
        sqlx::query_scalar("SELECT pg_get_functiondef('account_export_group_ids()'::regprocedure)")
            .fetch_one(&db)
            .await
            .unwrap();
    let branches: Vec<Vec<&str>> = definition
        .split("UNION")
        .map(|branch| {
            branch
                .split_whitespace()
                .map(|token| token.trim_end_matches(','))
                .collect()
        })
        .collect();
    for (table, column) in FAMILY_SCOPED {
        let matched = branches.iter().any(|tokens| {
            tokens.windows(3).any(|read| {
                matches!(read[0], "FROM" | "JOIN") && read[1] == *table && {
                    let compared = format!("{}.{column}", read[2]);
                    tokens
                        .windows(3)
                        .any(|cmp| cmp[0] == compared && cmp[1] == "=" && cmp[2] == "me.id")
                }
            })
        });
        assert!(
            matched,
            "no branch of account_export_group_ids() reads {table} and matches its {column} on the caller"
        );
    }
}

/// Issue #140: the two functions of 0019 look across families, so PUBLIC
/// cannot run them; a role holding only table grants is refused.
#[sqlx::test]
async fn the_export_functions_are_not_executable_by_public(db: PgPool) {
    let role = format!("no_exec_{}", uuid::Uuid::new_v4().simple());
    for statement in [
        format!("CREATE ROLE {role} NOSUPERUSER NOBYPASSRLS"),
        format!("GRANT SELECT ON ALL TABLES IN SCHEMA public TO {role}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&db)
            .await
            .unwrap();
    }
    for function in [
        "account_export_group_ids()",
        "account_export_received_invitations()",
    ] {
        let mut tx = manage_our_home::db::begin(&db).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
            .execute(&mut *tx)
            .await
            .unwrap();
        let err = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM {function}")))
            .execute(&mut *tx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("permission denied"),
            "{function}: {err}"
        );
        tx.rollback().await.unwrap();
    }
    for statement in [format!("DROP OWNED BY {role}"), format!("DROP ROLE {role}")] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&db)
            .await
            .unwrap();
    }
}

/// Issue #140: the two functions of 0019 run with their owner's rights, so
/// they must not resolve a name to an object the caller created. A caller
/// may create TEMP objects, and `pg_temp` is searched first unless the
/// function's `search_path` names it last. Here the runtime role shadows
/// `users` with a TEMP VIEW that gives every account the address of an
/// invitation sent to someone with no account at all: the function must
/// keep reading the real `users` and return nothing.
#[sqlx::test]
async fn a_temp_object_of_the_caller_cannot_shadow_the_export_functions_tables(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "shadow-owner@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer Ombre").await;
    let res = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({"invited_email": "no-account@example.test"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let caller_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'shadow-owner@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();

    let (role, app_db) = prescribed_role_pool(&db).await;
    let mut conn = app_db.acquire().await.unwrap();
    sqlx::query(
        "CREATE TEMP VIEW users AS
         SELECT id, 'no-account@example.test'::text AS email, display_name FROM public.users",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(caller_id.to_string())
        .execute(&mut *conn)
        .await
        .unwrap();
    // The view is live for this session: unqualified, `users` is the fake.
    let shadowed: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(caller_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(shadowed, "no-account@example.test");

    let leaked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_export_received_invitations()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(leaked, 0, "an invitation to another address came out");
    let groups: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT * FROM account_export_group_ids()")
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    assert_eq!(groups, [uuid::Uuid::parse_str(&group_id).unwrap()]);

    drop(conn);
    drop_prescribed_role(&db, app_db, &role).await;
}
