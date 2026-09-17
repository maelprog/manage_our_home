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
//! `/admin/*` endpoints (Epic #8) and the attachment reconcile job
//! (#215). Nothing describes it as the owner of
//! the tables, and folding DDL into the role that serves request traffic
//! would widen a deliberately narrow exception.
//!
//! The guard below is the half that survives a misconfiguration. Same
//! shape, and the same reason, as `attachment_reconcile::ensure_bypasses_rls`:
//! a connection that cannot see the rows produces a plausible-looking
//! success, so the connection is checked before anything is applied.

use anyhow::Context;
use futures::future::BoxFuture;
use futures::TryStreamExt;
use sqlx::migrate::{AppliedMigration, Migrate, MigrateError, Migration};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, Connection, Executor};
use std::time::{Duration, Instant};

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
        // `run_direct` rather than `run`, so the pass goes through
        // `LoggingMigrate` and each migration's row counts are logged
        // (#154). `sqlx::migrate!` is kept: the migrations stay in the
        // binary. See `LoggingMigrate` for what `run_direct` being
        // `#[doc(hidden)]` costs at the next sqlx bump.
        let mut logging = LoggingMigrate(&mut conn);
        sqlx::migrate!("./migrations")
            .run_direct(None, &mut logging, false)
            .await
            .context("applying migrations")
    }
    .await;

    pool.close().await;
    result
}

/// The row counts one migration produced, rendered for the log line.
#[derive(Debug, PartialEq, Eq)]
pub struct RowCountSummary {
    /// How many statements the migration file was executed as.
    pub statements: usize,
    /// Their row counts added up.
    pub rows_affected: u64,
    /// The individual counts, in execution order, as `[0, 2]`.
    pub per_statement: String,
}

/// Summarises the per-statement row counts of one migration.
///
/// The total alone hides the case #154 is about: a file that does DDL
/// *and* a backfill reports one number, and a `CREATE TABLE` contributes
/// 0 to it just as a backfill that matched nothing does. The list is what
/// makes a zero-row statement readable next to its neighbours.
///
/// It deliberately says nothing about whether a count is *right*. Per the
/// 2026-09-11 decision, that question is undecidable from the migration
/// file alone — on a fresh database a backfill legitimately affects zero
/// rows — so this is a log, never a guard.
pub fn summarize_row_counts(per_statement: &[u64]) -> RowCountSummary {
    let rendered = per_statement
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ");

    RowCountSummary {
        statements: per_statement.len(),
        // Saturating rather than summing: nothing in a log line is worth
        // an overflow panic in a debug build or a wrapped total in a
        // release one.
        rows_affected: per_statement
            .iter()
            .fold(0u64, |total, count| total.saturating_add(*count)),
        per_statement: format!("[{rendered}]"),
    }
}

/// A `Migrate` connection that logs what each migration's statements
/// touched (#154).
///
/// ## Why this exists
///
/// `#105` closed exactly one cause of "a migration's DML succeeds on zero
/// rows and is recorded as applied anyway": the RLS filtering documented
/// at the top of this module. The others — a `WHERE` that matches
/// nothing, a wrong enum value in a predicate, a data migration shipped
/// before the data it repairs — are indistinguishable from a legitimate
/// zero at the moment they happen. On a fresh database a backfill
/// correctly affects zero rows; under the failure it affects zero rows
/// too, and nothing in the file says which was expected.
///
/// So this does not decide: it *reports*. The row counts go to the log,
/// where whoever ran the migration can read them, and nothing is refused
/// on their account.
///
/// ## Why a wrapper, and not something simpler
///
/// `Migrator` exposes no per-migration hook, and `_sqlx_migrations` has
/// no column to put a row count in (`version`, `description`,
/// `installed_on`, `success`, `checksum`, `execution_time` — reading it
/// back afterwards cannot answer the question). But the `Migrate` trait
/// is public and `Migrator::run_direct` is generic over it, so a type of
/// ours can sit between the migrator and the connection. That keeps
/// `sqlx::migrate!`, and with it a binary that carries its migrations
/// instead of reading a directory at startup.
///
/// ## The catch, on every sqlx bump
///
/// `Migrator::run_direct` is `pub` but `#[doc(hidden)]`: it is outside
/// sqlx's semver contract and may change or disappear in any release.
/// Two consequences when the pin moves:
///
/// - if `run_direct` is gone, `apply()` below stops compiling. The
///   replacement is not to reimplement the migrator loop — that loses
///   `validate_applied_migrations`, which is private, and with it the
///   detection of a migration file edited after it was applied.
/// - `Migrate::apply` is reimplemented here rather than wrapped, because
///   it is the only place the row counts exist. It must keep matching
///   `sqlx-postgres`'s version statement for statement; the bookkeeping
///   note on [`LoggingMigrate::apply_logging`] says why.
///
/// ## What it does not see
///
/// Row counts come from the wire, one per `CommandComplete`. DML inside
/// a `DO` block, a function or a trigger is a single statement to the
/// protocol and reports zero however many rows it changed. A migration
/// that buries its backfill in PL/pgSQL is as silent as before — see
/// `tests/migration_rowcount_flow.rs`, which asserts that limit rather
/// than leaving it to be rediscovered.
pub struct LoggingMigrate<'c>(pub &'c mut sqlx::PgConnection);

