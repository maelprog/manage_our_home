//! The profile photo (#467) against a real database, and against a real
//! MinIO for the tests that look at the stored object (they skip themselves
//! without one, see `common::real_minio_from_env`).

mod common;

use axum::body::Body;
use axum::http::{header, Method, Response, StatusCode};
use chrono::Utc;
use common::{
    assert_status, call, call_multipart, insert_session, json_body, real_minio_from_env,
    test_router, test_router_with_storage,
};
use http_body_util::BodyExt;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ImageEncoder, Rgb, RgbImage};
use manage_our_home::attachment_reconcile::{reconcile, Options};
use manage_our_home::avatar::{AVATAR_SIDE, MAX_AVATAR_BYTES, MAX_JPEG_SIDE};
use manage_our_home::jobs::account_purge::{purge_account, purge_due_accounts_and_objects};
use manage_our_home::storage::Storage;
use sqlx::PgPool;
use uuid::Uuid;

/// An account able to use the API (age declared, CGU accepted), and the
/// cookie of a session of it.
async fn user(db: &PgPool, email: &str) -> (Uuid, String) {
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

async fn group_of(db: &PgPool, members: &[Uuid]) -> Uuid {
    let group: Uuid = sqlx::query_scalar(
        "INSERT INTO groups (name, created_by) VALUES ('Foyer', $1) RETURNING id",
    )
    .bind(members[0])
    .fetch_one(db)
    .await
    .unwrap();
    for (i, member) in members.iter().enumerate() {
        sqlx::query(
            "INSERT INTO group_members (group_id, user_id, role) VALUES ($1, $2, $3::group_role)",
        )
        .bind(group)
        .bind(member)
        .bind(if i == 0 { "owner" } else { "standard" })
        .execute(db)
        .await
        .unwrap();
    }
    group
}

async fn avatar_key(db: &PgPool, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT avatar_key FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn set_avatar_key(db: &PgPool, id: Uuid, key: &str) {
    sqlx::query("UPDATE users SET avatar_key = $2 WHERE id = $1")
        .bind(id)
        .bind(key)
        .execute(db)
        .await
        .unwrap();
}

async fn put_avatar(
    router: &axum::Router,
    cookie: &str,
    name: &str,
    bytes: &[u8],
) -> Response<Body> {
    call_multipart(router, Method::PUT, "/me/avatar", cookie, name, bytes).await
}

async fn body_bytes(response: Response<Body>) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn picture(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        Rgb([(x * 7) as u8, (y * 5) as u8, 90])
    })
}

fn png(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

/// A JPEG as a phone writes one: an APP1 EXIF segment holding a GPS IFD
/// with a latitude reference and a marker string.
fn jpeg_with_gps() -> Vec<u8> {
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&picture(120, 80))
        .unwrap();

    let marker = b"GPS-MARKER-48.8566N\0";
    let gps_offset: u32 = 8 + 2 + 12 + 4;
    let marker_offset: u32 = gps_offset + 2 + 2 * 12 + 4;
    let mut tiff = b"II*\0".to_vec();
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x8825u16.to_le_bytes());
    tiff.extend_from_slice(&4u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&gps_offset.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&0x0001u16.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&2u32.to_le_bytes());
    tiff.extend_from_slice(b"N\0\0\0");
    tiff.extend_from_slice(&0x0012u16.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&(marker.len() as u32).to_le_bytes());
    tiff.extend_from_slice(&marker_offset.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(marker);

    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}

async fn stored(s3: &aws_sdk_s3::Client, bucket: &str, key: &str) -> Option<Vec<u8>> {
    match s3.get_object().bucket(bucket).key(key).send().await {
        Ok(object) => Some(object.body.collect().await.unwrap().to_vec()),
        Err(_) => None,
    }
}

