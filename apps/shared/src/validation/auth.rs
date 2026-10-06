//! Pure, dependency-free input validation for the Auth endpoints and forms,
//! shared between `apps/api` (server-side enforcement, 422 error codes) and
//! `apps/web` (client-side inline feedback before hitting the API).
//!
//! These functions deliberately import nothing from sqlx/axum: they operate
//! on borrowed strings and return a small `&'static str` error code on
//! failure. `apps/api`'s handler layer maps them to a 422 with that code;
//! `apps/web` maps the same codes to French form messages — one
//! implementation, so client and server can never disagree on what is
//! valid. Written test-first per CLAUDE.md's TDD process (originally in
//! `apps/api/src/auth/validation.rs`, moved here verbatim once `apps/web`
//! needed it too).

/// Minimum accepted password length (characters). 12 rather than NIST's
/// 8-char floor because ANSSI/CNIL (the French guidance this product's
/// audience falls under) recommend ≥12 for standard accounts. Length and
/// the common-password blocklist are the whole policy: per NIST SP 800-63B
/// and OWASP, no character-composition rules (mandatory digits, upper/lower
/// case, symbols) are imposed — they push users toward predictable
/// patterns without adding real entropy.
pub const MIN_PASSWORD_LEN: usize = 12;

/// Maximum accepted password length (characters). Not a strength concern —
/// a DoS guard so the Argon2 hashing cost stays bounded. NIST requires
/// supporting at least 64; 128 leaves ample room for passphrases.
pub const MAX_PASSWORD_LEN: usize = 128;

/// Common passwords (≥12 chars, lowercased, deduped) from the SecLists
/// `Pwdb_top-100000.txt` breach corpus. NIST SP 800-63B requires checking
/// candidate passwords against a blocklist of commonly-used/compromised
/// values; ~1400 entries, so a linear scan is fine.
const COMMON_PASSWORDS: &str = include_str!("common_passwords.txt");

/// `password_too_short` under `MIN_PASSWORD_LEN` characters,
/// `password_too_long` over `MAX_PASSWORD_LEN`, `password_too_common` when
/// the lowercased password appears in the blocklist. Counts Unicode scalar
/// values, not bytes, so a 12-emoji password isn't rejected as "too short".
pub fn validate_password(password: &str) -> Result<(), &'static str> {
    let len = password.chars().count();
    if len < MIN_PASSWORD_LEN {
        return Err("password_too_short");
    }
    if len > MAX_PASSWORD_LEN {
        return Err("password_too_long");
    }
    let lowered = password.to_lowercase();
    if COMMON_PASSWORDS.lines().any(|common| common == lowered) {
        return Err("password_too_common");
    }
    Ok(())
}

/// `invalid_email` unless the value has the basic `x@y.z` shape: a non-empty
/// local part, a single `@`, and a domain containing a dot with non-empty
/// labels on both sides of it. No surrounding whitespace allowed. This is a
/// shape check, not RFC 5322 compliance.
pub fn validate_email(email: &str) -> Result<(), &'static str> {
    if email != email.trim() {
        return Err("invalid_email");
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("invalid_email");
    };
    if local.is_empty() {
        return Err("invalid_email");
    }
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return Err("invalid_email");
    };
    if host.is_empty() || tld.is_empty() {
        return Err("invalid_email");
    }
    Ok(())
}

/// `display_name_required` when the name is empty after trimming surrounding
/// whitespace.
pub fn validate_display_name(display_name: &str) -> Result<(), &'static str> {
    if display_name.trim().is_empty() {
        return Err("display_name_required");
    }
    Ok(())
}

/// Minimum age (years) to open an account, declared at registration. 15 is the
/// French threshold of art. 8 GDPR as transposed by art. 45 of the loi
/// Informatique et Libertés: below it, a processing based on consent needs the
/// holder of parental authority. The service takes the simple route the
/// arbitrage settled on — under-15s are not accepted at all, so there is no
/// parental-consent path to build and nothing to ask a parent for. The same
/// number is written in the CGU (`docs/terms-of-service.md`), and a test in
/// `validation::rgpd` pins the two together so they cannot drift.
pub const MINIMUM_AGE_YEARS: u32 = 15;