/// Runs a migration's SQL, then records it, on one connection.
///
/// Statement for statement `sqlx-postgres`'s private `execute_migration`,
/// with `execute` replaced by `execute_many` so each statement's row
/// count is kept instead of folded into one total and dropped.
async fn execute_and_count(
    conn: &mut sqlx::PgConnection,
    table_name: &str,
    migration: &Migration,
) -> Result<Vec<u64>, MigrateError> {
    let mut per_statement = Vec::new();
    {
        let mut results = (&mut *conn).execute_many(migration.sql.clone());
        while let Some(result) = results
            .try_next()
            .await
            .map_err(|e| MigrateError::ExecuteMigration(e, migration.version))?
        {
            per_statement.push(result.rows_affected());
        }
    }

    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {table_name} ( version, description, success, checksum, execution_time ) \
         VALUES ( $1, $2, TRUE, $3, -1 )"
    )))
    .bind(migration.version)
    .bind(&*migration.description)
    .bind(&*migration.checksum)
    .execute(conn)
    .await?;

    Ok(per_statement)
}

impl LoggingMigrate<'_> {
    /// [`Migrate::apply`], handing back the row counts it logs.
    ///
    /// ### The bookkeeping is the load-bearing part
    ///
    /// The migration's SQL and its `_sqlx_migrations` row are committed
    /// by **one** transaction, so the pass can never end up with one
    /// without the other. That is not a refinement: splitting them is
    /// <https://github.com/launchbadge/sqlx/issues/1966>, where a
    /// migration is applied but not recorded and runs again on the next
    /// boot. `execution_time` is then filled in by a second statement
    /// outside that transaction — it is initialised to `-1` on purpose
    /// and exists only for debugging, so losing it to a crash between
    /// the two is the accepted trade sqlx already makes.
    ///
    /// A migration declared `-- no-transaction` skips the transaction,
    /// as sqlx does; the repository has none today.
    pub async fn apply_logging(
        &mut self,
        table_name: &str,
        migration: &Migration,
    ) -> Result<(Duration, RowCountSummary), MigrateError> {
        let start = Instant::now();

        let per_statement = if migration.no_tx {
            execute_and_count(self.0, table_name, migration).await?
        } else {
            let mut tx = self.0.begin().await?;
            let counts = execute_and_count(&mut tx, table_name, migration).await?;
            tx.commit().await?;
            counts
        };

        let elapsed = start.elapsed();
        let nanos = i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX);
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE {table_name} SET execution_time = $1 WHERE version = $2"
        )))
        .bind(nanos)
        .bind(migration.version)
        .execute(&mut *self.0)
        .await?;

        Ok((elapsed, summarize_row_counts(&per_statement)))
    }
}

