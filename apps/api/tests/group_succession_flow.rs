//! #323, against a real database: a group the purge leaves without an owner
//! gets one again when a member is reactivated, or when the superadmin
//! designates one; and whoever becomes owner that way, or as the purge's
//! heir, is told — a notice in `GET /groups` until acknowledged, and one
//! email, whatever their reminder channel.

mod common;

use axum::http::{Method, StatusCode};
use common::{assert_status, call, insert_session, json_body, set_cookie, test_router};
use manage_our_home::groups::succession::{send_ownership_notices, OWNERSHIP_NOTICE_SUBJECT};
use manage_our_home::jobs::account_purge::purge_due_accounts;
use sqlx::PgPool;
use uuid::Uuid;

const GROUPS_URL: &str = "https://maison.example.org/groups";
const POLICY_URL: &str = "https://maison.example.org/privacy-policy";

/// A user with an age declaration and an acceptance of the CGU on file,
/// whose deletion was requested `requested_days_ago` days ago (`None`: no
/// request).
async fn insert_user(db: &PgPool, email: &str, requested_days_ago: Option<i32>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, display_name, email_verified, age_declared_at,
                            terms_accepted_version, terms_accepted_at, deletion_requested_at)
         VALUES ($1, 'not-a-real-hash', $1, true, now() - interval '1 year',
                 $3, now() - interval '1 year',
                 now() - make_interval(days => $2))
         RETURNING id",
    )
    .bind(email)
    .bind(requested_days_ago)
    .bind(manage_our_home_shared::validation::auth::TERMS_VERSION)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn deactivate(db: &PgPool, user: Uuid) {
    sqlx::query("UPDATE users SET deactivated_at = now() WHERE id = $1")
        .bind(user)
        .execute(db)
        .await
        .unwrap();
}

async fn insert_group(db: &PgPool, name: &str, owner: Uuid) -> Uuid {
    let group: Uuid =
        sqlx::query_scalar("INSERT INTO groups (name, created_by) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(owner)
            .fetch_one(db)
            .await
            .unwrap();
    add_member(db, group, owner, "owner", 100).await;
    group
}

async fn add_member(db: &PgPool, group: Uuid, user: Uuid, role: &str, days_ago: i32) {
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role, joined_at)
         VALUES ($1, $2, $3::group_role, now() - make_interval(days => $4))",
    )
    .bind(group)
    .bind(user)
    .bind(role)
    .bind(days_ago)
    .execute(db)
    .await
    .unwrap();
}

/// `(role, ownership_inherited_reason, email sent)` of one membership.
async fn membership(db: &PgPool, group: Uuid, user: Uuid) -> (String, Option<String>, bool) {
    sqlx::query_as(
        "SELECT role::text, ownership_inherited_reason, ownership_email_sent_at IS NOT NULL
         FROM group_members WHERE group_id = $1 AND user_id = $2",
    )
    .bind(group)
    .bind(user)
    .fetch_one(db)
    .await
    .unwrap()
}

/// The `ownership_notice` of `group` in the caller's `GET /groups`.
async fn notice_of(router: &axum::Router, cookie: &str, group: Uuid) -> serde_json::Value {
    let res = call(router, Method::GET, "/groups", Some(cookie), None).await;
    assert_status(&res, StatusCode::OK);
    json_body(res)
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group_id"] == group.to_string())
        .unwrap_or_else(|| panic!("{group} not in GET /groups"))["ownership_notice"]
        .clone()
}

type Sent = std::sync::Mutex<Vec<(String, String, String)>>;

async fn send_pass(db: &PgPool, sent: &Sent) {
    let record = |to: String, subject: String, body: String| {
        sent.lock().unwrap().push((to, subject, body));
        async { Ok::<(), anyhow::Error>(()) }
    };
    send_ownership_notices(db, GROUPS_URL, POLICY_URL, record)
        .await
        .unwrap();
}

