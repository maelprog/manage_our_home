//! Reconciliation pass for attachment objects orphaned in MinIO (#58).
//!
//! Nothing in the running API can find an object in the attachments bucket
//! that no `event_attachments` row points at, so nothing can clean one up.
//! This module lists the bucket, diffs it against the table, and deletes
//! the difference — driven by the `reconcile-attachments` binary
//! (`src/bin/reconcile_attachments.rs`), dry-run by default.
//!
//! It is a safety net, not a fix: the two compensation gaps in
//! `agenda::attachments::upload_attachment` (the best-effort delete when
//! the metadata insert fails, and the uncompensated `tx.commit()`) keep
//! producing orphans at a low rate no matter how clean the delete paths
//! get. Tightening those is its own change (#58, "Out of scope").

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use crate::storage::{Storage, StoredObject};

/// What the pass was asked to do. Parsed from argv by [`parse_args`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Deleting is opt-in. These are user files and the delete is
    /// unrecoverable, so the default has to be the harmless one.
    pub apply: bool,
    /// Only objects older than this are eligible. An object written
    /// seconds ago with no row yet is far more likely to be a live upload
    /// mid-flight than garbage: `put_object` happens well before the
    /// `INSERT` commits (`agenda/attachments.rs`), so the two are
    /// indistinguishable from the outside.
    pub min_age_hours: i64,
    /// Restricts the listing to one key prefix. Keys are
    /// `{group_id}/{event_id}/{uuid}`, so this can walk a single family
    /// (or a single event) when a full scan is too big to eyeball.
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Run(Options),
    Help,
}

/// 24h is generous next to the seconds an upload actually takes, and
/// costs nothing: an orphan missed by one pass is caught by the next.
pub const DEFAULT_MIN_AGE_HOURS: i64 = 24;

/// Roughly a century. Not a meaningful window — just far enough out that
/// anything past it is a typo, and near enough in that the cutoff
/// arithmetic in `reconcile` can't overflow.
const MAX_MIN_AGE_HOURS: i64 = 876_000;

pub const HELP: &str = "\
reconcile-attachments — delete attachment objects no event_attachments row points at (#58)

USAGE:
    reconcile-attachments [OPTIONS]

OPTIONS:
    --apply                   Actually delete. Without it the pass only reports (default).
    --min-age-hours <HOURS>   Ignore objects newer than this (default: 24). An object with
                              no row may be an upload still in flight, not an orphan.
    --prefix <PREFIX>         Only list keys under this prefix, e.g. '<group-id>/'.
    -h, --help                Print this help.

ENVIRONMENT:
    ADMIN_DATABASE_URL        Required. Must be a BYPASSRLS role: event_attachments is
                              FORCE ROW LEVEL SECURITY, so an unscoped read on a normal
                              connection returns zero rows — which would classify the
                              entire bucket as orphaned. There is deliberately no
                              DATABASE_URL fallback.
    MINIO_ENDPOINT, MINIO_ACCESS_KEY, MINIO_SECRET_KEY, MINIO_BUCKET, MINIO_REGION
";

/// Outcome of diffing a bucket listing against the known keys.
///
/// Everything that is *not* an orphan is still counted, because the
/// interesting number when you run this for the first time is the ratio:
/// a pass that reports the whole bucket as orphaned is far more likely to
/// be reading an empty table than to have found a total leak.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OrphanScan {
    /// No row points at these, and they are old enough to be safe to delete.
    pub orphans: Vec<StoredObject>,
    /// No row points at these, but they are newer than the cutoff — very
    /// possibly uploads still in flight. Left alone.
    pub in_flight: usize,
    /// No row points at these and the listing carried no `LastModified`,
    /// so the in-flight check can't be applied. Left alone and named, so
    /// a bucket that reports no timestamps is visible rather than silently
    /// skipped.
    pub unknown_age: Vec<String>,
    /// Objects a row points at.
    pub matched: usize,
}

