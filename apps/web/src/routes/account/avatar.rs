//! The profile photo (#468): its section of `/account`, its upload and
//! removal, and the page that serves the caller's own photo.
//!
//! apps/api holds the photo and is the only judge of it (#467, #588): it
//! sniffs the type from the bytes (JPEG and PNG, arbitrage of 2026-10-24),
//! bounds the decode and re-encodes a 256 px JPEG square. This module
//! relays the browser's form to it and turns its answer into French copy.

use std::pin::Pin;
use std::sync::{Mutex, PoisonError};
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::auth::MeResponse;

use super::{account_cookie, service_unavailable_page};
use crate::app::member_avatar;
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

/// Where an upload or a removal sends the browser back, as `/account`'s
/// `?notice=` or `?error=`, or the page it answers with instead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Notice(&'static str),
    Error(&'static str),
    /// apps/api's upload gate is full: its 503, passed on as such.
    Busy,
    /// The body came too slowly: apps/api's 408, passed on as such.
    TooSlow,
}

/// What apps/api's answer to `PUT /me/avatar` — its status and the `error`
/// code of its body — means for the page. The four 422 codes are
/// `avatar::AvatarError::code` in apps/api; a 413 is a body past that
/// route's limit, a file too large as well.
pub(crate) fn upload_outcome(status: u16, code: Option<&str>) -> Outcome {
    match (status, code) {
        (204, _) => Outcome::Notice("avatar_updated"),
        (422, Some("file_too_large")) | (413, _) => Outcome::Error("file_too_large"),
        (422, Some("unsupported_file_type")) => Outcome::Error("unsupported_file_type"),
        (422, Some("unreadable_image")) => Outcome::Error("unreadable_image"),
        (422, Some("image_too_large")) => Outcome::Error("image_too_large"),
        (503, _) => Outcome::Busy,
        (408, _) => Outcome::TooSlow,
        _ => Outcome::Error("upload_failed"),
    }
}

/// What apps/api's answer to `DELETE /me/avatar` means for the page.
pub(crate) fn removal_outcome(status: u16) -> Outcome {
    match status {
        204 => Outcome::Notice("avatar_removed"),
        _ => Outcome::Error("unavailable"),
    }
}

/// The French copy of an `/account?error=` this module sends.
pub(crate) fn error_message(code: &str) -> Option<&'static str> {
    Some(match code {
        // apps/api's `avatar::MAX_AVATAR_BYTES`, 32 MiB.
        "file_too_large" => "La photo dépasse 32 Mo. Choisissez un fichier plus léger.",
        "unsupported_file_type" => {
            "Format non pris en charge : la photo doit être un fichier JPEG ou PNG."
        }
        "unreadable_image" => {
            "Cette image n'a pas pu être lue : le fichier est peut-être endommagé. Essayez avec une autre photo."
        }
        // #588: decodable, but past the bounds of a decode — a PNG or a
        // progressive JPEG with too many pixels, a JPEG wider than 16384 px.
        "image_too_large" => {
            "Cette image a trop de pixels pour être traitée. Réduisez ses dimensions, puis réessayez."
        }
        "upload_failed" => "La photo n'a pas pu être envoyée. Merci de réessayer.",
        _ => return None,
    })
}

/// The French copy of an `/account?notice=` this module sends.
pub(crate) fn notice_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "avatar_updated" => "Photo de profil enregistrée.",
        "avatar_removed" => "Photo de profil retirée : votre initiale la remplace.",
        _ => return None,
    })
}

/// The page that serves the caller's own photo.
pub(crate) const OWN_PHOTO_URL: &str = "/account/avatar";

