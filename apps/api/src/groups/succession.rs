//! Who owns a group when nobody chose to (#323).
//!
//! A group can lose its owner to the account purge with nobody eligible to
//! inherit it — only members support deactivated are left (`successor`, in
//! `jobs::account_purge`). Two ways out, controller's decision of
//! 2026-10-04:
//!
//! - when the superadmin reactivates a member of such a group,
//!   [`assign_heir`] runs `successor` again and hands the group to whoever it
//!   names;
//! - the superadmin can designate an owner among its active members
//!   ([`check_designation`], `user_admin::admin::designate_owner`).
//!
//! Whoever becomes owner this way, or through the purge, is told: their
//! membership row is stamped ([`stamp_inheritance`]), the home page shows a
//! notice until they acknowledge it, and an email goes out on the purge job's
//! next pass ([`send_ownership_notices`]).

use serde_json::json;
use uuid::Uuid;

use crate::jobs::account_purge::{successor, RemainingMember};
use manage_our_home_shared::validation::groups::OwnershipReason;

/// Why the superadmin cannot designate this member owner of this group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignationRefusal {
    /// The group has an owner: designating one is only for a group left
    /// without.
    GroupHasOwner,
    /// The account is not a member of the group.
    NotAMember,
    /// The account is deactivated, or its deletion is pending: only an
    /// active member can be designated.
    NotActive,
}

/// Whether the superadmin may make `target` the owner of a group, `None`
/// when the account is not one of its members.
pub fn check_designation(
    has_owner: bool,
    target: Option<&RemainingMember>,
) -> Result<(), DesignationRefusal> {
    if has_owner {
        return Err(DesignationRefusal::GroupHasOwner);
    }
    match target {
        None => Err(DesignationRefusal::NotAMember),
        Some(m) if m.deactivated || m.pending_deletion => Err(DesignationRefusal::NotActive),
        Some(_) => Ok(()),
    }
}

/// The member who inherits a group, if it has no owner: [`successor`] over
/// its members. A group that still has one is left alone.
pub fn heir_if_ownerless(has_owner: bool, members: &[RemainingMember]) -> Option<Uuid> {
    if has_owner {
        return None;
    }
    successor(members)
}

type Tx<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