impl Migrate for LoggingMigrate<'_> {
    /// The one method with a body of its own; everything below only
    /// forwards.
    fn apply<'e>(
        &'e mut self,
        table_name: &'e str,
        migration: &'e Migration,
    ) -> BoxFuture<'e, Result<Duration, MigrateError>> {
        Box::pin(async move {
            let (elapsed, summary) = self.apply_logging(table_name, migration).await?;
            // One level, whatever the counts are. A zero here is not
            // known to be wrong — see the type's doc comment — and a
            // warning nobody can act on is a warning nobody reads.
            tracing::info!(
                version = migration.version,
                description = %migration.description,
                statements = summary.statements,
                rows_affected = summary.rows_affected,
                per_statement = %summary.per_statement,
                "migration applied"
            );
            Ok(elapsed)
        })
    }

    fn create_schema_if_not_exists<'e>(
        &'e mut self,
        schema_name: &'e str,
    ) -> BoxFuture<'e, Result<(), MigrateError>> {
        self.0.create_schema_if_not_exists(schema_name)
    }

    fn ensure_migrations_table<'e>(
        &'e mut self,
        table_name: &'e str,
    ) -> BoxFuture<'e, Result<(), MigrateError>> {
        self.0.ensure_migrations_table(table_name)
    }

    fn dirty_version<'e>(
        &'e mut self,
        table_name: &'e str,
    ) -> BoxFuture<'e, Result<Option<i64>, MigrateError>> {
        self.0.dirty_version(table_name)
    }

    fn list_applied_migrations<'e>(
        &'e mut self,
        table_name: &'e str,
    ) -> BoxFuture<'e, Result<Vec<AppliedMigration>, MigrateError>> {
        self.0.list_applied_migrations(table_name)
    }

    fn lock(&mut self) -> BoxFuture<'_, Result<(), MigrateError>> {
        self.0.lock()
    }

    fn unlock(&mut self) -> BoxFuture<'_, Result<(), MigrateError>> {
        self.0.unlock()
    }

    fn revert<'e>(
        &'e mut self,
        table_name: &'e str,
        migration: &'e Migration,
    ) -> BoxFuture<'e, Result<Duration, MigrateError>> {
        self.0.revert(table_name, migration)
    }

    fn skip<'e>(
        &'e mut self,
        table_name: &'e str,
        migration: &'e Migration,
    ) -> BoxFuture<'e, Result<(), MigrateError>> {
        self.0.skip(table_name, migration)
    }
}

#[cfg(test)]
mod tests {
    use super::{may_apply_migrations, resolve_migration_url, summarize_row_counts};

    /// The shape #154 exists to make visible: a file whose DDL succeeds
    /// and whose DML matched nothing. A single summed count would read
    /// `2` here and hide it; the list does not.
    #[test]
    fn a_zero_row_statement_stays_visible_next_to_its_neighbours() {
        let summary = summarize_row_counts(&[2, 0]);
        assert_eq!(summary.statements, 2);
        assert_eq!(summary.rows_affected, 2);
        assert_eq!(summary.per_statement, "[2, 0]");
    }

    #[test]
    fn a_single_statement_that_matched_nothing() {
        let summary = summarize_row_counts(&[0]);
        assert_eq!(summary.statements, 1);
        assert_eq!(summary.rows_affected, 0);
        assert_eq!(summary.per_statement, "[0]");
    }

    /// A pure-DDL migration — `0001..0012` — reports counts too, and they
    /// are all zero. That is not a failure, which is exactly why nothing
    /// here raises the level or refuses.
    #[test]
    fn pure_ddl_reports_zeroes_without_ceremony() {
        let summary = summarize_row_counts(&[0, 0, 0]);
        assert_eq!(summary.rows_affected, 0);
        assert_eq!(summary.per_statement, "[0, 0, 0]");
    }

    /// An empty migration file, or one holding only comments: Postgres
    /// answers `EmptyQueryResponse`, which yields no result at all.
    #[test]
    fn no_statement_at_all_renders_as_an_empty_list() {
        let summary = summarize_row_counts(&[]);
        assert_eq!(summary.statements, 0);
        assert_eq!(summary.rows_affected, 0);
        assert_eq!(summary.per_statement, "[]");
    }

    /// The total is a convenience, not an assertion: it saturates rather
    /// than wrapping or panicking, so no arithmetic in a log line can
    /// take a migration pass down.
    #[test]
    fn the_total_saturates_instead_of_overflowing() {
        let summary = summarize_row_counts(&[u64::MAX, 1]);
        assert_eq!(summary.rows_affected, u64::MAX);
    }

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