/// `age_declaration_required` unless the person declared being at least
/// `MINIMUM_AGE_YEARS` old.
///
/// The declaration is a plain yes/no: no birth date is asked for and none is
/// stored. Collecting a date to derive a single boolean would be more personal
/// data than the check needs, which art. 5.1.c GDPR (minimisation) forbids —
/// and a declared date is no more verifiable than a ticked box. What is kept
/// is that the declaration was made, and when (`users.age_declared_at`).
pub fn validate_age_declaration(declares_minimum_age: bool) -> Result<(), &'static str> {
    if !declares_minimum_age {
        return Err("age_declaration_required");
    }
    Ok(())
}

/// The version of the text in `docs/terms-of-service.md`, as the document
/// states it on its `Version en vigueur :` line (#319): the date,
/// `YYYY-MM-DD`, from which that text applies. It changes only with a
/// substantial modification — a typo fixed moves the « Dernière mise à
/// jour » date, not this — because a new version is what members are told
/// about. A test in `validation::rgpd` pins it to the document, so the two
/// cannot drift.
///
/// It is the version in force until [`TERMS_ANNOUNCED`]'s date: which
/// version an acceptance records or a page names is
/// [`terms_in_force_on`]'s answer for the day, not this constant (#367).
///
/// What is kept per account is the version accepted and when
/// (`users.terms_accepted_version` / `terms_accepted_at`).
pub const TERMS_VERSION: &str = "2026-10-04";

/// A version of the CGU published before it applies (#367). The CGU promise
/// that a substantial modification is announced before it takes effect:
/// until its date, the text in `docs/terms-of-service.md` stays the one in
/// force and is served as such, and this one is served beside it, as
/// announced. From its date — read in Europe/Paris on every request, never
/// fixed at startup — it is the version in force: acceptances record it, and
/// members who accepted an earlier one are told.
///
/// Announcing a version: write its text in
/// `docs/terms-of-service-announced.md`, its `Version en vigueur :` line
/// stating the date it applies from, and set [`TERMS_ANNOUNCED`] to that
/// date and that file. Once the date has passed, a later release folds it
/// in: the text replaces `docs/terms-of-service.md`, [`TERMS_VERSION`]
/// takes its date and [`TERMS_ANNOUNCED`] goes back to `None`. Tests in
/// `validation::rgpd` pin the date to the text, and after [`TERMS_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnouncedTerms {
    /// The date, `YYYY-MM-DD`, from which this text applies — its version.
    pub version: &'static str,
    /// The text, in the same markdown as `docs/terms-of-service.md`.
    pub markdown: &'static str,
}

/// The version announced and its text, if any — see [`AnnouncedTerms`].
pub const TERMS_ANNOUNCED: Option<AnnouncedTerms> = None;

/// The civil day `now` falls on in Europe/Paris, the fixed timezone the
/// service speaks (F3) and the one a CGU version's date is read in (#367).
/// The caller passes the clock: nothing in this crate reads it.
pub fn paris_day(now: chrono::DateTime<chrono::Utc>) -> chrono::NaiveDate {
    now.with_timezone(&chrono_tz::Europe::Paris).date_naive()
}

/// The version in force on `today`: `announced` from its date on, `current`
/// before — and `current` when `announced` is not a date, which no day
/// makes apply (#367).
pub fn terms_version_in_force<'a>(
    today: chrono::NaiveDate,
    current: &'a str,
    announced: Option<&'a str>,
) -> &'a str {
    match announced {
        Some(version) if version_date(version).is_some_and(|from| from <= today) => version,
        _ => current,
    }
}

/// The version announced and not yet in force on `today` — the one the CGU
/// page and the members' notice name, with its date (#367).
pub fn terms_version_announced(today: chrono::NaiveDate, announced: Option<&str>) -> Option<&str> {
    announced.filter(|version| version_date(version).is_some_and(|from| today < from))
}

/// [`terms_version_in_force`] for this release's texts: [`TERMS_VERSION`]
/// and [`TERMS_ANNOUNCED`].
pub fn terms_in_force_on(today: chrono::NaiveDate) -> &'static str {
    terms_version_in_force(today, TERMS_VERSION, TERMS_ANNOUNCED.map(|a| a.version))
}

