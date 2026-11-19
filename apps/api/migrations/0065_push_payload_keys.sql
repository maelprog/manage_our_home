-- #862: a click on a pushed notification marks its entry of the
-- notification centre read. The push carries the identifier of that entry
-- (`notifications.id`, an opaque UUID) and nothing else — the text shown
-- stays the fixed, neutral one of its kind (user's decision of
-- 2026-11-19). A push with a payload must be encrypted for the browser
-- that subscribed (RFC 8291), with the two keys of its subscription:
--
-- - `p256dh`: the browser's P-256 public key, the uncompressed point
--   (65 bytes) in unpadded base64url;
-- - `auth`: the 16-byte authentication secret, unpadded base64url.
--
-- Both or neither. NULL for a device registered before this migration:
-- it is pushed without payload, as before, until its page registers it
-- again (on every view once the permission is granted), keys included.
-- The push service only ever sees the ciphertext. Goes with the device.
ALTER TABLE push_subscriptions
    ADD COLUMN p256dh TEXT CHECK (char_length(p256dh) <= 100),
    ADD COLUMN auth   TEXT CHECK (char_length(auth) <= 100),
    ADD CONSTRAINT push_subscriptions_keys_together
        CHECK ((p256dh IS NULL) = (auth IS NULL));
