//! `/agenda/:id/attachments` — upload, download, delete event attachments.
//! Upload is a plain multipart `<form>`: `apps/web` reads the browser's
//! multipart body, **pre-validates size + extension** client-side
//! (`validate_attachment`, `architecture.md` § Uploads) before relaying the
//! file to apps/api, which stays the authority (sniffs the real MIME bytes).
//! Download resolves the short-lived presigned MinIO URL and 302-redirects
//! the browser onto it — never a public bucket link.

use axum::extract::{Multipart, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use manage_our_home_shared::validation::agenda::{validate_attachment, AttachmentError};
use uuid::Uuid;

use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{agenda_cookie, event_not_found_page, family_context, service_unavailable_page};

pub async fn upload(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/agenda/{event_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let detail = format!("/agenda/{event_id}");

    // Before a byte of the body (#219): the file is held in memory whole,
    // through the read and the relay below, so how many are held at once
    // is bounded per account and per process. Released when this returns,
    // or when the browser disconnects and the future is dropped.
    let _upload_permit = match state.upload_gate.try_acquire(me.user_id) {
        Ok(permit) => permit,
        Err(busy) => {
            tracing::info!(?busy, "upload turned away");
            return manage_our_home_http_guard::service_unavailable(
                service_unavailable_page().into_response(),
            );
        }
    };

    // Pull the single `file` field out of the multipart body, into one
    // buffer sized before the first chunk (#246): `Field::bytes` grows its
    // buffer by doubling, which leaves a file at the 20 MiB cap in a
    // 32 MiB allocation.
    let mut filename: Option<String> = None;
    let mut bytes: Option<axum::body::Bytes> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(mut field)) => {
                if field.name() == Some("file") {
                    filename = field.file_name().map(|s| s.to_string());
                    let mut file = Vec::with_capacity(file_buffer_capacity(&headers));
                    loop {
                        match field.chunk().await {
                            Ok(Some(chunk)) => file.extend_from_slice(&chunk),
                            Ok(None) => break,
                            Err(_) => {
                                return Redirect::to(&format!("{detail}?error=upload_failed"))
                                    .into_response()
                            }
                        }
                    }
                    // Takes the `Vec` over, no copy.
                    bytes = Some(axum::body::Bytes::from(file));
                }
            }
            Ok(None) => break,
            Err(_) => {
                return Redirect::to(&format!("{detail}?error=upload_failed")).into_response()
            }
        }
    }

    let (Some(filename), Some(bytes)) = (filename, bytes) else {
        return Redirect::to(&format!("{detail}?error=upload_failed")).into_response();
    };

    // Client-side pre-check (extension + size) before touching the network.
    match validate_attachment(&filename, bytes.len() as u64) {
        Err(AttachmentError::UnsupportedType) => {
            return Redirect::to(&format!("{detail}?error=unsupported_file_type")).into_response()
        }
        Err(AttachmentError::TooLarge) => {
            return Redirect::to(&format!("{detail}?error=file_too_large")).into_response()
        }
        Ok(()) => {}
    }

    // Forward to apps/api as a fresh multipart request (backend re-sniffs).
    let cookie = agenda_cookie(&headers);
    // The buffer itself goes out, not a copy of it (#246).
    let len = bytes.len() as u64;
    let part = reqwest::multipart::Part::stream_with_length(bytes, len).file_name(filename);
    let form = reqwest::multipart::Form::new().part("file", part);
    let mut req = state.http.post(format!(
        "{}/groups/{}/events/{}/attachments",
        state.api_internal_base_url, fam.gid, event_id
    ));
    if let Some(cookie) = cookie.as_deref() {
        req = req.header("cookie", cookie);
    }
    let resp = match req.multipart(form).send().await {
        Ok(r) => r,
        Err(_) => return Redirect::to(&format!("{detail}?error=upload_failed")).into_response(),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::CREATED {
        return Redirect::to(&format!("{detail}?notice=attachment_added")).into_response();
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return event_not_found_page().into_response();
    }
    // apps/api has its own upload gate and its own body bounds (#219), and
    // its gate is also filled by calls reaching `/api/*` directly: its 503
    // or 408 is passed on as such, not folded into a failed upload.
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return manage_our_home_http_guard::service_unavailable(
            service_unavailable_page().into_response(),
        );
    }
    // A 408 is a body that came too slowly, not a service that is down:
    // the same page and headers as apps/web's own 408 (#243).
    if status == reqwest::StatusCode::REQUEST_TIMEOUT {
        return manage_our_home_http_guard::request_timeout(
            crate::body_bounds::body_read_timeout_page(),
        );
    }
    if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        // Distinguish the two 422 codes the backend emits.
        let body = resp.json::<serde_json::Value>().await.unwrap_or_default();
        let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
        let mapped = match code {
            "file_too_large" => "file_too_large",
            _ => "unsupported_file_type",
        };
        return Redirect::to(&format!("{detail}?error={mapped}")).into_response();
    }
    Redirect::to(&format!("{detail}?error=upload_failed")).into_response()
}

