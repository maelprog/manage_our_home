//! Pure, dependency-light logic for the Google Calendar import screens (front
//! epic F11, issue #52), shared by `apps/web`'s SSR pages. Written test-first
//! per `.claude/CLAUDE.md`'s TDD process. Everything the screens need beyond
//! rendering lives here so the UI and the fixed backend
//! (`apps/api/src/google_calendar/`) can never drift:
//!
//! - `validate_import_form` mirrors `imports.rs`'s `label_required` /
//!   `feed_url_required` / `feed_url_must_be_http_or_https` guards, in the same
//!   order, so those never round-trip. It deliberately returns nothing but the
//!   verdict: the feed URL is a credential and this module never holds, copies
//!   or formats it.
//! - `can_configure` mirrors `apps/api/src/google_calendar/mod.rs::can_configure`
//!   — the admin/owner bar on creating and deleting a connection, stricter than
//!   the "any member" bar the rest of Agenda uses. A gate mirror in the same
//!   spirit as F9's `can_view_admin`: it decides which controls render, the
//!   backend stays the authority.
//! - `format_last_imported` renders a UTC instant in **Europe/Paris**, the fixed
//!   v1 display timezone (F3's `DISPLAY_TZ`), with an explicit "jamais importé"
//!   for a connection whose feed has never been pulled.
//! - `import_run_summary` turns `ImportRunResponse`'s three counters into the
//!   French sentence the post-import banner shows.
//! - `import_deleted_notice` does the same for the post-delete banner, whose
//!   two branches (#55) depend on whether the caller asked for the imported
//!   events to go with the connection.

use chrono::{DateTime, Utc};
use chrono_tz::Europe::Paris;

/// Why a connect-a-calendar form was rejected. Ordered to match the backend's
/// check order (`create_calendar_import` → `validate_feed_url`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormError {
    LabelRequired,
    FeedUrlRequired,
    FeedUrlMustBeHttpOrHttps,
}

/// Mirrors `apps/api/src/google_calendar/imports.rs`: a non-empty `label` after
/// trimming, then a non-empty `feed_url` whose scheme is `http://` or
/// `https://`. Only `Ok(())` is returned on success — the caller already holds
/// the raw form values and this keeps the credential from being cloned into an
/// extra place on its way to the API.
///
/// `http://` is accepted alongside `https://` because the backend accepts it
/// (see `validate_feed_url`'s doc comment: a loopback feed can then be used
/// without TLS, which is what the E2E fixture server relies on).
pub fn validate_import_form(label: &str, feed_url: &str) -> Result<(), ImportFormError> {
    if label.trim().is_empty() {
        return Err(ImportFormError::LabelRequired);
    }
    let feed_url = feed_url.trim();
    if feed_url.is_empty() {
        return Err(ImportFormError::FeedUrlRequired);
    }
    if !feed_url.starts_with("https://") && !feed_url.starts_with("http://") {
        return Err(ImportFormError::FeedUrlMustBeHttpOrHttps);
    }
    Ok(())
}

/// Mirror of `apps/api/src/google_calendar/mod.rs::can_configure`: only an
/// owner or admin may connect or remove a calendar, a stricter bar than the
/// rest of Agenda because the feed URL is a bearer credential for a member's
/// Google account rather than household data. Listing connections and
/// triggering an on-demand import stay open to any member and so have no mirror
/// here. The backend still 403s a forged request.
pub fn can_configure(role: &str) -> bool {
    role == "owner" || role == "admin"
}

/// The "importé pour la dernière fois le …" cell for a connection, in
/// Europe/Paris (F3's fixed v1 display timezone) — or `jamais importé` when
/// `last_imported_at` is still `NULL`. v1 is pull-on-demand only, so a brand-new
/// connection legitimately sits in that state until someone runs an import; the
/// table says so rather than leaving the cell blank.
pub fn format_last_imported(last_imported_at: Option<DateTime<Utc>>) -> String {
    match last_imported_at {
        Some(dt) => dt
            .with_timezone(&Paris)
            .format("%d/%m/%Y à %H:%M")
            .to_string(),
        None => "jamais importé".to_string(),
    }
}

