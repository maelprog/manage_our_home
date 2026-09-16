use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use oauth2::url::Url;
use oauth2::{
    AuthorizationCode, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, Scope, TokenResponse,
};
use serde::Deserialize;
use tower_cookies::{Cookie, Cookies};

use crate::error::{AppError, AppResult};
use crate::{AppState, GoogleOauthClient};

use super::session::{build_session_cookie, create_session};

const OAUTH_STATE_COOKIE: &str = "google_oauth_state";
/// PKCE `code_verifier` (RFC 7636) minted by `start`, spent by `callback`.
/// The CSRF `state` only proves the callback request follows one of our
/// `start`s; the verifier proves the *code* presented was issued for that
/// same `start`, so a code obtained elsewhere and replayed on
/// `/auth/google/callback` fails at Google's token endpoint (RFC 9700
/// requires PKCE for confidential clients too).
const OAUTH_PKCE_VERIFIER_COOKIE: &str = "google_oauth_pkce_verifier";

/// Builds Google's consent URL with a fresh CSRF `state` and a fresh S256
/// PKCE challenge. Returns the verifier the caller must persist until the
/// callback: it is never sent to Google on this leg.
fn authorization_request(client: &GoogleOauthClient) -> (Url, CsrfToken, PkceCodeVerifier) {
    todo!()
}

/// Decides, from what `start` left in the browser and what Google sent
/// back, whether `callback` may exchange the code — and with which
/// verifier. `None` means refuse: a missing, malformed or mismatched value
/// never degrades into an exchange without PKCE.
fn callback_verifier(
    expected_state: Option<String>,
    pkce_verifier: Option<String>,
    presented_state: &str,
) -> Option<PkceCodeVerifier> {
    todo!()
}

fn flow_cookie(name: &'static str, value: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(name, value);
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(cookie::SameSite::Lax);
    cookie.set_path("/");
    cookie
}

