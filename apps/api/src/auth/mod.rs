pub mod oauth_google;
pub mod session;
pub mod throttle;
pub mod timing;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Instant;
use tower_cookies::Cookies;
use uuid::Uuid;

use manage_our_home_shared::dto::auth::{
    ChangePasswordRequest, DeleteAccountRequest, ForgotPasswordRequest, LoginRequest, MeResponse,
    RegisterRequest, ResendVerificationRequest, ResetPasswordRequest, SetPasswordRequest,
};

use manage_our_home_shared::validation::auth::{
    validate_age_declaration, validate_display_name, validate_email, validate_password,
};

use crate::client_ip::ClientIp;
use crate::crypto::{hash_password, verify_password};
use crate::error::{AppError, AppResult};
use crate::AppState;

use self::session::{
    clear_session_cookie, create_session, revoke_all_sessions, revoke_session, set_session_cookie,
    user_scoped_tx, AuthUser,
};
use self::timing::{LoginBranch, LoginTiming};

const EMAIL_VERIFICATION_TTL_HOURS: i64 = 24;
const PASSWORD_RESET_TTL_HOURS: i64 = 24;
const ACCOUNT_DELETION_GRACE_DAYS: i64 = 30;

/// Maps a validation error code (`&'static str`) to a 422 carrying that
/// exact code in the response's `error` field.
fn unprocessable(code: &'static str) -> AppError {
    AppError::Unprocessable(code.into())
}

/// AC #15 (issue #15): "who am I" for the frontend's server-side session
/// check on every page load. Reuses the existing `AuthUser` extractor —
/// 200 with the shape below if extraction succeeds, 401
/// `{"error":"unauthorized"}` otherwise (identical to every other
/// `AuthUser`-gated handler, see `session.rs`'s `FromRequestParts` impl).
pub async fn me(auth: AuthUser) -> Json<MeResponse> {
    Json(MeResponse {
        user_id: auth.user_id,
        email: auth.email,
        display_name: auth.display_name,
        email_verified: auth.email_verified,
        is_superadmin: auth.is_superadmin,
        has_password: auth.has_password,
        deletion_requested_at: auth.deletion_requested_at,
    })
}

/// AC #1, #2: registers an email/password account, always unverified,
/// sends a verification email. Returns a generic 409 on duplicate email
/// so the response never reveals whether the existing account uses a
/// password, Google, or both.
///
/// #137: the account is also refused (422 `age_declaration_required`) unless
/// the request carries the art. 8 GDPR age declaration. The declaration is
/// checked last of the four, so a request that gets several things wrong
/// still reports the field errors the form can point at first.
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> AppResult<impl IntoResponse> {
    validate_email(&body.email).map_err(unprocessable)?;
    validate_password(&body.password).map_err(unprocessable)?;
    validate_display_name(&body.display_name).map_err(unprocessable)?;
    validate_age_declaration(body.declares_minimum_age).map_err(unprocessable)?;

    let existing = sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", body.email)
        .fetch_optional(&state.db)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict("account_already_exists".into()));
    }

    let password_hash = hash_password(&body.password).map_err(AppError::Internal)?;

    let mut tx = crate::db::begin(&state.db).await?;
    let user = sqlx::query!(
        r#"
        INSERT INTO users (email, password_hash, display_name, email_verified, age_declared_at)
        VALUES ($1, $2, $3, false, now())
        RETURNING id
        "#,
        body.email,
        password_hash,
        body.display_name,
    )
    .fetch_one(&mut *tx)
    .await?;

    let expires_at = Utc::now() + Duration::hours(EMAIL_VERIFICATION_TTL_HOURS);
    let token = sqlx::query_scalar!(
        r#"
        INSERT INTO email_verification_tokens (user_id, expires_at)
        VALUES ($1, $2)
        RETURNING token
        "#,
        user.id,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    // Lands on apps/web's /verify-email page (which consumes the token via
    // this API's GET /auth/verify-email), not on the API endpoint itself.
    let link = format!("{}/verify-email?token={token}", state.frontend_base_url);
    if let Err(e) = state
        .email
        .send(
            &body.email,
            "Vérifiez votre email",
            crate::email::EmailSender::verification_email_body(&link),
        )
        .await
    {
        tracing::error!(error = ?e, "failed to send verification email");
    }

    Ok((StatusCode::CREATED, Json(json!({ "user_id": user.id }))))
}