/// The announced text not yet in force on `today`, for this release.
pub fn terms_announced_on(today: chrono::NaiveDate) -> Option<AnnouncedTerms> {
    TERMS_ANNOUNCED.filter(|a| terms_version_announced(today, Some(a.version)).is_some())
}

/// Whether having accepted `accepted` covers the version `in_force` (#367):
/// the same version, or a later one. A later one is what a rollback shows —
/// a binary released before a version applied, put back after a member
/// accepted that version — and that member is not to be asked to accept the
/// earlier text again. Versions are dates; two that are not both dates cover
/// each other only when equal.
pub fn terms_acceptance_covers(accepted: &str, in_force: &str) -> bool {
    match (version_date(accepted), version_date(in_force)) {
        (Some(accepted), Some(in_force)) => accepted >= in_force,
        _ => accepted == in_force,
    }
}

/// A version's date, `None` when it is not one. Only `YYYY-MM-DD` written in
/// full counts: chrono alone also reads `2026-12-1`, whose order as text —
/// the order the api compares stored versions in — is not its order as a
/// date.
fn version_date(version: &str) -> Option<chrono::NaiveDate> {
    let well_formed = version.len() == 10
        && version.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !well_formed {
        return None;
    }
    chrono::NaiveDate::parse_from_str(version, "%Y-%m-%d").ok()
}

/// Whether `version` is a CGU version as stored and compared: a date written
/// `YYYY-MM-DD` in full (#367).
pub fn is_terms_version(version: &str) -> bool {
    version_date(version).is_some()
}

/// `terms_acceptance_required` unless the person ticked the box accepting
/// the CGU (#319) — at registration, or on the page a session without
/// acceptance on file is held at.
pub fn validate_terms_acceptance(accepts_terms: bool) -> Result<(), &'static str> {
    if !accepts_terms {
        return Err("terms_acceptance_required");
    }
    Ok(())
}

/// Whether an account must be told the CGU changed (#319): it accepted a
/// version that does not cover `in_force` ([`terms_acceptance_covers`]). No
/// acceptance at all is not an update to announce — that account is held at
/// the acceptance page instead.
pub fn terms_update_pending(accepted_version: Option<&str>, in_force: &str) -> bool {
    accepted_version.is_some_and(|accepted| !terms_acceptance_covers(accepted, in_force))
}

/// A CGU version as the pages show it, `04/10/2026` for `2026-10-04` — the
/// day format of `validation::rgpd::format_rgpd_date`. A version that is not
/// a date is shown as written rather than hidden.
pub fn format_terms_version(version: &str) -> String {
    chrono::NaiveDate::parse_from_str(version, "%Y-%m-%d")
        .map(|day| day.format("%d/%m/%Y").to_string())
        .unwrap_or_else(|_| version.to_string())
}

/// Length of a bearer token as the api spells it (#222, #335): 32 random
/// bytes in unpadded base64url are 43 characters.
pub const BEARER_TOKEN_LEN: usize = 43;