/// AC #3: redirects to Google's consent screen with a fresh CSRF `state`
/// and PKCE challenge; the `state` and the PKCE verifier are stashed in
/// short-lived HttpOnly cookies so `callback` can verify the one and
/// present the other.
pub async fn start(
    State(state): State<AppState>,
    cookies: Cookies,
) -> AppResult<impl IntoResponse> {
    let (auth_url, csrf_token, pkce_verifier) = authorization_request(&state.google_oauth);

    cookies.add(flow_cookie(
        OAUTH_STATE_COOKIE,
        csrf_token.secret().clone(),
        state.secure_cookies,
    ));
    cookies.add(flow_cookie(
        OAUTH_PKCE_VERIFIER_COOKIE,
        pkce_verifier.secret().clone(),
        state.secure_cookies,
    ));

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

/// AC #3, #8: validates the CSRF `state`, exchanges the code with the PKCE
/// verifier `start` stashed (refusing outright when it is missing), fetches
/// the verified Google profile, and either creates a new account or links
/// to the caller's existing session. The refresh token (if any) is stored
/// encrypted via `pgcrypto` and is never written to `tracing` logs.
pub async fn callback(
    State(state): State<AppState>,
    cookies: Cookies,
    Query(query): Query<CallbackQuery>,
) -> AppResult<impl IntoResponse> {
    let expected_state = cookies
        .get(OAUTH_STATE_COOKIE)
        .map(|c| c.value().to_string());
    let pkce_verifier = cookies
        .get(OAUTH_PKCE_VERIFIER_COOKIE)
        .map(|c| c.value().to_string());
    cookies.remove(Cookie::new(OAUTH_STATE_COOKIE, ""));
    cookies.remove(Cookie::new(OAUTH_PKCE_VERIFIER_COOKIE, ""));

    let pkce_verifier = callback_verifier(expected_state, pkce_verifier, &query.state)
        .ok_or(AppError::Unauthorized)?;

    let token = state
        .google_oauth
        .exchange_code(AuthorizationCode::new(query.code))
        .set_pkce_verifier(pkce_verifier)
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

    let mut tx = crate::db::begin(&state.db).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use oauth2::basic::BasicClient;
    use oauth2::{AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};

    fn client() -> GoogleOauthClient {
        BasicClient::new(ClientId::new("client-id".into()))
            .set_client_secret(ClientSecret::new("client-secret".into()))
            .set_auth_uri(AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".into()).unwrap())
            .set_token_uri(TokenUrl::new("https://oauth2.googleapis.com/token".into()).unwrap())
            .set_redirect_uri(
                RedirectUrl::new("http://localhost:8080/auth/google/callback".into()).unwrap(),
            )
    }

    fn query_param(url: &Url, name: &str) -> Option<String> {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }

    /// A 43-character verifier, the RFC 7636 minimum.
    const VERIFIER: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";

    #[test]
    fn authorization_url_carries_an_s256_challenge_of_the_returned_verifier() {
        let (url, _, verifier) = authorization_request(&client());
        assert_eq!(
            query_param(&url, "code_challenge_method").as_deref(),
            Some("S256")
        );
        let expected = PkceCodeChallenge::from_code_verifier_sha256(&verifier);
        assert_eq!(
            query_param(&url, "code_challenge").as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn authorization_url_never_leaks_the_verifier() {
        let (url, _, verifier) = authorization_request(&client());
        assert!(!url.as_str().contains(verifier.secret().as_str()));
        assert_eq!(query_param(&url, "code_verifier"), None);
    }

    #[test]
    fn authorization_url_state_is_the_returned_csrf_token() {
        let (url, csrf, _) = authorization_request(&client());
        assert_eq!(query_param(&url, "state").as_deref(), Some(csrf.secret().as_str()));
    }

    #[test]
    fn authorization_url_keeps_the_scopes_and_offline_access() {
        let (url, _, _) = authorization_request(&client());
        assert_eq!(query_param(&url, "scope").as_deref(), Some("openid email profile"));
        assert_eq!(query_param(&url, "access_type").as_deref(), Some("offline"));
        assert_eq!(query_param(&url, "prompt").as_deref(), Some("consent"));
    }

    #[test]
    fn each_authorization_request_mints_a_fresh_verifier() {
        let (_, _, a) = authorization_request(&client());
        let (_, _, b) = authorization_request(&client());
        assert_ne!(a.secret(), b.secret());
    }

    #[test]
    fn callback_proceeds_with_the_stored_verifier_when_state_matches() {
        let verifier =
            callback_verifier(Some("s".into()), Some(VERIFIER.into()), "s").expect("accepted");
        assert_eq!(verifier.secret(), VERIFIER);
    }

    #[test]
    fn callback_refuses_without_a_verifier_cookie() {
        assert!(callback_verifier(Some("s".into()), None, "s").is_none());
    }

    #[test]
    fn callback_refuses_a_verifier_rfc_7636_would_reject() {
        assert!(callback_verifier(Some("s".into()), Some(String::new()), "s").is_none());
        assert!(callback_verifier(Some("s".into()), Some(VERIFIER[..42].into()), "s").is_none());
        assert!(callback_verifier(Some("s".into()), Some("a".repeat(129)), "s").is_none());
        let bad_charset = format!("{}+", &VERIFIER[..42]);
        assert!(callback_verifier(Some("s".into()), Some(bad_charset), "s").is_none());
    }

    #[test]
    fn callback_accepts_the_rfc_7636_bounds_and_unreserved_characters() {
        let longest = "a".repeat(128);
        assert!(callback_verifier(Some("s".into()), Some(longest), "s").is_some());
        let unreserved = format!("{}-._~", &VERIFIER[..39]);
        assert!(callback_verifier(Some("s".into()), Some(unreserved), "s").is_some());
    }

    #[test]
    fn callback_refuses_without_a_state_cookie() {
        assert!(callback_verifier(None, Some(VERIFIER.into()), "s").is_none());
    }

    #[test]
    fn callback_refuses_a_mismatched_state() {
        assert!(callback_verifier(Some("s".into()), Some(VERIFIER.into()), "other").is_none());
    }
}
