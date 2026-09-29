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
        Some(serde_json::json!({"email": email, "password": password, "display_name": email, "declares_minimum_age": true})),
    )
    .await;
    let token = sqlx::query_scalar!(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = $1",
        email
    )
    .fetch_one(db)
    .await
    .unwrap();
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

    let export = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    assert_status(&export, StatusCode::OK);
    let doc = json_body(export).await;

    assert_eq!(doc["profile"]["email"], "export-owner@example.test");
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

/// The legal notice (LCEN art. 6-III) and the CGU are public documents, served
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
