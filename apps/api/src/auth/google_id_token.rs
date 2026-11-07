//! Google sign-in from the native SDK (#669).
//!
//! Google refuses its consent screen inside an embedded WebView
//! (`403 disallowed_useragent`), so the Android app signs in with the
//! platform's Credential Manager instead. That hands the app an **ID
//! token**: a JWT signed by Google, whose `aud` is this server's web OAuth
//! client (`GOOGLE_CLIENT_ID`, the "server client ID" the app asks for).
//! The page inside the WebView posts it here, on the same origin
//! (`/api/*`), so the session cookie lands in the WebView directly.
//!
//! Two routes:
//!
//! - [`issue_nonce`] mints a single-use nonce, kept in the browser by a
//!   flow cookie (the shape `oauth_google` gives its `state`) and in
//!   `google_sign_in_nonces` by its SHA-256, for [`NONCE_TTL`]. The app
//!   passes it to Credential Manager, and Google writes it into the token.
//! - [`sign_in`] checks the token — RS256 signature against Google's JWKS
//!   ([`JwksCache`]), then the claims ([`validate_claims`]): issuer,
//!   audience, expiry, `email_verified`, and `nonce` equal to the cookie's —
//!   then spends the nonce, and signs the profile in exactly as `callback`
//!   does (`oauth_google::sign_in_profile`).
//!
//! The nonce is what ties a token to the browser that asked for it: a
//! token lifted from another device carries that device's nonce, which
//! this browser's cookie does not hold; and a token presented twice finds
//! its nonce already spent. Both routes count against one per-address
//! limit ([`RATE`]).

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;
use jsonwebtoken::jwk::{AlgorithmParameters, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tower_cookies::{Cookie, Cookies};

use crate::client_ip::ClientIp;
use crate::error::{AppError, AppResult};
use crate::rate_limit::{SlidingLimiter, SlidingWindow};
use crate::AppState;

use super::oauth_google::{flow_cookie, sign_in_profile, GoogleProfile, SignIn};
use super::throttle::{address_scope, Pseudonyms};
use super::token::{new_token, token_hash};

/// Why an ID token was refused. Each case has its own code, logged with the
/// refusal; the client only ever sees a bare 401.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Not a JWT, not RS256, no `kid`, or claims missing or mistyped.
    Malformed,
    /// No key of Google's JWKS has the token's `kid`, even after a refresh.
    UnknownKey,
    BadSignature,
    WrongIssuer,
    WrongAudience,
    Expired,
    /// The token's `nonce` is not the one this browser's cookie holds, or
    /// that nonce is spent, expired or was never issued.
    NonceMismatch,
    EmailNotVerified,
}

impl Rejection {
    pub fn code(self) -> &'static str {
        match self {
            Rejection::Malformed => "malformed_token",
            Rejection::UnknownKey => "unknown_key",
            Rejection::BadSignature => "bad_signature",
            Rejection::WrongIssuer => "wrong_issuer",
            Rejection::WrongAudience => "wrong_audience",
            Rejection::Expired => "expired",
            Rejection::NonceMismatch => "nonce_mismatch",
            Rejection::EmailNotVerified => "email_not_verified",
        }
    }
}

/// The two spellings of Google's issuer its ID tokens carry
/// (OpenID Connect documentation, "Validate an ID token").
pub const ALLOWED_ISSUERS: [&str; 2] = ["accounts.google.com", "https://accounts.google.com"];

/// How far past its `exp` a token is still accepted, in seconds: the clock
/// drift between Google's servers and this one.
pub const CLOCK_SKEW: u64 = 60;

/// Where Google publishes the keys its ID tokens are signed with.
pub const GOOGLE_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

/// How long a fetched key set is used before it is fetched again. Google
/// rotates its keys over days and announces a new one in the set before
/// signing with it.
pub const JWKS_TTL: Duration = Duration::from_secs(60 * 60);

