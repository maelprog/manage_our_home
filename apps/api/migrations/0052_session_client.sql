-- #754: the client a session was opened from, the Android app or the web,
-- so the member can tell their sessions apart (shown by #755).
--
-- Deduced at the opening from the User-Agent
-- (`manage_our_home_shared::mobile_app::client_of_user_agent`: the marker
-- the app's WebView appends, `MHomeApp`), and only that verdict is kept:
-- neither the User-Agent nor the address, nor any device model (data
-- minimization, #143). A password login reaches apps/api through apps/web,
-- which relays only the marker when the request was the app's.
--
-- Nullable: the sessions opened before this column have no known client
-- and stay NULL. They are gone within 30 days at most (`expires_at`).
ALTER TABLE sessions
    ADD COLUMN client TEXT CHECK (client IN ('web', 'app'));

COMMENT ON COLUMN sessions.client IS
    'Client the session was opened from (issue #754): ''app'' (the Android app, by its User-Agent marker) or ''web''. NULL for a session opened before it was recorded. The User-Agent itself is not kept.';
