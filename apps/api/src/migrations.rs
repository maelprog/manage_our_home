//! Applying the schema migrations (#105).
//!
//! Migrations do not run on the runtime pool. They run on their own
//! connection, opened from `MIGRATION_DATABASE_URL`, as a role that owns
//! the tables *and* carries `BYPASSRLS`.

/// Environment variable naming the connection migrations are applied
/// through.
pub const MIGRATION_URL_VAR: &str = "MIGRATION_DATABASE_URL";

/// Resolves the connection string migrations must be applied through.
pub fn resolve_migration_url(raw: Option<String>) -> anyhow::Result<String> {
    let _ = raw;
    todo!("resolve_migration_url")
}

/// Whether a connection whose `pg_roles` row reports these attributes may
/// apply migrations.
pub fn may_apply_migrations(rolsuper: Option<bool>, rolbypassrls: Option<bool>) -> bool {
    let _ = (rolsuper, rolbypassrls);
    todo!("may_apply_migrations")
}

/// Aborts unless the connection provably bypasses RLS.
pub async fn ensure_migration_role(conn: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let _ = conn;
    todo!("ensure_migration_role")
}

/// Opens the migration connection, checks it, applies every pending
/// migration, and closes it again.
pub async fn apply(raw_url: Option<String>) -> anyhow::Result<()> {
    let _ = raw_url;
    todo!("apply")
}

#[cfg(test)]
mod tests {
    use super::{may_apply_migrations, resolve_migration_url};

    #[test]
    fn a_url_is_returned_as_is() {
        let url = "postgres://migration_role:pw@postgres/manage_our_home".to_string();
        assert_eq!(resolve_migration_url(Some(url.clone())).unwrap(), url);
    }

    /// The whole point of #105: there is no `DATABASE_URL` fallback. One
    /// would put the migration back on the runtime role, which is exactly
    /// the role `apps/api/README.md` prescribes as `NOSUPERUSER
    /// NOBYPASSRLS` — and under which a DML migration reads its source
    /// table back empty and applies to nothing.
    #[test]
    fn an_absent_variable_is_an_error_never_a_fallback() {
        let err = resolve_migration_url(None).unwrap_err().to_string();
        assert!(
            err.contains("MIGRATION_DATABASE_URL"),
            "error must name the variable: {err}"
        );
        assert!(
            err.contains("DATABASE_URL fallback"),
            "error must say there is no fallback: {err}"
        );
    }

    /// `MIGRATION_DATABASE_URL=` in a `.env` file is a set-but-empty
    /// variable, which would otherwise reach the pool and fail as an
    /// opaque connection-string parse error.
    #[test]
    fn a_blank_variable_is_rejected_like_an_absent_one() {
        for blank in ["", "   ", "\t\n"] {
            let err = resolve_migration_url(Some(blank.to_string()))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("MIGRATION_DATABASE_URL"),
                "blank {blank:?} must be rejected by name: {err}"
            );
        }
    }

    #[test]
    fn a_superuser_may_apply_migrations() {
        assert!(may_apply_migrations(Some(true), Some(false)));
    }

    #[test]
    fn a_bypassrls_role_may_apply_migrations() {
        assert!(may_apply_migrations(Some(false), Some(true)));
    }

    /// The #105 role: owns the tables, but `FORCE ROW LEVEL SECURITY`
    /// still applies to it and `app.family_id` is unset during a
    /// migration.
    #[test]
    fn a_plain_role_may_not_apply_migrations() {
        assert!(!may_apply_migrations(Some(false), Some(false)));
    }

    /// `SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname =
    /// current_user` can come back with no row at all, and sqlx types
    /// catalogue columns as nullable. Unknown is never a licence to
    /// proceed — the failure this guards against is silent, so the guard
    /// itself must fail closed.
    #[test]
    fn unknown_attributes_fail_closed() {
        assert!(!may_apply_migrations(None, None));
        assert!(!may_apply_migrations(None, Some(false)));
        assert!(!may_apply_migrations(Some(false), None));
    }

    /// ... but a single known-true attribute is enough, whatever the
    /// other one says.
    #[test]
    fn one_known_true_attribute_is_enough() {
        assert!(may_apply_migrations(None, Some(true)));
        assert!(may_apply_migrations(Some(true), None));
    }
}