/// The least time between two fetches of the key set. A token naming a
/// `kid` the set lacks triggers one refresh (a key rotated in since the
/// last fetch); within this interval, it is refused without one, so tokens
/// with made-up `kid`s cannot make this server hammer Google.
pub const JWKS_MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// The claims of a Google ID token this server reads. A token without one
/// of the required ones is [`Rejection::Malformed`].
#[derive(Debug, Clone, Deserialize)]
pub struct IdTokenClaims {
    pub iss: String,
    pub aud: String,
    pub exp: u64,
    pub sub: String,
    pub email: String,
    #[serde(default)]
    pub email_verified: bool,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// The `kid` of an RS256 token's header. Anything else — not a JWT, another
/// algorithm (`none`, or HMAC, where a public key would be taken for the
/// secret), no `kid` — is refused before any key is looked up.
fn key_id(token: &str) -> Result<String, Rejection> {
    let header = jsonwebtoken::decode_header(token).map_err(|_| Rejection::Malformed)?;
    if header.alg != Algorithm::RS256 {
        return Err(Rejection::Malformed);
    }
    header
        .kid
        .filter(|kid| !kid.is_empty())
        .ok_or(Rejection::Malformed)
}

/// Checks the claims of a token whose signature checked out, at `now`
/// (seconds since the epoch): Google's issuer, this server's `audience`, not
/// expired beyond [`CLOCK_SKEW`], the `expected_nonce` (the cookie's; none
/// or empty matches nothing), and an email Google verified.
fn validate_claims(
    claims: IdTokenClaims,
    audience: &str,
    expected_nonce: Option<&str>,
    now: u64,
) -> Result<GoogleProfile, Rejection> {
    if !ALLOWED_ISSUERS.contains(&claims.iss.as_str()) {
        return Err(Rejection::WrongIssuer);
    }
    if claims.aud != audience {
        return Err(Rejection::WrongAudience);
    }
    if now > claims.exp.saturating_add(CLOCK_SKEW) {
        return Err(Rejection::Expired);
    }
    match (expected_nonce, claims.nonce.as_deref()) {
        (Some(expected), Some(nonce)) if !expected.is_empty() && expected == nonce => {}
        _ => return Err(Rejection::NonceMismatch),
    }
    if !claims.email_verified {
        return Err(Rejection::EmailNotVerified);
    }
    Ok(GoogleProfile {
        sub: claims.sub,
        email: claims.email,
        name: claims.name,
    })
}

/// Whether the key set is fetched before looking a `kid` up: never fetched
/// yet; otherwise not within `min_interval` ([`JWKS_MIN_REFRESH_INTERVAL`])
/// of the last attempt, failed or not, and then only for a stale set or a
/// `kid` the set lacks.
fn should_refresh(
    last_attempt: Option<Instant>,
    fetched_at: Option<Instant>,
    kid_known: bool,
    min_interval: Duration,
    now: Instant,
) -> bool {
    let Some(last_attempt) = last_attempt else {
        return true;
    };
    if now.saturating_duration_since(last_attempt) < min_interval {
        return false;
    }
    !kid_known || fetched_at.is_none_or(|at| now.saturating_duration_since(at) >= JWKS_TTL)
}

#[derive(Default)]
struct CachedKeys {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
}

/// Google's signing keys by `kid`, fetched from `url` through
/// `outbound_http::oauth_client` (no redirect followed) and kept per
/// [`should_refresh`]. One lock: concurrent sign-ins that need a fetch
/// wait for the one fetch.
pub struct JwksCache {
    url: String,
    min_interval: Duration,
    cached: tokio::sync::Mutex<CachedKeys>,
}

impl JwksCache {
    pub fn new(url: impl Into<String>) -> Self {
        Self::with_min_interval(url, JWKS_MIN_REFRESH_INTERVAL)
    }

    /// [`JwksCache::new`] with another least time between two fetches, for
    /// tests that cannot wait out [`JWKS_MIN_REFRESH_INTERVAL`].
    pub fn with_min_interval(url: impl Into<String>, min_interval: Duration) -> Self {
        Self {
            url: url.into(),
            min_interval,
            cached: tokio::sync::Mutex::default(),
        }
    }