/// French sentence for one import run's counters, e.g.
/// `3 événements importés, 1 mis à jour, 12 inchangés`. Zero and one take the
/// singular (French agreement); `mis à jour` is invariable in the masculine
/// plural, so only the other two terms carry the `s`.
pub fn import_run_summary(imported: usize, updated: usize, skipped: usize) -> String {
    let s = |n: usize| if n > 1 { "s" } else { "" };
    format!(
        "{imported} événement{es} importé{is}, {updated} mis à jour, {skipped} inchangé{ss}",
        es = s(imported),
        is = s(imported),
        ss = s(skipped),
    )
}

/// The post-delete banner, for the two branches of "retirer la connexion"
/// (#55). `deleted_events` is `None` when the events were kept — the v1
/// default, and still the default the confirmation ships with — and
/// `Some(n)` when the caller asked for them to go too.
///
/// The two branches read differently on purpose: the kept branch has to
/// state the non-obvious survival of the events (the intuitive reading is
/// the opposite, see `imports.rs`'s confirmation copy), while the deleted
/// branch has to name a number, because "N événements ont disparu de
/// l'agenda" is not something a user should have to count for themselves.
pub fn import_deleted_notice(deleted_events: Option<usize>) -> String {
    match deleted_events {
        None => {
            "Agenda Google retiré. Les événements déjà importés restent dans l'agenda.".to_string()
        }
        // Not "ainsi que 0 événement" — asking to delete the events of a
        // connection that never imported anything is a no-op, not a failure.
        Some(0) => "Agenda Google retiré. Aucun événement importé n'était à supprimer.".to_string(),
        Some(n) => format!(
            "Agenda Google retiré, ainsi que {n} événement{s} importé{s}.",
            s = if n > 1 { "s" } else { "" },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    // -- validate_import_form ------------------------------------------------

    #[test]
    fn a_complete_form_is_accepted() {
        assert_eq!(
            validate_import_form(
                "Agenda de Marie",
                "https://calendar.google.com/calendar/ical/x/private-abc/basic.ics"
            ),
            Ok(())
        );
    }

    #[test]
    fn a_blank_label_is_rejected() {
        assert_eq!(
            validate_import_form("   ", "https://calendar.google.com/x.ics"),
            Err(ImportFormError::LabelRequired)
        );
    }

    #[test]
    fn a_blank_feed_url_is_rejected() {
        assert_eq!(
            validate_import_form("Agenda", "   "),
            Err(ImportFormError::FeedUrlRequired)
        );
    }

    #[test]
    fn the_label_is_checked_before_the_feed_url() {
        // Same order as apps/api's create_calendar_import, so the first error
        // the form surfaces is the one a forged request would hit.
        assert_eq!(
            validate_import_form("", ""),
            Err(ImportFormError::LabelRequired)
        );
    }

    #[test]
    fn a_non_http_scheme_is_rejected() {
        // Mirrors validate_feed_url: anything but http(s) would turn the import
        // endpoint into a local-file-read primitive.
        for url in [
            "file:///etc/passwd",
            "ftp://example.com/x.ics",
            "webcal://calendar.google.com/x.ics",
            "calendar.google.com/x.ics",
        ] {
            assert_eq!(
                validate_import_form("Agenda", url),
                Err(ImportFormError::FeedUrlMustBeHttpOrHttps),
                "{url} should be rejected"
            );
        }
    }

    #[test]
    fn a_plain_http_url_is_accepted() {
        // The backend deliberately accepts http:// alongside https:// so a
        // loopback feed can be used without TLS (validate_feed_url's doc
        // comment) — the E2E fixture server relies on it.
        assert_eq!(
            validate_import_form("Fixture", "http://127.0.0.1:9099/basic.ics"),
            Ok(())
        );
    }

    #[test]
    fn surrounding_whitespace_does_not_hide_a_valid_url() {
        // apps/api trims before checking the scheme; a pasted URL often carries
        // a trailing newline.
        assert_eq!(
            validate_import_form(" Agenda ", "  https://calendar.google.com/x.ics\n"),
            Ok(())
        );
    }

    // -- can_configure -------------------------------------------------------

    #[test]
    fn owner_and_admin_can_configure() {
        assert!(can_configure("owner"));
        assert!(can_configure("admin"));
    }

    #[test]
    fn a_standard_member_cannot_configure() {
        assert!(!can_configure("standard"));
    }

    #[test]
    fn an_unknown_role_cannot_configure() {
        assert!(!can_configure(""));
        assert!(!can_configure("Owner"));
    }

    // -- format_last_imported ------------------------------------------------

    #[test]
    fn a_never_imported_connection_says_so() {
        assert_eq!(format_last_imported(None), "jamais importé");
    }

    #[test]
    fn formats_the_last_run_in_paris_summer_time() {
        // 2026-07-24 12:05 UTC is 14:05 in Paris (CEST, UTC+2).
        assert_eq!(
            format_last_imported(Some(at(2026, 7, 24, 12, 5))),
            "24/07/2026 à 14:05"
        );
    }

    #[test]
    fn formats_the_last_run_in_paris_winter_time() {
        // 2026-01-05 12:05 UTC is 13:05 in Paris (CET, UTC+1).
        assert_eq!(
            format_last_imported(Some(at(2026, 1, 5, 12, 5))),
            "05/01/2026 à 13:05"
        );
    }

    // -- import_run_summary --------------------------------------------------

    #[test]
    fn summarises_a_mixed_run() {
        assert_eq!(
            import_run_summary(3, 1, 12),
            "3 événements importés, 1 mis à jour, 12 inchangés"
        );
    }

    #[test]
    fn uses_the_singular_for_one() {
        assert_eq!(
            import_run_summary(1, 1, 1),
            "1 événement importé, 1 mis à jour, 1 inchangé"
        );
    }

    #[test]
    fn uses_the_singular_for_zero() {
        // French agreement: zero takes the singular.
        assert_eq!(
            import_run_summary(0, 0, 0),
            "0 événement importé, 0 mis à jour, 0 inchangé"
        );
    }

    #[test]
    fn an_unchanged_feed_reads_as_entirely_skipped() {
        // The user-visible face of the backend's UID-keyed idempotence: a
        // re-import of an untouched feed adds nothing.
        assert_eq!(
            import_run_summary(0, 0, 4),
            "0 événement importé, 0 mis à jour, 4 inchangés"
        );
    }

    // -- import_deleted_notice -----------------------------------------------

    #[test]
    fn without_the_option_the_notice_still_says_the_events_remain() {
        // The default branch, and the one the copy has taught users to expect
        // since F11: the sentence it prints must not change shape.
        let notice = import_deleted_notice(None);
        assert!(notice.contains("restent dans l'agenda"), "{notice}");
    }

    #[test]
    fn the_deleted_count_is_stated_rather_than_implied() {
        assert_eq!(
            import_deleted_notice(Some(12)),
            "Agenda Google retiré, ainsi que 12 événements importés."
        );
    }

    #[test]
    fn one_deleted_event_takes_the_singular() {
        assert_eq!(
            import_deleted_notice(Some(1)),
            "Agenda Google retiré, ainsi que 1 événement importé."
        );
    }

    /// Asking to delete the events of a connection that never ran an import
    /// is not an error, but "ainsi que 0 événement importé" reads as a
    /// failure. The zero case gets its own sentence.
    #[test]
    fn zero_deleted_events_does_not_read_as_a_failure() {
        let notice = import_deleted_notice(Some(0));
        assert!(notice.contains("Aucun événement"), "{notice}");
        assert!(!notice.contains('0'), "{notice}");
        assert!(!notice.contains("restent dans l'agenda"), "{notice}");
    }
}