/// Upload, replacement and deletion, end to end: what is stored is the
/// re-encoded square, without the EXIF and GPS block of the upload; a
/// replacement removes the previous object; a deletion removes the object
/// and the key.
#[sqlx::test]
async fn a_photo_is_reencoded_replaced_and_deleted(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping a_photo_is_reencoded_replaced_and_deleted: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(db.clone(), Storage::new(s3.clone(), bucket.clone()));
    let (me, cookie) = user(&db, "photo@example.test").await;

    let source = jpeg_with_gps();
    assert!(contains(&source, b"GPS-MARKER"));
    assert_status(
        &put_avatar(&router, &cookie, "vacances.jpg", &source).await,
        StatusCode::NO_CONTENT,
    );

    let first = avatar_key(&db, me).await.expect("the key is written");
    assert!(first.starts_with(&format!("avatars/{me}/")), "{first}");
    let object = stored(&s3, &bucket, &first)
        .await
        .expect("the object is put");
    assert!(!contains(&object, b"Exif"), "an EXIF block was stored");
    assert!(!contains(&object, b"GPS-MARKER"), "the GPS data was stored");
    let decoded = image::load_from_memory(&object).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (AVATAR_SIDE, AVATAR_SIDE)
    );

    let served = call(&router, Method::GET, "/me/avatar", Some(&cookie), None).await;
    assert_status(&served, StatusCode::OK);
    assert_eq!(served.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(served.headers()[header::CACHE_CONTROL]
        .to_str()
        .unwrap()
        .contains("private"));
    assert_eq!(body_bytes(served).await, object);

    // Replacement: a new key, and the previous object gone.
    assert_status(
        &put_avatar(&router, &cookie, "autre.png", &png(&picture(40, 90))).await,
        StatusCode::NO_CONTENT,
    );
    let second = avatar_key(&db, me).await.expect("the new key is written");
    assert_ne!(second, first);
    assert!(stored(&s3, &bucket, &second).await.is_some());
    assert!(
        stored(&s3, &bucket, &first).await.is_none(),
        "the replaced photo stayed in the bucket"
    );

    // Deletion: the object and the key go, and the photo answers 404.
    let deleted = call(&router, Method::DELETE, "/me/avatar", Some(&cookie), None).await;
    assert_status(&deleted, StatusCode::NO_CONTENT);
    assert_eq!(avatar_key(&db, me).await, None);
    assert!(stored(&s3, &bucket, &second).await.is_none());
    assert_status(
        &call(&router, Method::GET, "/me/avatar", Some(&cookie), None).await,
        StatusCode::NOT_FOUND,
    );
    // Deleting nothing is no error.
    assert_status(
        &call(&router, Method::DELETE, "/me/avatar", Some(&cookie), None).await,
        StatusCode::NO_CONTENT,
    );
}

/// Criterion of #467: a 50 MB file, or a PDF named `.jpg`, is refused 4xx
/// and nothing is stored. A previous photo stays in place.
#[sqlx::test]
async fn a_refused_upload_stores_nothing(db: PgPool) {
    let router = test_router(db.clone());
    let (me, cookie) = user(&db, "refus@example.test").await;
    set_avatar_key(&db, me, "avatars/previous").await;

    let huge = vec![0xFFu8; 50 * 1024 * 1024];
    let response = put_avatar(&router, &cookie, "enorme.jpg", &huge).await;
    assert_status(&response, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(response).await["error"], "file_too_large");

    // Just past the cap: refused as well.
    let past_cap = vec![0u8; MAX_AVATAR_BYTES + 1];
    let response = put_avatar(&router, &cookie, "juste.jpg", &past_cap).await;
    assert_status(&response, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(response).await["error"], "file_too_large");

    let pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\ntrailer\n%%EOF\n";
    let response = put_avatar(&router, &cookie, "photo.jpg", pdf).await;
    assert_status(&response, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(response).await["error"], "unsupported_file_type");

    let truncated = png(&picture(64, 64));
    let response = put_avatar(&router, &cookie, "coupe.png", &truncated[..100]).await;
    assert_status(&response, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(response).await["error"], "unreadable_image");

    // Criterion of #588: a readable picture past the bounds says so.
    let mut strip = Vec::new();
    JpegEncoder::new_with_quality(&mut strip, 90)
        .encode_image(&picture(MAX_JPEG_SIDE + 8, 8))
        .unwrap();
    let response = put_avatar(&router, &cookie, "bandeau.jpg", &strip).await;
    assert_status(&response, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json_body(response).await["error"], "image_too_large");

    // The storage behind `test_router` is unreachable: had any of these
    // reached it, the answer would have been 500.
    assert_eq!(
        avatar_key(&db, me).await.as_deref(),
        Some("avatars/previous")
    );
}

