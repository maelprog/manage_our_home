//! Catch-up of the attachments' thumbnails (#777).
//!
//! An upload makes its picture's thumbnail (#774, `agenda::thumbnail`);
//! the pictures stored before that have none, nor those whose decode
//! permit stayed busy at upload, and the gallery shows them as a generic
//! tile. This pass makes the missing ones, once a day, the way
//! `jobs::attachment_reconcile` sweeps the bucket (#215): same admin pool,
//! same single-runner lock, first tick at startup.
//!
//! Idempotent: it selects only the pictures with no `thumbnail_key`, and
//! sets it with the thumbnail. A picture that fails is counted
//! (`thumbnail_attempts`, 0055) and left alone past `MAX_ATTEMPTS`; one
//! that does not decode spends them all at once, so a corrupt file is
//! never read back and decoded again on every pass.
//!
//! Without holding the server back: the rows are read `BATCH_SIZE` at a
//! time, no connection held while an object is read or decoded, and the
//! pictures are decoded one at a time, off the async workers and under the
//! decode permit the uploads and the avatar share (`avatar::DECODES`, a
//! fair semaphore): an upload waiting on it gets it before the pass's next
//! picture.

use std::time::Duration as StdDuration;

use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use crate::agenda::thumbnail::{make_thumbnail, thumbnail_key, THUMBNAIL_CONTENT_TYPE};
use crate::attachment_reconcile::ensure_bypasses_rls;
use crate::jobs::single_runner::{exclusively, Job};
use crate::storage::Storage;

/// Once a day, like the reconciliation sweep: the backlog is caught up by
/// the first pass after a deploy; afterwards a pass only finds the
/// pictures whose decode permit was busy at upload.
pub const CATCH_UP_INTERVAL: StdDuration = StdDuration::from_secs(24 * 3600);

/// The types `agenda::thumbnail::make_thumbnail` makes a thumbnail of.
/// Nothing else is selected: a PDF or a video gets none, and selecting it
/// would only spend its attempts.
pub const THUMBNAILABLE_MIME_TYPES: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];

/// Rows read per query. Each may be a 20 MiB object read and decoded in
/// turn: a batch is a handful of them, and the next is read after.
pub const BATCH_SIZE: i64 = 20;

/// Failed attempts after which an attachment is left without a thumbnail.
/// A storage that did not answer gets a few passes, a day apart.
pub const MAX_ATTEMPTS: i16 = 3;

/// Why an attempt made no thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The file was read and is no picture `make_thumbnail` decodes:
    /// corrupt, or past its bounds. It will not decode on the next pass.
    Undecodable,
    /// The file could not be read, or the thumbnail not stored or
    /// recorded: the storage or the database may answer on the next pass.
    Transient,
}

/// Whether the pass makes this attachment's thumbnail: a picture of a
/// type `make_thumbnail` decodes, without one, under its attempts. The
/// selection query (`next_batch`) says the same in SQL.
pub fn is_candidate(mime_type: &str, thumbnail_key: Option<&str>, attempts: i16) -> bool {
    thumbnail_key.is_none()
        && attempts < MAX_ATTEMPTS
        && THUMBNAILABLE_MIME_TYPES.contains(&mime_type)
}

/// The failed attempts once `failure` is counted on `attempts`.
pub fn attempts_after(failure: Failure, attempts: i16) -> i16 {
    match failure {
        Failure::Undecodable => attempts.max(MAX_ATTEMPTS),
        Failure::Transient => attempts.saturating_add(1),
    }
}

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CatchUpOutcome {
    /// Thumbnails made and recorded — or found deleted or thumbnailed by
    /// someone else between the batch's read and the write.
    pub generated: usize,
    /// Pictures that did not decode, given up on.
    pub undecodable: usize,
    /// Pictures whose file or thumbnail the storage or the database did
    /// not take: one attempt spent.
    pub failed: usize,
}

/// Polling worker, `attachment_reconcile::run`'s shape. `pool` must be the
/// `BYPASSRLS` admin pool (`AppState.admin_db`): `event_attachments` reads
/// back empty on any other role, and the pass refuses to run rather than
/// report nothing to do.
///
/// One process at a time catches up (#426): a replica that finds another
/// one at it skips its pass.
pub async fn run(pool: PgPool, storage: Storage) {
    let mut ticker = interval(CATCH_UP_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let pass = catch_up(&pool, &storage, BATCH_SIZE);
        match exclusively(&pool, Job::ThumbnailCatchUp, pass).await {
            Ok(Some(outcome)) => tracing::info!(
                generated = outcome.generated,
                undecodable = outcome.undecodable,
                failed = outcome.failed,
                "thumbnail catch-up pass done"
            ),
            Ok(None) => tracing::debug!("thumbnail catch-up skipped: another process runs it"),
            Err(e) => tracing::error!(error = ?e, "thumbnail catch-up job failed"),
        }
    }
}