#[derive(Deserialize)]
pub struct VerifyEmailQuery {
    pub token: Uuid,
}

pub async fn verify_email(
    State(state): State<AppState>,
    Query(query): Query<VerifyEmailQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.db).await?;
    let row = sqlx::query!(
        r#"
        SELECT user_id, expires_at, consumed_at
        FROM email_verification_tokens
        WHERE token = $1
        FOR UPDATE
        "#,
        query.token
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if row.consumed_at.is_some() {
        return Err(AppError::Gone);
    }
    if row.expires_at < Utc::now() {
        return Err(AppError::Gone);
    }

    sqlx::query!(
        "UPDATE email_verification_tokens SET consumed_at = now() WHERE token = $1",
        query.token
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE users SET email_verified = true WHERE id = $1",
        row.user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::OK)
}

/// Splits its own wall time between the argon2 verification, the two SQL
/// statements and the remainder, and logs the split on the `login_timing`
/// target (issue #113 §5). The body is `login_inner` so that each early
/// return is still accounted for: the phases a rejected login never reached
/// stay at zero, and `outcome_label` keeps a 500 apart from a 401, so a zero
/// can be read as the phase not needed rather than the phase that broke.
///
/// One line per request **that reaches this handler** — not per request to
/// `/auth/login`. A body axum's extractor refuses never gets here and is
/// never counted: malformed JSON (400), a missing `password` (422), a wrong
/// or absent `content-type` (415) all answer before `login` runs. The
/// instrument measures the handler, so the requests it never sees are
/// outside it.
pub async fn login(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    cookies: Cookies,
    Json(body): Json<LoginRequest>,
) -> AppResult<impl IntoResponse> {
    let started = Instant::now();
    let mut timing = LoginTiming::default();
    let (branch, result) = login_inner(&state, client_ip, &cookies, body, &mut timing).await;
    timing.total = started.elapsed();
    timing.emit(timing::outcome_label(&result));

    // The branch goes into a cumulative counter and, at most once a minute
    // and never for fewer than `timing::MIN_BATCH` new refusals, into one
    // aggregate line (#178 bis). It is deliberately absent from the
    // per-request line above: "this address has an account here" written
    // once per attempt does not close the enumeration oracle, it hands it to
    // everyone who can read the journal.
    state.login_branches.record(branch);
    if let Some(totals) = state.login_branches.take_due(Instant::now()) {
        totals.emit();
    }
    result
}

/// Refuses in the one generic way all four refusals share. The 401 carries
/// no detail, and the attempt was already counted against the (address,
/// email) pair when it was admitted, whatever the reason turns out to be —
/// so an attacker learns nothing from *which* refusal they got, and burns
/// the same budget either way.
fn refused(
    branch: LoginBranch,
) -> (
    LoginBranch,
    AppResult<(StatusCode, Json<serde_json::Value>)>,
) {
    (branch, Err(AppError::Unauthorized))
}

