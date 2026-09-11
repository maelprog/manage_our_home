//! Applying the schema migrations (#105).
//!
//! Migrations do not run on the runtime pool. They run on their own
//! connection, opened from `MIGRATION_DATABASE_URL`, as a role that owns
//! the tables *and* carries `BYPASSRLS`.
//!
//! Why a third role rather than the one `DATABASE_URL` already names:
//! `apps/api/README.md`'s "Deployment note on Row-Level Security"
//! prescribes `NOSUPERUSER NOBYPASSRLS` for the runtime connection, and
//! every family-scoped table is `FORCE ROW LEVEL SECURITY` with a policy
//! keyed on `current_setting('app.family_id', true)`. That setting is
//! `NULL` outside an HTTP request, so during a migration such a role sees
//! **zero rows** in those tables. DDL is unaffected — which is why
//! `0001..0012` never noticed — but the moment a migration does DML, its
//! source is filtered to nothing: `INSERT 0 0`, exit 0, no error, no
//! warning, and sqlx records the migration as applied in the same
//! transaction without ever looking at the row count. It never runs
//! again. `0013_backfill_event_assignees.sql` is the first migration to
//! hit that, and the one that made it visible.
//!
//! And why not `ADMIN_DATABASE_URL`: that role exists for the three
//! `/admin/*` endpoints (Epic #8). Nothing describes it as the owner of
//! the tables, and folding DDL into the role that serves request traffic
//! would widen a deliberately narrow exception.
//!
//! The guard below is the half that survives a misconfiguration. Same
//! shape, and the same reason, as `attachment_reconcile::ensure_bypasses_rls`:
//! a connection that cannot see the rows produces a plausible-looking
//! success, so the connection is checked before anything is applied.

use anyhow::Context;
use sqlx::postgres::PgPoolOptions;

/// Environment variable naming the connection migrations are applied
/// through.
pub const MIGRATION_URL_VAR: &str = "MIGRATION_DATABASE_URL";

/// Resolves the connection string migrations must be applied through.
///
/// Deliberately without the `unwrap_or_else(|_| database_url.clone())`
/// that `main.rs` applies to `ADMIN_DATABASE_URL`. That fallback is
/// survivable for the superadmin endpoints, which fail visibly without
/// the privilege; here it would put the migration straight back on the
/// runtime role and restore the silent no-op this module exists to
/// prevent.
pub fn resolve_migration_url(raw: Option<String>) -> anyhow::Result<String> {
    let url = raw.unwrap_or_default();
    if url.trim().is_empty() {
        anyhow::bail!(
            "{MIGRATION_URL_VAR} is required and has no DATABASE_URL fallback: migrations \
             must be applied by a role that owns the tables and carries BYPASSRLS. On the \
             NOSUPERUSER NOBYPASSRLS role apps/api/README.md prescribes for DATABASE_URL, \
             a migration's DML reads its source tables back empty and applies to nothing, \
             silently and once and for all (issue #105)"
        );
    }
    Ok(url)
}

/// Whether a connection whose `pg_roles` row reports these attributes may
/// apply migrations.
///
/// `None` stands for "the catalogue lookup told us nothing" — no row for
/// `current_user`, or a NULL column. Unknown is not a licence to proceed:
/// the failure this guards against leaves no trace, so the guard fails
/// closed.
pub fn may_apply_migrations(rolsuper: Option<bool>, rolbypassrls: Option<bool>) -> bool {
    rolsuper.unwrap_or(false) || rolbypassrls.unwrap_or(false)
}

/// Aborts unless the connection provably bypasses RLS.
pub async fn ensure_migration_role(conn: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let row =
        sqlx::query!("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_optional(&mut *conn)
            .await?;
    let (rolsuper, rolbypassrls) = match row {
        Some(r) => (r.rolsuper, r.rolbypassrls),
        None => (None, None),
    };

    if !may_apply_migrations(rolsuper, rolbypassrls) {
        anyhow::bail!(
            "refusing to migrate: the connection {MIGRATION_URL_VAR} names is neither \
             SUPERUSER nor BYPASSRLS, so FORCE ROW LEVEL SECURITY applies to it and \
             `app.family_id` is unset outside a request. Every family-scoped table would \
             read back empty, a migration's DML would apply to zero rows without saying \
             so, and sqlx would record it as applied anyway. Point {MIGRATION_URL_VAR} at \
             the NOSUPERUSER BYPASSRLS migration role (see apps/api/README.md)."
        );
    }
    Ok(())
}

/// Opens the migration connection, checks it, applies every pending
/// migration, and closes it again.
///
/// The pool is local to this call and closed before it returns: the
/// elevated role holds a connection for the length of a migration pass,
/// not for the life of the process.
pub async fn apply(raw_url: Option<String>) -> anyhow::Result<()> {
    let url = resolve_migration_url(raw_url)?;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .with_context(|| format!("connecting as the migration role ({MIGRATION_URL_VAR})"))?;

    let result = async {
        let mut conn = pool.acquire().await?;
        ensure_migration_role(&mut conn).await?;
        sqlx::migrate!("./migrations")
            .run(&mut *conn)
            .await
            .context("applying migrations")
    }
    .await;

    pool.close().await;
    result
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