/// One pass: every candidate, `batch_size` rows at a time. `batch_size` is
/// a parameter so the flow tests can spread a few rows over several
/// batches.
///
/// The rows are walked in id order from where the last batch ended, so a
/// row that failed is not read again within the pass; each batch is read
/// anew, so a row deleted meanwhile is simply not found.
pub async fn catch_up(
    pool: &PgPool,
    storage: &Storage,
    batch_size: i64,
) -> anyhow::Result<CatchUpOutcome> {
    {
        let mut conn = pool.acquire().await?;
        ensure_bypasses_rls(&mut conn).await?;
    }

    let mut outcome = CatchUpOutcome::default();
    let mut after = Uuid::nil();
    loop {
        let batch = next_batch(pool, after, batch_size).await?;
        for candidate in &batch {
            match make_one(pool, storage, candidate).await {
                Ok(()) => outcome.generated += 1,
                Err(failure) => {
                    match failure {
                        Failure::Undecodable => outcome.undecodable += 1,
                        Failure::Transient => outcome.failed += 1,
                    }
                    record_failure(pool, candidate, failure).await?;
                }
            }
        }
        match batch.last() {
            Some(last) if batch.len() as i64 == batch_size => after = last.id,
            _ => return Ok(outcome),
        }
    }
}

struct Candidate {
    id: Uuid,
    storage_key: String,
    mime_type: String,
    attempts: i16,
}

/// The next candidates after `after`, in id order: `is_candidate` in SQL.
async fn next_batch(pool: &PgPool, after: Uuid, batch_size: i64) -> anyhow::Result<Vec<Candidate>> {
    let rows = sqlx::query!(
        r#"
        SELECT id, storage_key, mime_type, thumbnail_attempts
        FROM event_attachments
        WHERE thumbnail_key IS NULL
          AND thumbnail_attempts < $1
          AND mime_type = ANY($2)
          AND id > $3
        ORDER BY id
        LIMIT $4
        "#,
        MAX_ATTEMPTS,
        &THUMBNAILABLE_MIME_TYPES[..] as &[&str],
        after,
        batch_size,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Candidate {
            id: r.id,
            storage_key: r.storage_key,
            mime_type: r.mime_type,
            attempts: r.thumbnail_attempts,
        })
        .collect())
}

