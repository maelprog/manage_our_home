//! Pure, dependency-light logic for the User-admin screens (front epic F9,
//! issue #24), shared by `apps/web`'s SSR pages. Written test-first per
//! `.claude/CLAUDE.md`'s TDD process. Everything the superadmin support screens
//! need beyond rendering lives here so the UI and the fixed backend
//! (`apps/api/src/user_admin/`) can never drift:
//!
//! - `can_view_admin` mirrors `apps/api/src/user_admin/mod.rs::is_superadmin`:
//!   the hard boolean gate that decides whether the `/admin` nav + route tree
//!   render at all. The backend's `SuperAdminUser` extractor stays the
//!   authority; this only keeps `apps/web` from showing a door it would 403.
//! - `user_status_label` turns the `deleted_at` / `deactivated_at` /
//!   `deletion_requested_at` triple from `GET /admin/users` into one French
//!   status, the purge taking precedence (it is final), then the
//!   deactivation (#256).
//! - `can_deactivate` / `can_reactivate` mirror the backend's guards on
//!   `POST /admin/users/:id/deactivate` and `/reactivate`: only an active
//!   account can be deactivated, only a deactivated, not yet purged one
//!   reactivated (anything else 404s), so each button shows only when it
//!   can succeed.
//! - `format_admin_datetime` renders a UTC instant in **Europe/Paris**, the
//!   fixed v1 display timezone (F3's convention), and `_opt` renders a `—` for
//!   the nullable columns.

use chrono::{DateTime, Utc};
use chrono_tz::Europe::Paris;

/// Mirror of `apps/api/src/user_admin/mod.rs::is_superadmin`: a hard boolean
/// gate, no partial/role nuance (v1 has a single expected superadmin account).
/// Decides whether `apps/web` renders the `/admin` nav link and lets the
/// `/admin/*` handlers run; the backend still 403s a forged request.
pub fn can_view_admin(is_superadmin: bool) -> bool {
    is_superadmin
}

/// French status for a user row from `GET /admin/users`, derived from its
/// three nullable timestamps. The purge (`deleted_at`) is final and wins over
/// everything; a deactivation wins over a pending self-service deletion
/// request, which the account can no longer cancel (#256).
pub fn user_status_label(
    deleted_at: Option<DateTime<Utc>>,
    deactivated_at: Option<DateTime<Utc>>,
    deletion_requested_at: Option<DateTime<Utc>>,
) -> &'static str {
    if deleted_at.is_some() {
        "Purgé"
    } else if deactivated_at.is_some() {
        "Désactivé"
    } else if deletion_requested_at.is_some() {
        "Suppression demandée"
    } else {
        "Actif"
    }
}

/// Mirror of the backend's `WHERE ... AND deactivated_at IS NULL AND
/// deleted_at IS NULL` guard on `POST /admin/users/:id/deactivate` (404
/// otherwise). Used to hide the confirm button — the backend stays the
/// authority.
pub fn can_deactivate(
    deleted_at: Option<DateTime<Utc>>,
    deactivated_at: Option<DateTime<Utc>>,
) -> bool {
    deleted_at.is_none() && deactivated_at.is_none()
}

/// Mirror of the backend's `WHERE ... AND deactivated_at IS NOT NULL AND
/// deleted_at IS NULL` guard on `POST /admin/users/:id/reactivate` (#256):
/// a purged account has nothing left to give back.
pub fn can_reactivate(
    deleted_at: Option<DateTime<Utc>>,
    deactivated_at: Option<DateTime<Utc>>,
) -> bool {
    deleted_at.is_none() && deactivated_at.is_some()
}

/// Formats a UTC instant in Europe/Paris (`24/07/2026 à 14:05`), the fixed v1
/// display timezone (F3's `DISPLAY_TZ`), so DST handling lives in one tested
/// place — same convention as `validation::messagerie::format_message_time`.
pub fn format_admin_datetime(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Paris)
        .format("%d/%m/%Y à %H:%M")
        .to_string()
}

