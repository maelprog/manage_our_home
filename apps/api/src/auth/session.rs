use axum::async_trait;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use chrono::{DateTime, Duration, Utc};
use cookie::{Cookie, SameSite};
use sqlx::{PgPool, Postgres, Transaction};
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::error::AppError;
use crate::AppState;

/// Private on purpose (#239): nothing outside this module may name the
/// cookie — see `only_this_module_reads_the_session_cookie`.
const SESSION_COOKIE_NAME: &str = "session_id";
/// Absolute lifetime of a session, fixed at creation in `expires_at` and in
/// the cookie's `max_age`, however much the session is used.
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
/// the account's `deactivated_at` / `deleted_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAccess {
    /// Every route `AuthUser` guards.
    Full,
    /// Only the routes `DeactivatedSession` guards.
    Deactivated,
    /// Nothing: 401.
    Refused,
}

fn session_access(restricted: bool, deactivated: bool, deleted: bool) -> SessionAccess {
    match (deleted, restricted, deactivated) {
        (false, false, false) => SessionAccess::Full,
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
}

/// The one verdict on a session (#221): the checks every session shares —
/// not revoked, within its absolute lifetime, not idle — then what it opens.
/// `load_session` asks it once per request, and the messagerie WebSocket
/// again on every recheck tick, so a revocation reaches both channels.
fn session_state(now: DateTime<Utc>, row: &SessionRow) -> SessionAccess {
    if row.revoked || row.expires_at < now || is_idle(now, row.last_seen_at) {
        return SessionAccess::Refused;
    }
    session_access(row.restricted, row.deactivated, row.deleted)
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
        .get(SESSION_COOKIE_NAME)
        .ok_or(AppError::Unauthorized)?;
    let session_id: Uuid = cookie.value().parse().map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query!(
        r#"
        SELECT s.id as session_id, s.expires_at, s.revoked_at, s.last_seen_at, s.restricted,
               u.id as user_id, u.email, u.display_name, u.email_verified,
               u.is_superadmin, u.deleted_at, u.deactivated_at, u.deletion_requested_at,
               (u.password_hash IS NOT NULL) as "has_password!"
        FROM sessions s
        JOIN users u ON u.id = s.user_id
        WHERE s.id = $1
        "#,
        session_id
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
        },
    );
    if access == SessionAccess::Refused {
        return Err(AppError::Unauthorized);
    }

    if last_seen_needs_refresh(now, row.last_seen_at) {
        sqlx::query!(
            "UPDATE sessions SET last_seen_at = now() WHERE id = $1",
            session_id
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
               u.deleted_at, u.deactivated_at
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
        },
    ) == SessionAccess::Full
}

/// A full session of an active account. A restricted session (#289) is
/// refused with 403 `account_deactivated` — its holder proved the
/// credentials, so the answer tells them nothing they do not know, and
/// `apps/web` sends them to the deactivated-account page on it. Every
/// other refusal is the bare 401.
#[async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = load_session(parts, state).await?;
        if session.access != SessionAccess::Full {
            return Err(AppError::AccountDeactivated);
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

/// Logs the caller in: the jar carries the session cookie on the response.
/// No function hands the `Cookie` value back to the caller (#239). The
/// cookie still sits in the jar the caller passes in, so its name can be
/// read back from it (`jar.list()`). That route stays open, and
/// `only_this_module_reads_the_session_cookie` does not see it.
pub fn set_session_cookie(cookies: &Cookies, session_id: Uuid, secure: bool) {
    cookies.add(build_session_cookie(session_id, secure));
}

/// Logs the caller out: the jar carries the removal of the session cookie.
pub fn clear_session_cookie(cookies: &Cookies) {
    cookies.add(expired_session_cookie());
}

fn build_session_cookie(session_id: Uuid, secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE_NAME, session_id.to_string()))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(cookie::time::Duration::days(SESSION_TTL_DAYS))
        .build()
}

fn expired_session_cookie() -> Cookie<'static> {
    let mut c = Cookie::build((SESSION_COOKIE_NAME, "")).path("/").build();
    c.make_removal();
    c
}

pub async fn create_session(pool: &PgPool, user_id: Uuid) -> Result<Uuid, sqlx::Error> {
    insert_session(pool, user_id, false).await
}

/// The session a correct login on a deactivated account opens (#289): same
/// lifetime and cookie as any other, but only [`DeactivatedSession`]
/// accepts it.
pub async fn create_restricted_session(pool: &PgPool, user_id: Uuid) -> Result<Uuid, sqlx::Error> {
    insert_session(pool, user_id, true).await
}

async fn insert_session(
    pool: &PgPool,
    user_id: Uuid,
    restricted: bool,
) -> Result<Uuid, sqlx::Error> {
    let expires_at = Utc::now() + Duration::days(SESSION_TTL_DAYS);
    let rec = sqlx::query!(
        r#"
        INSERT INTO sessions (user_id, expires_at, restricted)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
        user_id,
        expires_at,
        restricted
    )
    .fetch_one(pool)
    .await?;
    Ok(rec.id)
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

    // -- session_access (#289) -------------------------------------------

    #[test]
    fn a_full_session_of_an_active_account_opens_everything() {
        assert_eq!(session_access(false, false, false), SessionAccess::Full);
    }

    #[test]
    fn a_restricted_session_of_a_deactivated_account_opens_only_its_page() {
        assert_eq!(
            session_access(true, true, false),
            SessionAccess::Deactivated
        );
    }

    /// Deactivation revokes every session it finds; one that survived
    /// (the row left untouched) still opens nothing.
    #[test]
    fn a_full_session_of_a_deactivated_account_is_refused() {
        assert_eq!(session_access(false, true, false), SessionAccess::Refused);
    }

    /// Once the superadmin reactivates the account, the restricted session
    /// does not turn into a full one: the holder logs in again.
    #[test]
    fn a_restricted_session_of_a_reactivated_account_is_refused() {
        assert_eq!(session_access(true, false, false), SessionAccess::Refused);
    }

    #[test]
    fn no_session_of_a_purged_account_opens_anything() {
        for (restricted, deactivated) in
            [(false, false), (true, true), (false, true), (true, false)]
        {
            assert_eq!(
                session_access(restricted, deactivated, true),
                SessionAccess::Refused,
                "restricted={restricted} deactivated={deactivated}"
            );
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