impl OrphanScan {
    pub fn orphan_bytes(&self) -> i64 {
        self.orphans.iter().map(|o| o.size_bytes).sum()
    }

    pub fn orphan_keys(&self) -> Vec<String> {
        self.orphans.iter().map(|o| o.key.clone()).collect()
    }
}

/// Splits a bucket listing into orphans and everything else.
///
/// Pure: the caller supplies the listing, the keys the database knows
/// about, and the cutoff. It trusts `known_keys` completely — an empty
/// set means every old object is an orphan, which is the correct answer
/// for a genuinely empty table and a catastrophic one for a table that
/// only *looks* empty because RLS filtered it. Guarding that is
/// [`ensure_bypasses_rls`]'s job, upstream of here.
pub fn classify(
    listing: Vec<StoredObject>,
    known_keys: &HashSet<String>,
    cutoff: DateTime<Utc>,
) -> OrphanScan {
    let mut scan = OrphanScan::default();

    for object in listing {
        if known_keys.contains(&object.key) {
            scan.matched += 1;
            continue;
        }
        match object.last_modified {
            Some(modified) if modified < cutoff => scan.orphans.push(object),
            Some(_) => scan.in_flight += 1,
            None => scan.unknown_age.push(object.key),
        }
    }

    scan
}

/// Renders the counts the operator has to see before agreeing to delete
/// anything.
pub fn format_summary(scan: &OrphanScan, opts: &Options) -> String {
    let mode = if opts.apply {
        "DELETING"
    } else {
        "dry run — nothing will be deleted, pass --apply to delete"
    };

    // The window goes on its own line rather than into a label, so the
    // column doesn't shift when the operator passes a wider number.
    format!(
        "{mode}\n\
         \x20 age window             {}h (anything newer is kept)\n\
         \x20 objects listed         {}\n\
         \x20 referenced by a row    {}\n\
         \x20 newer than the window  {}\n\
         \x20 missing a timestamp    {}\n\
         \x20 orphaned               {} ({} bytes)",
        opts.min_age_hours,
        scan.matched + scan.in_flight + scan.unknown_age.len() + scan.orphans.len(),
        scan.matched,
        scan.in_flight,
        scan.unknown_age.len(),
        scan.orphans.len(),
        scan.orphan_bytes(),
    )
}

/// Parses argv (without the program name).
pub fn parse_args(args: &[String]) -> anyhow::Result<Invocation> {
    let mut opts = Options {
        apply: false,
        min_age_hours: DEFAULT_MIN_AGE_HOURS,
        prefix: None,
    };

    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Invocation::Help),
            "--apply" => opts.apply = true,
            "--min-age-hours" => {
                let raw = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--min-age-hours needs a value"))?;
                let hours: i64 = raw
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--min-age-hours: '{raw}' is not a number"))?;
                // A negative window puts the cutoff in the future, which
                // turns the in-flight guard inside out: every live upload
                // becomes eligible for deletion.
                if hours < 0 {
                    anyhow::bail!("--min-age-hours cannot be negative");
                }
                // Upper bound because `chrono::Duration::hours` panics
                // instead of saturating, and `reconcile` subtracts exactly
                // that to build the cutoff. A century is past any real use
                // and comfortably inside the range.
                if hours > MAX_MIN_AGE_HOURS {
                    anyhow::bail!(
                        "--min-age-hours cannot exceed {MAX_MIN_AGE_HOURS} (about a century)"
                    );
                }
                opts.min_age_hours = hours;
            }
            "--prefix" => {
                opts.prefix = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--prefix needs a value"))?
                        .clone(),
                );
            }
            other => anyhow::bail!("unrecognised argument '{other}'"),
        }
    }

    Ok(Invocation::Run(opts))
}