/// Every membership of `group_id`, locked for the rest of the transaction,
/// and whether one of them is the owner's. The lock is what keeps two
/// reactivations, or a reactivation and a designation, from both handing
/// out the group: the second waits, then finds the owner.
///
/// `group_members` is under a forced RLS policy: the caller's transaction
/// must be on a role that bypasses it (the admin pool), or this reads no
/// row.
pub(crate) async fn lock_members(
    tx: &mut Tx<'_>,
    group_id: Uuid,
) -> sqlx::Result<(bool, Vec<RemainingMember>)> {
    let rows = sqlx::query!(
        r#"SELECT gm.user_id, gm.role::text = 'owner' AS "is_owner!",
                  gm.role::text = 'admin' AS "is_admin!", gm.joined_at,
                  u.deactivated_at IS NOT NULL AS "deactivated!",
                  u.deletion_requested_at IS NOT NULL AS "pending_deletion!"
           FROM group_members gm JOIN users u ON u.id = gm.user_id
           WHERE gm.group_id = $1 FOR UPDATE OF gm"#,
        group_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let has_owner = rows.iter().any(|r| r.is_owner);
    let members = rows
        .into_iter()
        .map(|r| RemainingMember {
            user_id: r.user_id,
            is_admin: r.is_admin,
            joined_at: r.joined_at,
            deactivated: r.deactivated,
            pending_deletion: r.pending_deletion,
        })
        .collect();
    Ok((has_owner, members))
}

/// Makes `user_id` the owner of `group_id`, for `reason`, and stamps the
/// news on the membership: the home-page notice shows until acknowledged,
/// and the email goes out on the purge job's next pass. The group must have
/// no owner at this point (`one_owner_per_group`).
pub(crate) async fn stamp_inheritance(
    tx: &mut Tx<'_>,
    group_id: Uuid,
    user_id: Uuid,
    reason: OwnershipReason,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE group_members
           SET role = 'owner', ownership_inherited_at = now(),
               ownership_inherited_reason = $3, ownership_notice_seen_at = NULL,
               ownership_email_sent_at = NULL
           WHERE group_id = $1 AND user_id = $2"#,
        group_id,
        user_id,
        reason.as_str()
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Hands `group_id` to the member [`heir_if_ownerless`] names, if it has no
/// owner, and writes `transfer_ownership`'s audit entry with no actor and
/// the reason. Returns the new owner; `None` when the group has an owner,
/// or nobody eligible to inherit it.
pub(crate) async fn assign_heir(
    tx: &mut Tx<'_>,
    group_id: Uuid,
    reason: OwnershipReason,
) -> sqlx::Result<Option<Uuid>> {
    let (has_owner, members) = lock_members(tx, group_id).await?;
    let Some(heir) = heir_if_ownerless(has_owner, &members) else {
        return Ok(None);
    };
    stamp_inheritance(tx, group_id, heir, reason).await?;
    crate::audit::record(
        tx,
        None,
        "ownership_transferred",
        "group",
        &group_id.to_string(),
        json!({ "new_owner_id": heir, "reason": reason.as_str() }),
    )
    .await?;
    Ok(Some(heir))
}

/// Subject of the email [`send_ownership_notices`] sends. Names no group:
/// the body does.
pub const OWNERSHIP_NOTICE_SUBJECT: &str = "Vous êtes désormais propriétaire d'un groupe";

/// One pass of ownership emails: every membership stamped by
/// [`stamp_inheritance`] whose email has not gone gets one, sent through
/// `send(to, subject, body)` whatever the member's reminder channel, and
/// `ownership_email_sent_at` is stamped only once it has gone. A refused
/// email leaves the row to be tried again on the next pass. Each row is
/// handled in its own transaction, locked and read again first.
///
/// `pool` must bypass RLS (`group_members` is under a forced policy): on
/// any other role the pass refuses rather than find nothing to send.
pub async fn send_ownership_notices<F, Fut>(
    pool: &sqlx::PgPool,
    groups_url: &str,
    privacy_policy_url: &str,
    send: F,
) -> anyhow::Result<()>
where
    F: Fn(String, String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    use anyhow::Context;
    let mut conn = pool.acquire().await?;
    crate::attachment_reconcile::ensure_bypasses_rls(&mut conn)
        .await
        .context(
            "ownership notices refusing to run: `group_members` is FORCE ROW LEVEL \
             SECURITY, so on a role that does not bypass it no pending email is ever \
             found. Point ADMIN_DATABASE_URL at the BYPASSRLS admin_role.",
        )?;
    drop(conn);

    let pending = sqlx::query!(
        r#"SELECT group_id, user_id FROM group_members
           WHERE ownership_inherited_at IS NOT NULL AND ownership_email_sent_at IS NULL"#
    )
    .fetch_all(pool)
    .await?;

    for p in pending {
        let mut tx = crate::db::begin(pool).await?;
        let Some(row) = sqlx::query!(
            r#"SELECT u.email, g.name, gm.ownership_inherited_reason AS "reason!"
               FROM group_members gm
               JOIN users u ON u.id = gm.user_id
               JOIN groups g ON g.id = gm.group_id
               WHERE gm.group_id = $1 AND gm.user_id = $2
                 AND gm.ownership_inherited_at IS NOT NULL
                 AND gm.ownership_email_sent_at IS NULL
                 AND u.deleted_at IS NULL
               FOR UPDATE OF gm"#,
            p.group_id,
            p.user_id
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            tx.rollback().await?;
            continue;
        };
        let Some(reason) = OwnershipReason::parse(&row.reason) else {
            tx.rollback().await?;
            continue;
        };
        let body = manage_our_home_shared::validation::rgpd::ownership_inherited_email_body(
            &row.name,
            reason,
            groups_url,
            privacy_policy_url,
        );
        if let Err(e) = send(row.email, OWNERSHIP_NOTICE_SUBJECT.to_string(), body).await {
            // Never log the address — PII, as in `EmailSender::send`.
            tracing::error!(error = ?e, group_id = %p.group_id, user_id = %p.user_id,
                "ownership notice not sent");
            tx.rollback().await?;
            continue;
        }
        sqlx::query!(
            "UPDATE group_members SET ownership_email_sent_at = now()
             WHERE group_id = $1 AND user_id = $2",
            p.group_id,
            p.user_id
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn member(id: u128, is_admin: bool, day: u32) -> RemainingMember {
        RemainingMember {
            user_id: Uuid::from_u128(id),
            is_admin,
            joined_at: Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).unwrap(),
            deactivated: false,
            pending_deletion: false,
        }
    }

    // -- check_designation ----------------------------------------------------

    #[test]
    fn an_active_member_of_an_ownerless_group_can_be_designated() {
        assert_eq!(check_designation(false, Some(&member(1, false, 1))), Ok(()));
        assert_eq!(check_designation(false, Some(&member(1, true, 1))), Ok(()));
    }

    #[test]
    fn nobody_is_designated_in_a_group_that_has_an_owner() {
        assert_eq!(
            check_designation(true, Some(&member(1, true, 1))),
            Err(DesignationRefusal::GroupHasOwner)
        );
        assert_eq!(
            check_designation(true, None),
            Err(DesignationRefusal::GroupHasOwner)
        );
    }

    #[test]
    fn an_account_outside_the_group_cannot_be_designated() {
        assert_eq!(
            check_designation(false, None),
            Err(DesignationRefusal::NotAMember)
        );
    }

    #[test]
    fn a_deactivated_member_cannot_be_designated() {
        let m = RemainingMember {
            deactivated: true,
            ..member(1, true, 1)
        };
        assert_eq!(
            check_designation(false, Some(&m)),
            Err(DesignationRefusal::NotActive)
        );
    }

    /// "Parmi les membres actifs": an account whose deletion is pending
    /// would hand the group back to the purge within 30 days.
    #[test]
    fn a_member_awaiting_deletion_cannot_be_designated() {
        let m = RemainingMember {
            pending_deletion: true,
            ..member(1, true, 1)
        };
        assert_eq!(
            check_designation(false, Some(&m)),
            Err(DesignationRefusal::NotActive)
        );
    }

    // -- heir_if_ownerless ------------------------------------------------------

    #[test]
    fn an_ownerless_group_goes_to_its_successor() {
        let members = [
            RemainingMember {
                deactivated: true,
                ..member(1, true, 1)
            },
            member(2, false, 3),
            member(3, true, 9),
        ];
        assert_eq!(heir_if_ownerless(false, &members), Some(Uuid::from_u128(3)));
    }

    #[test]
    fn a_group_with_an_owner_has_no_heir() {
        assert_eq!(heir_if_ownerless(true, &[member(2, true, 3)]), None);
    }

    #[test]
    fn an_ownerless_group_of_deactivated_members_still_has_no_heir() {
        let members = [RemainingMember {
            deactivated: true,
            ..member(1, true, 1)
        }];
        assert_eq!(heir_if_ownerless(false, &members), None);
    }
}
