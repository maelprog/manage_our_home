use axum::async_trait;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use cookie::{Cookie, SameSite};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use tower_cookies::Cookies;
use uuid::Uuid;

use manage_our_home_shared::dto::auth::ActiveSession;

use crate::error::AppError;
use crate::AppState;

/// Private on purpose (#239): nothing outside this module may name the
/// cookie — see `only_this_module_reads_the_session_cookie`. The name a
/// response sets and a request is read under is [`session_cookie_name`]'s.
const SESSION_COOKIE_NAME: &str = "session_id";
/// The same cookie under the `__Host-` prefix (#224, RFC 6265bis): a
/// browser only accepts it `Secure`, in `Path=/` and without `Domain`, so
/// a sibling subdomain or a plain-HTTP page on the same host cannot plant
/// one that shadows the real session (cookie tossing, session fixation).
const HOST_SESSION_COOKIE_NAME: &str = "__Host-session_id";
/// Raw bytes of a session token (#222), drawn from the OS CSPRNG.
const SESSION_TOKEN_BYTES: usize = 32;
/// Absolute lifetime of a session, set at creation in `expires_at` (the
/// opening plus this, on the database's clock) and in the cookie's
/// `max_age`. Use never pushes it back. `expires_at` can only move earlier:
/// a superadmin session found idle for the admin routes has it brought
/// forward when its admin access is closed (#339,
/// `user_admin::closed_admin_expiry`), by at most 10 hours. The cookie then
/// outlives the session, which the api refuses past `expires_at` like any
/// other expired one.
pub const SESSION_TTL_DAYS: i64 = 30;
/// Inactivity timeout (#195): a session left unused for longer than this is
/// refused, even within its absolute lifetime. The row is left as it is, not
/// revoked: `revoked_at` keeps meaning a logout or a password change, and
/// `last_seen_at` already dates the end of the session.
pub const SESSION_IDLE_TIMEOUT_DAYS: i64 = 7;
/// `last_seen_at` is only rewritten once it is older than this, so an active
/// session costs one `UPDATE` per interval instead of one per request. The
/// price is precision: a session can be refused up to this long before
/// `SESSION_IDLE_TIMEOUT_DAYS` of actual inactivity.
const LAST_SEEN_REFRESH_MINUTES: i64 = 60;

/// Whether a session last used at `last_seen_at` has been idle too long.
/// Like `expires_at`, the bound itself is still valid.
fn is_idle(now: DateTime<Utc>, last_seen_at: DateTime<Utc>) -> bool {
    last_seen_at + Duration::days(SESSION_IDLE_TIMEOUT_DAYS) < now
}

/// Whether this request should rewrite `last_seen_at`.
fn last_seen_needs_refresh(now: DateTime<Utc>, last_seen_at: DateTime<Utc>) -> bool {
    last_seen_at + Duration::minutes(LAST_SEEN_REFRESH_MINUTES) < now
}

/// What a session opens (#289), from the session's `restricted` flag and
/// the account's `deactivated_at` / `deleted_at` / `age_declared_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAccess {
    /// Every route `AuthUser` guards.
    Full,
    /// Only the routes `DeactivatedSession` guards.
    Deactivated,
    /// Only the routes `AgeUndeclaredSession` guards (#318): the full
    /// session of an active account with no age declaration on file — one
    /// opened through Google, or before the declaration existed (#137).
    AgeUndeclared,
    /// Nothing: 401.
    Refused,
}

fn session_access(
    restricted: bool,
    deactivated: bool,
    deleted: bool,
    age_declared: bool,
) -> SessionAccess {
    match (deleted, restricted, deactivated) {
        (false, false, false) if age_declared => SessionAccess::Full,
        (false, false, false) => SessionAccess::AgeUndeclared,
        (false, true, true) => SessionAccess::Deactivated,
        _ => SessionAccess::Refused,
    }
}

/// What [`session_state`] needs from a `sessions` row and its account.
struct SessionRow {
    revoked: bool,
    expires_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    restricted: bool,
    deactivated: bool,
    deleted: bool,
    age_declared: bool,
}

/// The one verdict on a session (#221): the checks every session shares —
/// not revoked, within its absolute lifetime, not idle — then what it opens.
/// `load_session` asks it once per request, and the messagerie WebSocket
/// again on every recheck tick, so a revocation reaches both channels.
fn session_state(now: DateTime<Utc>, row: &SessionRow) -> SessionAccess {
    if row.revoked || row.expires_at < now || is_idle(now, row.last_seen_at) {
        return SessionAccess::Refused;
    }
    session_access(
        row.restricted,
        row.deactivated,
        row.deleted,
        row.age_declared,
    )
}

/// A `sessions` row of the caller, as `GET /auth/sessions` reads it (#225).
pub struct ListedSession {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub restricted: bool,
}

/// The caller's sessions that are still live (#225), `current` first, then
/// the most recently used. "Live" is [`session_state`]'s verdict, the one
/// every request gets: a revoked, expired, idle or restricted session is
/// left out. The caller holds a full session, so its account is neither
/// deactivated nor deleted.
pub fn active_sessions(
    now: DateTime<Utc>,
    current: Uuid,
    rows: Vec<ListedSession>,
) -> Vec<ActiveSession> {
    let mut live: Vec<ActiveSession> = rows
        .into_iter()
        .filter(|row| {
            let state = session_state(
                now,
                &SessionRow {
                    revoked: row.revoked,
                    expires_at: row.expires_at,
                    last_seen_at: row.last_seen_at,
                    restricted: row.restricted,
                    deactivated: false,
                    deleted: false,
                    age_declared: true,
                },
            );
            state == SessionAccess::Full
        })
        .map(|row| ActiveSession {
            id: row.id,
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            current: row.id == current,
        })
        .collect();
    live.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then(b.last_seen_at.cmp(&a.last_seen_at))
    });
    live
}

