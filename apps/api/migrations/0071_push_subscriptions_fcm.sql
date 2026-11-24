-- An Android device's FCM registration token in `push_subscriptions`
-- (#1001), first part of the native push of the Android app.
--
-- The app is a Capacitor WebView (`apps/mobile/README.md`): it has no Web
-- Push, so what reaches the notification centre never reaches the phone.
-- Its device is to be pushed through Firebase Cloud Messaging, which knows
-- it by a registration token. 0020 kept `platform` for such devices; it
-- now admits 'fcm' besides 'web':
--
-- - `endpoint` is the FCM registration token — opaque text, not a URL —
--   and stays unique, across both platforms;
-- - `p256dh` and `auth` (0065) are the keys a browser gives to encrypt a
--   Web Push payload for it: an 'fcm' row has neither
--   (`push_subscriptions_fcm_without_keys`).
--
-- Every other column means the same for both: the dates of registration
-- and delivery, the failures that make a device forgotten, the pushes
-- pending (0037). APNs (iOS) is not admitted.
--
-- No code writes an 'fcm' row yet (#1002). The web push reads 'web' rows
-- alone (`jobs::scheduled_notifications::push_devices`,
-- `notifications::preferences::received`): a token is never handed to
-- `notifications::push::send`. The settings' device count, the removal of
-- every device, the art. 15 export (which carries `platform`) and the
-- account purge take both.
ALTER TABLE push_subscriptions DROP CONSTRAINT push_subscriptions_platform_check;
ALTER TABLE push_subscriptions
    ADD CONSTRAINT push_subscriptions_platform_check CHECK (platform IN ('web', 'fcm')),
    ADD CONSTRAINT push_subscriptions_fcm_without_keys
        CHECK (platform <> 'fcm' OR (p256dh IS NULL AND auth IS NULL));

COMMENT ON TABLE push_subscriptions IS
    'One row per device that accepted notifications for an account (0020): '
    'a browser''s Web Push endpoint (platform ''web'', with its payload keys, 0065) '
    'or an Android device''s FCM registration token (platform ''fcm'', no keys, 0071). '
    'Account-level, no RLS; every read filters on user_id. The web push reads ''web'' rows only.';
