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

use super::{agenda_cookie, event_not_found_page, family_context};

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

    // Pull the single `file` field out of the multipart body.
    let mut filename: Option<String> = None;
    let mut bytes: Option<Vec<u8>> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if field.name() == Some("file") {
                    filename = field.file_name().map(|s| s.to_string());
                    match field.bytes().await {
                        Ok(b) => bytes = Some(b.to_vec()),
                        Err(_) => {
                            return Redirect::to(&format!("{detail}?error=upload_failed"))
                                .into_response()
                        }
                    }
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
    let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
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
