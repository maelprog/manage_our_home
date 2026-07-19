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
}