/// The photo section of `/account`: the icon as the other members see it,
/// the form to upload or replace the photo, and the one to remove it.
pub(crate) fn section(me: &MeResponse, has_photo: bool) -> String {
    let icon = member_avatar(
        me.user_id,
        &me.display_name,
        has_photo.then_some(OWN_PHOTO_URL),
    );
    let (state, submit, remove) = if has_photo {
        (
            "Votre icône, telle que les membres de vos familles la voient.",
            "Remplacer la photo",
            r#"
<form method="post" action="/account/avatar/delete">
<button type="submit" class="secondary">Retirer la photo</button>
</form>"#,
        )
    } else {
        (
            "Votre icône est votre initiale. Une photo peut la remplacer.",
            "Ajouter une photo",
            "",
        )
    };
    format!(
        r#"<section class="card">
<h2>Photo de profil</h2>
<p>{icon} <span class="muted">{state}</span></p>
<form method="post" action="/account/avatar" enctype="multipart/form-data">
<label class="field">Photo (JPEG ou PNG, 32 Mo au plus ; recadrée en carré, au centre)
<input type="file" name="file" accept="image/jpeg,image/png" required></label>
<button type="submit">{submit}</button>
</form>{remove}
</section>"#
    )
}

/// `GET /account/avatar` — the caller's own photo, from `GET /me/avatar`.
pub async fn get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cookie = account_cookie(&headers);
    relay_photo(&state, "/me/avatar", cookie.as_deref()).await
}

/// A photo from apps/api, served from this origin: the CSP's
/// `default-src 'self'` takes no image from elsewhere, and apps/api is
/// not reachable by the browser under its internal name. Any refusal —
/// no session, no photo, no group in common (apps/api answers 404 to all
/// of them, #467) — is an empty 404, as is apps/api being down: the image
/// is decorative, and the page around it already rendered.
///
/// The type is not apps/api's: it re-encodes every photo to JPEG
/// (`avatar::AVATAR_CONTENT_TYPE`), and this origin serves nothing else
/// under these paths. `private, no-cache` as apps/api sends it: a shared
/// cache must not keep a photo some members only may see, and a replaced
/// photo shows at once.
pub(crate) async fn relay_photo(state: &AppState, path: &str, cookie: Option<&str>) -> Response {
    let mut req = state
        .http
        .get(format!("{}{}", state.api_internal_base_url, path));
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    let photo = match req.send().await {
        Ok(resp) if resp.status() == reqwest::StatusCode::OK => resp.bytes().await.ok(),
        _ => None,
    };
    match photo {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "private, no-cache"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `POST /account/avatar` — the browser's form, relayed to
/// `PUT /me/avatar` as it arrives.
///
/// The body is streamed, never held: a photo may weigh up to 32 MiB, more
/// than an attachment, and holding it would add to what the upload gate
/// budgets for this process (`http_guard::gate::GLOBAL_UPLOADS`). So this
/// route takes no permit of that gate and needs no body limit of its own:
/// apps/api holds the photo, under its own gate, its own limit and its one
/// decode at a time (#588), and refuses past them. The body guard of every
/// route (#219) still bounds how slowly it may come.
pub async fn upload(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // apps/api reads the multipart body with the boundary the browser
    // chose: its `Content-Type` goes along, and only a form's is sent on.
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .filter(|v| v.as_bytes().starts_with(b"multipart/form-data"))
        .cloned()
    else {
        return redirect(Outcome::Error("upload_failed"));
    };
    let mut req = state
        .http
        .put(format!("{}/me/avatar", state.api_internal_base_url))
        .header(header::CONTENT_TYPE, content_type)
        .body(reqwest::Body::wrap(Relayed(Mutex::new(body))));
    // The declared length, which apps/api sizes its buffer from; hyper
    // already holds the browser's body to it.
    if let Some(length) = headers.get(header::CONTENT_LENGTH) {
        req = req.header(header::CONTENT_LENGTH, length.clone());
    }
    if let Some(cookie) = account_cookie(&headers) {
        req = req.header(header::COOKIE, cookie);
    }
    let outcome = match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body = resp.json::<serde_json::Value>().await.unwrap_or_default();
            upload_outcome(status, body.get("error").and_then(|v| v.as_str()))
        }
        Err(error) => {
            tracing::warn!(%error, "avatar upload not relayed");
            Outcome::Error("upload_failed")
        }
    };
    redirect(outcome)
}

/// `POST /account/avatar/delete` — removes the caller's photo.
pub async fn delete(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = account_cookie(&headers);
    let outcome = match api_request_auth(
        &state,
        reqwest::Method::DELETE,
        "/me/avatar",
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) => removal_outcome(resp.status.as_u16()),
        Err(_) => Outcome::Error("unavailable"),
    };
    redirect(outcome)
}