/// The caller's live sessions (#225), read for `GET /auth/sessions`. The
/// SQL only narrows to the caller's unrevoked rows; what counts as live is
/// [`active_sessions`]'s to say.
pub async fn list_active_sessions(
    pool: &PgPool,
    user_id: Uuid,
    current: Uuid,
) -> Result<Vec<ActiveSession>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT id, created_at, last_seen_at, expires_at, restricted
        FROM sessions
        WHERE user_id = $1 AND revoked_at IS NULL
        "#,
        user_id
    )
    .fetch_all(pool)
    .await?;
    Ok(active_sessions(
        Utc::now(),
        current,
        rows.into_iter()
            .map(|r| ListedSession {
                id: r.id,
                created_at: r.created_at,
                last_seen_at: r.last_seen_at,
                expires_at: r.expires_at,
                revoked: false,
                restricted: r.restricted,
            })
            .collect(),
    ))
}

/// Revokes one session of `user_id` (#225). `false` when the caller has no
/// such unrevoked session — someone else's included, so the answer does
/// not tell whether that id exists.
pub async fn revoke_own_session(
    pool: &PgPool,
    user_id: Uuid,
    session_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        UPDATE sessions SET revoked_at = now()
        WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL
        "#,
        session_id,
        user_id
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub session_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub email_verified: bool,
    /// The global technical superadmin flag (`users.is_superadmin`, migration
    /// 0009). Carried here so `GET /auth/me` can tell `apps/web` whether to
    /// render the `/admin` nav/route tree (front epic F9, #24) — the backend's
    /// `SuperAdminUser` extractor stays the authority for the `/admin/*`
    /// endpoints themselves.
    pub is_superadmin: bool,
    /// Whether the account authenticates with a password
    /// (`users.password_hash IS NOT NULL`). Carried here so `GET /auth/me` can
    /// tell `apps/web` whether the RGPD deletion flow must ask for the current
    /// password (front epic F10, #25) — `delete_account` verifies it only for
    /// such accounts, and stays the authority.
    pub has_password: bool,
    /// Pending self-service deletion request (`users.deletion_requested_at`),
    /// exposed through `GET /auth/me` so `apps/web` can render the grace-period
    /// banner + cancel action (front epic F10, #25). Read live on every request,
    /// like `is_superadmin`, so requesting or cancelling takes effect on the next
    /// page load.
    pub deletion_requested_at: Option<chrono::DateTime<Utc>>,
    /// When the session was opened (`sessions.created_at`), for the
    /// superadmin session cap (#226, `SuperAdminUser`).
    pub session_created_at: DateTime<Utc>,
    /// The session's `expires_at`, the admin access closed if this request
    /// found it idle (#339), for the same cap.
    pub session_expires_at: DateTime<Utc>,
    /// The session's `last_seen_at` as this request found it, before the
    /// refresh, for the same cap.
    pub session_last_seen_at: DateTime<Utc>,
}

/// The session the request's cookie names, with the account it belongs to,
/// once the checks every session shares have passed: known, not revoked,
/// within its absolute lifetime, not idle. `last_seen_at` is refreshed
/// here. What the session then opens is [`session_state`]'s to say.
struct LoadedSession {
    session_id: Uuid,
    user_id: Uuid,
    email: String,
    display_name: String,
    email_verified: bool,
    is_superadmin: bool,
    has_password: bool,
    deletion_requested_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    access: SessionAccess,
}

async fn load_session<S>(parts: &mut Parts, state: &S) -> Result<LoadedSession, AppError>
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    let app_state = AppState::from_ref(state);
    let cookies = Cookies::from_request_parts(parts, state)
        .await
        .map_err(|_| AppError::Unauthorized)?;
    let cookie = cookies
        .get(session_cookie_name(app_state.secure_cookies))
        .ok_or(AppError::Unauthorized)?;
    let token_hash = session_token_hash(cookie.value()).ok_or(AppError::Unauthorized)?;

    let row = sqlx::query!(
        r#"
        SELECT s.id as session_id, s.created_at, s.expires_at, s.revoked_at, s.last_seen_at,
               s.restricted, u.id as user_id, u.email, u.display_name, u.email_verified,
               u.is_superadmin, u.deleted_at, u.deactivated_at, u.deletion_requested_at,
               u.age_declared_at, (u.password_hash IS NOT NULL) as "has_password!"
        FROM sessions s
        JOIN users u ON u.id = s.user_id
        WHERE s.token_hash = $1
        "#,
        &token_hash[..]
    )
    .fetch_optional(&app_state.db)
    .await
    .map_err(AppError::from)?
    .ok_or(AppError::Unauthorized)?;

    let now = Utc::now();
    let access = session_state(
        now,
        &SessionRow {
            revoked: row.revoked_at.is_some(),
            expires_at: row.expires_at,
            last_seen_at: row.last_seen_at,
            restricted: row.restricted,
            deactivated: row.deactivated_at.is_some(),
            deleted: row.deleted_at.is_some(),
            age_declared: row.age_declared_at.is_some(),
        },
    );
    if access == SessionAccess::Refused {
        return Err(AppError::Unauthorized);
    }

    // A superadmin session found idle past the admin timeout has its admin
    // access closed (#339), in the same statement as the refresh: if it
    // fails, `last_seen_at` is left as it is too, and the next request
    // finds the session idle again. Idle past 2 hours is always stale past
    // the refresh interval, so the closure never goes without the refresh.
    let closed_expiry = if row.is_superadmin {
        crate::user_admin::closed_admin_expiry(now, row.expires_at, row.last_seen_at)
    } else {
        None
    };
    if last_seen_needs_refresh(now, row.last_seen_at) {
        sqlx::query!(
            r#"
            UPDATE sessions
            SET last_seen_at = now(), expires_at = LEAST(expires_at, COALESCE($2, expires_at))
            WHERE id = $1
            "#,
            row.session_id,
            closed_expiry
        )
        .execute(&app_state.db)
        .await
        .ok();
    }

    Ok(LoadedSession {
        session_id: row.session_id,
        user_id: row.user_id,
        email: row.email,
        display_name: row.display_name,
        email_verified: row.email_verified,
        is_superadmin: row.is_superadmin,
        has_password: row.has_password,
        deletion_requested_at: row.deletion_requested_at,
        created_at: row.created_at,
        expires_at: closed_expiry.unwrap_or(row.expires_at),
        last_seen_at: row.last_seen_at,
        access,
    })
}

