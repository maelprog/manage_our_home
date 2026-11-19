//! The family threshold of an edit bar (#866), checked the same way on
//! stocks, recipes, the grocery list, the agenda and the messagerie: an
//! owner, an admin and a standard member, the owner moving the threshold of
//! one feature, and who may then edit or delete what another member wrote.
//!
//! Every helper is `#[allow(dead_code)]`: each test binary compiles its own
//! copy of `common`, and those that do not use this module would otherwise
//! warn (see the note on `test_state`).

use axum::http::{Method, StatusCode};
use sqlx::PgPool;

use super::{assert_status, call, json_body, set_cookie, verification_token};

/// An owner, an admin and a standard member of one family, logged in.
#[allow(dead_code)]
pub struct Family {
    pub owner: String,
    pub admin: String,
    pub standard: String,
    pub group_id: String,
}

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    let password = "threshold-password1";
    let res = call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": email,
            "password": password,
            "display_name": email,
            "declares_minimum_age": true,
            "accepts_terms": true,
        })),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let token = verification_token(db, email).await;
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

/// `email`'s account joins the family as a standard member, then is given
/// `role` by the owner unless it is `standard`.
async fn join(
    router: &axum::Router,
    db: &PgPool,
    owner: &str,
    group_id: &str,
    email: &str,
    role: &str,
) -> String {
    let cookie = register_verify_login(router, db, email).await;
    let invite = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/invitations"),
        Some(owner),
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
        Some(&cookie),
        None,
    )
    .await;
    assert_status(&accept, StatusCode::OK);
    if role != "standard" {
        let member_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(email)
            .fetch_one(db)
            .await
            .unwrap();
        let res = call(
            router,
            Method::POST,
            &format!("/groups/{group_id}/members/{member_id}/role"),
            Some(owner),
            Some(serde_json::json!({ "role": role })),
        )
        .await;
        assert_status(&res, StatusCode::OK);
    }
    cookie
}

/// A fresh family: its owner, an admin and a standard member.
#[allow(dead_code)]
pub async fn family(router: &axum::Router, db: &PgPool, prefix: &str) -> Family {
    let owner = register_verify_login(router, db, &format!("{prefix}-owner@example.test")).await;
    let res = call(
        router,
        Method::POST,
        "/groups",
        Some(&owner),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    let group_id = json_body(res).await["id"].as_str().unwrap().to_string();
    let admin = join(
        router,
        db,
        &owner,
        &group_id,
        &format!("{prefix}-admin@example.test"),
        "admin",
    )
    .await;
    let standard = join(
        router,
        db,
        &owner,
        &group_id,
        &format!("{prefix}-standard@example.test"),
        "standard",
    )
    .await;
    Family {
        owner,
        admin,
        standard,
        group_id,
    }
}

/// The owner sets the family's threshold of `feature` to `min_role`.
#[allow(dead_code)]
pub async fn set_threshold(router: &axum::Router, fam: &Family, feature: &str, min_role: &str) {
    let res = call(
        router,
        Method::PUT,
        &format!("/groups/{}/access-policies", fam.group_id),
        Some(&fam.owner),
        Some(serde_json::json!({"policies": [{"feature": feature, "min_role": min_role}]})),
    )
    .await;
    assert_status(&res, StatusCode::OK);
}

/// What a module's edit bar is checked on: `POST /groups/:id/<collection>`
/// with `create` makes a record, `PATCH …/<collection>/:id` with `edit`
/// edits it, `DELETE …/<collection>/:id` deletes it.
#[allow(dead_code)]
pub struct Editable {
    pub feature: &'static str,
    pub collection: &'static str,
    pub create: serde_json::Value,
    pub edit: serde_json::Value,
}

impl Editable {
    async fn create(&self, router: &axum::Router, fam: &Family, cookie: &str) -> String {
        let res = call(
            router,
            Method::POST,
            &format!("/groups/{}/{}", fam.group_id, self.collection),
            Some(cookie),
            Some(self.create.clone()),
        )
        .await;
        assert_status(&res, StatusCode::CREATED);
        json_body(res).await["id"].as_str().unwrap().to_string()
    }

    /// The status of `cookie`'s edit, then of its delete, of record `id`.
    async fn edit_and_delete(
        &self,
        router: &axum::Router,
        fam: &Family,
        cookie: &str,
        id: &str,
    ) -> (StatusCode, StatusCode) {
        let uri = format!("/groups/{}/{}/{id}", fam.group_id, self.collection);
        let edit = call(
            router,
            Method::PATCH,
            &uri,
            Some(cookie),
            Some(self.edit.clone()),
        )
        .await;
        let delete = call(router, Method::DELETE, &uri, Some(cookie), None).await;
        (edit.status(), delete.status())
    }
}

/// AC #866 on one module: lowered to `standard`, the threshold lets a
/// standard member edit and delete what the owner wrote; raised to `owner`,
/// it refuses both to an admin (and to the standard member), while the
/// owner still may and a member keeps the hand on what they wrote.
#[allow(dead_code)]
pub async fn assert_edit_follows_the_threshold(router: &axum::Router, db: &PgPool, what: Editable) {
    let fam = family(router, db, what.feature).await;
    const OK: (StatusCode, StatusCode) = (StatusCode::OK, StatusCode::NO_CONTENT);
    const REFUSED: (StatusCode, StatusCode) = (StatusCode::FORBIDDEN, StatusCode::FORBIDDEN);

    // The default threshold, `admin`: a standard member may not.
    let owners = what.create(router, &fam, &fam.owner).await;
    assert_eq!(
        what.edit_and_delete(router, &fam, &fam.standard, &owners)
            .await,
        REFUSED,
        "a standard member under the default threshold"
    );

    set_threshold(router, &fam, what.feature, "standard").await;
    assert_eq!(
        what.edit_and_delete(router, &fam, &fam.standard, &owners)
            .await,
        OK,
        "a standard member under `standard`"
    );

    set_threshold(router, &fam, what.feature, "owner").await;
    let owners = what.create(router, &fam, &fam.owner).await;
    for (who, cookie) in [("admin", &fam.admin), ("standard", &fam.standard)] {
        assert_eq!(
            what.edit_and_delete(router, &fam, cookie, &owners).await,
            REFUSED,
            "{who} under `owner`"
        );
    }
    let admins = what.create(router, &fam, &fam.admin).await;
    assert_eq!(
        what.edit_and_delete(router, &fam, &fam.admin, &admins)
            .await,
        OK,
        "the admin on their own record under `owner`"
    );
    let standards = what.create(router, &fam, &fam.standard).await;
    assert_eq!(
        what.edit_and_delete(router, &fam, &fam.standard, &standards)
            .await,
        OK,
        "the standard member on their own record under `owner`"
    );
    assert_eq!(
        what.edit_and_delete(router, &fam, &fam.owner, &owners)
            .await,
        OK,
        "the owner on their own record under `owner`"
    );
}
