mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use common::{
    assert_status, call, call_upload, json_body, real_minio_from_env, set_cookie, test_router,
    test_router_with_storage,
};
use sqlx::PgPool;
use uuid::Uuid;

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
        Some(serde_json::json!({"email": email, "password": password, "display_name": email})),
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

/// AC #9-#14: create -> invite -> accept -> role change -> leave.
#[sqlx::test]
async fn full_group_lifecycle(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let member_cookie =
        register_verify_login(&router, &db, "member@example.test", "member-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner_cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&create, StatusCode::CREATED);
    let group = json_body(create).await;
    let group_id = group["id"].as_str().unwrap().to_string();

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner_cookie),
        Some(serde_json::json!({"invited_email": "member@example.test"})),
    )
    .await;
    assert_status(&invite, StatusCode::CREATED);
    let invite_body = json_body(invite).await;
    let invite_token = invite_body["token"].as_str().unwrap().to_string();

    let accept = call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{invite_token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);

    // AC #14: re-using the same invitation token is rejected as Gone.
    let reuse = call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{invite_token}/accept"),
        Some(&member_cookie),
        None,
    )
    .await;
    assert_status(&reuse, StatusCode::GONE);

    let member_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'member@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();

    let promote = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/members/{member_id}/role"),
        Some(&owner_cookie),
        Some(serde_json::json!({"role": "admin"})),
    )
    .await;
    assert_status(&promote, StatusCode::OK);

    // AC #13: the newly promoted admin cannot touch the owner.
    let owner_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'owner@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let admin_attacks_owner = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/members/{owner_id}/role"),
        Some(&member_cookie),
        Some(serde_json::json!({"role": "standard"})),
    )
    .await;
    assert_status(&admin_attacks_owner, StatusCode::FORBIDDEN);

    // AC #11: owner cannot leave without naming a successor.
    let leave_without_successor = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&owner_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&leave_without_successor, StatusCode::UNPROCESSABLE_ENTITY);

    let leave_with_successor = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&owner_cookie),
        Some(serde_json::json!({"new_owner_id": member_id})),
    )
    .await;
    assert_status(&leave_with_successor, StatusCode::OK);

    // AC #12: the last remaining member must delete the group, not leave.
    let last_member_leaves = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/leave"),
        Some(&member_cookie),
        Some(serde_json::json!({})),
    )
    .await;
    assert_status(&last_member_leaves, StatusCode::CONFLICT);
}

/// GET /groups returns exactly the caller's memberships with roles, is empty
/// for a user in no groups, and never leaks another user's groups.
#[sqlx::test]
async fn list_groups_is_scoped_to_caller(db: PgPool) {
    let router = test_router(db.clone());
    let alice = register_verify_login(&router, &db, "alice@example.test", "alice-password1").await;
    let bob = register_verify_login(&router, &db, "bob@example.test", "bob-password1").await;

    // Bob has no groups yet -> [].
    let empty = call(&router, Method::GET, "/groups", Some(&bob), None).await;
    assert_status(&empty, StatusCode::OK);
    assert_eq!(json_body(empty).await, serde_json::json!([]));

    // Alice creates two groups.
    for name in ["Alpha", "Beta"] {
        let res = call(
            &router,
            Method::POST,
            "/groups",
            Some(&alice),
            Some(serde_json::json!({"name": name})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }

    let alice_list = call(&router, Method::GET, "/groups", Some(&alice), None).await;
    assert_status(&alice_list, StatusCode::OK);
    let list = json_body(alice_list).await;
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    for g in arr {
        assert_eq!(g["role"], "owner");
        assert!(g["group_id"].is_string());
        assert!(g["created_at"].is_string());
    }

    // Bob still sees none of Alice's groups.
    let bob_list = call(&router, Method::GET, "/groups", Some(&bob), None).await;
    assert_eq!(json_body(bob_list).await, serde_json::json!([]));
}

/// PATCH /groups/:id renames as admin/owner, is forbidden for a standard
/// member, and rejects an empty name with 422.
#[sqlx::test]
async fn rename_group_permissions_and_validation(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let member =
        register_verify_login(&router, &db, "member@example.test", "member-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Old Name"})),
    )
    .await;
    let group_id = json_body(create).await["id"].as_str().unwrap().to_string();

    // Add member via invitation.
    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner),
        Some(serde_json::json!({"invited_email": "member@example.test"})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member),
        None,
    )
    .await;

    // Standard member cannot rename.
    let forbidden = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}"),
        Some(&member),
        Some(serde_json::json!({"name": "Hacked"})),
    )
    .await;
    assert_status(&forbidden, StatusCode::FORBIDDEN);

    // Empty-after-trim name is 422.
    let empty = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}"),
        Some(&owner),
        Some(serde_json::json!({"name": "   "})),
    )
    .await;
    assert_status(&empty, StatusCode::UNPROCESSABLE_ENTITY);

    // Owner renames successfully.
    let renamed = call(
        &router,
        Method::PATCH,
        &format!("/groups/{group_id}"),
        Some(&owner),
        Some(serde_json::json!({"name": "  New Name  "})),
    )
    .await;
    assert_status(&renamed, StatusCode::OK);
    assert_eq!(json_body(renamed).await["name"], "New Name");
}