/// A superadmin logged in through the real endpoints: the admin routes
/// refuse a session `insert_session` opens (#226, #339).
async fn superadmin_cookie(router: &axum::Router, db: &PgPool) -> (Uuid, String) {
    let email = "support@example.test";
    let password = "support-password1";
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(
            serde_json::json!({"email": email, "password": password, "display_name": "Support",
                                "declares_minimum_age": true, "accepts_terms": true}),
        ),
    )
    .await;
    sqlx::query("UPDATE users SET email_verified = true, is_superadmin = true WHERE email = $1")
        .bind(email)
        .execute(db)
        .await
        .unwrap();
    let login = call(
        router,
        Method::POST,
        "/auth/login",
        None,
        Some(serde_json::json!({"email": email, "password": password})),
    )
    .await;
    assert_status(&login, StatusCode::OK);
    let id = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap();
    (id, set_cookie(&login).unwrap())
}

/// A group whose owner the purge removed with only a deactivated admin
/// left: no owner. Returns `(group, the deactivated admin)`.
async fn ownerless_group(db: &PgPool) -> (Uuid, Uuid) {
    let owning = insert_user(db, "owning@example.test", Some(31)).await;
    let disabled = insert_user(db, "disabled@example.test", None).await;
    deactivate(db, disabled).await;
    let group = insert_group(db, "Famille", owning).await;
    add_member(db, group, disabled, "admin", 90).await;
    purge_due_accounts(db).await.unwrap();
    let owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM group_members WHERE group_id = $1 AND role = 'owner'",
    )
    .bind(group)
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(owners, 0);
    (group, disabled)
}

/// The purge's heir sees the notice in `GET /groups` until they
/// acknowledge it, and gets one email, to their address, naming the group;
/// a second pass sends nothing. Another member sees no notice. The
/// acknowledgement is idempotent and closed to a non-member; the export
/// carries the stamps.
#[sqlx::test]
async fn the_heir_of_a_purge_is_told_by_a_notice_and_once_by_email(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let heir = insert_user(&db, "heir@example.test", None).await;
    let other = insert_user(&db, "other@example.test", None).await;
    let outsider = insert_user(&db, "outsider@example.test", None).await;
    let group = insert_group(&db, "Famille Martin", owning).await;
    add_member(&db, group, heir, "admin", 60).await;
    add_member(&db, group, other, "standard", 90).await;
    // Reminders by notification only: the ownership email goes all the same.
    sqlx::query("UPDATE users SET reminder_channel = 'push' WHERE id = $1")
        .bind(heir)
        .execute(&db)
        .await
        .unwrap();

    purge_due_accounts(&db).await.unwrap();

    assert_eq!(
        membership(&db, group, heir).await,
        ("owner".into(), Some("account_purged".into()), false)
    );
    let router = test_router(db.clone());
    let heir_cookie = insert_session(&db, heir).await;
    let other_cookie = insert_session(&db, other).await;
    let notice = notice_of(&router, &heir_cookie, group).await;
    assert_eq!(notice["reason"], "account_purged", "{notice}");
    assert!(notice["inherited_at"].is_string(), "{notice}");
    assert!(notice_of(&router, &other_cookie, group).await.is_null());

    let sent = Sent::default();
    send_pass(&db, &sent).await;
    send_pass(&db, &sent).await;
    let sent = sent.into_inner().unwrap();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (to, subject, body) = &sent[0];
    assert_eq!(to, "heir@example.test");
    assert_eq!(subject, OWNERSHIP_NOTICE_SUBJECT);
    assert!(body.contains("« Famille Martin »"), "{body}");
    assert!(body.contains(GROUPS_URL), "{body}");
    assert!(membership(&db, group, heir).await.2);

    // The export carries the stamps of the membership.
    let export = json_body(
        call(
            &router,
            Method::GET,
            "/account/export",
            Some(&heir_cookie),
            None,
        )
        .await,
    )
    .await;
    let m = &export["group_memberships"][0];
    assert_eq!(m["ownership_inherited_reason"], "account_purged", "{m}");
    assert!(m["ownership_email_sent_at"].is_string(), "{m}");
    assert!(m["ownership_notice_seen_at"].is_null(), "{m}");

    let path = format!("/groups/{group}/ownership-notice/seen");
    let outsider_cookie = insert_session(&db, outsider).await;
    let res = call(&router, Method::POST, &path, Some(&outsider_cookie), None).await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert!(!notice_of(&router, &heir_cookie, group).await.is_null());

    for _ in 0..2 {
        let res = call(&router, Method::POST, &path, Some(&heir_cookie), None).await;
        assert_status(&res, StatusCode::NO_CONTENT);
    }
    assert!(notice_of(&router, &heir_cookie, group).await.is_null());
}

