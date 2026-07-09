//! Root auth-gate: every non-auth route extracts `CurrentUser`, every
//! auth-entry route (`/login`, `/register`) extracts `RedirectIfAuthenticated`
//! first. Both call `GET /auth/me` server-side, forwarding the incoming
//! request's session cookie — this is the "layout" behavior issue #15
//! describes, implemented as axum extractors (one per request, run before
//! the handler body) rather than a Leptos component wrapper, since
//! `apps/web` doesn't use Leptos's router/hydration here (see
//! `Cargo.toml`'s doc comment).

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use manage_our_home_shared::dto::auth::MeResponse;

use crate::state::{fetch_me, AppState};

fn cookie_header(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Extracts the authenticated user or redirects to `/login`. Use on every
/// handler for a route that requires a session (AC #3: "an unauthenticated
/// visitor hitting any non-auth route is redirected to /login").
pub struct CurrentUser(pub MeResponse);

#[axum::async_trait]
impl<S> FromRequestParts<S> for CurrentUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let cookie = cookie_header(parts);
        match fetch_me(&app_state, cookie.as_deref()).await {
            Some(me) => Ok(CurrentUser(me)),
            None => Err(Redirect::to("/login").into_response()),
        }
    }
}

/// Same lookup as `CurrentUser` but never rejects — `None` means
/// unauthenticated. Used by pages that render differently depending on
/// auth state without hard-requiring a session.
pub struct CurrentUserOpt(pub Option<MeResponse>);

#[axum::async_trait]
#[axum::async_trait]
impl<S> FromRequestParts<S> for CurrentUserOpt
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let cookie = cookie_header(parts);
        Ok(CurrentUserOpt(fetch_me(&app_state, cookie.as_deref()).await))
    }
}

/// Extracted at the top of `/login` and `/register` handlers: redirects
/// an already-authenticated visitor to `/` (AC #3, second half), otherwise
/// lets the handler render the form as normal.
pub struct RedirectIfAuthenticated;

#[axum::async_trait]
impl<S> FromRequestParts<S> for RedirectIfAuthenticated
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let cookie = cookie_header(parts);
        if fetch_me(&app_state, cookie.as_deref()).await.is_some() {
            Err(Redirect::to("/").into_response())
        } else {
            Ok(RedirectIfAuthenticated)
        }
    }
}