/// Whether `session_id` still opens what [`AuthUser`] opens, read afresh
/// (#221). For a channel authenticated once and kept open — the messagerie
/// WebSocket — so a logout, a password change or reset, a deactivation, a
/// purge, the expiry or the inactivity timeout closes it too. Unlike
/// [`load_session`], it does not refresh `last_seen_at`: an open socket is
/// not activity, or a tab left on the messagerie would keep its session
/// alive forever. A database error counts as a refusal.
pub async fn is_full_session(pool: &PgPool, session_id: Uuid) -> bool {
    let row = sqlx::query!(
        r#"
        SELECT s.expires_at, s.revoked_at, s.last_seen_at, s.restricted,
               u.deleted_at, u.deactivated_at, u.age_declared_at
        FROM sessions s
        JOIN users u ON u.id = s.user_id
        WHERE s.id = $1
        "#,
        session_id
    )
    .fetch_optional(pool)
    .await;
    let Ok(Some(row)) = row else {
        return false;
    };
    session_state(
        Utc::now(),
        &SessionRow {
            revoked: row.revoked_at.is_some(),
            expires_at: row.expires_at,
            last_seen_at: row.last_seen_at,
            restricted: row.restricted,
            deactivated: row.deactivated_at.is_some(),
            deleted: row.deleted_at.is_some(),
            age_declared: row.age_declared_at.is_some(),
        },
    ) == SessionAccess::Full
}

/// A full session of an active account with an age declaration on file. A
/// restricted session (#289) is refused with 403 `account_deactivated` —
/// its holder proved the credentials, so the answer tells them nothing they
/// do not know, and `apps/web` sends them to the deactivated-account page on
/// it. A session of an account without age declaration (#318) is refused
/// with 403 `age_not_declared`, on which `apps/web` sends its holder to the
/// declaration page. Every other refusal is the bare 401.
#[async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = load_session(parts, state).await?;
        match session.access {
            SessionAccess::Full => {}
            SessionAccess::AgeUndeclared => return Err(AppError::AgeNotDeclared),
            SessionAccess::Deactivated | SessionAccess::Refused => {
                return Err(AppError::AccountDeactivated)
            }
        }
        Ok(AuthUser {
            user_id: session.user_id,
            session_id: session.session_id,
            email: session.email,
            display_name: session.display_name,
            email_verified: session.email_verified,
            is_superadmin: session.is_superadmin,
            has_password: session.has_password,
            deletion_requested_at: session.deletion_requested_at,
            session_created_at: session.created_at,
            session_expires_at: session.expires_at,
            session_last_seen_at: session.last_seen_at,
        })
    }
}

/// Any live session, full or restricted — what logging out needs, and
/// nothing else (#289): the holder of a deactivated account logs out
/// through the same `POST /auth/logout` as everyone.
#[derive(Debug, Clone)]
pub struct AnySession {
    pub session_id: Uuid,
}

#[async_trait]
impl<S> FromRequestParts<S> for AnySession
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = load_session(parts, state).await?;
        Ok(AnySession {
            session_id: session.session_id,
        })
    }
}

/// The restricted session a correct login on a deactivated account opens
/// (#289), and nothing else: a full session, or a restricted one whose
/// account has since been reactivated or purged, is a 401. Guards only the
/// deactivated-account routes (`auth::deactivated`); logging out takes
/// [`AnySession`].
#[derive(Debug, Clone)]
pub struct DeactivatedSession {
    pub user_id: Uuid,
    pub session_id: Uuid,
}

#[async_trait]
impl<S> FromRequestParts<S> for DeactivatedSession
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = load_session(parts, state).await?;
        if session.access != SessionAccess::Deactivated {
            return Err(AppError::Unauthorized);
        }
        Ok(DeactivatedSession {
            user_id: session.user_id,
            session_id: session.session_id,
        })
    }
}

/// The full session of an account with no age declaration on file (#318),
/// and nothing else: any other session is a 401. Guards only
/// `POST /auth/age-declaration`; logging out takes [`AnySession`].
#[derive(Debug, Clone)]
pub struct AgeUndeclaredSession {
    pub user_id: Uuid,
}

#[async_trait]
impl<S> FromRequestParts<S> for AgeUndeclaredSession
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = load_session(parts, state).await?;
        if session.access != SessionAccess::AgeUndeclared {
            return Err(AppError::Unauthorized);
        }
        Ok(AgeUndeclaredSession {
            user_id: session.user_id,
        })
    }
}

/// Logs the caller in: the jar carries the session cookie on the response.
/// No function hands the `Cookie` value back to the caller (#239). The
/// cookie still sits in the jar the caller passes in, so its name can be
/// read back from it (`jar.list()`). That route stays open, and
/// `only_this_module_reads_the_session_cookie` does not see it.
pub fn set_session_cookie(cookies: &Cookies, token: &SessionToken, secure: bool) {
    cookies.add(build_session_cookie(token, secure));
}

/// Logs the caller out: the jar carries the removal of the session cookie.
pub fn clear_session_cookie(cookies: &Cookies, secure: bool) {
    cookies.add(expired_session_cookie(secure));
}

/// The session cookie's name (#224). Behind `SECURE_COOKIES` it carries
/// the `__Host-` prefix. Without it — local development over plain HTTP,
/// `infra/.env.example` — a browser rejects a `__Host-` cookie that is not
/// `Secure`, so the bare name stays, or logging in would set nothing.
fn session_cookie_name(secure: bool) -> &'static str {
    if secure {
        HOST_SESSION_COOKIE_NAME
    } else {
        SESSION_COOKIE_NAME
    }
}

