//! Pure validation for Auth forms, shared between `apps/web` (client-side
//! inline feedback) and, potentially, `apps/api`.
//!
//! Mirrors the *actual* constraints enforced by `apps/api`'s `register`/
//! `login`/`reset_password` handlers today (see `apps/api/src/auth/mod.rs`
//! and `apps/api/src/crypto.rs`): there is no minimum password length
//! enforced anywhere in the backend (no DB `CHECK` constraint on
//! `password_hash`, no length check in `hash_password`/`verify_password`),
//! so this module does not invent one. It only rejects what would
//! unconditionally fail server-side too: empty fields and structurally
//! invalid email addresses.

/// True if `email` looks like a syntactically valid address: exactly one
/// `@`, a non-empty local part, and a domain part containing at least one
/// `.` with non-empty labels on both sides. Deliberately not a full RFC
/// 5322 parser — just enough to catch obvious typos before hitting the API.
pub fn is_valid_email(email: &str) -> bool {
    let email = email.trim();
    if email.is_empty() {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if domain.contains('@') {
        return false;
    }
    let Some((domain_head, domain_tail)) = domain.rsplit_once('.') else {
        return false;
    };
    !domain_head.is_empty() && !domain_tail.is_empty()
}

/// True if `password` is non-empty. `apps/api` enforces no minimum length
/// today (verified: no length check in `crypto::hash_password`, no
/// `CHECK` constraint on `users.password_hash` in
/// `migrations/0001_users_auth_groups.sql`) — this stays honest about
/// that rather than inventing a client-side-only rule the backend would
/// happily accept a violation of.
pub fn is_valid_password(password: &str) -> bool {
    !password.is_empty()
}

/// True if `display_name` is non-empty once surrounding whitespace is
/// trimmed. `apps/api`'s `register` handler stores whatever is sent
/// (`display_name TEXT NOT NULL` per the migration, no length check), so
/// the only unconditionally-failing case client-side worth catching is
/// blank input.
pub fn is_valid_display_name(display_name: &str) -> bool {
    !display_name.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- is_valid_email -----------------------------------------------

    #[test]
    fn empty_email_is_invalid() {
        assert!(!is_valid_email(""));
    }

    #[test]
    fn whitespace_only_email_is_invalid() {
        assert!(!is_valid_email("   "));
    }

    #[test]
    fn email_missing_at_sign_is_invalid() {
        assert!(!is_valid_email("alice.example.test"));
    }

    #[test]
    fn email_missing_domain_dot_is_invalid() {
        assert!(!is_valid_email("alice@example"));
    }

    #[test]
    fn email_with_empty_local_part_is_invalid() {
        assert!(!is_valid_email("@example.test"));
    }

    #[test]
    fn email_with_double_at_is_invalid() {
        assert!(!is_valid_email("alice@@example.test"));
    }

    #[test]
    fn well_formed_email_is_valid() {
        assert!(is_valid_email("alice@example.test"));
    }

    #[test]
    fn well_formed_email_with_surrounding_whitespace_is_valid() {
        assert!(is_valid_email("  alice@example.test  "));
    }

    #[test]
    fn well_formed_email_with_plus_tag_is_valid() {
        assert!(is_valid_email("alice+family@example.test"));
    }

    // -- is_valid_password ---------------------------------------------

    #[test]
    fn empty_password_is_invalid() {
        assert!(!is_valid_password(""));
    }

    #[test]
    fn single_character_password_is_valid() {
        // apps/api enforces no minimum length today (see module docs) —
        // this deliberately does not invent one.
        assert!(is_valid_password("a"));
    }

    #[test]
    fn ordinary_password_is_valid() {
        assert!(is_valid_password("correct horse battery staple"));
    }

    // -- is_valid_display_name ------------------------------------------

    #[test]
    fn empty_display_name_is_invalid() {
        assert!(!is_valid_display_name(""));
    }

    #[test]
    fn whitespace_only_display_name_is_invalid() {
        assert!(!is_valid_display_name("   "));
    }

    #[test]
    fn ordinary_display_name_is_valid() {
        assert!(is_valid_display_name("Alice"));
    }
}