/// Whether the caller has a photo, for `/account`: `GET /auth/me` does not
/// say (#468 left the session's query as it was), so `/me/avatar` is
/// asked, its answer's status only. `false` when apps/api cannot say: the
/// initial is shown, and the form still offers to add a photo.
pub(crate) async fn has_own_photo(state: &AppState, cookie: Option<&str>) -> bool {
    let mut req = state
        .http
        .head(format!("{}/me/avatar", state.api_internal_base_url));
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, cookie);
    }
    matches!(req.send().await, Ok(resp) if resp.status() == reqwest::StatusCode::OK)
}

fn redirect(outcome: Outcome) -> Response {
    match outcome {
        Outcome::Notice(code) => Redirect::to(&format!("/account?notice={code}")).into_response(),
        Outcome::Error(code) => Redirect::to(&format!("/account?error={code}")).into_response(),
        Outcome::Busy => manage_our_home_http_guard::service_unavailable(
            service_unavailable_page().into_response(),
        ),
        Outcome::TooSlow => {
            manage_our_home_http_guard::request_timeout(crate::body_bounds::body_read_timeout_page())
        }
    }
}

/// The browser's body as reqwest takes one: `reqwest::Body::wrap` asks
/// `Sync` of it, which axum's body is not. The mutex is never locked to
/// read it: `poll_frame` has the body to itself (`Mutex::get_mut`).
struct Relayed(Mutex<Body>);

impl http_body::Body for Relayed {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>> {
        let body = self
            .get_mut()
            .0
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        Pin::new(body).poll_frame(cx)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.0
            .lock()
            .map(|body| body.size_hint())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::member_avatar;
    use uuid::Uuid;

    fn me() -> MeResponse {
        MeResponse {
            user_id: Uuid::from_u128(5),
            email: "alice@example.test".into(),
            display_name: "Alice".into(),
            email_verified: true,
            is_superadmin: false,
            has_password: true,
            deletion_requested_at: None,
            terms_accepted_version: None,
        }
    }

    #[test]
    fn a_stored_photo_is_announced() {
        assert_eq!(upload_outcome(204, None), Outcome::Notice("avatar_updated"));
    }

    /// Each refusal apps/api names (`avatar::AvatarError::code`) keeps its
    /// own code, so the page can say which one it was.
    #[test]
    fn each_refusal_of_the_api_keeps_its_code() {
        for code in [
            "file_too_large",
            "unsupported_file_type",
            "unreadable_image",
            "image_too_large",
        ] {
            assert_eq!(upload_outcome(422, Some(code)), Outcome::Error(code));
        }
    }

    /// A body past apps/api's route limit is cut there with a 413: that is
    /// a file too large too.
    #[test]
    fn a_body_past_the_api_limit_is_a_file_too_large() {
        assert_eq!(upload_outcome(413, None), Outcome::Error("file_too_large"));
    }

    #[test]
    fn a_full_gate_and_a_slow_body_are_passed_on() {
        assert_eq!(upload_outcome(503, None), Outcome::Busy);
        assert_eq!(upload_outcome(408, None), Outcome::TooSlow);
    }

    /// Anything else — an unknown 422 code, a malformed form (400), a
    /// session lost on the way (401), a failure (500) — is an upload that
    /// failed, never a code the page would not know.
    #[test]
    fn any_other_answer_is_a_failed_upload() {
        assert_eq!(
            upload_outcome(422, Some("something_new")),
            Outcome::Error("upload_failed")
        );
        assert_eq!(upload_outcome(422, None), Outcome::Error("upload_failed"));
        for status in [200, 400, 401, 404, 500, 502] {
            assert_eq!(
                upload_outcome(status, Some("invalid_multipart")),
                Outcome::Error("upload_failed"),
                "{status}"
            );
        }
    }

    #[test]
    fn a_removal_is_announced_and_a_failure_is_unavailable() {
        assert_eq!(removal_outcome(204), Outcome::Notice("avatar_removed"));
        for status in [401, 500, 503] {
            assert_eq!(removal_outcome(status), Outcome::Error("unavailable"));
        }
    }