/// Aborts unless the connection genuinely bypasses RLS.
///
/// `event_attachments` is `FORCE ROW LEVEL SECURITY` and its policy keys
/// on `current_setting('app.family_id', true)` (`0002_agenda.sql`), so an
/// unscoped `SELECT storage_key` on a normal application connection
/// returns **zero rows, not all rows**. A pass that trusted that would
/// conclude the entire bucket is orphaned and delete all of it. Failing
/// loudly here is the difference between "no orphans found" and "bucket
/// emptied".
pub async fn ensure_bypasses_rls(conn: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let bypasses = sqlx::query_scalar!(
        "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user"
    )
    .fetch_optional(&mut *conn)
    .await?
    .flatten()
    .unwrap_or(false);

    if !bypasses {
        anyhow::bail!(
            "refusing to run: the database connection does not bypass RLS, so \
             event_attachments would read back empty and every object in the \
             bucket would look orphaned. Point ADMIN_DATABASE_URL at the \
             BYPASSRLS admin_role (see apps/api/README.md)."
        );
    }
    Ok(())
}

/// Every storage key the database knows about.
pub async fn known_storage_keys(conn: &mut sqlx::PgConnection) -> anyhow::Result<HashSet<String>> {
    let keys = sqlx::query_scalar!("SELECT storage_key FROM event_attachments")
        .fetch_all(&mut *conn)
        .await?;
    Ok(keys.into_iter().collect())
}

#[derive(Debug)]
pub struct ReconcileOutcome {
    pub scan: OrphanScan,
    /// Keys actually deleted — empty on a dry run.
    pub deleted: Vec<String>,
}

