-- #464: a message can answer another one of the same family.
--
-- `reply_to_id` names the parent. The API only accepts a parent of the same
-- family (422 `reply_to_invalid` otherwise, `messagerie/messages.rs`); the
-- column itself only says the parent exists.
--
-- Deleting the parent keeps the reply: the link turns NULL and the reply is
-- then answered as a reply to nothing (`reply_to: null`). The parent's
-- excerpt shown with a reply is decrypted from the parent's own `content` on
-- each read; nothing of it is copied here, in clear or otherwise.
--
-- `messages_isolation` (0008) already covers the new column: it filters
-- whole rows on `group_id`.

ALTER TABLE messages
    ADD COLUMN reply_to_id UUID NULL REFERENCES messages(id) ON DELETE SET NULL;

-- Deleting a message looks up its replies to set their link to NULL: without
-- this index, each deletion scans the whole table.
CREATE INDEX messages_reply_to_id_idx ON messages (reply_to_id);