async fn login_inner(
    state: &AppState,
    client_ip: std::net::IpAddr,
    cookies: &Cookies,
    body: LoginRequest,
    timing: &mut LoginTiming,
) -> (
    LoginBranch,
    AppResult<(StatusCode, Json<serde_json::Value>)>,
) {
    let key = throttle::key(client_ip, &body.email);

    // **Before** the lookup and before argon2, never after — and counted in
    // the same step. The decoy hash below makes every invalid attempt cost
    // a full argon2id; a lock consulted afterwards would let an attacker
    // spend that CPU first, and a count recorded only once the outcome is
    // known would let a burst of concurrent requests all pass the check
    // before the first one finished hashing (#178). A success clears the
    // pair below.
    if let throttle::Decision::Locked { .. } = state.login_throttle.admit(&key, Instant::now()) {
        return (LoginBranch::Throttled, Err(AppError::TooManyRequests));
    }

    let at = Instant::now();
    let user = sqlx::query!(
        "SELECT id, password_hash, email_verified FROM users WHERE email = $1 AND deleted_at IS NULL",
        body.email
    )
    .fetch_optional(&state.db)
    .await;
    timing.lookup = at.elapsed();
    let user = match user {
        Ok(user) => user,
        Err(e) => return (LoginBranch::Error, Err(e.into())),
    };

    // The three regimes of #178 — no such email, a Google-only account with
    // `password_hash = NULL`, and an account with a hash — all pay the same
    // argon2id from here on. The first two verify the submitted password
    // against a decoy nothing matches (`crypto::decoy_hash`) instead of
    // returning on the spot; before this, they answered in ~0,22 ms against
    // the ~256 ms of the third, which is three orders of magnitude of
    // "does this address have an account here", readable with a stopwatch
    // and no credentials.
    let stored_hash = user.as_ref().and_then(|u| u.password_hash.clone());
    let hash: &str = match stored_hash.as_deref() {
        Some(hash) => hash,
        None => crate::crypto::decoy_hash(),
    };

    let at = Instant::now();
    let verified = verify_password(&body.password, hash);
    timing.verify = at.elapsed();
    let verified = match verified {
        Ok(verified) => verified,
        Err(e) => return (LoginBranch::Error, Err(AppError::Internal(e))),
    };

    let Some(user) = user else {
        return refused(LoginBranch::UnknownEmail);
    };
    if stored_hash.is_none() {
        // Verified against the decoy, so `verified` is false here whatever
        // was submitted; the branch is kept separate for the counter only.
        return refused(LoginBranch::NoPassword);
    }
    if !verified {
        return refused(LoginBranch::WrongPassword);
    }
    // A password is only usable once its owning email has been verified —
    // covers both fresh registrations and password added to a
    // previously Google-only account (AC #7).
    if !user.email_verified {
        return refused(LoginBranch::Unverified);
    }

    let at = Instant::now();
    let session_id = match create_session(&state.db, user.id).await {
        Ok(session_id) => session_id,
        Err(e) => return (LoginBranch::Error, Err(e.into())),
    };
    timing.session = at.elapsed();
    set_session_cookie(cookies, session_id, state.secure_cookies);
    state.login_throttle.record_success(&key);

    (
        LoginBranch::Ok,
        Ok((StatusCode::OK, Json(json!({ "user_id": user.id })))),
    )
}

pub async fn logout(
    State(state): State<AppState>,
    cookies: Cookies,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    revoke_session(&state.db, auth.session_id).await?;
    clear_session_cookie(&cookies);
    Ok(StatusCode::NO_CONTENT)
}

/// The link mailed by `forgot_password`. The token rides in the fragment,
/// never the query string (#142): a browser does not send the fragment to
/// the server, so it stays out of every access log along the way and out of
/// any `Referer`. apps/web's `/reset-password` page moves it into the POST
/// body with a few lines of inline script.
fn password_reset_link(frontend_base_url: &str, token: Uuid) -> String {
    format!("{frontend_base_url}/reset-password#token={token}")
}