/// A refused email leaves the membership unstamped, and the next pass sends
/// it.
#[sqlx::test]
async fn a_refused_ownership_email_is_tried_again(db: PgPool) {
    let owning = insert_user(&db, "owning@example.test", Some(31)).await;
    let heir = insert_user(&db, "heir@example.test", None).await;
    let group = insert_group(&db, "Famille", owning).await;
    add_member(&db, group, heir, "standard", 60).await;
    purge_due_accounts(&db).await.unwrap();

    let refuse = |_: String, _: String, _: String| async { Err(anyhow::anyhow!("relay down")) };
    send_ownership_notices(&db, GROUPS_URL, POLICY_URL, refuse)
        .await
        .unwrap();
    assert!(!membership(&db, group, heir).await.2);

    let sent = Sent::default();
    send_pass(&db, &sent).await;
    assert_eq!(sent.into_inner().unwrap().len(), 1);
    assert!(membership(&db, group, heir).await.2);
}

/// Reactivating a member of a group the purge left without an owner runs
/// the succession again: the member becomes owner, is told, and the
/// transfer is in `audit_log` with its reason. A group that has an owner is
/// left alone.
#[sqlx::test]
async fn reactivating_a_member_of_an_ownerless_group_makes_them_owner(db: PgPool) {
    let (group, disabled) = ownerless_group(&db).await;
    let keeper = insert_user(&db, "keeper@example.test", None).await;
    let owned = insert_group(&db, "Avec propriétaire", keeper).await;
    add_member(&db, owned, disabled, "admin", 50).await;
    let router = test_router(db.clone());
    let (_, admin_cookie) = superadmin_cookie(&router, &db).await;

    let res = call(
        &router,
        Method::POST,
        &format!("/admin/users/{disabled}/reactivate"),
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);

    assert_eq!(
        membership(&db, group, disabled).await,
        ("owner".into(), Some("member_reactivated".into()), false)
    );
    assert_eq!(
        membership(&db, owned, disabled).await,
        ("admin".into(), None, false)
    );
    assert_eq!(membership(&db, owned, keeper).await.0, "owner");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log
         WHERE action = 'ownership_transferred' AND target_id = $1
           AND metadata->>'new_owner_id' = $2 AND metadata->>'reason' = 'member_reactivated'",
    )
    .bind(group.to_string())
    .bind(disabled.to_string())
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    let sent = Sent::default();
    send_pass(&db, &sent).await;
    let sent = sent.into_inner().unwrap();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "disabled@example.test");
}

