//! `/groups/:id/members/:user_id/avatar` — a member's profile photo (#468),
//! from `GET /groups/:id/members/:user_id/avatar`: apps/api serves it only
//! when the caller and the member both belong to `:id` (#467), and answers
//! 404 otherwise, as for a member with no photo. `app::member_photo_url`
//! builds the link, `routes::account::avatar::relay_photo` serves it.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use uuid::Uuid;

use super::cookie_of;
use crate::routes::account::avatar::relay_photo;
use crate::state::AppState;

pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((group_id, user_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let cookie = cookie_of(&headers);
    relay_photo(
        &state,
        &format!("/groups/{group_id}/members/{user_id}/avatar"),
        cookie.as_deref(),
    )
    .await
}