/// Whether `token` is spelled like a bearer token the api hands out — a
/// session cookie, an invitation, a password reset or an email verification
/// link (#335): exactly [`BEARER_TOKEN_LEN`] characters of the base64url
/// alphabet. apps/web checks this before putting a token in an api URL, so
/// a mangled link gets its "invalid link" page without a request. The api
/// decides the rest (canonical spelling, existence).
pub fn is_bearer_token(token: &str) -> bool {
    token.len() == BEARER_TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_password ---------------------------------------------

    #[test]
    fn password_shorter_than_minimum_is_rejected() {
        assert_eq!(validate_password("short"), Err("password_too_short"));
        assert_eq!(validate_password("12345678"), Err("password_too_short"));
        assert_eq!(validate_password("elevenchars"), Err("password_too_short"));
    }

    #[test]
    fn empty_password_is_rejected() {
        assert_eq!(validate_password(""), Err("password_too_short"));
    }

    #[test]
    fn password_at_or_above_minimum_is_accepted() {
        assert_eq!(validate_password("twelve-chars"), Ok(()));
        assert_eq!(validate_password("a-long-passphrase"), Ok(()));
        assert_eq!(validate_password("correct horse battery staple"), Ok(()));
    }

    #[test]
    fn password_length_counts_chars_not_bytes() {
        // 12 multi-byte characters => 12 scalar values, accepted.
        assert_eq!(validate_password("éééééééééééé"), Ok(()));
    }

    #[test]
    fn password_over_maximum_is_rejected() {
        assert_eq!(validate_password(&"a".repeat(MAX_PASSWORD_LEN)), Ok(()));
        assert_eq!(
            validate_password(&"a".repeat(MAX_PASSWORD_LEN + 1)),
            Err("password_too_long")
        );
        // Chars, not bytes: 128 multi-byte chars is still at the limit.
        assert_eq!(validate_password(&"é".repeat(MAX_PASSWORD_LEN)), Ok(()));
    }

    #[test]
    fn common_passwords_are_rejected() {
        assert_eq!(
            validate_password("password1234"),
            Err("password_too_common")
        );
        assert_eq!(
            validate_password("administrator"),
            Err("password_too_common")
        );
    }

    #[test]
    fn common_password_check_is_case_insensitive() {
        assert_eq!(
            validate_password("Password1234"),
            Err("password_too_common")
        );
        assert_eq!(
            validate_password("PASSWORD1234"),
            Err("password_too_common")
        );
    }

    #[test]
    fn uncommon_long_password_is_accepted_without_composition_rules() {
        // No character-class requirements: all-lowercase, no digit, no
        // symbol is fine as long as it's long enough and not common.
        assert_eq!(validate_password("blue houses drift slowly"), Ok(()));
        assert_eq!(validate_password("test-password-1234"), Ok(()));
    }

    // -- validate_email --------------------------------------------------

    #[test]
    fn valid_emails_are_accepted() {
        assert_eq!(validate_email("a@b.co"), Ok(()));
        assert_eq!(validate_email("user.name@example.test"), Ok(()));
        assert_eq!(validate_email("x@sub.domain.org"), Ok(()));
    }

    #[test]
    fn email_with_plus_tag_is_accepted() {
        assert_eq!(validate_email("alice+family@example.test"), Ok(()));
    }

    #[test]
    fn empty_or_whitespace_only_email_is_rejected() {
        assert_eq!(validate_email(""), Err("invalid_email"));
        assert_eq!(validate_email("   "), Err("invalid_email"));
    }

    #[test]
    fn emails_without_at_or_dot_are_rejected() {
        assert_eq!(validate_email("plainaddress"), Err("invalid_email"));
        assert_eq!(validate_email("no-at-sign.com"), Err("invalid_email"));
        assert_eq!(validate_email("no-tld@example"), Err("invalid_email"));
    }

    #[test]
    fn emails_with_empty_parts_are_rejected() {
        assert_eq!(validate_email("@example.test"), Err("invalid_email"));
        assert_eq!(validate_email("user@.com"), Err("invalid_email"));
        assert_eq!(validate_email("user@example."), Err("invalid_email"));
        assert_eq!(validate_email("a@@b.co"), Err("invalid_email"));
    }

    #[test]
    fn emails_with_surrounding_whitespace_are_rejected() {
        // Server-side semantics: the API stores and matches the exact
        // string, so the form must reject padded input rather than
        // silently trimming it.
        assert_eq!(validate_email(" a@b.co"), Err("invalid_email"));
        assert_eq!(validate_email("a@b.co "), Err("invalid_email"));
    }

    // -- validate_display_name --------------------------------------------

    #[test]
    fn empty_or_whitespace_display_name_is_rejected() {
        assert_eq!(validate_display_name(""), Err("display_name_required"));
        assert_eq!(
            validate_display_name("   \t\n"),
            Err("display_name_required")
        );
    }

    #[test]
    fn non_empty_display_name_is_accepted() {
        assert_eq!(validate_display_name("Alice"), Ok(()));
        assert_eq!(validate_display_name("  Bob  "), Ok(()));
    }

    // -- validate_age_declaration -----------------------------------------

    #[test]
    fn registering_without_declaring_the_minimum_age_is_rejected() {
        assert_eq!(
            validate_age_declaration(false),
            Err("age_declaration_required")
        );
    }

    #[test]
    fn a_declared_minimum_age_is_accepted() {
        assert_eq!(validate_age_declaration(true), Ok(()));
    }

    // -- terms acceptance (#319) ------------------------------------------

    #[test]
    fn unaccepted_terms_are_refused() {
        assert_eq!(
            validate_terms_acceptance(false),
            Err("terms_acceptance_required")
        );
    }

    #[test]
    fn accepted_terms_pass() {
        assert_eq!(validate_terms_acceptance(true), Ok(()));
    }

    #[test]
    fn an_older_accepted_version_is_an_update_to_announce() {
        assert!(terms_update_pending(Some("2026-10-04"), "2026-12-01"));
    }

    #[test]
    fn the_current_version_accepted_announces_nothing() {
        assert!(!terms_update_pending(Some("2026-12-01"), "2026-12-01"));
    }

    /// An account with no acceptance on file is held at the acceptance page;
    /// the notice is for members who accepted an earlier text.
    #[test]
    fn no_acceptance_on_file_is_not_an_update() {
        assert!(!terms_update_pending(None, "2026-12-01"));
    }

    // -- announced version and its date (#367) ----------------------------

    fn day(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn utc(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// The day changes at midnight in Paris, not in UTC: 22:30 UTC on
    /// 30 November is already 1 December there in winter (UTC+1), and
    /// 22:30 UTC on 30 June is 1 July in summer (UTC+2).
    #[test]
    fn the_day_is_read_in_paris() {
        assert_eq!(paris_day(utc("2026-11-30T22:59:59Z")), day("2026-11-30"));
        assert_eq!(paris_day(utc("2026-11-30T23:00:00Z")), day("2026-12-01"));
        assert_eq!(paris_day(utc("2026-06-30T21:59:59Z")), day("2026-06-30"));
        assert_eq!(paris_day(utc("2026-06-30T22:00:00Z")), day("2026-07-01"));
    }

    #[test]
    fn before_its_date_the_announced_version_is_not_in_force() {
        let today = day("2026-11-30");
        assert_eq!(
            terms_version_in_force(today, "2026-10-04", Some("2026-12-01")),
            "2026-10-04"
        );
        assert_eq!(
            terms_version_announced(today, Some("2026-12-01")),
            Some("2026-12-01")
        );
    }

    /// From its date on — the day itself included — the announced version
    /// is in force, and nothing is announced any more.
    #[test]
    fn from_its_date_the_announced_version_is_in_force() {
        for today in [day("2026-12-01"), day("2027-03-15")] {
            assert_eq!(
                terms_version_in_force(today, "2026-10-04", Some("2026-12-01")),
                "2026-12-01"
            );
            assert_eq!(terms_version_announced(today, Some("2026-12-01")), None);
        }
    }

    #[test]
    fn nothing_announced_leaves_the_current_version_in_force() {
        let today = day("2026-12-01");
        assert_eq!(
            terms_version_in_force(today, "2026-10-04", None),
            "2026-10-04"
        );
        assert_eq!(terms_version_announced(today, None), None);
    }

    /// A version that is not a date has no day to apply from: it is never
    /// in force, and it is not announced with a date it does not have.
    #[test]
    fn an_announced_version_that_is_not_a_date_never_applies() {
        let today = day("2999-01-01");
        assert_eq!(
            terms_version_in_force(today, "2026-10-04", Some("v2")),
            "2026-10-04"
        );
        assert_eq!(terms_version_announced(today, Some("v2")), None);
    }

    #[test]
    fn this_release_has_a_version_in_force_every_day() {
        let today = day("2026-10-06");
        let in_force = terms_in_force_on(today);
        assert!(in_force == TERMS_VERSION || Some(in_force) == TERMS_ANNOUNCED.map(|a| a.version));
        if let Some(announced) = terms_announced_on(today) {
            assert_ne!(announced.version, in_force);
        }
    }

    /// A version is a date written `YYYY-MM-DD` in full: chrono alone reads
    /// `2026-12-1` too, and the api compares versions as text, where
    /// `2026-12-1` sorts after `2026-12-01`'s successors. Not written in
    /// full, it is not a date: never in force, never announced.
    #[test]
    fn a_version_not_written_yyyy_mm_dd_in_full_is_not_a_date() {
        for version in [
            "2026-12-1",
            "2026-1-01",
            "26-12-01",
            "+2026-12-01",
            " 2026-12-01",
        ] {
            assert_eq!(version_date(version), None, "{version}");
            let today = day("2999-01-01");
            assert_eq!(
                terms_version_in_force(today, "2026-10-04", Some(version)),
                "2026-10-04",
                "{version}"
            );
            assert_eq!(
                terms_version_announced(today, Some(version)),
                None,
                "{version}"
            );
        }
        assert_eq!(version_date("2026-12-01"), Some(day("2026-12-01")));
        assert!(!terms_acceptance_covers("2026-12-9", "2026-12-10"));
    }

    // -- an acceptance covers its version and the earlier ones (#367) ------

    #[test]
    fn accepting_the_version_in_force_covers_it() {
        assert!(terms_acceptance_covers("2026-10-04", "2026-10-04"));
    }

    #[test]
    fn accepting_an_earlier_version_does_not_cover_a_later_one() {
        assert!(!terms_acceptance_covers("2026-10-04", "2026-12-01"));
    }

    /// The rollback case: a binary released before 2026-12-01 applied holds
    /// 2026-10-04 as the version in force. A member who already accepted
    /// 2026-12-01 under the newer binary is not asked again.
    #[test]
    fn accepting_a_later_version_covers_an_earlier_one() {
        assert!(terms_acceptance_covers("2026-12-01", "2026-10-04"));
        assert!(!terms_update_pending(Some("2026-12-01"), "2026-10-04"));
    }

    /// Not dates, nothing to order: only the same version covers.
    #[test]
    fn versions_that_are_not_dates_cover_only_themselves() {
        assert!(terms_acceptance_covers("v2", "v2"));
        assert!(!terms_acceptance_covers("v3", "v2"));
        assert!(!terms_acceptance_covers("2026-12-01", "v2"));
    }

    #[test]
    fn a_version_reads_as_a_french_day() {
        assert_eq!(format_terms_version("2026-10-04"), "04/10/2026");
    }

    #[test]
    fn a_version_that_is_not_a_date_reads_as_written() {
        assert_eq!(format_terms_version("v2"), "v2");
    }

    /// The version in force is a date: the notice and the CGU state it as
    /// one.
    #[test]
    fn the_version_in_force_is_a_date() {
        assert!(is_terms_version(TERMS_VERSION), "{TERMS_VERSION}");
    }

    // -- is_bearer_token (#335) -----------------------------------------

    #[test]
    fn a_token_of_43_base64url_characters_is_well_formed() {
        assert!(is_bearer_token(
            "Zq3_k9XvT2mQ8pLw4rYb7nHc1sDf6gJ0aEuIoVtBy5M"
        ));
        assert!(is_bearer_token(&"A".repeat(43)));
        assert!(is_bearer_token(&format!("{}-_", "a".repeat(41))));
    }

    /// The former format (a UUID), a hash in hex, padded or standard base64,
    /// one character short or long, and characters outside the alphabet —
    /// a `/` or a `?` would also change the URL the token is put in.
    #[test]
    fn anything_else_is_not_a_bearer_token() {
        for token in [
            String::new(),
            "b6f1a4c2-3d5e-4f60-9a71-8b2c3d4e5f60".to_string(),
            "66".repeat(32),
            format!("{}=", "A".repeat(42)),
            format!("{}+A", "A".repeat(41)),
            format!("{}/A", "A".repeat(41)),
            format!("{}?A", "A".repeat(41)),
            format!("{} A", "A".repeat(41)),
            "A".repeat(42),
            "A".repeat(44),
            format!("{}é", "A".repeat(41)),
        ] {
            assert!(!is_bearer_token(&token), "{token:?}");
        }
    }
}