/// The superadmin's lever: `GET /admin/groups` says which group has no
/// owner; its members are listed only while it has none; only an active
/// member can be designated, and only in a group without an owner. The
/// designated member is told; the transfer is in `audit_log`, the
/// superadmin its actor.
#[sqlx::test]
async fn the_superadmin_designates_an_owner_among_active_members(db: PgPool) {
    let (group, disabled) = ownerless_group(&db).await;
    let pending = insert_user(&db, "pending@example.test", Some(5)).await;
    let active = insert_user(&db, "active@example.test", None).await;
    let outsider = insert_user(&db, "outsider@example.test", None).await;
    add_member(&db, group, pending, "standard", 40).await;
    add_member(&db, group, active, "standard", 10).await;
    let other_group = insert_group(&db, "Autre", outsider).await;
    let router = test_router(db.clone());
    let (superadmin, admin_cookie) = superadmin_cookie(&router, &db).await;

    let res = call(
        &router,
        Method::GET,
        "/admin/groups",
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let groups = json_body(res).await["groups"].as_array().unwrap().clone();
    let has_owner = |id: Uuid| {
        groups.iter().find(|g| g["id"] == id.to_string()).unwrap()["has_owner"]
            .as_bool()
            .unwrap()
    };
    assert!(!has_owner(group));
    assert!(has_owner(other_group));

    let members_path = format!("/admin/groups/{group}/members");
    let res = call(
        &router,
        Method::GET,
        &members_path,
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::OK);
    let listed = json_body(res).await;
    let members = listed["members"].as_array().unwrap();
    assert_eq!(members.len(), 3, "{listed}");
    let res = call(
        &router,
        Method::GET,
        &format!("/admin/groups/{other_group}/members"),
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);

    let owner_path = format!("/admin/groups/{group}/owner");
    for (user, status, code) in [
        (
            disabled,
            StatusCode::UNPROCESSABLE_ENTITY,
            "owner_must_be_active",
        ),
        (
            pending,
            StatusCode::UNPROCESSABLE_ENTITY,
            "owner_must_be_active",
        ),
        (outsider, StatusCode::UNPROCESSABLE_ENTITY, "not_a_member"),
    ] {
        let res = call(
            &router,
            Method::POST,
            &owner_path,
            Some(&admin_cookie),
            Some(serde_json::json!({ "user_id": user })),
        )
        .await;
        assert_status(&res, status);
        assert_eq!(json_body(res).await["error"], code);
    }
    let res = call(
        &router,
        Method::POST,
        &format!("/admin/groups/{}/owner", Uuid::new_v4()),
        Some(&admin_cookie),
        Some(serde_json::json!({ "user_id": active })),
    )
    .await;
    assert_status(&res, StatusCode::NOT_FOUND);

    let res = call(
        &router,
        Method::POST,
        &owner_path,
        Some(&admin_cookie),
        Some(serde_json::json!({ "user_id": active })),
    )
    .await;
    assert_status(&res, StatusCode::NO_CONTENT);
    assert_eq!(
        membership(&db, group, active).await,
        ("owner".into(), Some("designated_by_support".into()), false)
    );
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log
         WHERE action = 'ownership_transferred' AND target_id = $1 AND actor_user_id = $2
           AND metadata->>'new_owner_id' = $3 AND metadata->>'reason' = 'designated_by_support'",
    )
    .bind(group.to_string())
    .bind(superadmin)
    .bind(active.to_string())
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // Now it has an owner: nothing more to designate, nothing to list.
    let res = call(
        &router,
        Method::POST,
        &owner_path,
        Some(&admin_cookie),
        Some(serde_json::json!({ "user_id": active })),
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);
    let res = call(
        &router,
        Method::GET,
        &members_path,
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::CONFLICT);

    let active_cookie = insert_session(&db, active).await;
    assert_eq!(
        notice_of(&router, &active_cookie, group).await["reason"],
        "designated_by_support"
    );
}

/// The two new admin routes are the superadmin's alone.
#[sqlx::test]
async fn a_member_gets_403_on_the_designation_routes(db: PgPool) {
    let (group, _) = ownerless_group(&db).await;
    let member = insert_user(&db, "member@example.test", None).await;
    add_member(&db, group, member, "standard", 5).await;
    let router = test_router(db.clone());
    let cookie = insert_session(&db, member).await;

    let res = call(
        &router,
        Method::GET,
        &format!("/admin/groups/{group}/members"),
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    let res = call(
        &router,
        Method::POST,
        &format!("/admin/groups/{group}/owner"),
        Some(&cookie),
        Some(serde_json::json!({ "user_id": member })),
    )
    .await;
    assert_status(&res, StatusCode::FORBIDDEN);
    assert_eq!(membership(&db, group, member).await.0, "standard");
}