/// A fresh session token (#222): the value the cookie carries, and the
/// hash `sessions.token_hash` keeps. Neither `Debug` nor `Display`, so the
/// value does not end up in a log by accident.
pub struct SessionToken {
    value: String,
    hash: [u8; 32],
}

impl SessionToken {
    /// What the cookie carries: [`SESSION_TOKEN_BYTES`] random bytes,
    /// unpadded base64url.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// What `sessions.token_hash` keeps: [`session_token_hash`] of
    /// [`SessionToken::value`].
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
}

pub fn new_session_token() -> SessionToken {
    let mut bytes = [0u8; SESSION_TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    SessionToken {
        value: URL_SAFE_NO_PAD.encode(bytes),
        hash: Sha256::digest(bytes).into(),
    }
}

/// The `sessions.token_hash` a cookie value names (#222): the SHA-256 of
/// the token's raw bytes. `None` for anything that is not a token as
/// [`new_session_token`] spells it — exactly [`SESSION_TOKEN_BYTES`]
/// bytes in canonical unpadded base64url — so a malformed cookie is
/// refused without a query. Computed here, not in SQL, so the token never
/// travels in a statement or its logged parameters.
pub fn session_token_hash(cookie_value: &str) -> Option<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD.decode(cookie_value).ok()?;
    if bytes.len() != SESSION_TOKEN_BYTES {
        return None;
    }
    Some(Sha256::digest(bytes).into())
}

fn build_session_cookie(token: &SessionToken, secure: bool) -> Cookie<'static> {
    Cookie::build((session_cookie_name(secure), token.value().to_owned()))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(cookie::time::Duration::days(SESSION_TTL_DAYS))
        .build()
}

fn expired_session_cookie(secure: bool) -> Cookie<'static> {
    // `Secure` too: a browser drops a `__Host-` cookie without it, the
    // removal included, and the session cookie would stay (#224).
    let mut c = Cookie::build((session_cookie_name(secure), ""))
        .secure(secure)
        .path("/")
        .build();
    c.make_removal();
    c
}

/// Opens a session for `user_id` and returns the token its cookie carries.
/// The table keeps only the token's hash (#222).
pub async fn create_session(pool: &PgPool, user_id: Uuid) -> Result<SessionToken, sqlx::Error> {
    insert_session(pool, user_id, false).await
}