/// AC #4: identical response whether or not the account exists, to avoid
/// leaking which emails are registered (anti-enumeration).
pub async fn forgot_password(
    State(state): State<AppState>,
    Json(body): Json<ForgotPasswordRequest>,
) -> AppResult<impl IntoResponse> {
    if let Some(user) = sqlx::query!(
        "SELECT id FROM users WHERE email = $1 AND deleted_at IS NULL",
        body.email
    )
    .fetch_optional(&state.db)
    .await?
    {
        let expires_at = Utc::now() + Duration::hours(PASSWORD_RESET_TTL_HOURS);
        let token = sqlx::query_scalar!(
            r#"
            INSERT INTO password_reset_tokens (user_id, expires_at)
            VALUES ($1, $2)
            RETURNING token
            "#,
            user.id,
            expires_at,
        )
        .fetch_one(&state.db)
        .await?;

        // Lands on apps/web's /reset-password form (which POSTs the new
        // password to this API's /auth/password/reset), not on the API
        // endpoint itself (POST-only — a GET there would 405).
        let link = password_reset_link(&state.frontend_base_url, token);
        if let Err(e) = state
            .email
            .send(
                &body.email,
                "Réinitialisation de mot de passe",
                crate::email::EmailSender::password_reset_body(&link),
            )
            .await
        {
            tracing::error!(error = ?e, "failed to send password reset email");
        }
    }

    Ok(StatusCode::OK)
}

const RESEND_COOLDOWN_MINUTES: i32 = 5;

