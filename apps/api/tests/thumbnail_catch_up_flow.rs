//! End-to-end cover for the catch-up of thumbnails (#777), against a real
//! MinIO and a real database.
//!
//! The unit tests in `jobs::thumbnail_catch_up` pin the selection and the
//! attempts; what only a real stack can show is that the pass reads the
//! rows and the objects it thinks it does, stores the thumbnail where the
//! upload would have, and leaves the rest alone.
//!
//! An attachment "from before #774" is made by uploading a picture, then
//! removing its thumbnail object and clearing its `thumbnail_key`: the row
//! and the bucket as an upload of that time left them. `sqlx::test` gives
//! each test its own database, so a pass only ever sees that test's rows,
//! even though the bucket is shared.

mod common;

use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use common::{
    assert_status, call, call_upload, json_body, real_minio_from_env, set_cookie,
    test_router_with_storage,
};
use manage_our_home::agenda::thumbnail::thumbnail_key;
use manage_our_home::jobs::thumbnail_catch_up::{
    catch_up, CatchUpOutcome, BATCH_SIZE, MAX_ATTEMPTS,
};
use manage_our_home::storage::Storage;
use sqlx::PgPool;
use uuid::Uuid;

/// A PNG signature and the start of an IHDR, nothing more: sniffed as a
/// PNG at upload, never decoded.
const CORRUPT_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

const PDF: &[u8] = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n";

fn real_png(width: u32, height: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let picture =
        image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 120]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            picture.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

async fn register_verify_login(router: &axum::Router, db: &PgPool, email: &str) -> String {
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({
            "email": email, "password": "owner-password1", "display_name": email,
            "declares_minimum_age": true, "accepts_terms": true
        })),
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
        Some(serde_json::json!({"email": email, "password": "owner-password1"})),
    )
    .await;
    set_cookie(&login).unwrap()
}

/// A signed-in owner with one event, to upload onto.
struct Family {
    router: axum::Router,
    cookie: String,
    group_id: String,
    event_id: String,
}

async fn family(db: &PgPool, storage: &Storage, email: &str) -> Family {
    let router = test_router_with_storage(db.clone(), storage.clone());
    let cookie = register_verify_login(&router, db, email).await;
    let create_group = call(
        &router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&create_group, StatusCode::CREATED);
    let group_id = json_body(create_group).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let starts_at = Utc::now() + Duration::days(1);
    let create_event = call(
        &router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&cookie),
        Some(serde_json::json!({
            "title": "Anniversaire",
            "starts_at": starts_at,
            "ends_at": starts_at + Duration::hours(1),
        })),
    )
    .await;
    assert_status(&create_event, StatusCode::CREATED);
    let event_id = json_body(create_event).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    Family {
        router,
        cookie,
        group_id,
        event_id,
    }
}

/// Uploads `bytes`, returns the attachment's id.
async fn upload(family: &Family, filename: &str, bytes: &[u8]) -> Uuid {
    let res = call_upload(
        &family.router,
        &format!(
            "/groups/{}/events/{}/attachments",
            family.group_id, family.event_id
        ),
        &family.cookie,
        filename,
        bytes,
    )
    .await;
    assert_status(&res, StatusCode::CREATED);
    Uuid::parse_str(json_body(res).await["id"].as_str().unwrap()).unwrap()
}

/// The row's storage key, thumbnail key and failed attempts.
async fn row(db: &PgPool, id: Uuid) -> (String, Option<String>, i16) {
    let r = sqlx::query!(
        "SELECT storage_key, thumbnail_key, thumbnail_attempts FROM event_attachments WHERE id = $1",
        id
    )
    .fetch_one(db)
    .await
    .unwrap();
    (r.storage_key, r.thumbnail_key, r.thumbnail_attempts)
}

/// The attachment as an upload from before #774 left it: its file, no
/// thumbnail object, no `thumbnail_key`.
async fn as_before_thumbnails(db: &PgPool, storage: &Storage, id: Uuid) {
    let (_, key, _) = row(db, id).await;
    let key = key.expect("the upload made a thumbnail to remove");
    storage.delete_objects(&[key]).await.unwrap();
    sqlx::query!(
        "UPDATE event_attachments SET thumbnail_key = NULL WHERE id = $1",
        id
    )
    .execute(db)
    .await
    .unwrap();
}

async fn exists(s3: &aws_sdk_s3::Client, bucket: &str, key: &str) -> bool {
    s3.head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .is_ok()
}

const NOTHING: CatchUpOutcome = CatchUpOutcome {
    generated: 0,
    undecodable: 0,
    failed: 0,
};