/// The session a correct login on a deactivated account opens (#289): same
/// lifetime and cookie as any other, but only [`DeactivatedSession`]
/// accepts it.
pub async fn create_restricted_session(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<SessionToken, sqlx::Error> {
    insert_session(pool, user_id, true).await
}

async fn insert_session(
    pool: &PgPool,
    user_id: Uuid,
    restricted: bool,
) -> Result<SessionToken, sqlx::Error> {
    let token = new_session_token();
    // `expires_at` on the database's clock, the one `created_at` and
    // `last_seen_at` are read off: exactly the opening plus the lifetime,
    // which bounds what closing the admin access takes off it (#339).
    sqlx::query!(
        r#"
        INSERT INTO sessions (user_id, expires_at, restricted, token_hash)
        VALUES ($1, now() + make_interval(days => $2), $3, $4)
        "#,
        user_id,
        SESSION_TTL_DAYS as i32,
        restricted,
        &token.hash()[..]
    )
    .execute(pool)
    .await?;
    Ok(token)
}

pub async fn revoke_session(pool: &PgPool, session_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        session_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Revokes every active session for a user. `except` optionally keeps one
/// session alive (used by the authenticated password-change flow, AC #5).
pub async fn revoke_all_sessions(
    pool: &PgPool,
    user_id: Uuid,
    except: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE sessions
        SET revoked_at = now()
        WHERE user_id = $1 AND revoked_at IS NULL AND id IS DISTINCT FROM $2
        "#,
        user_id,
        except
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Opens a transaction with `app.family_id` / `app.user_id` set via
/// `SET LOCAL`, so RLS policies on `group_members`/`invitations`/`groups`
/// enforce tenant isolation at the DB layer for every query run on it
/// (architecture.md's RLS-from-v1 requirement, AC #15).
pub async fn scoped_tx<'a>(
    pool: &'a PgPool,
    family_id: Uuid,
    user_id: Uuid,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("SELECT set_config('app.family_id', $1, true)")
        .bind(family_id.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Same as `scoped_tx` but without a known `family_id` yet (e.g. listing
/// "my groups"). Only `app.user_id` is set; the `groups` RLS policy falls
/// back to membership-based visibility in this case.
pub async fn user_scoped_tx<'a>(
    pool: &'a PgPool,
    user_id: Uuid,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Resolves an invitation's `group_id` from its token alone (before
/// `family_id` is known), relying on the `invitations` RLS policy's
/// token-based branch rather than bypassing RLS.
pub async fn token_scoped_tx<'a>(
    pool: &'a PgPool,
    token: Uuid,
) -> Result<Transaction<'a, Postgres>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("SELECT set_config('app.invitation_token', $1, true)")
        .bind(token.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    // -- session token (#222) ----------------------------------------------

    #[test]
    fn a_new_token_is_32_random_bytes_in_unpadded_base64url() {
        let token = new_session_token();
        assert_eq!(token.value().len(), 43, "{}", token.value());
        assert!(token
            .value()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    #[test]
    fn a_new_token_carries_the_hash_of_its_own_value() {
        let token = new_session_token();
        assert_eq!(session_token_hash(token.value()), Some(token.hash()));
    }

    #[test]
    fn two_new_tokens_differ() {
        let (a, b) = (new_session_token(), new_session_token());
        assert_ne!(a.value(), b.value());
        assert_ne!(a.hash(), b.hash());
    }

    /// The hash is SHA-256 of the 32 decoded bytes: 43 `A`s are 32 zero
    /// bytes, whose SHA-256 is the published constant below.
    #[test]
    fn the_hash_is_sha256_of_the_decoded_bytes() {
        let zeros = "A".repeat(43);
        let expected = "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925";
        let hash = session_token_hash(&zeros).expect("a well-formed token");
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, expected);
    }

    /// Whatever is not a token as `new_session_token` spells it is refused
    /// before any query: the former cookie (a `sessions.id`), a hash in hex,
    /// padded or standard base64, a token one character short or long, and
    /// a non-canonical spelling of a valid one (the last character's spare
    /// bits set).
    #[test]
    fn anything_but_a_well_formed_token_has_no_hash() {
        let uuid = Uuid::new_v4().to_string();
        let hex = "66".repeat(32);
        let padded = format!("{}=", "A".repeat(43));
        let standard = format!("{}+A", "A".repeat(41));
        let short = "A".repeat(42);
        let long = "A".repeat(44);
        let non_canonical = format!("{}B", "A".repeat(42));
        for value in [
            "",
            uuid.as_str(),
            hex.as_str(),
            padded.as_str(),
            standard.as_str(),
            short.as_str(),
            long.as_str(),
            non_canonical.as_str(),
        ] {
            assert_eq!(session_token_hash(value), None, "{value:?}");
        }
    }

    // -- cookie name (#224) -------------------------------------------------

    #[test]
    fn a_secure_cookie_carries_the_host_prefix() {
        assert_eq!(session_cookie_name(true), "__Host-session_id");
    }

    /// A browser rejects a `__Host-` cookie without `Secure`: over plain
    /// HTTP the bare name stays, or logging in would set nothing.
    #[test]
    fn an_insecure_cookie_keeps_the_bare_name() {
        assert_eq!(session_cookie_name(false), "session_id");
    }

    /// What the browser checks before it accepts a `__Host-` cookie:
    /// `Secure`, `Path=/`, no `Domain`. On the cookie that logs in and on
    /// the one that logs out, or the removal would be dropped and the
    /// session cookie left in place.
    #[test]
    fn both_secure_cookies_meet_the_host_prefix_requirements() {
        let token = new_session_token();
        for cookie in [
            build_session_cookie(&token, true),
            expired_session_cookie(true),
        ] {
            assert_eq!(cookie.name(), "__Host-session_id");
            assert_eq!(cookie.secure(), Some(true));
            assert_eq!(cookie.path(), Some("/"));
            assert_eq!(cookie.domain(), None);
        }
    }

    #[test]
    fn insecure_cookies_keep_the_bare_name_and_no_secure_flag() {
        let token = new_session_token();
        for cookie in [
            build_session_cookie(&token, false),
            expired_session_cookie(false),
        ] {
            assert_eq!(cookie.name(), "session_id");
            assert_ne!(cookie.secure(), Some(true));
            assert_eq!(cookie.path(), Some("/"));
        }
    }

    /// #222: the cookie carries the token, never the hash the table keeps.
    #[test]
    fn the_session_cookie_carries_the_token() {
        let token = new_session_token();
        let cookie = build_session_cookie(&token, true);
        assert_eq!(cookie.value(), token.value());
        assert_eq!(cookie.http_only(), Some(true));
    }

    #[test]
    fn the_logout_cookie_removes_the_session_cookie() {
        let cookie = expired_session_cookie(true);
        assert_eq!(cookie.value(), "");
        assert_eq!(cookie.max_age(), Some(cookie::time::Duration::ZERO));
    }

    // -- session_access (#289) -------------------------------------------

    #[test]
    fn a_full_session_of_an_active_account_opens_everything() {
        assert_eq!(
            session_access(false, false, false, true),
            SessionAccess::Full
        );
    }

    #[test]
    fn a_restricted_session_of_a_deactivated_account_opens_only_its_page() {
        assert_eq!(
            session_access(true, true, false, true),
            SessionAccess::Deactivated
        );
    }

    /// Deactivation revokes every session it finds; one that survived
    /// (the row left untouched) still opens nothing.
    #[test]
    fn a_full_session_of_a_deactivated_account_is_refused() {
        assert_eq!(
            session_access(false, true, false, true),
            SessionAccess::Refused
        );
    }

    /// Once the superadmin reactivates the account, the restricted session
    /// does not turn into a full one: the holder logs in again.
    #[test]
    fn a_restricted_session_of_a_reactivated_account_is_refused() {
        assert_eq!(
            session_access(true, false, false, true),
            SessionAccess::Refused
        );
    }

    #[test]
    fn no_session_of_a_purged_account_opens_anything() {
        for (restricted, deactivated) in
            [(false, false), (true, true), (false, true), (true, false)]
        {
            for age_declared in [true, false] {
                assert_eq!(
                    session_access(restricted, deactivated, true, age_declared),
                    SessionAccess::Refused,
                    "restricted={restricted} deactivated={deactivated} age_declared={age_declared}"
                );
            }
        }
    }

    // -- age declaration (#318) -------------------------------------------

    /// An account with no age declaration on file — opened through Google,
    /// or before #137 — gets the declaration page and nothing else.
    #[test]
    fn a_full_session_of_an_account_without_age_declaration_opens_only_the_declaration() {
        assert_eq!(
            session_access(false, false, false, false),
            SessionAccess::AgeUndeclared
        );
    }

    /// Deactivation outranks the missing declaration: the restricted session
    /// still opens the deactivated-account page, and the full session of a
    /// deactivated account still opens nothing.
    #[test]
    fn deactivation_outranks_a_missing_age_declaration() {
        assert_eq!(
            session_access(true, true, false, false),
            SessionAccess::Deactivated
        );
        assert_eq!(
            session_access(false, true, false, false),
            SessionAccess::Refused
        );
        assert_eq!(
            session_access(true, false, false, false),
            SessionAccess::Refused
        );
    }

    /// Liveness comes first here too: a revoked, expired or idle session of
    /// an account without declaration opens nothing, not even the page.
    #[test]
    fn a_dead_session_of_an_account_without_age_declaration_is_refused() {
        let now = at("2026-09-19T12:00:00Z");
        let undeclared = || SessionRow {
            age_declared: false,
            ..live(now)
        };
        assert_eq!(
            session_state(now, &undeclared()),
            SessionAccess::AgeUndeclared
        );
        for dead in [
            SessionRow {
                revoked: true,
                ..undeclared()
            },
            SessionRow {
                expires_at: at("2026-09-19T11:59:59Z"),
                ..undeclared()
            },
            SessionRow {
                last_seen_at: at("2026-09-12T11:59:59Z"),
                ..undeclared()
            },
        ] {
            assert_eq!(session_state(now, &dead), SessionAccess::Refused);
        }
    }

    // -- session_state (#221) ---------------------------------------------

    /// A live full session, the baseline the cases below each break once.
    fn live(now: DateTime<Utc>) -> SessionRow {
        SessionRow {
            revoked: false,
            expires_at: now + Duration::days(1),
            last_seen_at: now,
            restricted: false,
            deactivated: false,
            deleted: false,
            age_declared: true,
        }
    }

    #[test]
    fn a_live_full_session_opens_everything() {
        let now = at("2026-09-19T12:00:00Z");
        assert_eq!(session_state(now, &live(now)), SessionAccess::Full);
    }

    /// Logout, password change and reset, and deactivation all set
    /// `revoked_at`.
    #[test]
    fn a_revoked_session_is_refused() {
        let now = at("2026-09-19T12:00:00Z");
        let row = SessionRow {
            revoked: true,
            ..live(now)
        };
        assert_eq!(session_state(now, &row), SessionAccess::Refused);
    }

    #[test]
    fn a_session_past_its_absolute_lifetime_is_refused() {
        let now = at("2026-09-19T12:00:00Z");
        let row = SessionRow {
            expires_at: at("2026-09-19T11:59:59Z"),
            ..live(now)
        };
        assert_eq!(session_state(now, &row), SessionAccess::Refused);
        let row = SessionRow {
            expires_at: now,
            ..live(now)
        };
        assert_eq!(session_state(now, &row), SessionAccess::Full);
    }

    #[test]
    fn an_idle_session_is_refused() {
        let now = at("2026-09-19T12:00:00Z");
        let row = SessionRow {
            last_seen_at: at("2026-09-12T11:59:59Z"),
            ..live(now)
        };
        assert_eq!(session_state(now, &row), SessionAccess::Refused);
    }

    #[test]
    fn a_session_of_a_deleted_account_is_refused() {
        let now = at("2026-09-19T12:00:00Z");
        let row = SessionRow {
            deleted: true,
            ..live(now)
        };
        assert_eq!(session_state(now, &row), SessionAccess::Refused);
    }

    /// Liveness comes first: a revoked restricted session opens nothing,
    /// not even the deactivated-account page.
    #[test]
    fn a_revoked_restricted_session_is_refused() {
        let now = at("2026-09-19T12:00:00Z");
        let row = SessionRow {
            restricted: true,
            deactivated: true,
            ..live(now)
        };
        assert_eq!(session_state(now, &row), SessionAccess::Deactivated);
        let row = SessionRow {
            revoked: true,
            ..row
        };
        assert_eq!(session_state(now, &row), SessionAccess::Refused);
    }

    #[test]
    fn a_session_used_within_the_idle_timeout_is_not_idle() {
        let now = at("2026-09-19T12:00:00Z");
        assert!(!is_idle(now, now));
        assert!(!is_idle(now, at("2026-09-13T12:00:00Z")));
        assert!(!is_idle(now, at("2026-09-12T12:00:01Z")));
    }

    #[test]
    fn a_session_idle_for_exactly_the_timeout_is_still_valid() {
        assert!(!is_idle(
            at("2026-09-19T12:00:00Z"),
            at("2026-09-12T12:00:00Z")
        ));
    }

    #[test]
    fn a_session_idle_past_the_timeout_is_idle() {
        let now = at("2026-09-19T12:00:00Z");
        assert!(is_idle(now, at("2026-09-12T11:59:59Z")));
        assert!(is_idle(now, at("2026-08-20T12:00:00Z")));
    }

    /// The durations arbitrated for #195 (2026-09-19), which the privacy
    /// policy and the register state: changing one means updating both.
    #[test]
    fn the_idle_timeout_is_seven_days_within_thirty() {
        assert_eq!(SESSION_IDLE_TIMEOUT_DAYS, 7);
        assert_eq!(SESSION_TTL_DAYS, 30);
    }

    #[test]
    fn last_seen_is_not_rewritten_within_the_refresh_interval() {
        let now = at("2026-09-19T12:00:00Z");
        assert!(!last_seen_needs_refresh(now, now));
        assert!(!last_seen_needs_refresh(now, at("2026-09-19T11:30:00Z")));
        assert!(!last_seen_needs_refresh(now, at("2026-09-19T11:00:00Z")));
    }

    #[test]
    fn last_seen_is_rewritten_past_the_refresh_interval() {
        let now = at("2026-09-19T12:00:00Z");
        assert!(last_seen_needs_refresh(now, at("2026-09-19T10:59:59Z")));
        assert!(last_seen_needs_refresh(now, at("2026-09-18T12:00:00Z")));
    }

    /// A `last_seen_at` ahead of the api's clock (the database's `now()`
    /// set it) neither refuses the session nor gets rewritten.
    #[test]
    fn a_last_seen_in_the_future_is_neither_idle_nor_stale() {
        let now = at("2026-09-19T12:00:00Z");
        let later = at("2026-09-19T12:05:00Z");
        assert!(!is_idle(now, later));
        assert!(!last_seen_needs_refresh(now, later));
    }

    /// `load_session` closes a superadmin session's admin access in the
    /// statement that refreshes `last_seen_at` (#339): a session idle for
    /// the admin routes must always be due for that refresh.
    #[test]
    fn a_session_idle_for_the_admin_routes_is_due_for_a_refresh() {
        let now = at("2026-10-03T12:00:00Z");
        let just_idle = now
            - Duration::hours(crate::user_admin::SUPERADMIN_IDLE_TIMEOUT_HOURS)
            - Duration::seconds(1);
        assert!(crate::user_admin::superadmin_session_is_idle(
            now, just_idle
        ));
        assert!(last_seen_needs_refresh(now, just_idle));
    }

    // -- active_sessions (#225) --------------------------------------------

    fn listed(id: u128, now: DateTime<Utc>, last_seen_minutes_ago: i64) -> ListedSession {
        ListedSession {
            id: Uuid::from_u128(id),
            created_at: now - Duration::days(2),
            last_seen_at: now - Duration::minutes(last_seen_minutes_ago),
            expires_at: now + Duration::days(28),
            revoked: false,
            restricted: false,
        }
    }

    #[test]
    fn the_current_session_is_listed_and_marked() {
        let now = at("2026-10-02T12:00:00Z");
        let listed = active_sessions(now, Uuid::from_u128(1), vec![listed(1, now, 0)]);
        assert_eq!(
            listed,
            vec![ActiveSession {
                id: Uuid::from_u128(1),
                created_at: now - Duration::days(2),
                last_seen_at: now,
                current: true,
            }]
        );
    }

    /// Current first whatever its last use, then the most recently used.
    #[test]
    fn the_current_session_comes_first_then_the_most_recently_used() {
        let now = at("2026-10-02T12:00:00Z");
        let rows = vec![
            listed(1, now, 300),
            listed(2, now, 120),
            listed(3, now, 600),
            listed(4, now, 5),
        ];
        let ids: Vec<_> = active_sessions(now, Uuid::from_u128(3), rows)
            .into_iter()
            .map(|s| (s.id.as_u128(), s.current))
            .collect();
        assert_eq!(ids, vec![(3, true), (4, false), (2, false), (1, false)]);
    }

    /// What `AuthUser` would refuse is not offered for revocation: a
    /// revoked, expired or idle session, and a restricted one (#289),
    /// which an active account's holder cannot use anyway.
    #[test]
    fn a_session_no_request_would_accept_is_left_out() {
        let now = at("2026-10-02T12:00:00Z");
        let revoked = ListedSession {
            revoked: true,
            ..listed(2, now, 1)
        };
        let expired = ListedSession {
            expires_at: now - Duration::seconds(1),
            ..listed(3, now, 1)
        };
        let idle = ListedSession {
            last_seen_at: now - Duration::days(7) - Duration::seconds(1),
            ..listed(4, now, 1)
        };
        let restricted = ListedSession {
            restricted: true,
            ..listed(5, now, 1)
        };
        let still_live = ListedSession {
            last_seen_at: now - Duration::days(7),
            ..listed(6, now, 1)
        };
        let ids: Vec<_> = active_sessions(
            now,
            Uuid::from_u128(1),
            vec![
                listed(1, now, 0),
                revoked,
                expired,
                idle,
                restricted,
                still_live,
            ],
        )
        .into_iter()
        .map(|s| s.id.as_u128())
        .collect();
        assert_eq!(ids, vec![1, 6]);
    }

    #[test]
    fn no_session_but_the_current_one_is_marked_current() {
        let now = at("2026-10-02T12:00:00Z");
        let listed = active_sessions(
            now,
            Uuid::from_u128(9),
            vec![listed(1, now, 0), listed(2, now, 0)],
        );
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|s| !s.current));
    }

    /// `src` with its comments — doc comments included — blanked out, so a
    /// comment explaining what not to write does not trip a textual check
    /// (#240). String, raw-string and char literals are copied as they are:
    /// a `//` inside one opens no comment, and a quote inside a char literal
    /// opens no string. A lifetime (`'a`) is not a char literal.
    fn code_only(src: &str) -> String {
        let c: Vec<char> = src.chars().collect();
        let starts = |k: usize| k == 0 || !(c[k - 1].is_alphanumeric() || c[k - 1] == '_');
        let mut out = String::with_capacity(src.len());
        let mut i = 0;
        while i < c.len() {
            let at = |k: usize| c.get(k).copied();
            match c[i] {
                '/' if at(i + 1) == Some('/') => {
                    while i < c.len() && c[i] != '\n' {
                        i += 1;
                    }
                }
                '/' if at(i + 1) == Some('*') => {
                    let mut depth = 0;
                    while i < c.len() {
                        if c[i] == '/' && at(i + 1) == Some('*') {
                            depth += 1;
                            i += 2;
                        } else if c[i] == '*' && at(i + 1) == Some('/') {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        } else {
                            i += 1;
                        }
                    }
                    out.push(' ');
                }
                '"' => {
                    let start = i;
                    i += 1;
                    while i < c.len() && c[i] != '"' {
                        i += if c[i] == '\\' { 2 } else { 1 };
                    }
                    i = (i + 1).min(c.len());
                    out.extend(&c[start..i]);
                }
                // `r"…"`, `r#"…"#`, `br"…"`: an `r` that starts a token.
                'r' if starts(i) || (c[i - 1] == 'b' && starts(i - 1)) => {
                    let mut j = i + 1;
                    while at(j) == Some('#') {
                        j += 1;
                    }
                    if at(j) != Some('"') {
                        out.push('r');
                        i += 1;
                        continue;
                    }
                    let hashes = j - i - 1;
                    let start = i;
                    i = j + 1;
                    while i < c.len()
                        && !(c[i] == '"' && (1..=hashes).all(|h| at(i + h) == Some('#')))
                    {
                        i += 1;
                    }
                    i = (i + 1 + hashes).min(c.len());
                    out.extend(&c[start..i]);
                }
                '\'' => {
                    let start = i;
                    if at(i + 1) == Some('\\') {
                        i += 3;
                        while i < c.len() && c[i] != '\'' {
                            i += 1;
                        }
                        i = (i + 1).min(c.len());
                    } else if at(i + 2) == Some('\'') {
                        i += 3;
                    } else {
                        i += 1;
                    }
                    out.extend(&c[start..i]);
                }
                ch => {
                    out.push(ch);
                    i += 1;
                }
            }
        }
        out
    }

    fn names_the_session_cookie(src: &str) -> bool {
        let code = code_only(src);
        code.contains("SESSION_COOKIE_NAME")
            || code.contains("\"session_id")
            || code.contains("\"__Host-session_id")
            || code.contains("session_id=")
    }

    #[test]
    fn line_and_doc_comments_are_dropped() {
        for src in [
            "let a = 1; // jar.get(SESSION_COOKIE_NAME)\nlet b = 2;",
            "/// Never `.get(SESSION_COOKIE_NAME)` here.\nfn f() {}",
            "//! Reads \"session_id\" nowhere.\nfn f() {}",
            "// strip_prefix(\"session_id=\")\nfn f() {}",
        ] {
            let code = code_only(src);
            assert!(!names_the_session_cookie(src), "{src:?} -> {code:?}");
            assert!(code.contains("fn f() {}") || code.contains("let b = 2;"));
        }
    }

    #[test]
    fn block_comments_are_dropped_nested_ones_included() {
        let src = "let a = /* x /* SESSION_COOKIE_NAME */ \"session_id */ 1;\n/** session_id= */ fn f() {}";
        let code = code_only(src);
        assert!(!names_the_session_cookie(src), "{code:?}");
        assert!(code.contains("let a =") && code.contains(" 1;") && code.contains("fn f() {}"));
    }

    #[test]
    fn code_is_still_seen_after_stripping() {
        for src in [
            "let k = SESSION_COOKIE_NAME;",
            "jar.get(\"session_id\")",
            "h.strip_prefix(\"session_id=\")",
            "jar.get(\"__Host-session_id\")",
            "let k = HOST_SESSION_COOKIE_NAME;",
        ] {
            assert!(names_the_session_cookie(src), "{src:?}");
        }
    }

    /// A `//` or `/*` inside a literal does not open a comment, so the code
    /// that follows it on the line is still read.
    #[test]
    fn comment_markers_inside_literals_are_not_comments() {
        for src in [
            "let u = \"http://x\"; let k = SESSION_COOKIE_NAME;",
            "let u = \"a /* b\"; let k = SESSION_COOKIE_NAME; // */",
            "let u = \"a\\\"// b\"; let k = SESSION_COOKIE_NAME;",
            "let u = \"a\\\\\"; let k = SESSION_COOKIE_NAME; // \"",
            "let u = r#\"a \" // b\"#; let k = SESSION_COOKIE_NAME;",
            "let u = br\"//\"; let k = SESSION_COOKIE_NAME;",
            "let c = '/'; let d = '/'; let k = SESSION_COOKIE_NAME;",
        ] {
            assert!(
                names_the_session_cookie(src),
                "{src:?} -> {:?}",
                code_only(src)
            );
        }
    }

    /// A quote in a char literal does not open a string, and a lifetime is
    /// not a char literal: the comment after either is still dropped.
    #[test]
    fn char_literals_and_lifetimes_do_not_hide_a_comment() {
        for src in [
            "let q = '\"'; // SESSION_COOKIE_NAME",
            "let q = b'\"'; // SESSION_COOKIE_NAME",
            "let q = '\\''; let r = '\"'; // SESSION_COOKIE_NAME",
            "let q = '\\u{22}'; // \"session_id",
            "fn f<'a>(x: &'a str) -> &'a str { x } // SESSION_COOKIE_NAME \"",
        ] {
            assert!(
                !names_the_session_cookie(src),
                "{src:?} -> {:?}",
                code_only(src)
            );
        }
        assert!(names_the_session_cookie(
            "fn f<'a>(x: &'a str) -> &'a str { SESSION_COOKIE_NAME }"
        ));
    }

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// #196: `load_session` is the only place that reads the session cookie.
    /// Every extractor that needs a caller — `AuthUser`, `AnySession`,
    /// `DeactivatedSession`, and `SuperAdminUser` through `AuthUser` — goes
    /// through it, so a change to session validity (inactivity timeout,
    /// hashed token, MFA step) lands once. A second copy of that check once
    /// lived in `user_admin`; this fails if one reappears anywhere under
    /// `src/`.
    ///
    /// #239: matching `.get(<key>)` on the key's spelling let any alias
    /// through (`const K: &str = SESSION_COOKIE_NAME; jar.get(K)`), as well
    /// as any expression wrapped around the constant. No file outside this
    /// one has a reason to name the cookie at all — building and expiring it
    /// live here too — so the check is on the constant's name and on its
    /// literal value, wherever they appear: an import, an alias or a read
    /// all trip it. The value is matched as the source text `"session_id`,
    /// the start of a string literal, or as `session_id=`. Parsing the
    /// `Cookie` header by hand (`strip_prefix("session_id=")`) trips it too.
    ///
    /// #240: comments are left out of the search (`code_only`), so a doc
    /// comment explaining why not to read the cookie does not trip it.
    /// String literals are kept: that is where the value lives.
    ///
    /// This check is textual, so it catches slips, not deliberate
    /// workarounds. It does not see a key spelled another way in the source
    /// (`"session\x5fid"`, `concat!`, `format!`). It does not see a name
    /// read back from a jar that `set_session_cookie` filled
    /// (`jar.list()`). Nor does it see a cookie picked out by its value.
    #[test]
    fn only_this_module_reads_the_session_cookie() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let this_file = src.join("auth").join("session.rs");
        let mut files = Vec::new();
        rust_sources(&src, &mut files);
        assert!(files.contains(&this_file), "walked the wrong tree");

        let readers: Vec<_> = files
            .iter()
            .filter(|f| **f != this_file)
            .filter(|f| names_the_session_cookie(&std::fs::read_to_string(f).unwrap()))
            .collect();
        assert!(
            readers.is_empty(),
            "session cookie named outside auth/session.rs, go through AuthUser: {readers:?}"
        );
    }
}