    /// Every code an outcome can carry has its French copy, and each
    /// refusal its own: the message must say which one it was (#588 left a
    /// 48 Mpx photo answered "unreadable" for want of its own code).
    #[test]
    fn every_code_has_its_own_message() {
        let errors = [
            "file_too_large",
            "unsupported_file_type",
            "unreadable_image",
            "image_too_large",
            "upload_failed",
        ];
        let messages: Vec<&str> = errors
            .iter()
            .map(|c| error_message(c).unwrap_or_else(|| panic!("no message for {c}")))
            .collect();
        for (i, a) in messages.iter().enumerate() {
            for b in &messages[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert!(notice_message("avatar_updated").is_some());
        assert!(notice_message("avatar_removed").is_some());
        assert_eq!(error_message("deletion_requested"), None);
        assert_eq!(notice_message("upload_failed"), None);
    }

    /// The messages say what to do: the formats accepted, the size cap of
    /// apps/api (`avatar::MAX_AVATAR_BYTES`, 32 MiB, written "Mo" as the
    /// app's other caps are).
    #[test]
    fn the_refusals_name_the_formats_and_the_cap() {
        let unsupported = error_message("unsupported_file_type").unwrap();
        assert!(unsupported.contains("JPEG"), "{unsupported}");
        assert!(unsupported.contains("PNG"), "{unsupported}");
        assert!(!unsupported.contains("WebP"), "{unsupported}");
        assert!(
            error_message("file_too_large").unwrap().contains("32 Mo"),
            "{:?}",
            error_message("file_too_large")
        );
    }

    #[test]
    fn the_section_shows_the_current_icon_and_an_upload_form() {
        let html = section(&me(), false);
        assert!(
            html.contains(&member_avatar(Uuid::from_u128(5), "Alice", None)),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<form method="post" action="/account/avatar" enctype="multipart/form-data">"#
            ),
            "{html}"
        );
        // JPEG and PNG only, as apps/api takes them (arbitrage of
        // 2026-10-24): a WebP is not even offered by the picker.
        assert!(
            html.contains(
                r#"<input type="file" name="file" accept="image/jpeg,image/png" required>"#
            ),
            "{html}"
        );
        assert!(html.contains(r#"<label class="field">"#), "{html}");
        assert!(html.contains("Ajouter une photo"), "{html}");
        // Nothing to remove yet.
        assert!(!html.contains("/account/avatar/delete"), "{html}");
    }

    #[test]
    fn with_a_photo_the_section_shows_it_and_offers_to_replace_or_remove_it() {
        let html = section(&me(), true);
        assert!(
            html.contains(&member_avatar(
                Uuid::from_u128(5),
                "Alice",
                Some(OWN_PHOTO_URL)
            )),
            "{html}"
        );
        assert!(html.contains("Remplacer la photo"), "{html}");
        assert!(
            html.contains(r#"<form method="post" action="/account/avatar/delete">"#),
            "{html}"
        );
        assert!(html.contains("Retirer la photo"), "{html}");
    }
}

/// The upload and the photo pages through the real router, against a
/// stand-in for apps/api.
#[cfg(test)]
mod relay_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use axum::body::{Body, Bytes};
    use axum::http::{header, HeaderMap, Method, Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, put};
    use axum::{Json, Router};
    use http_body_util::channel::Channel;
    use http_body_util::BodyExt;
    use manage_our_home_http_guard::{BodyReadLimits, UploadGate};
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::state::AppState;

    const BOUNDARY: &str = "----manageourhomeavatarboundary";
    const SESSION: &str = "session=00000000-0000-0000-0000-000000000005";

    /// What the stand-in for apps/api saw of the relayed upload.
    #[derive(Default)]
    struct Seen {
        uploads: AtomicUsize,
        received: AtomicUsize,
        content_type: Mutex<Option<String>>,
        content_length: Mutex<Option<String>>,
        cookie: Mutex<Option<String>>,
    }

    /// apps/api: `/auth/me` knows the session; `PUT /me/avatar` reads the
    /// whole body, counting it as it comes, then answers `status` and
    /// `body`; `GET /me/avatar` and a member's photo answer `photo`.
    async fn fake_api(
        status: u16,
        body: &'static str,
        photo: Option<&'static [u8]>,
        seen: Arc<Seen>,
    ) -> String {
        let me = || async {
            Json(serde_json::json!({
                "user_id": Uuid::from_u128(5),
                "email": "alice@example.test",
                "display_name": "Alice",
                "email_verified": true,
            }))
        };
        let serve = move || async move {
            match photo {
                Some(bytes) => ([(header::CONTENT_TYPE, "text/html")], bytes).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        };
        let app = Router::new()
            .route("/auth/me", get(me))
            .route(
                "/me/avatar",
                put(move |headers: HeaderMap, request_body: Body| async move {
                    seen.uploads.fetch_add(1, Ordering::SeqCst);
                    *seen.content_type.lock().unwrap() = headers
                        .get(header::CONTENT_TYPE)
                        .map(|v| v.to_str().unwrap().to_string());
                    *seen.content_length.lock().unwrap() = headers
                        .get(header::CONTENT_LENGTH)
                        .map(|v| v.to_str().unwrap().to_string());
                    *seen.cookie.lock().unwrap() = headers
                        .get(header::COOKIE)
                        .map(|v| v.to_str().unwrap().to_string());
                    let mut request_body = request_body;
                    while let Some(frame) = request_body.frame().await {
                        if let Ok(data) = frame.unwrap().into_data() {
                            seen.received.fetch_add(data.len(), Ordering::SeqCst);
                        }
                    }
                    (StatusCode::from_u16(status).unwrap(), body)
                })
                .get(serve),
            )
            .route("/groups/:gid/members/:uid/avatar", get(serve));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn web(
        status: u16,
        body: &'static str,
        photo: Option<&'static [u8]>,
    ) -> (Router, Arc<Seen>) {
        let seen = Arc::new(Seen::default());
        let api = fake_api(status, body, photo, seen.clone()).await;
        let router = crate::build_router(AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: api,
            api_public_base_url: "/api".into(),
            body_read_limits: BodyReadLimits::PRODUCTION,
            upload_gate: UploadGate::new(8, 2),
            shutdown: manage_our_home_http_guard::Shutdown::new(),
        });
        (router, seen)
    }

    fn multipart_head() -> Vec<u8> {
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"moi.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n"
        )
        .into_bytes()
    }

    fn multipart_tail() -> Vec<u8> {
        format!("\r\n--{BOUNDARY}--\r\n").into_bytes()
    }

    fn upload(body: Body, length: Option<usize>) -> Request<Body> {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/account/avatar")
            .header(header::COOKIE, SESSION)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            );
        if let Some(length) = length {
            request = request.header(header::CONTENT_LENGTH, length);
        }
        request.body(body).unwrap()
    }

    async fn send(router: &Router, request: Request<Body>) -> axum::response::Response {
        tokio::time::timeout(Duration::from_secs(20), router.clone().oneshot(request))
            .await
            .expect("no answer within 20 s")
            .unwrap()
    }

    fn location(resp: &axum::response::Response) -> &str {
        resp.headers()[header::LOCATION].to_str().unwrap()
    }

    /// The whole form reaches apps/api as the browser sent it: its
    /// boundary, its session, its declared length, every byte — 5 MiB
    /// here, past the 2 MiB axum allows a route that sets no limit of its
    /// own. The body comes in chunks, its size known by its header only,
    /// so the length apps/api receives is the one relayed, not one hyper
    /// could have read from a body of known size.
    #[tokio::test]
    async fn the_form_reaches_the_api_whole_and_a_stored_photo_is_announced() {
        let (router, seen) = web(204, "", None).await;
        let mut body = multipart_head();
        body.resize(body.len() + 5 * 1024 * 1024, 0xAB);
        body.extend(multipart_tail());
        let length = body.len();
        let (mut tx, rx) = Channel::<Bytes>::new(4);
        tokio::spawn(async move {
            for chunk in Bytes::from(body).chunks(64 * 1024) {
                tx.send_data(Bytes::copy_from_slice(chunk)).await.unwrap();
            }
        });

        let resp = send(&router, upload(Body::new(rx), Some(length))).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&resp), "/account?notice=avatar_updated");
        assert_eq!(seen.received.load(Ordering::SeqCst), length);
        // apps/api sizes its buffer from the declared length: it must
        // arrive, though the body itself goes on as a stream.
        assert_eq!(
            seen.content_length.lock().unwrap().as_deref(),
            Some(length.to_string().as_str())
        );
        assert_eq!(
            seen.content_type.lock().unwrap().as_deref(),
            Some(format!("multipart/form-data; boundary={BOUNDARY}").as_str())
        );
        assert_eq!(seen.cookie.lock().unwrap().as_deref(), Some(SESSION));
    }