/// Reads the file, makes its thumbnail, stores and records it.
///
/// Row first, object second, commit last, as at upload: the row is
/// claimed (`thumbnail_key IS NULL` still) before the object is written,
/// so a row deleted or thumbnailed meanwhile gets no object, and a failed
/// write leaves the row as it was. An object written whose commit then
/// fails is left to the reconciliation pass (#58).
async fn make_one(pool: &PgPool, storage: &Storage, candidate: &Candidate) -> Result<(), Failure> {
    let bytes = storage
        .get_object(&candidate.storage_key)
        .await
        .map_err(|error| {
            tracing::warn!(id = %candidate.id, ?error, "thumbnail catch-up: file not read");
            Failure::Transient
        })?;

    let permit = crate::avatar::DECODES
        .acquire()
        .await
        .map_err(|_| Failure::Transient)?;
    let mime_type = candidate.mime_type.clone();
    let thumbnail = tokio::task::spawn_blocking(move || make_thumbnail(&bytes, &mime_type))
        .await
        .map_err(|error| {
            tracing::warn!(id = %candidate.id, %error, "thumbnail catch-up: decode failed");
            Failure::Transient
        })?;
    drop(permit);
    let Some(thumbnail) = thumbnail else {
        tracing::info!(id = %candidate.id, "thumbnail catch-up: picture not decoded, given up");
        return Err(Failure::Undecodable);
    };

    let key = thumbnail_key(&candidate.storage_key);
    let stored = async {
        let mut tx = crate::db::begin(pool).await?;
        let claimed = sqlx::query_scalar!(
            "UPDATE event_attachments SET thumbnail_key = $2
             WHERE id = $1 AND thumbnail_key IS NULL
             RETURNING id",
            candidate.id,
            key,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if claimed.is_none() {
            // Deleted or thumbnailed since the batch was read: nothing to
            // store, nothing failed.
            return anyhow::Ok(());
        }
        storage
            .put_object(&key, bytes::Bytes::from(thumbnail), THUMBNAIL_CONTENT_TYPE)
            .await?;
        tx.commit().await?;
        Ok(())
    };
    stored.await.map_err(|error| {
        tracing::warn!(id = %candidate.id, ?error, "thumbnail catch-up: thumbnail not stored");
        Failure::Transient
    })
}

/// Counts `failure` on the row, unless it was deleted or thumbnailed since.
async fn record_failure(
    pool: &PgPool,
    candidate: &Candidate,
    failure: Failure,
) -> anyhow::Result<()> {
    sqlx::query!(
        "UPDATE event_attachments SET thumbnail_attempts = $2
         WHERE id = $1 AND thumbnail_key IS NULL",
        candidate.id,
        attempts_after(failure, candidate.attempts),
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- the selection -------------------------------------------------

    /// A picture stored before #774: no thumbnail, never tried.
    #[test]
    fn a_picture_without_a_thumbnail_is_selected() {
        for mime_type in ["image/jpeg", "image/png", "image/webp"] {
            assert!(is_candidate(mime_type, None, 0), "{mime_type}");
        }
    }

    /// A PDF or a video gets no thumbnail, at upload or here.
    #[test]
    fn a_pdf_or_a_video_is_never_selected() {
        for mime_type in ["application/pdf", "video/mp4", "video/webm"] {
            assert!(!is_candidate(mime_type, None, 0), "{mime_type}");
        }
    }

    /// Done is done: a second pass touches nothing a first one did.
    #[test]
    fn a_picture_with_a_thumbnail_is_not_selected_again() {
        assert!(!is_candidate("image/png", Some("g/e/k-thumbnail"), 0));
    }

    /// A picture that failed every attempt is left alone for good.
    #[test]
    fn a_picture_past_its_attempts_is_not_selected_again() {
        assert!(is_candidate("image/png", None, MAX_ATTEMPTS - 1));
        assert!(!is_candidate("image/png", None, MAX_ATTEMPTS));
        assert!(!is_candidate("image/png", None, i16::MAX));
    }

    /// Every type allowed at upload is either thumbnailed or excluded by
    /// type, never selected only to fail: what `make_thumbnail` decodes
    /// is exactly the selected list.
    #[test]
    fn the_selected_types_are_the_ones_make_thumbnail_decodes() {
        for mime_type in ["image/jpeg", "image/png", "image/webp"] {
            assert!(THUMBNAILABLE_MIME_TYPES.contains(&mime_type));
        }
        assert_eq!(THUMBNAILABLE_MIME_TYPES.len(), 3);
        assert!(THUMBNAILABLE_MIME_TYPES
            .iter()
            .all(|m| m.starts_with("image/")));
    }

    /// A batch reads a handful of rows: each one may be a 20 MiB object
    /// read and decoded in turn, and a pass must not hold a long query.
    #[test]
    fn a_batch_is_bounded() {
        assert!((1..=100).contains(&BATCH_SIZE), "{BATCH_SIZE}");
    }

    // -- the attempts --------------------------------------------------

    /// A picture that does not decode will not decode tomorrow: given up
    /// on at once, not retried every pass.
    #[test]
    fn an_undecodable_picture_is_given_up_on_at_once() {
        assert_eq!(attempts_after(Failure::Undecodable, 0), MAX_ATTEMPTS);
        assert!(!is_candidate(
            "image/jpeg",
            None,
            attempts_after(Failure::Undecodable, 0)
        ));
    }

    /// A storage that did not answer may answer next pass: one attempt
    /// spent, a few in all.
    #[test]
    fn a_transient_failure_spends_one_attempt() {
        const { assert!(MAX_ATTEMPTS > 1) };
        assert_eq!(attempts_after(Failure::Transient, 0), 1);
        assert_eq!(attempts_after(Failure::Transient, 1), 2);
    }

    /// However many failures, the count stays within the column and past
    /// the bound once reached: never retried forever.
    #[test]
    fn the_attempts_never_overflow_nor_fall_back() {
        assert_eq!(attempts_after(Failure::Transient, i16::MAX), i16::MAX);
        assert_eq!(attempts_after(Failure::Undecodable, i16::MAX), i16::MAX);
        let mut attempts = 0;
        for _ in 0..MAX_ATTEMPTS {
            assert!(is_candidate("image/webp", None, attempts));
            attempts = attempts_after(Failure::Transient, attempts);
        }
        assert!(!is_candidate("image/webp", None, attempts));
    }
}