/// Criterion of #588: the photo of a 48/50 Mpx phone mode, 8000 × 6000, is
/// accepted (204, as every upload) and served as the usual square — sent
/// at exactly `MAX_AVATAR_BYTES`, the largest body the route lets through.
#[sqlx::test]
async fn a_50_mpx_photo_is_accepted_and_served_as_the_square(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping a_50_mpx_photo_is_accepted_and_served_as_the_square: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(db.clone(), Storage::new(s3, bucket));
    let (_, cookie) = user(&db, "capteur@example.test").await;

    // Smooth gradients: `picture`'s steep ones would weigh past the cap at
    // this size.
    let smooth = RgbImage::from_fn(8000, 6000, |x, y| Rgb([(x / 32) as u8, (y / 24) as u8, 90]));
    let mut photo = Vec::new();
    JpegEncoder::new_with_quality(&mut photo, 90)
        .encode_image(&smooth)
        .unwrap();
    assert!(photo.len() <= MAX_AVATAR_BYTES, "{}", photo.len());
    // Padded past its end marker to exactly the cap: the route's body
    // limit lets the largest accepted file through.
    photo.resize(MAX_AVATAR_BYTES, 0);
    assert_status(
        &put_avatar(&router, &cookie, "capteur-50mpx.jpg", &photo).await,
        StatusCode::NO_CONTENT,
    );

    let served = call(&router, Method::GET, "/me/avatar", Some(&cookie), None).await;
    assert_status(&served, StatusCode::OK);
    let decoded = image::load_from_memory(&body_bytes(served).await).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (AVATAR_SIDE, AVATAR_SIDE)
    );
}

#[sqlx::test]
async fn the_routes_need_a_session(db: PgPool) {
    let router = test_router(db.clone());
    let (me, _) = user(&db, "anonyme@example.test").await;
    let group = group_of(&db, &[me]).await;
    for (method, uri) in [
        (Method::GET, "/me/avatar".to_string()),
        (Method::DELETE, "/me/avatar".to_string()),
        (Method::PUT, "/me/avatar".to_string()),
        (Method::GET, format!("/groups/{group}/members/{me}/avatar")),
    ] {
        let response = call(&router, method.clone(), &uri, None, None).await;
        assert_status(&response, StatusCode::UNAUTHORIZED);
    }
}

/// Criterion of #467: the rule of `GET /groups/:id/members/:user_id`
/// (#466). Every refusal is the 404 of a member without a photo; none of
/// them reaches the storage (unreachable behind `test_router`). Run under
/// the runtime role of the job, then on the harness pool, which bypasses
/// RLS: there only the checks written in the query hold.
#[sqlx::test]
async fn a_member_photo_is_refused_outside_a_shared_group(db: PgPool) {
    let mut bypassing = common::test_state(db.clone());
    bypassing.db = db.clone();
    for (tag, router) in [
        ("runtime", test_router(db.clone())),
        ("bypass", manage_our_home::build_router(bypassing)),
    ] {
        refusals_outside_a_shared_group(&db, &router, tag).await;
    }
}