    /// The body is handed on as it arrives, never held whole: apps/api has
    /// the first bytes while the browser has not sent the rest.
    #[tokio::test]
    async fn the_body_is_relayed_as_it_arrives() {
        let (router, seen) = web(204, "", None).await;
        let (mut tx, rx) = Channel::<Bytes>::new(4);
        let head = multipart_head();
        let head_len = head.len();
        tx.send_data(Bytes::from(head)).await.unwrap();
        let task = tokio::spawn({
            let router = router.clone();
            async move { send(&router, upload(Body::new(rx), None)).await }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while seen.received.load(Ordering::SeqCst) < head_len {
            assert!(
                Instant::now() < deadline,
                "apps/api had nothing while the body was still open"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tx.send_data(Bytes::from(multipart_tail())).await.unwrap();
        drop(tx);
        let resp = task.await.unwrap();
        assert_eq!(location(&resp), "/account?notice=avatar_updated");
    }

    /// apps/api's refusal reaches the page under its own code.
    #[tokio::test]
    async fn a_refusal_of_the_api_reaches_the_page_with_its_code() {
        let (router, _) = web(422, r#"{"error":"image_too_large"}"#, None).await;
        let mut body = multipart_head();
        body.extend(b"\xFF\xD8\xFF");
        body.extend(multipart_tail());
        let resp = send(&router, upload(Body::from(body), None)).await;
        assert_eq!(location(&resp), "/account?error=image_too_large");
    }

    /// A body that is no form is not sent on.
    #[tokio::test]
    async fn a_body_that_is_no_form_is_not_relayed() {
        let (router, seen) = web(204, "", None).await;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/account/avatar")
            .header(header::COOKIE, SESSION)
            .header(header::CONTENT_TYPE, "image/jpeg")
            .body(Body::from(&b"\xFF\xD8\xFF"[..]))
            .unwrap();
        let resp = send(&router, request).await;
        assert_eq!(location(&resp), "/account?error=upload_failed");
        assert_eq!(seen.uploads.load(Ordering::SeqCst), 0);
    }

    /// The photo is served from this origin as a JPEG, whatever type
    /// apps/api names (the stand-in says `text/html`), privately and
    /// revalidated; a missing one is an empty 404.
    #[tokio::test]
    async fn a_photo_is_served_as_a_private_jpeg_and_a_missing_one_is_404() {
        let (router, _) = web(204, "", Some(b"\xFF\xD8\xFFjpeg")).await;
        let (g, u) = (Uuid::from_u128(1), Uuid::from_u128(2));
        for uri in [
            "/account/avatar".to_string(),
            crate::app::member_photo_url(g, u),
        ] {
            let request = Request::builder()
                .uri(&uri)
                .header(header::COOKIE, SESSION)
                .body(Body::empty())
                .unwrap();
            let resp = send(&router, request).await;
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
            assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/jpeg");
            assert_eq!(resp.headers()[header::CACHE_CONTROL], "private, no-cache");
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&bytes[..], b"\xFF\xD8\xFFjpeg");
        }

        let (router, _) = web(204, "", None).await;
        let request = Request::builder()
            .uri(crate::app::member_photo_url(g, u))
            .header(header::COOKIE, SESSION)
            .body(Body::empty())
            .unwrap();
        let resp = send(&router, request).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.is_empty());
    }
}