/// AC (#777): a picture from before #774 gets its thumbnail, at the key
/// and size the upload would have given it; a PDF is ignored; a second
/// pass does nothing.
#[sqlx::test]
async fn an_old_picture_gets_its_thumbnail_once_and_a_pdf_is_ignored(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping an_old_picture_gets_its_thumbnail_once_and_a_pdf_is_ignored: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let family = family(&db, &storage, "catch-up-a@example.test").await;
    let picture = upload(&family, "photo.png", &real_png(800, 600)).await;
    as_before_thumbnails(&db, &storage, picture).await;
    let pdf = upload(&family, "ordonnance.pdf", PDF).await;

    let first = catch_up(&db, &storage, BATCH_SIZE).await.unwrap();
    assert_eq!(
        first,
        CatchUpOutcome {
            generated: 1,
            ..NOTHING
        }
    );

    let (storage_key, key, attempts) = row(&db, picture).await;
    assert_eq!(key.as_deref(), Some(thumbnail_key(&storage_key).as_str()));
    assert_eq!(attempts, 0);
    let thumbnail = storage.get_object(&key.unwrap()).await.unwrap();
    let decoded = image::load_from_memory_with_format(&thumbnail, image::ImageFormat::Jpeg)
        .expect("the thumbnail is a JPEG");
    assert_eq!((decoded.width(), decoded.height()), (400, 300));
    assert!(exists(&s3, &bucket, &storage_key).await, "the file stays");

    let (_, pdf_key, pdf_attempts) = row(&db, pdf).await;
    assert_eq!((pdf_key, pdf_attempts), (None, 0), "a PDF is not tried");

    let second = catch_up(&db, &storage, BATCH_SIZE).await.unwrap();
    assert_eq!(second, NOTHING, "a second pass finds nothing to do");
}

/// AC (#777): a corrupt picture neither stops the pictures around it,
/// across several batches, nor is it tried again on the next pass.
#[sqlx::test]
async fn a_corrupt_picture_does_not_hold_the_batch_and_is_not_retried(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping a_corrupt_picture_does_not_hold_the_batch_and_is_not_retried: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let family = family(&db, &storage, "catch-up-b@example.test").await;
    let corrupt = upload(&family, "illisible.png", CORRUPT_PNG).await;
    let mut pictures = Vec::new();
    for name in ["un.png", "deux.png", "trois.png"] {
        let id = upload(&family, name, &real_png(64, 48)).await;
        as_before_thumbnails(&db, &storage, id).await;
        pictures.push(id);
    }

    // One row a batch: the corrupt one sits in a batch of its own,
    // wherever its id falls among the others.
    let first = catch_up(&db, &storage, 1).await.unwrap();
    assert_eq!(
        first,
        CatchUpOutcome {
            generated: 3,
            undecodable: 1,
            failed: 0,
        }
    );
    for id in pictures {
        let (storage_key, key, _) = row(&db, id).await;
        let key = key.expect("every readable picture gets its thumbnail");
        assert!(exists(&s3, &bucket, &key).await, "{storage_key}");
    }
    let (_, key, attempts) = row(&db, corrupt).await;
    assert_eq!(key, None);
    assert_eq!(attempts, MAX_ATTEMPTS, "given up on at once");

    let second = catch_up(&db, &storage, 1).await.unwrap();
    assert_eq!(second, NOTHING, "the corrupt picture is not tried again");
}

/// A picture whose file cannot be read is tried again on the next passes,
/// `MAX_ATTEMPTS` in all, then left alone: never retried forever.
#[sqlx::test]
async fn an_unreadable_file_is_tried_a_bounded_number_of_times(db: PgPool) {
    let Some((s3, bucket)) = real_minio_from_env() else {
        eprintln!(
            "skipping an_unreadable_file_is_tried_a_bounded_number_of_times: \
             no MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET in the environment"
        );
        return;
    };
    let storage = Storage::new(s3.clone(), bucket.clone());
    let family = family(&db, &storage, "catch-up-c@example.test").await;
    let picture = upload(&family, "photo.png", &real_png(64, 48)).await;
    as_before_thumbnails(&db, &storage, picture).await;
    let (storage_key, _, _) = row(&db, picture).await;
    storage
        .delete_objects(std::slice::from_ref(&storage_key))
        .await
        .unwrap();
    assert!(!exists(&s3, &bucket, &storage_key).await);

    for pass in 1..=MAX_ATTEMPTS {
        let outcome = catch_up(&db, &storage, BATCH_SIZE).await.unwrap();
        assert_eq!(
            outcome,
            CatchUpOutcome {
                failed: 1,
                ..NOTHING
            },
            "pass {pass}"
        );
        assert_eq!(row(&db, picture).await.2, pass);
    }
    let after = catch_up(&db, &storage, BATCH_SIZE).await.unwrap();
    assert_eq!(after, NOTHING, "past its attempts, left alone");
    assert_eq!(row(&db, picture).await.1, None);
}
