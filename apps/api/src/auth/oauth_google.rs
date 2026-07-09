use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use axum::{http::StatusCode, Json};
use manage_our_home_shared::dto::auth::LinkGoogleRequest;
use oauth2::{AuthorizationCode, CsrfToken, Scope, TokenResponse};
use serde::Deserialize;
use tower_cookies::{Cookie, Cookies};

use crate::error::{AppError, AppResult};
use crate::AppState;

use super::session::{build_session_cookie, create_session, AuthUser};

const OAUTH_STATE_COOKIE: &str = "google_oauth_state";

/// AC #3: redirects to Google's consent screen with a fresh CSRF `state`,
/// stashed in a short-lived HttpOnly cookie so `callback` can verify it.
pub async fn start(
    State(state): State<AppState>,
    cookies: Cookies,
) -> AppResult<impl IntoResponse> {
    let (auth_url, csrf_token) = state
        .google_oauth
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .add_extra_param("access_type", "offline")
        .add_extra_param("prompt", "consent")
        .url();

    let mut cookie = Cookie::new(OAUTH_STATE_COOKIE, csrf_token.secret().clone());
    cookie.set_http_only(true);
    cookie.set_secure(state.secure_cookies);
    cookie.set_same_site(cookie::SameSite::Lax);
    cookie.set_path("/");
    cookies.add(cookie);

    Ok(Redirect::to(auth_url.as_str()))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct GoogleUserInfo {
    sub: String,
    email: String,
    email_verified: bool,
    name: Option<String>,
}

async fn fetch_google_userinfo(access_token: &str) -> anyhow::Result<GoogleUserInfo> {
    let client = reqwest::Client::new();
    let info: GoogleUserInfo = client
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(access_token)
        .send()
        .await?
        .json()
        .await?;
    Ok(info)
}

/// AC #3, #8: validates the CSRF `state`, exchanges the code, fetches the
/// verified Google profile, and either creates a new account or links to
/// the caller's existing session. The refresh token (if any) is stored
/// encrypted via `pgcrypto` and is never written to `tracing` logs.
pub async fn callback(
    State(state): State<AppState>,
    cookies: Cookies,
    Query(query): Query<CallbackQuery>,
) -> AppResult<impl IntoResponse> {
    let expected_state = cookies
        .get(OAUTH_STATE_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or(AppError::Unauthorized)?;
    cookies.remove(Cookie::new(OAUTH_STATE_COOKIE, ""));

    if query.state != expected_state {
        return Err(AppError::Unauthorized);
    }

    let token = state
        .google_oauth
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(&reqwest::Client::new())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("token exchange failed: {e}")))?;

    let access_token = token.access_token().secret();
    let userinfo = fetch_google_userinfo(access_token)
        .await
        .map_err(AppError::Internal)?;

    if !userinfo.email_verified {
        return Err(AppError::Unauthorized);
    }

    let refresh_token_plain = token.refresh_token().map(|t| t.secret().clone());

    let mut tx = state.db.begin().await?;

    let existing_identity = sqlx::query!(
        "SELECT user_id FROM oauth_identities WHERE provider = 'google' AND provider_user_id = $1",
        userinfo.sub
    )
    .fetch_optional(&mut *tx)
    .await?;

    let user_id = if let Some(identity) = existing_identity {
        identity.user_id
    } else {
        let existing_user = sqlx::query!("SELECT id FROM users WHERE email = $1", userinfo.email)
            .fetch_optional(&mut *tx)
            .await?;

        let user_id = match existing_user {
            Some(u) => u.id,
            None => {
                let display_name = userinfo
                    .name
                    .clone()
                    .unwrap_or_else(|| userinfo.email.clone());
                sqlx::query_scalar!(
                    r#"
                    INSERT INTO users (email, email_verified, display_name)
                    VALUES ($1, true, $2)
                    RETURNING id
                    "#,
                    userinfo.email,
                    display_name,
                )
                .fetch_one(&mut *tx)
                .await?
            }
        };

        if let Some(refresh_token) = &refresh_token_plain {
            sqlx::query!(
                r#"
                INSERT INTO oauth_identities (user_id, provider, provider_user_id, refresh_token_encrypted)
                VALUES ($1, 'google', $2, pgp_sym_encrypt($3, $4))
                "#,
                user_id,
                userinfo.sub,
                refresh_token,
                state.oauth_encryption_key,
            )
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query!(
                r#"
                INSERT INTO oauth_identities (user_id, provider, provider_user_id)
                VALUES ($1, 'google', $2)
                "#,
                user_id,
                userinfo.sub,
            )
            .execute(&mut *tx)
            .await?;
        }
        user_id
    };

    tx.commit().await?;

    let session_id = create_session(&state.db, user_id).await?;
    cookies.add(build_session_cookie(session_id, state.secure_cookies));

    Ok(Redirect::to(&state.frontend_base_url))
}

/// AC #8: links Google to an already-authenticated account. Requires a
/// live session (`AuthUser`) and that Google's verified email matches the
/// account's email exactly.
pub async fn link(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<LinkGoogleRequest>,
) -> AppResult<impl IntoResponse> {
    let token = state
        .google_oauth
        .exchange_code(AuthorizationCode::new(body.code))
        .request_async(&reqwest::Client::new())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("token exchange failed: {e}")))?;

    let userinfo = fetch_google_userinfo(token.access_token().secret())
        .await
        .map_err(AppError::Internal)?;

    if !userinfo.email_verified {
        return Err(AppError::Unauthorized);
    }
    if userinfo.email != auth.email {
        return Err(AppError::Unprocessable("email_mismatch".into()));
    }

    let refresh_token_plain = token.refresh_token().map(|t| t.secret().clone());

    if let Some(refresh_token) = &refresh_token_plain {
        sqlx::query!(
            r#"
            INSERT INTO oauth_identities (user_id, provider, provider_user_id, refresh_token_encrypted)
            VALUES ($1, 'google', $2, pgp_sym_encrypt($3, $4))
            ON CONFLICT (provider, provider_user_id) DO NOTHING
            "#,
            auth.user_id,
            userinfo.sub,
            refresh_token,
            state.oauth_encryption_key,
        )
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query!(
            r#"
            INSERT INTO oauth_identities (user_id, provider, provider_user_id)
            VALUES ($1, 'google', $2)
            ON CONFLICT (provider, provider_user_id) DO NOTHING
            "#,
            auth.user_id,
            userinfo.sub,
        )
        .execute(&state.db)
        .await?;
    }

    Ok(StatusCode::OK)
}
