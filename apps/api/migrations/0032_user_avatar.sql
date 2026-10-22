-- #467: a profile photo.
--
-- `avatar_key` names the object holding it in the attachments bucket,
-- `avatars/{user_id}/{uuid}`; NULL when the account has none. The object is
-- the photo re-encoded by apps/api (`avatar.rs`): a JPEG square of fixed
-- side, with no metadata. The upload as sent is never stored.
--
-- A key, not the bytes: the photo lives in object storage like every other
-- file the service holds, and the reconciliation pass
-- (`attachment_reconcile`) reads this column to tell an avatar from an
-- orphan. The account purge clears it and deletes the object.
--
-- `users` is under no RLS policy: the read rule (a member of a common group
-- only) is written in the handler's query, as for `GET
-- /groups/:id/members/:user_id` (#466).

ALTER TABLE users ADD COLUMN avatar_key TEXT NULL;