/// POST /groups/:id/transfer-ownership swaps roles atomically with one audit
/// row; rejects non-owner (403), self target (422), and non-member (404).
#[sqlx::test]
async fn transfer_ownership_swaps_roles_and_audits(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;
    let member =
        register_verify_login(&router, &db, "member@example.test", "member-password1").await;
    let _outsider =
        register_verify_login(&router, &db, "outsider@example.test", "outsider-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    let group_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let invite = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(&owner),
        Some(serde_json::json!({"invited_email": "member@example.test"})),
    )
    .await;
    let token = json_body(invite).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &router,
        Method::POST,
        &format!("/groups/invitations/{token}/accept"),
        Some(&member),
        None,
    )
    .await;

    let owner_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'owner@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let member_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'member@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();
    let outsider_id: Uuid =
        sqlx::query_scalar!("SELECT id FROM users WHERE email = 'outsider@example.test'")
            .fetch_one(&db)
            .await
            .unwrap();

    // Non-owner (the member) cannot transfer -> 403.
    let by_member = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&member),
        Some(serde_json::json!({"new_owner_id": owner_id})),
    )
    .await;
    assert_status(&by_member, StatusCode::FORBIDDEN);

    // Owner transferring to itself -> 422.
    let to_self = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&owner),
        Some(serde_json::json!({"new_owner_id": owner_id})),
    )
    .await;
    assert_status(&to_self, StatusCode::UNPROCESSABLE_ENTITY);

    // Owner transferring to a non-member -> 404.
    let to_outsider = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&owner),
        Some(serde_json::json!({"new_owner_id": outsider_id})),
    )
    .await;
    assert_status(&to_outsider, StatusCode::NOT_FOUND);

    // Ensure outsider ID was actually distinct/valid (guards the test).
    assert_ne!(outsider_id, member_id);

    // Valid transfer -> 200, roles swapped, one audit row.
    let ok = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/transfer-ownership"),
        Some(&owner),
        Some(serde_json::json!({"new_owner_id": member_id})),
    )
    .await;
    assert_status(&ok, StatusCode::OK);

    let gid = Uuid::parse_str(&group_id).unwrap();
    let old_owner_role = sqlx::query_scalar!(
        r#"SELECT role::text as "role!" FROM group_members WHERE group_id = $1 AND user_id = $2"#,
        gid,
        owner_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let new_owner_role = sqlx::query_scalar!(
        r#"SELECT role::text as "role!" FROM group_members WHERE group_id = $1 AND user_id = $2"#,
        gid,
        member_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(old_owner_role, "admin");
    assert_eq!(new_owner_role, "owner");

    let audit_count = sqlx::query_scalar!(
        "SELECT count(*) FROM audit_log WHERE action = 'ownership_transferred' AND target_id = $1",
        group_id
    )
    .fetch_one(&db)
    .await
    .unwrap()
    .unwrap_or(0);
    assert_eq!(audit_count, 1);
}

/// GET /groups/:id members are enriched with display_name and email.
#[sqlx::test]
async fn get_group_members_include_identity(db: PgPool) {
    let router = test_router(db.clone());
    let owner = register_verify_login(&router, &db, "owner@example.test", "owner-password1").await;

    let create = call(
        &router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    let group_id = json_body(create).await["id"].as_str().unwrap().to_string();

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);
    let body = json_body(get).await;
    let members = body["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["email"], "owner@example.test");
    assert_eq!(members[0]["display_name"], "owner@example.test");
    assert_eq!(members[0]["role"], "owner");
}

/// AC #9: creating an 11th group is rejected.
#[sqlx::test]
async fn group_creation_capped_at_ten(db: PgPool) {
    let router = test_router(db.clone());
    let cookie =
        register_verify_login(&router, &db, "prolific@example.test", "prolific-password1").await;

    for i in 0..10 {
        let res = call(
            &router,
            Method::POST,
            "/groups",
            Some(&cookie),
            Some(serde_json::json!({"name": format!("Group {i}")})),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
    }
    let eleventh = call(
        &router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Group 11"})),
    )
    .await;
    assert_status(&eleventh, StatusCode::UNPROCESSABLE_ENTITY);
}

/// AC #10: the DB itself enforces a single owner per group.
#[sqlx::test]
async fn only_one_owner_per_group_at_db_level(db: PgPool) {
    let user_id: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('single@example.test', 'x', 'Single', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let other_id: Uuid = sqlx::query_scalar!(
        "INSERT INTO users (email, password_hash, display_name, email_verified) VALUES ('single2@example.test', 'x', 'Single2', true) RETURNING id"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let group_id: Uuid = sqlx::query_scalar!(
        "INSERT INTO groups (name, created_by) VALUES ('G', $1) RETURNING id",
        user_id
    )
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')",
        group_id,
        user_id
    )
    .execute(&db)
    .await
    .unwrap();

    let result = sqlx::query!(
        "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, 'owner')",
        group_id,
        other_id
    )
    .execute(&db)
    .await;
    assert!(
        result.is_err(),
        "expected unique violation for a second owner"
    );
}

/// PNG magic bytes. `sniff_and_validate_mime` reads the signature rather
/// than decoding the image, so this is all an upload needs to clear the
/// MIME allow-list.
const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

/// `event_attachments` is RLS-scoped through its event's group, and the
/// policy applies to the pool's own role (FORCE ROW LEVEL SECURITY), so
/// test-side reads and writes have to set `app.family_id` the way
/// `scoped_tx` does. Runtime `sqlx::query` on purpose: test-only SQL that
/// would otherwise need a `.sqlx` offline cache entry.
async fn with_family_scope<'a>(
    db: &PgPool,
    group_id: &str,
) -> sqlx::Transaction<'a, sqlx::Postgres> {
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(group_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

async fn create_group(router: &axum::Router, cookie: &str, name: &str) -> String {
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(cookie),
        Some(serde_json::json!({ "name": name })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

async fn create_event(router: &axum::Router, cookie: &str, group_id: &str, title: &str) -> String {
    let starts_at = Utc::now() + Duration::days(1);
    let res = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(cookie),
        Some(serde_json::json!({
            "title": title,
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
        })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    json_body(res).await["id"].as_str().unwrap().to_string()
}

/// Every attachment key in the group, across all of its events. Has to be
/// read *before* the group goes away: the rows cascade with it.
async fn group_storage_keys(db: &PgPool, group_id: &str) -> Vec<String> {
    let mut tx = with_family_scope(db, group_id).await;
    let keys = sqlx::query_scalar(
        "SELECT storage_key FROM event_attachments
         WHERE event_id IN (SELECT id FROM events WHERE group_id = $1)",
    )
    .bind(Uuid::parse_str(group_id).unwrap())
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    keys
}

async fn object_exists(s3: &aws_sdk_s3::Client, bucket: &str, key: &str) -> bool {
    s3.head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .is_ok()
}

/// AC (#57): deleting a group deletes the objects behind its events'
/// attachments, not just the rows. `events` cascades from `groups` and
/// `event_attachments` from `events`, so one owner-only call drops every
/// attachment row in the group at once — and `storage_key` is the sole
/// record of which object belonged to it.
///
/// Needs a real MinIO; skipped otherwise (see `real_minio_from_env`).
#[sqlx::test]
async fn deleting_a_group_removes_its_events_attachment_objects(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping deleting_a_group_removes_its_events_attachment_objects: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(s3.clone(), bucket.clone()),
    );
    let owner_cookie =
        register_verify_login(&router, &db, "group-attach@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id, "Réunion").await;

    let upload = call_upload(
        &router,
        &format!("/groups/{group_id}/events/{event_id}/attachments"),
        &owner_cookie,
        "ordonnance.png",
        PNG_BYTES,
    )
    .await;
    assert_status(&upload, StatusCode::CREATED);

    let keys = group_storage_keys(&db, &group_id).await;
    assert_eq!(keys.len(), 1);
    assert!(
        object_exists(&s3, &bucket, &keys[0]).await,
        "the uploaded object should exist before the delete"
    );

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    assert!(
        !object_exists(&s3, &bucket, &keys[0]).await,
        "deleting the group should have removed its attachment object from storage, \
         not just the row pointing at it"
    );
}

/// AC (#57): the radius is the whole group, not one event — every event's
/// attachments go, in a single batched `DeleteObjects` rather than one
/// round trip per key inside the open transaction.
#[sqlx::test]
async fn deleting_a_group_removes_the_objects_of_all_of_its_events(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping deleting_a_group_removes_the_objects_of_all_of_its_events: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(
        db.clone(),
        manage_our_home::storage::Storage::new(s3.clone(), bucket.clone()),
    );
    let owner_cookie = register_verify_login(
        &router,
        &db,
        "group-attach2@example.test",
        "owner-password1",
    )
    .await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;

    for title in ["Réunion", "Rendez-vous"] {
        let event_id = create_event(&router, &owner_cookie, &group_id, title).await;
        let upload = call_upload(
            &router,
            &format!("/groups/{group_id}/events/{event_id}/attachments"),
            &owner_cookie,
            "ordonnance.png",
            PNG_BYTES,
        )
        .await;
        assert_status(&upload, StatusCode::CREATED);
    }

    let keys = group_storage_keys(&db, &group_id).await;
    assert_eq!(keys.len(), 2, "one attachment per event");
    for key in &keys {
        assert!(object_exists(&s3, &bucket, key).await);
    }

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    for key in &keys {
        assert!(
            !object_exists(&s3, &bucket, key).await,
            "every event's attachment object should be gone, not just the first: {key}"
        );
    }
}

/// AC (#57): pins the ordering choice, as `event_delete_aborts_...` does
/// for the per-event path. Objects go first, the group row after, so a
/// storage failure aborts the whole delete: the caller sees an error and
/// can retry against a group that is still there, rather than a "deleted"
/// group whose bytes stay in the bucket with nothing pointing at them.
///
/// `test_router`'s storage points at an unreachable endpoint, which is
/// exactly the failure being pinned here.
#[sqlx::test]
async fn group_delete_aborts_when_an_attachment_object_cannot_be_removed(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "group-leak@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    let event_id = create_event(&router, &owner_cookie, &group_id, "Réunion").await;

    // Insert the attachment row directly: the upload path would need a
    // reachable MinIO, and this test wants an unreachable one.
    let user_id: Uuid = sqlx::query_scalar!(
        "SELECT id FROM users WHERE email = $1",
        "group-leak@example.test"
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let mut tx = with_family_scope(&db, &group_id).await;
    sqlx::query(
        "INSERT INTO event_attachments (event_id, uploaded_by, storage_key, filename, mime_type, size_bytes)
         VALUES ($1, $2, $3, 'ordonnance.png', 'image/png', 42)",
    )
    .bind(Uuid::parse_str(&event_id).unwrap())
    .bind(user_id)
    .bind(format!("{group_id}/{event_id}/{}", Uuid::new_v4()))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::INTERNAL_SERVER_ERROR);

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::OK);

    let get_event = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}/events/{event_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get_event, StatusCode::OK);
    assert_eq!(
        group_storage_keys(&db, &group_id).await.len(),
        1,
        "a failed object delete must leave the attachment row for the retry"
    );
}

/// A group with no attachments at all is the common case: it must not turn
/// into an empty `DeleteObjects` call, which S3 rejects — and here would
/// mean an owner can never delete an attachment-free group.
#[sqlx::test]
async fn deleting_a_group_with_no_attachments_still_succeeds(db: PgPool) {
    let router = test_router(db.clone());
    let owner_cookie =
        register_verify_login(&router, &db, "group-empty@example.test", "owner-password1").await;
    let group_id = create_group(&router, &owner_cookie, "Foyer").await;
    create_event(&router, &owner_cookie, &group_id, "Réunion").await;

    let delete = call(
        &router,
        Method::DELETE,
        &format!("/groups/{group_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&delete, StatusCode::NO_CONTENT);

    let get = call(
        &router,
        Method::GET,
        &format!("/groups/{group_id}"),
        Some(&owner_cookie),
        None,
    )
    .await;
    assert_status(&get, StatusCode::NOT_FOUND);
}