/// Anti-enumeration: always returns 200 with an empty body, whether the
/// email is unknown, already verified, or unverified. Mirrors
/// `forgot_password`. When the account exists and is still unverified, any
/// outstanding verification tokens are invalidated and a fresh one is
/// issued and emailed (best-effort). A per-email cooldown is enforced
/// entirely in SQL: if a token was issued for this user within the last
/// `RESEND_COOLDOWN_MINUTES`, the whole operation is a silent no-op (no new
/// token, no email).
pub async fn resend_verification(
    State(state): State<AppState>,
    Json(body): Json<ResendVerificationRequest>,
) -> AppResult<impl IntoResponse> {
    let Some(user) = sqlx::query!(
        "SELECT id FROM users WHERE email = $1 AND email_verified = false AND deleted_at IS NULL",
        body.email
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(StatusCode::OK);
    };

    let expires_at = Utc::now() + Duration::hours(EMAIL_VERIFICATION_TTL_HOURS);
    // Cooldown, invalidation and issuance happen atomically in one
    // statement: the new token is inserted (and old ones consumed) only
    // when no token was created within the cooldown window, so concurrent
    // resends can't slip past the rate limit.
    let token = sqlx::query_scalar!(
        r#"
        WITH recent AS (
            SELECT 1
            FROM email_verification_tokens
            WHERE user_id = $1
              AND created_at > now() - make_interval(mins => $3::int)
            LIMIT 1
        ),
        invalidated AS (
            UPDATE email_verification_tokens
            SET consumed_at = now()
            WHERE user_id = $1
              AND consumed_at IS NULL
              AND NOT EXISTS (SELECT 1 FROM recent)
        )
        INSERT INTO email_verification_tokens (user_id, expires_at)
        SELECT $1, $2
        WHERE NOT EXISTS (SELECT 1 FROM recent)
        RETURNING token
        "#,
        user.id,
        expires_at,
        RESEND_COOLDOWN_MINUTES,
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(token) = token {
        let link = format!("{}/auth/verify-email?token={token}", state.public_base_url);
        if let Err(e) = state
            .email
            .send(
                &body.email,
                "Vérifiez votre email",
                crate::email::EmailSender::verification_email_body(&link),
            )
            .await
        {
            tracing::error!(error = ?e, "failed to send verification email");
        }
    }

    Ok(StatusCode::OK)
}

/// AC #4: consuming a valid reset token revokes every active session.
pub async fn reset_password(
    State(state): State<AppState>,
    Json(body): Json<ResetPasswordRequest>,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.db).await?;
    let row = sqlx::query!(
        r#"
        SELECT user_id, expires_at, consumed_at
        FROM password_reset_tokens
        WHERE token = $1
        FOR UPDATE
        "#,
        body.token
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if row.consumed_at.is_some() || row.expires_at < Utc::now() {
        return Err(AppError::Gone);
    }

    validate_password(&body.new_password).map_err(unprocessable)?;
    let password_hash = hash_password(&body.new_password).map_err(AppError::Internal)?;

    sqlx::query!(
        "UPDATE password_reset_tokens SET consumed_at = now() WHERE token = $1",
        body.token
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE users SET password_hash = $1 WHERE id = $2",
        password_hash,
        row.user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    revoke_all_sessions(&state.db, row.user_id, None).await?;

    Ok(StatusCode::OK)
}

/// AC #5: requires the current password; keeps the calling session alive
/// and revokes every other active session.
pub async fn change_password(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<ChangePasswordRequest>,
) -> AppResult<impl IntoResponse> {
    let user = sqlx::query!(
        "SELECT password_hash FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;

    let Some(hash) = user.password_hash else {
        return Err(AppError::Unprocessable("no_password_set".into()));
    };
    if !verify_password(&body.current_password, &hash).map_err(AppError::Internal)? {
        return Err(AppError::Unauthorized);
    }

    validate_password(&body.new_password).map_err(unprocessable)?;
    let new_hash = hash_password(&body.new_password).map_err(AppError::Internal)?;
    sqlx::query!(
        "UPDATE users SET password_hash = $1 WHERE id = $2",
        new_hash,
        auth.user_id
    )
    .execute(&state.db)
    .await?;

    revoke_all_sessions(&state.db, auth.user_id, Some(auth.session_id)).await?;

    Ok(StatusCode::OK)
}

/// AC #7: adding a password to a Google-only account resets
/// `email_verified` to false and sends a fresh verification link; `login`
/// refuses password auth until that link is consumed, so the password is
/// inert in the meantime.
pub async fn set_password(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SetPasswordRequest>,
) -> AppResult<impl IntoResponse> {
    let user = sqlx::query!(
        "SELECT password_hash FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;
    if user.password_hash.is_some() {
        return Err(AppError::Conflict("password_already_set".into()));
    }

    validate_password(&body.new_password).map_err(unprocessable)?;
    let password_hash = hash_password(&body.new_password).map_err(AppError::Internal)?;

    let mut tx = crate::db::begin(&state.db).await?;
    sqlx::query!(
        "UPDATE users SET password_hash = $1, email_verified = false WHERE id = $2",
        password_hash,
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    let expires_at = Utc::now() + Duration::hours(EMAIL_VERIFICATION_TTL_HOURS);
    let token = sqlx::query_scalar!(
        r#"
        INSERT INTO email_verification_tokens (user_id, expires_at)
        VALUES ($1, $2)
        RETURNING token
        "#,
        auth.user_id,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    // Lands on apps/web's /verify-email page (which consumes the token via
    // this API's GET /auth/verify-email), not on the API endpoint itself.
    let link = format!("{}/verify-email?token={token}", state.frontend_base_url);
    if let Err(e) = state
        .email
        .send(
            &auth.email,
            "Vérifiez votre email",
            crate::email::EmailSender::verification_email_body(&link),
        )
        .await
    {
        tracing::error!(error = ?e, "failed to send verification email");
    }

    Ok(StatusCode::OK)
}

#[derive(Serialize)]
struct BlockingGroup {
    group_id: Uuid,
    name: String,
}

/// AC #6: blocked (409, with the list of owned groups) while the user
/// owns any group; otherwise schedules deletion at now()+30d.
pub async fn delete_account(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<DeleteAccountRequest>,
) -> AppResult<impl IntoResponse> {
    let user = sqlx::query!(
        "SELECT password_hash FROM users WHERE id = $1",
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;
    if let Some(hash) = user.password_hash {
        let provided = body.current_password.ok_or(AppError::Unauthorized)?;
        if !verify_password(&provided, &hash).map_err(AppError::Internal)? {
            return Err(AppError::Unauthorized);
        }
    }
    // Google-only accounts: re-consent is validated on the frontend flow
    // before this endpoint is called (session freshness is enforced by
    // requiring `AuthUser`, i.e. a live session).

    // Read under `app.user_id`, not on the bare pool: the caller's
    // membership rows and the groups they belong to are only visible to the
    // policies of 0014 inside that scope. Off it, the role apps/api's
    // README prescribes for `DATABASE_URL` (`NOSUPERUSER NOBYPASSRLS`) sees
    // nothing, the guard finds no owned group, and a group's owner is
    // scheduled for deletion instead of being blocked (issue #207). A role
    // that bypasses RLS — the superuser the `e2e` job and
    // infra/docker-compose.yml connect as — answered correctly either way,
    // which is what hid this: the `WHERE gm.user_id` filter is the one that
    // does the work there, and it is unchanged.
    let mut tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let owned_groups = sqlx::query_as!(
        BlockingGroup,
        r#"
        SELECT g.id as group_id, g.name
        FROM group_members gm
        JOIN groups g ON g.id = gm.group_id
        WHERE gm.user_id = $1 AND gm.role = 'owner'
        "#,
        auth.user_id
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    if !owned_groups.is_empty() {
        return Err(AppError::ConflictJson(json!({
            "error": "owner_of_groups",
            "groups": owned_groups.iter().map(|g| json!({"id": g.group_id, "name": g.name})).collect::<Vec<_>>(),
        })));
    }

    let mut tx = crate::db::begin(&state.db).await?;
    sqlx::query!(
        "UPDATE users SET deletion_requested_at = now() WHERE id = $1",
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    crate::audit::record(
        &mut tx,
        Some(auth.user_id),
        "account_deletion_requested",
        "user",
        &auth.user_id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;

    Ok(StatusCode::OK)
}

pub async fn cancel_delete_account(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let mut tx = crate::db::begin(&state.db).await?;
    let updated = sqlx::query!(
        r#"
        UPDATE users
        SET deletion_requested_at = NULL
        WHERE id = $1 AND deletion_requested_at IS NOT NULL AND deleted_at IS NULL
        "#,
        auth.user_id
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    crate::audit::record(
        &mut tx,
        Some(auth.user_id),
        "account_deletion_cancelled",
        "user",
        &auth.user_id.to_string(),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::OK)
}

pub const _ACCOUNT_DELETION_GRACE_DAYS: i64 = ACCOUNT_DELETION_GRACE_DAYS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_reset_link_carries_the_token_in_the_fragment() {
        let token = Uuid::parse_str("5f0c7a3e-2b1d-4c8e-9a6f-0d3b2e1c4a5b").unwrap();
        assert_eq!(
            password_reset_link("https://maison.example", token),
            "https://maison.example/reset-password#token=5f0c7a3e-2b1d-4c8e-9a6f-0d3b2e1c4a5b"
        );
    }

    #[test]
    fn password_reset_link_has_no_query_string() {
        let link = password_reset_link("http://localhost:3000", Uuid::new_v4());
        let before_fragment = link.split('#').next().unwrap();
        assert!(!before_fragment.contains('?'), "{link}");
        assert_eq!(before_fragment, "http://localhost:3000/reset-password");
    }

    /// `me`'s only logic is reshaping an already-extracted `AuthUser` into
    /// `MeResponse` — the 401 path is entirely the `AuthUser` extractor's
    /// responsibility (identical to every other `AuthUser`-gated handler)
    /// and is exercised end-to-end in `apps/api/tests/auth_flow.rs`
    /// (`GET /auth/me` without a session cookie). This covers the 200
    /// shape directly, without spinning up a full router + DB.
    #[tokio::test]
    async fn me_returns_authenticated_user_shape() {
        let auth = AuthUser {
            user_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            email: "alice@example.test".into(),
            display_name: "Alice".into(),
            email_verified: true,
            is_superadmin: true,
            has_password: true,
            deletion_requested_at: None,
        };
        let user_id = auth.user_id;

        let Json(body) = me(auth).await;

        assert_eq!(body.user_id, user_id);
        assert_eq!(body.email, "alice@example.test");
        assert_eq!(body.display_name, "Alice");
        assert!(body.email_verified);
        assert!(body.is_superadmin);
        assert!(body.has_password);
        assert_eq!(body.deletion_requested_at, None);
    }
}