async fn refusals_outside_a_shared_group(db: &PgPool, router: &axum::Router, tag: &str) {
    let (db, router) = (db.clone(), router.clone());
    let (alice, _) = user(&db, &format!("alice-{tag}@example.test")).await;
    let (bob, bob_cookie) = user(&db, &format!("bob-{tag}@example.test")).await;
    let (carol, _) = user(&db, &format!("carol-{tag}@example.test")).await;
    let (_, dave_cookie) = user(&db, &format!("dave-{tag}@example.test")).await;
    let shared = group_of(&db, &[alice, bob]).await;
    let other = group_of(&db, &[alice, carol]).await;
    for id in [alice, bob, carol] {
        set_avatar_key(&db, id, &format!("avatars/{id}/x")).await;
    }

    for (cookie, group, target, case) in [
        (&dave_cookie, shared, alice, "caller outside the group"),
        (&bob_cookie, shared, carol, "target outside the group"),
        (
            &bob_cookie,
            other,
            carol,
            "caller outside the target's group",
        ),
        (&bob_cookie, Uuid::new_v4(), alice, "unknown group"),
        (&bob_cookie, shared, Uuid::new_v4(), "unknown target"),
    ] {
        let response = call(
            &router,
            Method::GET,
            &format!("/groups/{group}/members/{target}/avatar"),
            Some(cookie),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{tag}: {case}");
    }

    // A member of a shared group with no photo: the same 404.
    sqlx::query("UPDATE users SET avatar_key = NULL WHERE id = $1")
        .bind(alice)
        .execute(&db)
        .await
        .unwrap();
    let response = call(
        &router,
        Method::GET,
        &format!("/groups/{shared}/members/{alice}/avatar"),
        Some(&bob_cookie),
        None,
    )
    .await;
    assert_status(&response, StatusCode::NOT_FOUND);
}

/// #468: the member list and a member's profile say who has a photo, so
/// that apps/web shows it in place of the initial — and never the key.
#[sqlx::test]
async fn the_member_list_and_profile_say_who_has_a_photo(db: PgPool) {
    let router = test_router(db.clone());
    let (alice, alice_cookie) = user(&db, "alice-has@example.test").await;
    let (bob, _) = user(&db, "bob-has@example.test").await;
    let group = group_of(&db, &[alice, bob]).await;
    set_avatar_key(&db, alice, &format!("avatars/{alice}/{}", Uuid::new_v4())).await;

    let response = call(
        &router,
        Method::GET,
        &format!("/groups/{group}"),
        Some(&alice_cookie),
        None,
    )
    .await;
    assert_status(&response, StatusCode::OK);
    let detail = json_body(response).await;
    assert!(!detail.to_string().contains("avatars/"), "{detail}");
    for member in detail["members"].as_array().unwrap() {
        let expected = member["user_id"] == alice.to_string();
        assert_eq!(member["has_avatar"], expected, "{member}");
    }

    for (target, expected) in [(alice, true), (bob, false)] {
        let response = call(
            &router,
            Method::GET,
            &format!("/groups/{group}/members/{target}"),
            Some(&alice_cookie),
            None,
        )
        .await;
        assert_status(&response, StatusCode::OK);
        let profile = json_body(response).await;
        assert_eq!(profile["has_avatar"], expected, "{profile}");
        assert!(!profile.to_string().contains("avatars/"), "{profile}");
    }
}

/// The positive side of the rule: a member reads another member's photo,
/// and their own, through the group they share.
#[sqlx::test]
async fn a_member_photo_is_served_inside_a_shared_group(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping a_member_photo_is_served_inside_a_shared_group: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let router = test_router_with_storage(db.clone(), Storage::new(s3.clone(), bucket.clone()));
    let (alice, alice_cookie) = user(&db, "alice-photo@example.test").await;
    let (bob, bob_cookie) = user(&db, "bob-photo@example.test").await;
    let group = group_of(&db, &[alice, bob]).await;
    assert_status(
        &put_avatar(&router, &alice_cookie, "a.png", &png(&picture(30, 30))).await,
        StatusCode::NO_CONTENT,
    );
    let key = avatar_key(&db, alice).await.unwrap();
    let object = stored(&s3, &bucket, &key).await.unwrap();

    for cookie in [&bob_cookie, &alice_cookie] {
        let response = call(
            &router,
            Method::GET,
            &format!("/groups/{group}/members/{alice}/avatar"),
            Some(cookie),
            None,
        )
        .await;
        assert_status(&response, StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(body_bytes(response).await, object);
    }

    // #468: apps/web asks `HEAD /me/avatar` whether the caller has a photo,
    // for `/account`.
    for (cookie, status) in [
        (&alice_cookie, StatusCode::OK),
        (&bob_cookie, StatusCode::NOT_FOUND),
    ] {
        let response = call(&router, Method::HEAD, "/me/avatar", Some(cookie), None).await;
        assert_status(&response, status);
    }

    s3.delete_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .unwrap();
}

/// Criterion of #467: the purge of an account clears its key and hands the
/// object back to be deleted — with no storage at all.
#[sqlx::test]
async fn the_purge_clears_the_key_and_hands_back_the_object(db: PgPool) {
    let (me, _) = user(&db, "purge@example.test").await;
    set_avatar_key(&db, me, &format!("avatars/{me}/photo")).await;
    sqlx::query(
        "UPDATE users SET deletion_requested_at = now() - interval '31 days' WHERE id = $1",
    )
    .bind(me)
    .execute(&db)
    .await
    .unwrap();

    let keys = purge_account(&db, me).await.unwrap();

    assert_eq!(keys, vec![format!("avatars/{me}/photo")]);
    assert_eq!(avatar_key(&db, me).await, None);
}

/// The same, through the purge step of the job, against a real bucket:
/// the object is gone once the pass has run.
#[sqlx::test]
async fn the_purge_deletes_the_photo_object(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping the_purge_deletes_the_photo_object: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let router = test_router_with_storage(db.clone(), storage.clone());
    let (me, cookie) = user(&db, "purge-photo@example.test").await;
    assert_status(
        &put_avatar(&router, &cookie, "p.png", &png(&picture(20, 20))).await,
        StatusCode::NO_CONTENT,
    );
    let key = avatar_key(&db, me).await.unwrap();
    assert!(stored(&s3, &bucket, &key).await.is_some());
    sqlx::query(
        "UPDATE users SET deletion_requested_at = now() - interval '31 days' WHERE id = $1",
    )
    .bind(me)
    .execute(&db)
    .await
    .unwrap();

    purge_due_accounts_and_objects(&db, &storage).await.unwrap();

    assert_eq!(avatar_key(&db, me).await, None);
    assert!(
        stored(&s3, &bucket, &key).await.is_none(),
        "the purged account's photo stayed in the bucket"
    );
}

/// The reconciliation pass knows the avatar keys: a photo a row names is
/// kept, an object under `avatars/` that none names is deleted. Scoped to
/// this account's prefix, so that it touches no other test's objects.
#[sqlx::test]
async fn the_reconciliation_keeps_a_photo_and_deletes_an_orphan(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping the_reconciliation_keeps_a_photo_and_deletes_an_orphan: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let router = test_router_with_storage(db.clone(), storage.clone());
    let (me, cookie) = user(&db, "reconcile-photo@example.test").await;
    assert_status(
        &put_avatar(&router, &cookie, "r.png", &png(&picture(20, 20))).await,
        StatusCode::NO_CONTENT,
    );
    let key = avatar_key(&db, me).await.unwrap();
    let orphan = format!("avatars/{me}/orphan");
    storage
        .put_object(&orphan, bytes::Bytes::from_static(b"x"), "image/jpeg")
        .await
        .unwrap();

    let mut conn = db.acquire().await.unwrap();
    let outcome = reconcile(
        &storage,
        &mut conn,
        &Options {
            apply: true,
            min_age_hours: 0,
            prefix: Some(format!("avatars/{me}/")),
        },
        // Past both objects, so that age keeps neither.
        Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();

    assert_eq!(outcome.deleted, vec![orphan.clone()]);
    assert!(stored(&s3, &bucket, &key).await.is_some());
    assert!(stored(&s3, &bucket, &orphan).await.is_none());

    s3.delete_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .unwrap();
}

/// Art. 15 and 20: the export says whether there is a photo and gives a
/// short-lived link to it, never its storage key. Signing is local: no
/// storage needs to answer.
#[sqlx::test]
async fn the_export_carries_a_link_to_the_photo(db: PgPool) {
    let router = test_router(db.clone());
    let (me, cookie) = user(&db, "export-photo@example.test").await;

    let export =
        json_body(call(&router, Method::GET, "/account/export", Some(&cookie), None).await).await;
    assert!(
        export["profile"]["avatar"].is_null(),
        "{}",
        export["profile"]
    );

    let key = format!("avatars/{me}/secret-key-part");
    set_avatar_key(&db, me, &key).await;
    let response = call(&router, Method::GET, "/account/export", Some(&cookie), None).await;
    assert_status(&response, StatusCode::OK);
    let export = json_body(response).await;
    let avatar = &export["profile"]["avatar"];
    assert_eq!(avatar["content_type"], "image/jpeg");
    assert!(
        avatar["download_url"]
            .as_str()
            .is_some_and(|url| url.contains("X-Amz-")),
        "{avatar}"
    );
    assert!(avatar.get("avatar_key").is_none() && avatar.get("storage_key").is_none());
}
