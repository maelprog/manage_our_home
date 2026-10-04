-- Issue #323. A member who becomes the owner of a group without asking for
-- it is told, by email and by a notice on the home page (controller's
-- decision of 2026-10-04). Three ways it happens: the previous owner's
-- account is purged (`jobs::account_purge`), a member of a group left with
-- no owner is reactivated by the superadmin, or the superadmin designates
-- an owner for such a group (`groups::succession`).
--
-- The news belongs to the membership it is about, so it lives on its
-- `group_members` row: leaving the group, or the purge of the account,
-- deletes it with the row. Inheriting again rewrites all four columns.
--
--   ownership_inherited_at / _reason — when and why the member became the
--     owner; NULL (both together): never inherited, or a membership older
--     than this migration;
--   ownership_notice_seen_at — when the member acknowledged the notice on
--     the home page; NULL: not yet, the notice shows;
--   ownership_email_sent_at — when the email went out; NULL: not yet, the
--     purge job's next pass sends it (a refused email is retried).
ALTER TABLE group_members
    ADD COLUMN ownership_inherited_at TIMESTAMPTZ,
    ADD COLUMN ownership_inherited_reason TEXT,
    ADD COLUMN ownership_notice_seen_at TIMESTAMPTZ,
    ADD COLUMN ownership_email_sent_at TIMESTAMPTZ,
    ADD CONSTRAINT group_members_ownership_inheritance_complete
        CHECK ((ownership_inherited_at IS NULL) = (ownership_inherited_reason IS NULL)),
    ADD CONSTRAINT group_members_ownership_inherited_reason_known
        CHECK (ownership_inherited_reason IN
               ('account_purged', 'member_reactivated', 'designated_by_support'));

-- The purge job's pass reads the emails still to send.
CREATE INDEX group_members_ownership_email_pending
    ON group_members (ownership_inherited_at)
    WHERE ownership_inherited_at IS NOT NULL AND ownership_email_sent_at IS NULL;

COMMENT ON COLUMN group_members.ownership_inherited_at IS
    'When the member became the owner of the group without asking for it (issue #323). NULL: never.';
COMMENT ON COLUMN group_members.ownership_inherited_reason IS
    'Why (issue #323): account_purged, member_reactivated or designated_by_support. NULL with ownership_inherited_at.';
COMMENT ON COLUMN group_members.ownership_notice_seen_at IS
    'When the member acknowledged the home-page notice of that inheritance (issue #323). NULL: not yet.';
COMMENT ON COLUMN group_members.ownership_email_sent_at IS
    'When the email announcing that inheritance went out (issue #323). NULL: not yet.';
