//! What an attachment upload costs apps/api in memory (#249).
//!
//! One test, alone in its binary: the allocator below counts every heap
//! byte of the process, which only measures the uploads while nothing else
//! runs beside them. Counting per thread would miss what a multi-threaded
//! runtime moves between its workers — and production runs one
//! (`#[tokio::main]` in `src/main.rs`).

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{Method, StatusCode};
use axum::Router;
use bytes::Bytes;
use common::{assert_status, call, json_body, set_cookie, test_state};
use manage_our_home::storage::{Storage, MAX_ATTACHMENT_SIZE_BYTES, MAX_UPLOAD_BODY_BYTES};
use manage_our_home_http_guard::gate::GLOBAL_UPLOADS;
use manage_our_home_http_guard::UploadGate;
use sqlx::postgres::PgConnectOptions;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Heap bytes live in the process, and their high-water mark.
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicIsize, Ordering};

    struct Counting;

    static LIVE: AtomicIsize = AtomicIsize::new(0);
    static PEAK: AtomicIsize = AtomicIsize::new(0);

    fn add(delta: isize) {
        let now = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }

    fn size(n: usize) -> isize {
        isize::try_from(n).unwrap_or(isize::MAX)
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = System.alloc(layout);
            if !ptr.is_null() {
                add(size(layout.size()));
            }
            ptr
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = System.alloc_zeroed(layout);
            if !ptr.is_null() {
                add(size(layout.size()));
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout);
            add(-size(layout.size()));
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let moved = System.realloc(ptr, layout, new_size);
            if !moved.is_null() {
                add(size(new_size) - size(layout.size()));
            }
            moved
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    /// Starts a new high-water mark from what is live now, and returns it.
    pub fn start() -> isize {
        let live = LIVE.load(Ordering::Relaxed);
        PEAK.store(live, Ordering::Relaxed);
        live
    }

    pub fn peak() -> isize {
        PEAK.load(Ordering::Relaxed)
    }
}

const BOUNDARY: &str = "----manageourhomememoryboundary";

/// A stand-in object storage that answers no `PutObject` before `holders`
/// of them have reached it: until then, every upload is held whole by
/// apps/api, body read and object not yet written.
async fn holding_storage(holders: usize) -> Storage {
    let barrier = Arc::new(tokio::sync::Barrier::new(holders));
    let app = Router::new()
        .fallback(move |body: Body| {
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                // Read and drop, chunk by chunk: what this side keeps is
                // not what is being measured.
                let mut body = body;
                while let Some(frame) = http_body_util::BodyExt::frame(&mut body).await {
                    frame.unwrap();
                }
                StatusCode::OK
            }
        })
        .layer(DefaultBodyLimit::disable());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::{BehaviorVersion, Region};
    let config = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .endpoint_url(format!("http://{addr}"))
        .credentials_provider(Credentials::new("test", "test", None, None, "test"))
        .force_path_style(true)
        .build();
    Storage::new(aws_sdk_s3::Client::from_conf(config), "test-bucket".into())
}

/// A verified member with a group and an event in it: the session cookie
/// and the event's attachments URL.
async fn uploader(router: &Router, db: &PgPool) -> (String, String) {
    let email = "memoire@example.test";
    let password = "memory-password1";
    call(
        router,
        Method::POST,
        "/auth/register",
        None,
        Some(serde_json::json!({"email": email, "password": password, "display_name": "Mémoire", "declares_minimum_age": true})),
    )
    .await;
    let token: uuid::Uuid = sqlx::query_scalar(
        "SELECT token FROM email_verification_tokens t JOIN users u ON u.id = t.user_id WHERE u.email = $1",
    )
    .bind(email)
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
    let cookie = set_cookie(&login).unwrap();

    let group = call(
        router,
        Method::POST,
        "/groups",
        Some(&cookie),
        Some(serde_json::json!({"name": "Foyer"})),
    )
    .await;
    assert_status(&group, StatusCode::CREATED);
    let group_id = json_body(group).await["id"].as_str().unwrap().to_string();
    let starts_at = chrono::Utc::now() + chrono::Duration::days(1);
    let event = call(
        router,
        Method::POST,
        &format!("/groups/{group_id}/events"),
        Some(&cookie),
        Some(serde_json::json!({
            "title": "Rendez-vous",
            "starts_at": starts_at,
            "ends_at": starts_at + chrono::Duration::hours(1),
        })),
    )
    .await;
    assert_status(&event, StatusCode::CREATED);
    let event_id = json_body(event).await["id"].as_str().unwrap().to_string();
    (
        cookie,
        format!("/groups/{group_id}/events/{event_id}/attachments"),
    )
}

