//! The cap on the "you already have an account" notice (#570).
//!
//! Since #460, `POST /auth/register` on a verified address mails its holder
//! that the account exists (`auth::register`). Anyone can type that address
//! into the form: without a cap, anyone could have it mailed to its holder
//! as often as they liked. The notice is now mailed at most [`RATE`] times
//! per address, counted in `rate_limit_log` (`rate_limit`) — every replica
//! against the same budget, the address stored as a keyed pseudonym.
//!
//! A registration past the cap, or one the cap cannot count, mails nothing
//! and answers exactly as the others: the cap must not tell anyone whether
//! the address has an account.

use std::time::Duration;

use sqlx::PgPool;

use crate::auth::throttle::Pseudonyms;
use crate::rate_limit::{SlidingLimiter, SlidingWindow};

/// `rate_limit_log.kind` of this cap, and the label of its pseudonyms.
pub const KIND: &str = "account_exists_notice";

/// At most 3 notices per address per 24 hours, sliding — "cap how often you
/// can generate this email at 3 per day" (arbitrated 2026-10-22, on #460).
pub const RATE: SlidingWindow = SlidingWindow {
    limit: 3,
    window: Duration::from_secs(24 * 60 * 60),
};

/// [`RATE`], counted per address in `rate_limit_log`.
#[derive(Debug, Clone)]
pub struct NoticeThrottle(SlidingLimiter);

impl NoticeThrottle {
    /// `db` is the limits' own pool (`rate_limit::POOL_SIZE`).
    pub fn new(db: PgPool, pseudonyms: Pseudonyms) -> Self {
        Self(SlidingLimiter::new(db, pseudonyms, KIND, RATE))
    }

    /// Whether a notice may go to `email` (normalized) now; counted when it
    /// may. An error counts nothing, and the caller sends nothing.
    pub async fn admit(&self, email: &str) -> Result<bool, sqlx::Error> {
        self.0.admit(email.as_bytes()).await
    }
}