/// How much room to make for the file before reading it (#246).
///
/// The declared body, which the file is part of, and never more than the
/// route's body limit: a client declaring a gigabyte and sending ten bytes
/// gets a buffer of the limit, not of its claim. Without a usable
/// `Content-Length`, the limit itself — one buffer of it is what the
/// upload gate budgets for each upload anyway.
fn file_buffer_capacity(headers: &HeaderMap) -> usize {
    let limit = manage_our_home_http_guard::MAX_UPLOAD_BODY_BYTES;
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(limit, |declared| {
            usize::try_from(declared).map_or(limit, |declared| declared.min(limit))
        })
}

pub async fn download(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((event_id, attachment_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/agenda/{event_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!(
            "/groups/{}/events/{}/attachments/{}/download",
            fam.gid, event_id, attachment_id
        ),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match resp.body.get("url").and_then(|v| v.as_str()) {
                Some(url) => Redirect::to(url).into_response(),
                None => event_not_found_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            event_not_found_page().into_response()
        }
        _ => Redirect::to(&format!("/agenda/{event_id}?error=unavailable")).into_response(),
    }
}

pub async fn delete(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((event_id, attachment_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/agenda/{event_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let detail = format!("/agenda/{event_id}");
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!(
            "/groups/{}/events/{}/attachments/{}",
            fam.gid, event_id, attachment_id
        ),
        cookie.as_deref(),
        None,
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            format!("{detail}?notice=attachment_deleted")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return event_not_found_page().into_response()
        }
        Ok(_) | Err(_) => format!("{detail}?error=unavailable"),
    };
    Redirect::to(&target).into_response()
}

/// The upload page under the body bounds and the upload gate (#219),
/// driven through the real router against a stand-in for apps/api.
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use axum::body::{Body, Bytes};
    use axum::extract::DefaultBodyLimit;
    use axum::http::{header, HeaderMap, Method, Request, Response, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use http_body_util::channel::{Channel, Sender};
    use manage_our_home_http_guard::{BodyReadLimits, UploadGate};
    use manage_our_home_shared::validation::agenda::MAX_ATTACHMENT_SIZE_BYTES;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::state::AppState;

    fn with_content_length(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, value.parse().unwrap());
        headers
    }

    #[test]
    fn the_file_buffer_is_sized_from_the_declared_body() {
        assert_eq!(
            super::file_buffer_capacity(&with_content_length("4096")),
            4096
        );
    }

    #[test]
    fn a_declared_body_past_the_limit_gets_no_more_than_the_limit() {
        let limit = manage_our_home_http_guard::MAX_UPLOAD_BODY_BYTES;
        for declared in [limit + 1, 10 * limit] {
            assert_eq!(
                super::file_buffer_capacity(&with_content_length(&declared.to_string())),
                limit
            );
        }
        assert_eq!(
            super::file_buffer_capacity(&with_content_length("18446744073709551616")),
            limit
        );
    }

    /// No length, or one that is not a number: the body limit is the only
    /// bound known, and one buffer of it is what the gate counts anyway.
    #[test]
    fn an_undeclared_or_unreadable_length_gets_the_limit() {
        let limit = manage_our_home_http_guard::MAX_UPLOAD_BODY_BYTES;
        assert_eq!(super::file_buffer_capacity(&HeaderMap::new()), limit);
        assert_eq!(
            super::file_buffer_capacity(&with_content_length("beaucoup")),
            limit
        );
    }

    const BOUNDARY: &str = "----manageourhomewebboundary";
    const EVENT: &str = "00000000-0000-0000-0000-0000000000e1";

    const SHORT: BodyReadLimits = BodyReadLimits {
        idle: Duration::from_millis(400),
        min_bytes_per_sec: 1_000,
        grace: Duration::from_millis(300),
        total: Duration::from_secs(3),
    };

    async fn me(headers: HeaderMap) -> Response<Body> {
        let user = headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|c| c.strip_prefix("session="))
            .and_then(|id| id.parse::<Uuid>().ok());
        match user {
            Some(id) => Json(serde_json::json!({
                "user_id": id,
                "email": "membre@example.test",
                "display_name": "Membre",
                "email_verified": true,
            }))
            .into_response(),
            None => StatusCode::UNAUTHORIZED.into_response(),
        }
    }

    /// Stands in for apps/api: `/auth/me` knows whoever the `session`
    /// cookie names, every user belongs to one group, and the attachments
    /// endpoint answers `upload_status` once it has read the body, whose
    /// length it leaves in `received`. It reads up to apps/api's own body
    /// limit, so what it receives is what apps/web relayed.
    async fn fake_api(upload_status: Arc<AtomicU16>, received: Arc<AtomicUsize>) -> String {
        let app = Router::new()
            .route("/auth/me", get(me))
            .route(
                "/groups",
                get(|| async {
                    Json(serde_json::json!([{
                        "group_id": Uuid::nil(),
                        "name": "Foyer",
                        "role": "owner",
                    }]))
                }),
            )
            .route(
                "/groups/:gid/events/:eid/attachments",
                post(move |body: Bytes| async move {
                    received.store(body.len(), Ordering::SeqCst);
                    let status =
                        StatusCode::from_u16(upload_status.load(Ordering::SeqCst)).unwrap();
                    let mut resp = Response::new(Body::from("{}"));
                    *resp.status_mut() = status;
                    if status == StatusCode::SERVICE_UNAVAILABLE {
                        resp.headers_mut()
                            .insert(header::RETRY_AFTER, "30".parse().unwrap());
                    }
                    resp
                })
                .layer(DefaultBodyLimit::max(
                    manage_our_home_http_guard::MAX_UPLOAD_BODY_BYTES,
                )),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    struct Web {
        router: Router,
        gate: Arc<UploadGate<Uuid>>,
        api_upload_status: Arc<AtomicU16>,
        api_received: Arc<AtomicUsize>,
    }

    async fn web(limits: BodyReadLimits, gate: Arc<UploadGate<Uuid>>) -> Web {
        let api_upload_status = Arc::new(AtomicU16::new(201));
        let api_received = Arc::new(AtomicUsize::new(0));
        let api = fake_api(api_upload_status.clone(), api_received.clone()).await;
        let router = crate::build_router(AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: api,
            api_public_base_url: "/api".into(),
            body_read_limits: limits,
            upload_gate: gate.clone(),
        });
        Web {
            router,
            gate,
            api_upload_status,
            api_received,
        }
    }

    /// A session cookie naming a user no other call has named.
    fn session() -> String {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = Uuid::from_u128(u128::from(NEXT.fetch_add(1, Ordering::SeqCst)));
        format!("session={id}")
    }

    fn multipart_head() -> Bytes {
        Bytes::from(format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"scan.png\"\r\n\r\n"
        ))
    }

    fn whole_upload() -> Body {
        let mut body = multipart_head().to_vec();
        body.extend_from_slice(b"\x89PNG\r\n\x1a\n not really an image");
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        Body::from(body)
    }

    /// A PNG signature padded to `size` bytes, as the one file of a
    /// multipart body.
    fn upload_of_size(size: usize) -> Body {
        let mut file = b"\x89PNG\r\n\x1a\n".to_vec();
        file.resize(size, 0);
        let mut body = multipart_head().to_vec();
        body.extend_from_slice(&file);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        Body::from(body)
    }

    fn upload(cookie: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(format!("/agenda/{EVENT}/attachments"))
            .header(header::COOKIE, cookie)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(body)
            .unwrap()
    }

    /// Bounded, so a body the router fails to cut fails the test instead
    /// of hanging it.
    async fn send(web: &Web, request: Request<Body>) -> Response<Body> {
        tokio::time::timeout(Duration::from_secs(10), web.router.clone().oneshot(request))
            .await
            .expect("no answer within 10 s")
            .unwrap()
    }

    /// A body fed by hand, with its first chunk already sent.
    async fn fed(first: Bytes) -> (Sender<Bytes>, Body) {
        let (mut tx, rx) = Channel::<Bytes>::new(16);
        tx.send_data(first).await.unwrap();
        (tx, Body::new(rx))
    }

    fn drip(mut tx: Sender<Bytes>, chunk: &'static [u8], every: Duration) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                if tx.send_data(Bytes::from_static(chunk)).await.is_err() {
                    return;
                }
            }
        });
    }

    async fn wait_for_in_flight(gate: &UploadGate<Uuid>, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while gate.in_flight() != n {
            assert!(
                Instant::now() < deadline,
                "expected {n} uploads in flight, still {}",
                gate.in_flight()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// An upload whose body never finishes, once it holds its permit.
    async fn hold_upload(
        web: &Web,
        cookie: &str,
    ) -> (Sender<Bytes>, tokio::task::JoinHandle<Response<Body>>) {
        let before = web.gate.in_flight();
        let (tx, body) = fed(multipart_head()).await;
        let request = upload(cookie, body);
        let router = web.router.clone();
        let task = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
        wait_for_in_flight(&web.gate, before + 1).await;
        (tx, task)
    }

    fn assert_request_timeout(resp: &Response<Body>) {
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(resp.headers()[header::CONNECTION], "close");
    }

    #[tokio::test]
    async fn an_upload_dripped_under_the_minimum_rate_is_answered_408() {
        let web = web(SHORT, UploadGate::new(8, 2)).await;
        let (tx, body) = fed(multipart_head()).await;
        // 10 bytes every 50 ms: 200 B/s, never 400 ms of silence.
        drip(tx, b"0123456789", Duration::from_millis(50));

        let started = Instant::now();
        let resp = send(&web, upload(&session(), body)).await;
        assert_request_timeout(&resp);
        assert!(started.elapsed() < SHORT.total);
        assert_eq!(web.gate.in_flight(), 0, "the permit must be back");
    }

    /// Every route, not only the upload: a login form dripped is cut too,
    /// where `Form`'s own rejection would have said 400.
    #[tokio::test]
    async fn a_form_dripped_is_answered_408() {
        let web = web(SHORT, UploadGate::new(8, 2)).await;
        let (tx, body) = fed(Bytes::from_static(b"email=a%40example.test&password=")).await;
        drip(tx, b"x", Duration::from_millis(50));

        let request = Request::builder()
            .method(Method::POST)
            .uri("/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .unwrap();
        assert_request_timeout(&send(&web, request).await);
    }

    #[tokio::test]
    async fn an_upload_at_a_normal_pace_is_relayed() {
        let web = web(SHORT, UploadGate::new(8, 2)).await;
        let resp = send(&web, upload(&session(), whole_upload())).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            resp.headers()[header::LOCATION],
            format!("/agenda/{EVENT}?notice=attachment_added")
        );
        assert_eq!(web.gate.in_flight(), 0);
    }

    #[tokio::test]
    async fn a_third_upload_from_one_account_is_turned_away_and_others_are_not() {
        let web = web(BodyReadLimits::PRODUCTION, UploadGate::new(8, 2)).await;
        let a = session();
        let _first = hold_upload(&web, &a).await;
        let _second = hold_upload(&web, &a).await;

        let third = send(&web, upload(&a, whole_upload())).await;
        assert_eq!(third.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(third.headers()[header::RETRY_AFTER], "30");

        let other = send(&web, upload(&session(), whole_upload())).await;
        assert_eq!(other.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn the_upload_past_the_process_pool_is_turned_away_until_a_client_disconnects() {
        let web = web(BodyReadLimits::PRODUCTION, UploadGate::new(2, 2)).await;
        let (_a_tx, a_task) = hold_upload(&web, &session()).await;
        let _b = hold_upload(&web, &session()).await;

        let refused = send(&web, upload(&session(), whole_upload())).await;
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refused.headers()[header::RETRY_AFTER], "30");

        // The browser behind the first upload goes away: hyper drops the
        // request's future, and the permit with it.
        a_task.abort();
        let _ = a_task.await;
        wait_for_in_flight(&web.gate, 1).await;

        let admitted = send(&web, upload(&session(), whole_upload())).await;
        assert_eq!(admitted.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn a_503_from_the_api_is_relayed_as_a_503() {
        let web = web(SHORT, UploadGate::new(8, 2)).await;
        web.api_upload_status.store(503, Ordering::SeqCst);
        let resp = send(&web, upload(&session(), whole_upload())).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "30");
    }

    #[tokio::test]
    async fn a_408_from_the_api_is_relayed_as_a_408() {
        let web = web(SHORT, UploadGate::new(8, 2)).await;
        web.api_upload_status.store(408, Ordering::SeqCst);
        let resp = send(&web, upload(&session(), whole_upload())).await;
        assert_request_timeout(&resp);
        // The page apps/web answers a body cut for its pace (#243), not the
        // one for a service that is down.
        let page = body_text(resp).await;
        assert!(page.contains("Envoi interrompu"), "{page}");
        assert!(!page.contains("indisponible"), "{page}");
    }

    async fn body_text(resp: Response<Body>) -> String {
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn assert_redirected_to(resp: &Response<Body>, query: &str) {
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            resp.headers()[header::LOCATION],
            format!("/agenda/{EVENT}?{query}")
        );
    }

    /// axum's `DefaultBodyLimit` is 2 MiB unless a route says otherwise:
    /// the page had no limit of its own, so a 3 MiB file died in the
    /// multipart read as `upload_failed` while apps/api takes 20 MiB (#243).
    #[tokio::test]
    async fn a_file_over_two_mebibytes_is_relayed_whole() {
        let web = web(BodyReadLimits::PRODUCTION, UploadGate::new(8, 2)).await;
        let size = 3 * 1024 * 1024;
        let resp = send(&web, upload(&session(), upload_of_size(size))).await;
        assert_redirected_to(&resp, "notice=attachment_added");
        assert!(web.api_received.load(Ordering::SeqCst) > size);
    }

    /// A file of exactly the cap fits, multipart framing included.
    #[tokio::test]
    async fn a_file_of_exactly_the_cap_is_relayed() {
        let web = web(BodyReadLimits::PRODUCTION, UploadGate::new(8, 2)).await;
        let size = usize::try_from(MAX_ATTACHMENT_SIZE_BYTES).unwrap();
        let resp = send(&web, upload(&session(), upload_of_size(size))).await;
        assert_redirected_to(&resp, "notice=attachment_added");
        assert!(web.api_received.load(Ordering::SeqCst) > size);
    }

    /// One byte over the cap is answered by the page's own size check,
    /// which knows it is a size problem, and never reaches apps/api.
    #[tokio::test]
    async fn a_file_one_byte_over_the_cap_is_too_large_not_a_failed_upload() {
        let web = web(BodyReadLimits::PRODUCTION, UploadGate::new(8, 2)).await;
        let size = usize::try_from(MAX_ATTACHMENT_SIZE_BYTES).unwrap() + 1;
        let resp = send(&web, upload(&session(), upload_of_size(size))).await;
        assert_redirected_to(&resp, "error=file_too_large");
        assert_eq!(web.api_received.load(Ordering::SeqCst), 0);
    }

    /// Heap bytes live on the threads of one runtime, and their high-water
    /// mark. Only threads that ask to be counted are: the test binary runs
    /// tests side by side, and only the runtime standing in for apps/web's
    /// process enrols its threads. Counted across them, not per thread,
    /// because production runs a multi-threaded runtime (`#[tokio::main]`
    /// in `src/main.rs`), whose workers pass tasks and buffers between
    /// them (#250).
    mod heap {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;
        use std::sync::atomic::{AtomicIsize, Ordering};

        struct Counting;

        static LIVE: AtomicIsize = AtomicIsize::new(0);
        static PEAK: AtomicIsize = AtomicIsize::new(0);

        thread_local! {
            static COUNTED: Cell<bool> = const { Cell::new(false) };
        }

        fn add(delta: isize) {
            if COUNTED.try_with(Cell::get).unwrap_or(false) {
                let now = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
                PEAK.fetch_max(now, Ordering::Relaxed);
            }
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

        /// From now on, what this thread allocates and frees is counted.
        pub fn count_this_thread() {
            COUNTED.with(|counted| counted.set(true));
        }

        /// Starts a new high-water mark from what is live now, and
        /// returns it.
        pub fn start() -> isize {
            let live = LIVE.load(Ordering::Relaxed);
            PEAK.store(live, Ordering::Relaxed);
            live
        }

        pub fn peak() -> isize {
            PEAK.load(Ordering::Relaxed)
        }
    }

    /// A stand-in apps/api that reads no body until `holders` uploads have
    /// reached it: until then, every one of them is held whole by apps/web.
    async fn holding_api(holders: usize) -> String {
        let barrier = Arc::new(tokio::sync::Barrier::new(holders));
        let app = Router::new()
            .route("/auth/me", get(me))
            .route(
                "/groups",
                get(|| async {
                    Json(serde_json::json!([{
                        "group_id": Uuid::nil(),
                        "name": "Foyer",
                        "role": "owner",
                    }]))
                }),
            )
            .route(
                "/groups/:gid/events/:eid/attachments",
                post(move |body: Body| async move {
                    barrier.wait().await;
                    // Read and drop, chunk by chunk: what this side keeps
                    // is not what is being measured.
                    let mut body = body;
                    while let Some(frame) = http_body_util::BodyExt::frame(&mut body).await {
                        frame.unwrap();
                    }
                    StatusCode::CREATED
                })
                .layer(DefaultBodyLimit::disable()),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// Posts `whole` to apps/web over a real connection, as a browser's
    /// would arrive: in 16 KiB writes, each a view of `whole` rather than a
    /// copy of it. Returns the status code and the `Location` header.
    async fn post_as_a_browser(addr: std::net::SocketAddr, whole: Bytes) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "POST /agenda/{EVENT}/attachments HTTP/1.1\r\nHost: web\r\nCookie: {}\r\n\
             Content-Type: multipart/form-data; boundary={BOUNDARY}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            session(),
            whole.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        for chunk in whole.chunks(16 * 1024) {
            socket.write_all(chunk).await.unwrap();
        }
        let mut answer = Vec::new();
        socket.read_to_end(&mut answer).await.unwrap();
        let answer = String::from_utf8_lossy(&answer).to_string();
        let status = answer
            .strip_prefix("HTTP/1.1 ")
            .and_then(|rest| rest.get(..3))
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {answer:?}"));
        let location = answer
            .lines()
            .find_map(|line| line.strip_prefix("location: "))
            .unwrap_or_default()
            .to_string();
        (status, location)
    }

    /// What the gate's arithmetic assumes (#246): a full pool of uploads,
    /// each of a file at the cap, costs apps/web one copy of each body —
    /// not a copy while reading plus a second one to relay it, nor the
    /// slack of a buffer grown by doubling.
    ///
    /// Measured as production runs (#250): apps/web on a multi-threaded
    /// runtime of its own, of the size `#[tokio::main]` gives it, serving
    /// real connections, so the buffers it reads each socket through count
    /// too. The browsers and the stand-in apps/api run on a second runtime
    /// whose threads are not counted: in production they are other
    /// processes.
    #[test]
    fn a_full_pool_of_uploads_at_the_cap_holds_one_copy_of_each() {
        let pool = manage_our_home_http_guard::gate::GLOBAL_UPLOADS;
        let outside = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let web = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .on_thread_start(heap::count_this_thread)
            .build()
            .unwrap();

        let api = outside.block_on(holding_api(pool));
        // Built on one of the counted threads, like everything apps/web
        // allocates: what a counted thread frees must have been counted
        // when it was allocated.
        let addr = web
            .block_on(web.spawn(async move {
                let router = crate::build_router(AppState {
                    http: reqwest::Client::new(),
                    api_internal_base_url: api,
                    api_public_base_url: "/api".into(),
                    body_read_limits: BodyReadLimits::PRODUCTION,
                    upload_gate: UploadGate::new(pool, 2),
                });
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tokio::spawn(async move {
                    axum::serve(
                        listener,
                        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                    )
                    .await
                    .unwrap()
                });
                addr
            }))
            .unwrap();

        let size = usize::try_from(MAX_ATTACHMENT_SIZE_BYTES).unwrap();
        let mut whole = multipart_head().to_vec();
        whole.extend(b"\x89PNG\r\n\x1a\n");
        whole.resize(whole.len() + size - 8, 0);
        whole.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        let whole = Bytes::from(whole);

        let baseline = heap::start();
        let answered = outside.block_on(async {
            let mut uploads = tokio::task::JoinSet::new();
            for _ in 0..pool {
                uploads.spawn(post_as_a_browser(addr, whole.clone()));
            }
            tokio::time::timeout(Duration::from_secs(60), async {
                let mut answered = Vec::new();
                while let Some(answer) = uploads.join_next().await {
                    answered.push(answer.unwrap());
                }
                answered
            })
            .await
            .expect("the uploads did not all complete within 60 s")
        });
        let held = usize::try_from(heap::peak() - baseline).unwrap();

        for (status, location) in &answered {
            assert_eq!(*status, 303);
            assert_eq!(
                *location,
                format!("/agenda/{EVENT}?notice=attachment_added")
            );
        }
        let mib = |n: usize| n as f64 / (1024.0 * 1024.0);
        let body = manage_our_home_http_guard::MAX_UPLOAD_BODY_BYTES;
        println!(
            "{pool} uploads of {size} bytes on {} workers: peak {:.1} MiB over the baseline, \
             {:.2} MiB per upload over its body",
            web.metrics().num_workers(),
            mib(held),
            mib(held.saturating_sub(pool * whole.len())) / pool as f64
        );
        // Every body is held at once, so a count below that missed the
        // threads apps/web ran on: a budget would pass on nothing.
        assert!(
            held >= pool * whole.len(),
            "{:.1} MiB held: the bodies themselves were not counted",
            mib(held)
        );
        // One body each, plus 1.5 MiB each for the buffers it passes
        // through in apps/web: the connection's read buffer, which hyper
        // grows to about 400 KiB, and multer's parse buffer, which grows as
        // much while the socket is read faster than it parses. When #250
        // was fixed, a full pool peaked at 168.4 to 169.2 MiB against this
        // 172.5, about 1.1 MiB per upload, whatever the worker count. A
        // doubled buffer costs 12 MiB more per upload, a second copy 20.
        let budget = pool * (body + 1024 * 1024 + 512 * 1024);
        assert!(
            held <= budget,
            "{:.1} MiB held, budget {:.1} MiB",
            mib(held),
            mib(budget)
        );
    }
}