/// Posts `whole` over a real connection, as a browser's would arrive: in
/// 16 KiB writes, each a view of `whole` rather than a copy of it. Returns
/// the status code.
async fn post(addr: SocketAddr, uri: &str, cookie: &str, whole: Bytes) -> u16 {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST {uri} HTTP/1.1\r\nHost: api\r\nCookie: {cookie}\r\n\
         Content-Type: multipart/form-data; boundary={BOUNDARY}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        whole.len()
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    for chunk in whole.chunks(16 * 1024) {
        socket.write_all(chunk).await.unwrap();
    }
    let mut answer = Vec::new();
    socket.read_to_end(&mut answer).await.unwrap();
    let status_line = String::from_utf8_lossy(&answer[..answer.len().min(12)]).to_string();
    status_line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line in {status_line:?}"))
}

async fn measure(options: PgConnectOptions) {
    let pool = GLOBAL_UPLOADS;
    // Production's pool size (`src/main.rs`): each upload holds a
    // transaction open while its object is written, so the pool has to
    // take a full pool of uploads at once.
    let db = manage_our_home::db::pool_options()
        .max_connections(20)
        .connect_with(options)
        .await
        .unwrap();
    let mut state = test_state(db.clone());
    state.db = common::runtime_pool_sized(&db, 20);
    state.storage = holding_storage(pool).await;
    // One account for the whole pool: the per-account bound is not what
    // is measured here.
    state.upload_gate = UploadGate::new(pool, pool);
    let router = manage_our_home::build_router(state);
    let (cookie, uri) = uploader(&router, &db).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });

    let size = MAX_ATTACHMENT_SIZE_BYTES;
    let mut whole = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"scan.png\"\r\n\r\n"
    )
    .into_bytes();
    whole.extend(b"\x89PNG\r\n\x1a\n");
    whole.resize(whole.len() + size - 8, 0);
    whole.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let whole = Bytes::from(whole);

    let baseline = heap::start();
    let mut uploads = tokio::task::JoinSet::new();
    for _ in 0..pool {
        let (cookie, uri, whole) = (cookie.clone(), uri.clone(), whole.clone());
        uploads.spawn(async move { post(addr, &uri, &cookie, whole).await });
    }
    let statuses = tokio::time::timeout(Duration::from_secs(120), async {
        let mut statuses = Vec::new();
        while let Some(status) = uploads.join_next().await {
            statuses.push(status.unwrap());
        }
        statuses
    })
    .await
    .expect("the uploads did not all complete within 120 s");
    let held = usize::try_from(heap::peak() - baseline).unwrap();

    server.abort();
    db.close().await;

    assert_eq!(statuses, vec![201; pool]);
    let mib = |n: usize| n as f64 / (1024.0 * 1024.0);
    println!(
        "{pool} uploads of {size} bytes: peak {:.1} MiB over the baseline",
        mib(held)
    );
    // One body each, plus 1 MiB each for the buffers the body passes
    // through in apps/api — the connection's read buffer, which hyper
    // grows to about 400 KiB, is the bulk of it. Plus 512 KiB each for the
    // stand-in storage, which runs in this process where MinIO would not:
    // it reads each object through a buffer of the same kind. When #249
    // was fixed, a full pool peaked at about 169 MiB against this 172.5.
    // A doubled buffer costs 12 MiB more per upload, a second copy 20, and
    // keeping the multipart reader until the object is written about 0.5.
    let budget = pool * (MAX_UPLOAD_BODY_BYTES + 1024 * 1024 + 512 * 1024);
    assert!(
        held <= budget,
        "{:.1} MiB held, budget {:.1} MiB",
        mib(held),
        mib(budget)
    );
}

/// What the upload gate's arithmetic assumes (`GLOBAL_UPLOADS`, #249): a
/// full pool of uploads, each of a file at the cap, costs apps/api one copy
/// of each body — not the slack of a buffer grown by doubling while
/// reading, nor a second copy handed to the object storage.
///
/// Measured on a multi-threaded runtime, as production runs, over real
/// connections: the socket buffers the server reads through count too.
#[sqlx::test]
async fn a_full_pool_of_uploads_at_the_cap_holds_one_copy_of_each(db: PgPool) {
    let options = (*db.connect_options()).clone();
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(measure(options))
    })
    .await
    .unwrap();
}