/// Runs the whole pass: verify the connection, list, diff, and (only with
/// `--apply`) delete.
///
/// `now` is a parameter rather than read inside so tests can pin the
/// cutoff instead of sleeping out the in-flight window.
pub async fn reconcile(
    storage: &Storage,
    conn: &mut sqlx::PgConnection,
    opts: &Options,
    now: DateTime<Utc>,
) -> anyhow::Result<ReconcileOutcome> {
    ensure_bypasses_rls(&mut *conn).await?;

    // Listing first, then the table. The other order has a race with a
    // live upload: an object written after the table read but before the
    // listing appears in the listing with no key in the known set, and is
    // classified as an orphan. This order can only mis-classify in the
    // safe direction (a key read from the table that isn't in the listing
    // is simply ignored).
    let listing = storage.list_objects(opts.prefix.as_deref()).await?;
    let known = known_storage_keys(&mut *conn).await?;

    let cutoff = now - chrono::Duration::hours(opts.min_age_hours);
    let scan = classify(listing, &known, cutoff);

    if !opts.apply {
        return Ok(ReconcileOutcome {
            scan,
            deleted: Vec::new(),
        });
    }

    // Logged before the delete, not after: these are user files and the
    // delete is unrecoverable, so the log is the only trace left — and a
    // pass that dies mid-batch has to leave that trace for the keys it
    // had already reached.
    let keys = scan.orphan_keys();
    for key in &keys {
        tracing::info!(key = %key, "deleting orphaned attachment object");
    }
    storage.delete_objects(&keys).await?;

    Ok(ReconcileOutcome {
        scan,
        deleted: keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 29, hour, 0, 0).unwrap()
    }

    fn object(key: &str, size_bytes: i64, last_modified: Option<DateTime<Utc>>) -> StoredObject {
        StoredObject {
            key: key.to_string(),
            size_bytes,
            last_modified,
        }
    }

    fn known(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    fn options() -> Options {
        Options {
            apply: false,
            min_age_hours: DEFAULT_MIN_AGE_HOURS,
            prefix: None,
        }
    }

    #[test]
    fn an_empty_bucket_yields_nothing() {
        let scan = classify(Vec::new(), &known(&[]), at(12));

        assert_eq!(scan, OrphanScan::default());
        assert_eq!(scan.orphan_bytes(), 0);
    }

    #[test]
    fn an_object_a_row_points_at_is_never_an_orphan() {
        let scan = classify(
            vec![object("g/e/live", 1024, Some(at(1)))],
            &known(&["g/e/live"]),
            at(12),
        );

        assert!(scan.orphans.is_empty());
        assert_eq!(scan.matched, 1);
    }

    #[test]
    fn an_old_object_with_no_row_is_an_orphan() {
        let scan = classify(
            vec![object("g/e/leaked", 4096, Some(at(1)))],
            &known(&[]),
            at(12),
        );

        assert_eq!(scan.orphan_keys(), vec!["g/e/leaked".to_string()]);
        assert_eq!(scan.orphan_bytes(), 4096);
        assert_eq!(scan.in_flight, 0);
    }

    /// Trap 2 from #58: `put_object` runs before the row commits, so an
    /// object written seconds ago with no row yet may be a live request
    /// mid-flight. Deleting it would break an upload the user is watching
    /// succeed.
    #[test]
    fn a_recent_object_with_no_row_is_left_alone() {
        let scan = classify(
            vec![object("g/e/in-flight", 4096, Some(at(13)))],
            &known(&[]),
            at(12),
        );

        assert!(scan.orphans.is_empty(), "{:?}", scan.orphans);
        assert_eq!(scan.in_flight, 1);
        assert_eq!(scan.orphan_bytes(), 0);
    }

    /// The cutoff is exclusive on purpose: at exactly the boundary the
    /// object is not old *enough*, and one pass later costs nothing.
    #[test]
    fn an_object_exactly_at_the_cutoff_is_left_alone() {
        let scan = classify(
            vec![object("g/e/boundary", 1, Some(at(12)))],
            &known(&[]),
            at(12),
        );

        assert!(scan.orphans.is_empty());
        assert_eq!(scan.in_flight, 1);
    }

    /// No `LastModified` means the in-flight check can't be applied at
    /// all, so the object can't be shown to be safe to delete.
    #[test]
    fn an_object_of_unknown_age_is_left_alone_and_named() {
        let scan = classify(vec![object("g/e/undated", 7, None)], &known(&[]), at(12));

        assert!(scan.orphans.is_empty());
        assert_eq!(scan.unknown_age, vec!["g/e/undated".to_string()]);
        assert_eq!(scan.in_flight, 0);
    }

    /// A dated object with no row is *not* saved by the missing timestamp
    /// rule when a row does exist for it — matching wins over age.
    #[test]
    fn a_known_key_of_unknown_age_still_counts_as_matched() {
        let scan = classify(
            vec![object("g/e/undated", 7, None)],
            &known(&["g/e/undated"]),
            at(12),
        );

        assert!(scan.unknown_age.is_empty());
        assert_eq!(scan.matched, 1);
    }

    #[test]
    fn a_mixed_bucket_is_split_across_every_bucket() {
        let scan = classify(
            vec![
                object("g/e/live", 10, Some(at(1))),
                object("g/e/leaked-a", 20, Some(at(2))),
                object("g/e/leaked-b", 30, Some(at(3))),
                object("g/e/in-flight", 40, Some(at(13))),
                object("g/e/undated", 50, None),
            ],
            &known(&["g/e/live"]),
            at(12),
        );

        assert_eq!(
            scan.orphan_keys(),
            vec!["g/e/leaked-a".to_string(), "g/e/leaked-b".to_string()]
        );
        assert_eq!(scan.orphan_bytes(), 50);
        assert_eq!(scan.matched, 1);
        assert_eq!(scan.in_flight, 1);
        assert_eq!(scan.unknown_age.len(), 1);
    }

    /// Documents the sharp edge the RLS guard exists to prevent: given an
    /// empty key set, every old object *is* an orphan by this function's
    /// definition. `classify` can't tell "the table is empty" from "the
    /// table read back empty", which is why the connection is checked
    /// before this is ever called.
    #[test]
    fn an_empty_key_set_orphans_the_whole_bucket() {
        let scan = classify(
            vec![
                object("g/e/one", 1, Some(at(1))),
                object("g/e/two", 2, Some(at(1))),
            ],
            &known(&[]),
            at(12),
        );

        assert_eq!(scan.orphans.len(), 2);
    }

    #[test]
    fn the_summary_reports_the_count_and_the_bytes() {
        let scan = classify(
            vec![
                object("g/e/leaked", 4096, Some(at(1))),
                object("g/e/live", 10, Some(at(1))),
            ],
            &known(&["g/e/live"]),
            at(12),
        );

        let summary = format_summary(&scan, &options());

        assert!(summary.contains('1'), "{summary}");
        assert!(summary.contains("4096"), "{summary}");
    }

    /// A dry run must say so, or an operator reading the summary can't
    /// tell whether the files are still there.
    #[test]
    fn the_summary_distinguishes_a_dry_run_from_a_delete() {
        let scan = classify(
            vec![object("g/e/leaked", 4096, Some(at(1)))],
            &known(&[]),
            at(12),
        );

        let dry = format_summary(&scan, &options());
        let applied = format_summary(
            &scan,
            &Options {
                apply: true,
                ..options()
            },
        );

        assert_ne!(dry, applied);
        assert!(dry.to_lowercase().contains("dry"), "{dry}");
    }

    #[test]
    fn no_arguments_means_a_dry_run_with_the_default_window() {
        assert_eq!(
            parse_args(&[]).unwrap(),
            Invocation::Run(Options {
                apply: false,
                min_age_hours: DEFAULT_MIN_AGE_HOURS,
                prefix: None,
            })
        );
    }

    #[test]
    fn deleting_requires_an_explicit_flag() {
        let Invocation::Run(opts) = parse_args(&["--apply".into()]).unwrap() else {
            panic!("--apply should run, not print help");
        };
        assert!(opts.apply);
    }

    #[test]
    fn the_age_window_and_prefix_are_configurable() {
        let Invocation::Run(opts) = parse_args(&[
            "--min-age-hours".into(),
            "48".into(),
            "--prefix".into(),
            "0d1b/".into(),
        ])
        .unwrap() else {
            panic!("expected a run");
        };

        assert_eq!(opts.min_age_hours, 48);
        assert_eq!(opts.prefix.as_deref(), Some("0d1b/"));
    }

    #[test]
    fn help_is_requested_by_either_spelling() {
        assert_eq!(parse_args(&["--help".into()]).unwrap(), Invocation::Help);
        assert_eq!(parse_args(&["-h".into()]).unwrap(), Invocation::Help);
    }

    #[test]
    fn a_non_numeric_age_window_is_rejected() {
        assert!(parse_args(&["--min-age-hours".into(), "soon".into()]).is_err());
    }

    #[test]
    fn an_age_window_without_a_value_is_rejected() {
        assert!(parse_args(&["--min-age-hours".into()]).is_err());
        assert!(parse_args(&["--prefix".into()]).is_err());
    }

    /// A negative window puts the cutoff in the future, which turns the
    /// in-flight guard inside out and makes live uploads deletable.
    #[test]
    fn a_negative_age_window_is_rejected() {
        assert!(parse_args(&["--min-age-hours".into(), "-1".into()]).is_err());
    }

    /// `chrono::Duration::hours` panics rather than saturating once the
    /// value stops fitting, and `reconcile` builds the cutoff by
    /// subtracting exactly that. A fat-fingered window has to be a clean
    /// error at the edge, not a panic in the middle of a pass.
    #[test]
    fn an_absurd_age_window_is_rejected() {
        assert!(parse_args(&["--min-age-hours".into(), i64::MAX.to_string()]).is_err());
        assert!(parse_args(&["--min-age-hours".into(), "999999999".into()]).is_err());
        // A century still parses: the bound exists to catch typos, not to
        // second-guess an operator who wants to scan without deleting.
        assert!(parse_args(&["--min-age-hours".into(), "876000".into()]).is_ok());
    }

    /// A mistyped flag must not be silently ignored — `--dry-run` (which
    /// this doesn't have, dry being the default) silently accepted next
    /// to `--apply` would be exactly the wrong failure.
    #[test]
    fn an_unknown_flag_is_rejected() {
        assert!(parse_args(&["--dry-run".into()]).is_err());
        assert!(parse_args(&["oops".into()]).is_err());
    }
}