    /// The key named `kid`, fetching the set first if [`should_refresh`]
    /// says so. `Ok(None)`: no such key. A failed fetch falls back on the
    /// keys already held, and is an error only when they lack `kid`.
    pub async fn key(&self, kid: &str) -> anyhow::Result<Option<DecodingKey>> {
        let mut cached = self.cached.lock().await;
        let now = Instant::now();
        if should_refresh(
            cached.last_attempt,
            cached.fetched_at,
            cached.keys.contains_key(kid),
            self.min_interval,
            now,
        ) {
            cached.last_attempt = Some(now);
            match fetch_jwks(&self.url).await {
                Ok(keys) => {
                    cached.keys = keys;
                    cached.fetched_at = Some(now);
                }
                Err(e) if cached.keys.contains_key(kid) => {
                    tracing::warn!(error = ?e, "Google JWKS refresh failed, using the keys held");
                }
                Err(e) => return Err(e),
            }
        }
        Ok(cached.keys.get(kid).cloned())
    }
}

/// The RSA keys of the set at `url`, by `kid`; keys without one, or of
/// another type, are left out. A redirect is not followed (#389).
async fn fetch_jwks(url: &str) -> anyhow::Result<HashMap<String, DecodingKey>> {
    let response = crate::outbound_http::oauth_client().get(url).send().await?;
    if response.status().is_redirection() {
        anyhow::bail!("JWKS answered a redirect: {}", response.status());
    }
    let set: JwkSet = response.error_for_status()?.json().await?;
    Ok(set
        .keys
        .iter()
        .filter(|jwk| matches!(jwk.algorithm, AlgorithmParameters::RSA(_)))
        .filter_map(|jwk| {
            let kid = jwk.common.key_id.clone()?;
            Some((kid, DecodingKey::from_jwk(jwk).ok()?))
        })
        .collect())
}

/// Why [`IdTokenVerifier::verify`] did not return a profile.
#[derive(Debug)]
pub enum VerifyError {
    Rejected(Rejection),
    /// Google's key set could not be fetched: not the token's fault.
    Unavailable(anyhow::Error),
}

/// Checks Google ID tokens issued for `audience`, against the keys of a
/// [`JwksCache`].
pub struct IdTokenVerifier {
    audience: String,
    jwks: JwksCache,
}

impl IdTokenVerifier {
    /// `audience` is the web OAuth client ID (`GOOGLE_CLIENT_ID`);
    /// `jwks_url` is [`GOOGLE_JWKS_URL`] in production.
    pub fn new(audience: impl Into<String>, jwks_url: impl Into<String>) -> Self {
        Self {
            audience: audience.into(),
            jwks: JwksCache::new(jwks_url),
        }
    }

    /// The verified profile in `token`, whose nonce must be
    /// `expected_nonce`, at `now` (seconds since the epoch).
    pub async fn verify(
        &self,
        token: &str,
        expected_nonce: Option<&str>,
        now: u64,
    ) -> Result<GoogleProfile, VerifyError> {
        let kid = key_id(token).map_err(VerifyError::Rejected)?;
        let key = self
            .jwks
            .key(&kid)
            .await
            .map_err(VerifyError::Unavailable)?
            .ok_or(VerifyError::Rejected(Rejection::UnknownKey))?;
        // The signature only: every claim is checked by `validate_claims`,
        // which gives each refusal its own code.
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();
        let claims = jsonwebtoken::decode::<IdTokenClaims>(token, &key, &validation)
            .map_err(|e| {
                VerifyError::Rejected(match e.kind() {
                    jsonwebtoken::errors::ErrorKind::InvalidSignature => Rejection::BadSignature,
                    _ => Rejection::Malformed,
                })
            })?
            .claims;
        validate_claims(claims, &self.audience, expected_nonce, now).map_err(VerifyError::Rejected)
    }
}

/// The flow cookie holding the nonce [`issue_nonce`] minted.
pub const NONCE_COOKIE: &str = "google_id_token_nonce";

/// How long a nonce can be spent: the time to pick an account in
/// Credential Manager and post the token back.
pub const NONCE_TTL: Duration = Duration::from_secs(5 * 60);

/// `rate_limit_log.kind` of the limit on both routes, and the label of its
/// pseudonyms.
pub const KIND: &str = "google_sign_in";

/// At most 20 requests per client address per 10 minutes, sliding, both
/// routes together: ten sign-ins, a nonce and a token each.
pub const RATE: SlidingWindow = SlidingWindow {
    limit: 20,
    window: Duration::from_secs(10 * 60),
};

/// Connections of the limit's own pool, per replica, when
/// [`POOL_SIZE_VAR`] is unset. Both routes answer anyone without a session:
/// like the "account exists" notice's, their admissions wait on their
/// subject's lock in a pool of their own (#483).
pub const POOL_SIZE: u32 = 1;

/// Environment variable sizing that pool, per replica.
pub const POOL_SIZE_VAR: &str = "GOOGLE_SIGN_IN_POOL_SIZE";

/// [`RATE`], counted per client address in `rate_limit_log`.
#[derive(Debug, Clone)]
pub struct SignInThrottle(SlidingLimiter);

impl SignInThrottle {
    /// `db` is the limit's own pool ([`POOL_SIZE`] connections unless
    /// [`POOL_SIZE_VAR`] says otherwise).
    pub fn new(db: PgPool, pseudonyms: Pseudonyms) -> Self {
        Self(SlidingLimiter::new(db, pseudonyms, KIND, RATE))
    }