/// Same as [`format_admin_datetime`] for the nullable admin columns
/// (`deleted_at`, `deletion_requested_at`): renders a `—` placeholder when the
/// timestamp is absent, so a table cell is never blank.
pub fn format_admin_datetime_opt(dt: Option<DateTime<Utc>>) -> String {
    match dt {
        Some(dt) => format_admin_datetime(dt),
        None => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    // -- can_view_admin ------------------------------------------------------

    #[test]
    fn superadmin_flag_true_can_view_admin() {
        assert!(can_view_admin(true));
    }

    #[test]
    fn superadmin_flag_false_cannot_view_admin() {
        assert!(!can_view_admin(false));
    }

    // -- user_status_label ---------------------------------------------------

    #[test]
    fn active_user_has_no_timestamps() {
        assert_eq!(user_status_label(None, None, None), "Actif");
    }

    #[test]
    fn deletion_requested_shows_when_only_that_timestamp_is_set() {
        assert_eq!(
            user_status_label(None, None, Some(at(2026, 7, 24, 12, 0))),
            "Suppression demandée"
        );
    }

    #[test]
    fn deactivated_shows_when_deactivated_at_is_set() {
        assert_eq!(
            user_status_label(None, Some(at(2026, 7, 24, 12, 0)), None),
            "Désactivé"
        );
    }

    #[test]
    fn deactivation_takes_precedence_over_a_pending_deletion_request() {
        // A user who requested deletion and was then deactivated by support
        // reads as Désactivé: it can no longer cancel its request.
        assert_eq!(
            user_status_label(
                None,
                Some(at(2026, 7, 24, 13, 0)),
                Some(at(2026, 7, 20, 9, 0))
            ),
            "Désactivé"
        );
    }

    #[test]
    fn a_purged_account_reads_as_purged_whatever_came_before() {
        // #256: `deleted_at` means the purge anonymised the row, nothing else.
        assert_eq!(
            user_status_label(
                Some(at(2028, 7, 24, 13, 0)),
                Some(at(2026, 7, 24, 13, 0)),
                Some(at(2026, 7, 20, 9, 0))
            ),
            "Purgé"
        );
        assert_eq!(
            user_status_label(Some(at(2026, 8, 24, 13, 0)), None, None),
            "Purgé"
        );
    }

    // -- can_deactivate / can_reactivate -------------------------------------

    #[test]
    fn an_active_user_can_be_deactivated_not_reactivated() {
        assert!(can_deactivate(None, None));
        assert!(!can_reactivate(None, None));
    }

    #[test]
    fn a_deactivated_user_can_be_reactivated_not_deactivated_again() {
        let on = Some(at(2026, 7, 24, 12, 0));
        assert!(!can_deactivate(None, on));
        assert!(can_reactivate(None, on));
    }

    #[test]
    fn a_purged_account_can_be_neither_deactivated_nor_reactivated() {
        let purged = Some(at(2028, 7, 24, 12, 0));
        let on = Some(at(2026, 7, 24, 12, 0));
        assert!(!can_deactivate(purged, None));
        assert!(!can_reactivate(purged, None));
        assert!(!can_deactivate(purged, on));
        assert!(!can_reactivate(purged, on));
    }

    // -- format_admin_datetime ----------------------------------------------

    #[test]
    fn formats_in_paris_summer_time() {
        // 2026-07-24 12:05 UTC is 14:05 in Paris (CEST, UTC+2).
        assert_eq!(
            format_admin_datetime(at(2026, 7, 24, 12, 5)),
            "24/07/2026 à 14:05"
        );
    }

    #[test]
    fn formats_in_paris_winter_time() {
        // 2026-01-05 12:05 UTC is 13:05 in Paris (CET, UTC+1).
        assert_eq!(
            format_admin_datetime(at(2026, 1, 5, 12, 5)),
            "05/01/2026 à 13:05"
        );
    }

    #[test]
    fn opt_renders_a_dash_when_absent() {
        assert_eq!(format_admin_datetime_opt(None), "—");
    }

    #[test]
    fn opt_formats_the_instant_when_present() {
        assert_eq!(
            format_admin_datetime_opt(Some(at(2026, 7, 24, 12, 5))),
            "24/07/2026 à 14:05"
        );
    }
}