    /// Spends one request of `client`'s budget, counted on its scope (its
    /// /64 in IPv6, as the login lock counts). `Err(TooManyRequests)` past
    /// the budget; a database error counts nothing and lets nothing through.
    async fn admit(&self, client: std::net::IpAddr) -> AppResult<()> {
        let subject = address_scope(client).to_string();
        if self.0.admit(subject.as_bytes()).await? {
            Ok(())
        } else {
            Err(AppError::TooManyRequests)
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NonceResponse {
    /// To pass to Credential Manager (`GetGoogleIdOption.setNonce`); Google
    /// writes it into the ID token.
    pub nonce: String,
}

/// `POST /auth/google/nonce` (#669): a fresh nonce for one native sign-in,
/// also set in an HttpOnly flow cookie that expires with it.
#[utoipa::path(
    post,
    path = "/auth/google/nonce",
    tag = "auth",
    responses(
        (status = 200, description = "The nonce, with its flow cookie set", body = NonceResponse),
        (status = 429, description = "`too_many_attempts`: the address's budget is spent", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::PublicErrors,
    ),
)]
pub async fn issue_nonce(
    State(state): State<AppState>,
    ClientIp(client): ClientIp,
    cookies: Cookies,
) -> AppResult<Json<NonceResponse>> {
    state.google_sign_in_throttle.admit(client).await?;

    let nonce = new_token();
    let ttl_secs = NONCE_TTL.as_secs_f64();
    // The expired nonces of every browser go with each new one: the table
    // holds the last few minutes' only.
    sqlx::query!(
        "WITH expired AS (DELETE FROM google_sign_in_nonces WHERE expires_at <= now())
         INSERT INTO google_sign_in_nonces (nonce_hash, expires_at)
         VALUES ($1, now() + make_interval(secs => $2))",
        &nonce.hash()[..],
        ttl_secs
    )
    .execute(&state.db)
    .await?;

    let mut cookie = flow_cookie(NONCE_COOKIE, nonce.value().to_owned(), state.secure_cookies);
    cookie.set_max_age(cookie::time::Duration::seconds(NONCE_TTL.as_secs() as i64));
    cookies.add(cookie);

    Ok(Json(NonceResponse {
        nonce: nonce.value().to_owned(),
    }))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct IdTokenRequest {
    /// The ID token Credential Manager returned.
    pub id_token: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SignInResponse {
    /// The path to send the browser to: the app once signed in, the login
    /// page with the reason otherwise — where `callback` redirects.
    pub redirect: String,
    /// Set when nothing was signed in to: the code the login page has copy
    /// for (`?google=`, `apps/web` `google_refusal`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<&'static str>,
}

/// `POST /auth/google/id-token` (#669): verifies the ID token against the
/// nonce of this browser's flow cookie, spends the nonce, then signs the
/// profile in as `callback` does, session cookie included. Every refusal of
/// the token is the same bare 401; its reason goes to the log only.
#[utoipa::path(
    post,
    path = "/auth/google/id-token",
    tag = "auth",
    request_body = IdTokenRequest,
    responses(
        (status = 200, description = "Signed in (session cookie set), or refused with a reason the login page shows", body = SignInResponse),
        (status = 401, description = "`unauthorized`: the token or its nonce did not check out, or the account is purged", body = manage_our_home_shared::dto::auth::ErrorResponse),
        (status = 429, description = "`too_many_attempts`: the address's budget is spent", body = manage_our_home_shared::dto::auth::ErrorResponse),
        crate::api_docs::PublicErrors,
    ),
)]
pub async fn sign_in(
    State(state): State<AppState>,
    ClientIp(client): ClientIp,
    cookies: Cookies,
    body: Result<Json<IdTokenRequest>, JsonRejection>,
) -> AppResult<Json<SignInResponse>> {
    state.google_sign_in_throttle.admit(client).await?;

    let expected_nonce = cookies.get(NONCE_COOKIE).map(|c| c.value().to_owned());
    // Single-use, spent or not; the removal carries the cookie's `Path=/`.
    cookies.remove(Cookie::build((NONCE_COOKIE, "")).path("/").into());

    let Json(body) = body?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| AppError::Internal(e.into()))?
        .as_secs();
    let profile = match state
        .google_id_token
        .verify(&body.id_token, expected_nonce.as_deref(), now)
        .await
    {
        Ok(profile) => profile,
        Err(VerifyError::Rejected(reason)) => return Err(refuse(reason)),
        Err(VerifyError::Unavailable(e)) => return Err(AppError::Internal(e)),
    };

    // The token carries this browser's nonce: spend it, once.
    let hash = expected_nonce
        .as_deref()
        .and_then(token_hash)
        .ok_or_else(|| refuse(Rejection::NonceMismatch))?;
    let spent = sqlx::query_scalar!(
        r#"DELETE FROM google_sign_in_nonces
            WHERE nonce_hash = $1 AND expires_at > now()
            RETURNING 1 AS "spent!""#,
        &hash[..]
    )
    .fetch_optional(&state.db)
    .await?;
    if spent.is_none() {
        return Err(refuse(Rejection::NonceMismatch));
    }

    Ok(Json(
        match sign_in_profile(&state, &cookies, &profile, None).await? {
            SignIn::Session => SignInResponse {
                redirect: "/".into(),
                refusal: None,
            },
            SignIn::Refused(reason) => SignInResponse {
                redirect: format!("/login?google={reason}"),
                refusal: Some(reason),
            },
        },
    ))
}

/// The bare 401 of a refused token, its reason logged (never the token).
fn refuse(reason: Rejection) -> AppError {
    tracing::info!(reason = reason.code(), "Google ID token refused");
    AppError::Unauthorized
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    const AUD: &str = "web-client.apps.googleusercontent.com";
    const NONCE: &str = "the-nonce";
    const NOW: u64 = 1_800_000_000;

    fn claims() -> IdTokenClaims {
        IdTokenClaims {
            iss: "https://accounts.google.com".into(),
            aud: AUD.into(),
            exp: NOW + 3600,
            sub: "google-sub".into(),
            email: "someone@example.test".into(),
            email_verified: true,
            nonce: Some(NONCE.into()),
            name: Some("Someone".into()),
        }
    }

    fn check(claims: IdTokenClaims) -> Result<GoogleProfile, Rejection> {
        validate_claims(claims, AUD, Some(NONCE), NOW)
    }

    #[test]
    fn valid_claims_give_the_profile() {
        let profile = check(claims()).expect("accepted");
        assert_eq!(profile.sub, "google-sub");
        assert_eq!(profile.email, "someone@example.test");
        assert_eq!(profile.name.as_deref(), Some("Someone"));
    }

    #[test]
    fn both_google_issuers_are_accepted_and_no_other() {
        for iss in ALLOWED_ISSUERS {
            assert!(check(IdTokenClaims {
                iss: iss.into(),
                ..claims()
            })
            .is_ok());
        }
        for iss in [
            "",
            "https://accounts.google.com/",
            "http://accounts.google.com",
            "https://evil.example",
            "ACCOUNTS.GOOGLE.COM",
        ] {
            assert_eq!(
                check(IdTokenClaims {
                    iss: iss.into(),
                    ..claims()
                })
                .unwrap_err(),
                Rejection::WrongIssuer,
                "{iss:?}"
            );
        }
    }

    #[test]
    fn another_audience_is_refused() {
        for aud in [
            "",
            "android-client.apps.googleusercontent.com",
            "web-client",
        ] {
            assert_eq!(
                check(IdTokenClaims {
                    aud: aud.into(),
                    ..claims()
                })
                .unwrap_err(),
                Rejection::WrongAudience,
                "{aud:?}"
            );
        }
    }

    #[test]
    fn expiry_tolerates_a_bounded_clock_skew() {
        assert!(check(IdTokenClaims {
            exp: NOW - CLOCK_SKEW,
            ..claims()
        })
        .is_ok());
        assert_eq!(
            check(IdTokenClaims {
                exp: NOW - CLOCK_SKEW - 1,
                ..claims()
            })
            .unwrap_err(),
            Rejection::Expired
        );
        assert_eq!(
            check(IdTokenClaims { exp: 0, ..claims() }).unwrap_err(),
            Rejection::Expired
        );
    }

    #[test]
    fn the_nonce_must_be_the_expected_one() {
        assert_eq!(
            check(IdTokenClaims {
                nonce: Some("another-nonce".into()),
                ..claims()
            })
            .unwrap_err(),
            Rejection::NonceMismatch
        );
        assert_eq!(
            check(IdTokenClaims {
                nonce: None,
                ..claims()
            })
            .unwrap_err(),
            Rejection::NonceMismatch
        );
    }

    #[test]
    fn without_an_expected_nonce_nothing_matches() {
        assert_eq!(
            validate_claims(claims(), AUD, None, NOW).unwrap_err(),
            Rejection::NonceMismatch
        );
        assert_eq!(
            validate_claims(
                IdTokenClaims {
                    nonce: Some(String::new()),
                    ..claims()
                },
                AUD,
                Some(""),
                NOW
            )
            .unwrap_err(),
            Rejection::NonceMismatch,
            "two empty nonces matching prove nothing"
        );
    }

    #[test]
    fn an_unverified_email_is_refused() {
        assert_eq!(
            check(IdTokenClaims {
                email_verified: false,
                ..claims()
            })
            .unwrap_err(),
            Rejection::EmailNotVerified
        );
    }

    #[test]
    fn every_rejection_has_its_own_code() {
        let all = [
            Rejection::Malformed,
            Rejection::UnknownKey,
            Rejection::BadSignature,
            Rejection::WrongIssuer,
            Rejection::WrongAudience,
            Rejection::Expired,
            Rejection::NonceMismatch,
            Rejection::EmailNotVerified,
        ];
        let codes: std::collections::BTreeSet<_> = all.iter().map(|r| r.code()).collect();
        assert_eq!(codes.len(), all.len());
        assert!(codes.iter().all(|c| !c.is_empty()));
        assert_eq!(Rejection::Malformed.code(), "malformed_token");
        assert_eq!(Rejection::WrongIssuer.code(), "wrong_issuer");
        assert_eq!(Rejection::WrongAudience.code(), "wrong_audience");
        assert_eq!(Rejection::Expired.code(), "expired");
        assert_eq!(Rejection::NonceMismatch.code(), "nonce_mismatch");
        assert_eq!(Rejection::EmailNotVerified.code(), "email_not_verified");
    }

    fn b64(json: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    }

    #[test]
    fn the_key_id_comes_from_an_rs256_header() {
        let token = format!("{}.{}.sig", b64(r#"{"alg":"RS256","kid":"k1"}"#), b64("{}"));
        assert_eq!(key_id(&token).unwrap(), "k1");
    }

    #[test]
    fn a_malformed_token_is_refused_before_any_key_is_looked_up() {
        let payload = b64("{}");
        for token in [
            String::new(),
            "not-a-jwt".into(),
            "a.b".into(),
            format!("{}.{payload}.sig", b64("not json")),
            format!("{}.{payload}.sig", "%%%"),
            // No `kid`: no key to pick.
            format!("{}.{payload}.sig", b64(r#"{"alg":"RS256"}"#)),
            // Google signs with RS256 only: `none`, HMAC (which a public
            // key would be mistaken for a secret of) and others are refused.
            format!("{}.{payload}.", b64(r#"{"alg":"none","kid":"k1"}"#)),
            format!("{}.{payload}.sig", b64(r#"{"alg":"HS256","kid":"k1"}"#)),
            format!("{}.{payload}.sig", b64(r#"{"alg":"RS512","kid":"k1"}"#)),
        ] {
            assert_eq!(
                key_id(&token).unwrap_err(),
                Rejection::Malformed,
                "{token:?}"
            );
        }
    }

    fn refresh(
        last_attempt: Option<Instant>,
        fetched_at: Option<Instant>,
        kid_known: bool,
        now: Instant,
    ) -> bool {
        should_refresh(
            last_attempt,
            fetched_at,
            kid_known,
            JWKS_MIN_REFRESH_INTERVAL,
            now,
        )
    }

    #[test]
    fn the_key_set_is_fetched_when_never_fetched() {
        assert!(refresh(None, None, false, Instant::now()));
    }

    #[test]
    fn a_known_key_in_a_fresh_set_needs_no_fetch() {
        let t0 = Instant::now();
        let now = t0 + JWKS_TTL - Duration::from_secs(1);
        assert!(!refresh(Some(t0), Some(t0), true, now));
    }

    #[test]
    fn a_stale_set_is_fetched_again() {
        let t0 = Instant::now();
        assert!(refresh(Some(t0), Some(t0), true, t0 + JWKS_TTL));
    }

    #[test]
    fn an_unknown_key_refreshes_once_per_interval() {
        let t0 = Instant::now();
        assert!(!refresh(
            Some(t0),
            Some(t0),
            false,
            t0 + JWKS_MIN_REFRESH_INTERVAL - Duration::from_millis(1)
        ));
        assert!(refresh(
            Some(t0),
            Some(t0),
            false,
            t0 + JWKS_MIN_REFRESH_INTERVAL
        ));
    }

    /// A failed fetch counts as an attempt: Google down does not turn every
    /// sign-in into a fetch, stale set or not.
    #[test]
    fn a_failed_fetch_holds_the_next_one_back_for_the_interval() {
        let t0 = Instant::now();
        let failed = t0 + JWKS_TTL;
        assert!(!refresh(
            Some(failed),
            Some(t0),
            true,
            failed + Duration::from_secs(1)
        ));
        assert!(!refresh(
            Some(failed),
            None,
            false,
            failed + Duration::from_secs(1)
        ));
        assert!(refresh(
            Some(failed),
            Some(t0),
            true,
            failed + JWKS_MIN_REFRESH_INTERVAL
        ));
    }

    // The key set itself, served from a local listener with the test keys
    // of `tests/fixtures/google_id_token` (made for these tests only).

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const JWK_A: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_id_token/jwk_a.json"
    ));
    const JWK_B: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_id_token/jwk_b.json"
    ));
    const KEY_A: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_id_token/key_a.der"
    ));
    const KEY_B: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_id_token/key_b.der"
    ));

    fn key_set(jwks: &[&str]) -> String {
        format!(r#"{{"keys":[{}]}}"#, jwks.join(","))
    }

    /// Answers every request with the current `body` as JSON, counting the
    /// requests.
    async fn jwks_peer(body: Arc<Mutex<String>>) -> (String, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/certs", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let body = body.lock().unwrap().clone();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
        });
        (url, hits)
    }

    #[tokio::test]
    async fn an_unknown_kid_triggers_a_single_refresh_then_a_refusal() {
        let (url, hits) = jwks_peer(Arc::new(Mutex::new(key_set(&[JWK_A])))).await;
        let cache = JwksCache::with_min_interval(url, Duration::ZERO);
        assert!(cache.key("test-key-a").await.unwrap().is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        assert!(cache.key("test-key-a").await.unwrap().is_some());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a known key fetches nothing"
        );

        assert!(cache.key("made-up").await.unwrap().is_none());
        assert_eq!(hits.load(Ordering::SeqCst), 2, "one refresh, then refused");
    }

    #[tokio::test]
    async fn within_the_interval_an_unknown_kid_is_refused_without_a_fetch() {
        let (url, hits) = jwks_peer(Arc::new(Mutex::new(key_set(&[JWK_A])))).await;
        let cache = JwksCache::new(url);
        assert!(cache.key("test-key-a").await.unwrap().is_some());
        for _ in 0..5 {
            assert!(cache.key("made-up").await.unwrap().is_none());
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_key_rotated_in_is_found_by_the_refresh() {
        let body = Arc::new(Mutex::new(key_set(&[JWK_A])));
        let (url, hits) = jwks_peer(body.clone()).await;
        let cache = JwksCache::with_min_interval(url, Duration::ZERO);
        assert!(cache.key("test-key-a").await.unwrap().is_some());
        *body.lock().unwrap() = key_set(&[JWK_A, JWK_B]);
        assert!(cache.key("test-key-b").await.unwrap().is_some());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_unreachable_key_set_is_an_error_not_a_refusal() {
        let cache = JwksCache::new("http://127.0.0.1:1/certs");
        assert!(cache.key("test-key-a").await.is_err());
    }

    fn signed(key: &[u8], kid: &str, claims: serde_json::Value) -> String {
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some(kid.into());
        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_rsa_der(key),
        )
        .unwrap()
    }

    fn token_claims() -> serde_json::Value {
        serde_json::json!({
            "iss": "https://accounts.google.com",
            "aud": AUD,
            "exp": NOW + 3600,
            "sub": "google-sub",
            "email": "someone@example.test",
            "email_verified": true,
            "nonce": NONCE,
        })
    }

    async fn verifier() -> IdTokenVerifier {
        let (url, _) = jwks_peer(Arc::new(Mutex::new(key_set(&[JWK_A, JWK_B])))).await;
        IdTokenVerifier::new(AUD, url)
    }

    #[tokio::test]
    async fn a_token_signed_by_a_published_key_is_verified() {
        let token = signed(KEY_A, "test-key-a", token_claims());
        let profile = verifier()
            .await
            .verify(&token, Some(NONCE), NOW)
            .await
            .unwrap();
        assert_eq!(profile.sub, "google-sub");
    }

    #[tokio::test]
    async fn a_signature_by_another_key_than_the_kid_names_is_refused() {
        let token = signed(KEY_B, "test-key-a", token_claims());
        assert!(matches!(
            verifier().await.verify(&token, Some(NONCE), NOW).await,
            Err(VerifyError::Rejected(Rejection::BadSignature))
        ));
    }

    #[tokio::test]
    async fn a_tampered_payload_is_refused() {
        let token = signed(KEY_A, "test-key-a", token_claims());
        let mut parts: Vec<&str> = token.split('.').collect();
        let mut forged = token_claims();
        forged["email"] = "victim@example.test".into();
        let forged = b64(&forged.to_string());
        parts[1] = &forged;
        assert!(matches!(
            verifier()
                .await
                .verify(&parts.join("."), Some(NONCE), NOW)
                .await,
            Err(VerifyError::Rejected(Rejection::BadSignature))
        ));
    }

    #[tokio::test]
    async fn claims_missing_from_a_signed_token_make_it_malformed() {
        let mut claims = token_claims();
        claims.as_object_mut().unwrap().remove("email");
        let token = signed(KEY_A, "test-key-a", claims);
        assert!(matches!(
            verifier().await.verify(&token, Some(NONCE), NOW).await,
            Err(VerifyError::Rejected(Rejection::Malformed))
        ));
    }

    #[tokio::test]
    async fn the_claims_of_a_signed_token_are_still_checked() {
        let token = signed(KEY_A, "test-key-a", token_claims());
        assert!(matches!(
            verifier().await.verify(&token, Some("other"), NOW).await,
            Err(VerifyError::Rejected(Rejection::NonceMismatch))
        ));
    }
}
