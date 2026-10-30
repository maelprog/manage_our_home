pub mod agenda;
pub mod attachment_reconcile;
pub mod audit;
pub mod auth;
pub mod avatar;
pub mod budget;
pub mod client_ip;
pub mod crypto;
pub mod csp_report;
pub mod csp_report_throttle;
pub mod db;
pub mod dev_seed;
pub mod email;
pub mod error;
pub mod google_calendar;
pub mod grocery_list;
pub mod groups;
pub mod health;
pub mod jobs;
pub mod messagerie;
pub mod migrations;
pub mod notifications;
pub mod outbound_http;
pub mod public_destination;
pub mod rate_limit;
pub mod recipes;
pub mod rgpd;
pub mod stocks;
pub mod storage;
pub mod user_admin;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use axum::Router;
use oauth2::{basic::BasicClient, EndpointNotSet, EndpointSet};
use sqlx::PgPool;
use tower_cookies::CookieManagerLayer;

use crate::email::EmailSender;
use crate::storage::Storage;

/// Google's client: auth + token URLs configured, no device-auth/introspection/revocation.
pub type GoogleOauthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub google_oauth: GoogleOauthClient,
    /// Google's OpenID Connect userinfo endpoint, queried by
    /// `auth::oauth_google::callback` after the code exchange. Always
    /// `auth::oauth_google::GOOGLE_USERINFO_URL` in production; a field so
    /// the callback's account resolution can be driven end to end against
    /// a local listener, like the token endpoint already is (#194).
    pub google_userinfo_url: String,
    pub email: EmailSender,
    /// Base URL of this API, used to build links sent in emails.
    pub public_base_url: String,
    /// Base URL of the SvelteKit frontend, used for post-OAuth redirects.
    pub frontend_base_url: String,
    /// Symmetric key passed to `pgp_sym_encrypt`/`pgp_sym_decrypt` for
    /// OAuth refresh tokens (pgcrypto, AC #3). Loaded from env, never
    /// logged.
    pub oauth_encryption_key: String,
    /// Symmetric key passed to `pgp_sym_encrypt`/`pgp_sym_decrypt` for
    /// Messagerie content (pgcrypto). Dedicated and isolated from
    /// `oauth_encryption_key` per Epic #7 decision #1.
    pub message_encryption_key: String,
    /// Symmetric key passed to `pgp_sym_encrypt`/`pgp_sym_decrypt` for
    /// Google Calendar ICS feed URLs (Epic #9). Dedicated and isolated
    /// from `oauth_encryption_key`/`message_encryption_key`, same
    /// per-secret-key-per-epic pattern as Messagerie.
    pub calendar_feed_encryption_key: String,
    /// Per-family broadcast registry for Messagerie's WS push (Epic #7
    /// decision #3), fed by Postgres `LISTEN/NOTIFY` so every API replica
    /// pushes every write (#429).
    pub message_hubs: messagerie::MessageHub,
    /// How often an open Messagerie WS connection re-validates that the
    /// caller is still a group member, bounding how long a removed member
    /// stays connected (Epic #7 AC #7). 30s in production (`main.rs`);
    /// integration tests shorten it so the removal-disconnect test doesn't
    /// have to sleep out the real bound. See `messagerie/ws.rs` for why
    /// this is a periodic tick rather than a per-event check.
    pub message_ws_recheck_interval: std::time::Duration,
    pub secure_cookies: bool,
    /// MinIO/S3 client for event file attachments (architecture.md epic #10).
    pub storage: Storage,
    /// Peers whose `X-Forwarded-For` is believed when resolving a client
    /// address (#178). Empty in integration tests, which reach the router
    /// without a socket at all — see `client_ip::resolve`.
    pub trusted_proxies: std::sync::Arc<client_ip::TrustedProxies>,
    /// Per-(address, email) lock on login attempts (#178): every attempt is
    /// counted when admitted, before the argon2 work, and a success clears
    /// the pair. Counted in Postgres (`login_throttle`, #427), so every
    /// replica shares one count and a restart keeps it.
    pub login_throttle: std::sync::Arc<auth::throttle::LoginThrottle>,
    /// Per-address limit on `POST /csp-report` (#375), keyed like
    /// `login_throttle` and, like it, counted in Postgres
    /// (`csp_report_throttle`, #428).
    pub csp_report_throttle: std::sync::Arc<csp_report_throttle::ReportThrottle>,
    /// Cumulative count of logins per ending, published as an aggregate
    /// and never per request (#178 bis).
    pub login_branches: std::sync::Arc<auth::timing::BranchCounters>,
    /// Second pool, connected as `admin_role` (`BYPASSRLS`). Used by the
    /// handlers gated behind `SuperAdminUser` (Epic #8, see
    /// `src/user_admin/mod.rs` for why this is a narrow, deliberate
    /// exception to the RLS boundary rather than a general bypass), and by
    /// four background jobs that serve no request and read or write across
    /// every family: attachment reconcile (`jobs::attachment_reconcile`,
    /// #215), retention purge (`jobs::retention_purge`, #138), account
    /// purge (`jobs::account_purge`, #139) and the reminder worker
    /// (`jobs::scheduled_notifications`, #293). Request handlers outside
    /// `SuperAdminUser` never use it.
    pub admin_db: PgPool,
    /// How long a client may take to send a request body, on every route
    /// (#219). `BodyReadLimits::PRODUCTION` in production; flow tests
    /// shorten it so a slow body is cut in a second rather than in ten.
    pub body_read_limits: manage_our_home_http_guard::BodyReadLimits,
    /// Uploads this process holds in memory at once, per account and in
    /// all (#219). Taken by `agenda::attachments::upload_attachment` before
    /// it reads the body, and by `avatar::put_avatar` (#467).
    pub upload_gate: std::sync::Arc<manage_our_home_http_guard::UploadGate<uuid::Uuid>>,
    /// The server's VAPID identity for reminders sent as a notification
    /// (#306), from `VAPID_PRIVATE_KEY`/`VAPID_SUBJECT`. `None`: no key
    /// configured, notifications are off on this server — no device can
    /// subscribe, and members who chose them are warned they get nothing.
    pub push: Option<std::sync::Arc<notifications::push::Vapid>>,
    /// Open Food Facts, asked for the product behind a scanned code (#402).
    /// `stocks::openfoodfacts::DEFAULT_BASE_URL` unless
    /// `OPENFOODFACTS_BASE_URL` says otherwise; tests point it at a local
    /// stub, or at nothing.
    pub openfoodfacts_base_url: String,
    /// The limit on reads sent to Open Food Facts (#402), for the whole
    /// server: counted in Postgres (`rate_limit_log`, #428).
    pub off_throttle: std::sync::Arc<stocks::openfoodfacts::Throttle>,
    /// The client the recipe import (#405) fetches pages with:
    /// `outbound_http::recipe_import_client()`, which reaches public
    /// addresses only. The flow tests use the same configuration
    /// (`outbound_http::recipe_import_builder`), with a test name pinned to
    /// their local server.
    pub recipe_import_client: reqwest::Client,
    /// The per-member limit on recipe imports (#405), counted in
    /// Postgres (`rate_limit_log`, #428).
    pub recipe_import_throttle: std::sync::Arc<recipes::import::ImportThrottle>,
    /// Whether the process is stopping (#424): `/readyz` answers 503 from
    /// then on, and every Messagerie WebSocket closes with a Close frame.
    /// `main.rs` triggers it on SIGTERM; tests trigger it themselves.
    pub shutdown: manage_our_home_http_guard::Shutdown,
}

/// Body of the 408 a too-slow request body gets (#219), in the API's usual
/// `{"error": ...}` shape. Status and `Connection: close` are set by the
/// middleware.
fn body_read_timeout_response() -> axum::response::Response {
    use axum::response::IntoResponse;
    axum::Json(serde_json::json!({ "error": "request_timeout" })).into_response()
}

/// Body of the 403 a cross-origin request gets (#223). Status is set by
/// the middleware.
fn cross_origin_response() -> axum::response::Response {
    use axum::response::IntoResponse;
    axum::Json(serde_json::json!({ "error": "cross_origin_request" })).into_response()
}

pub fn build_router(state: AppState) -> Router {
    // Derive the login decoy hash now rather than inside whichever login
    // happens to be first: it costs one argon2id, and paying it in a
    // request would make that one request the odd one out on a stopwatch —
    // the exact signal #178 exists to remove.
    let _ = crypto::decoy_hash();

    // Every route, not only the uploads: a JSON body can be dripped as
    // slowly as a multipart one. It spares the Messagerie WebSocket, whose
    // upgrade has no body to time (see `manage_our_home_http_guard::body`).
    let body_guard = manage_our_home_http_guard::BodyGuard {
        limits: state.body_read_limits,
        render: body_read_timeout_response,
    };

    // Every unsafe method and the Messagerie WebSocket's upgrade (#223):
    // trusted when `Origin` is the frontend's own origin — its pages open
    // the socket, on another port in CI — and otherwise judged on
    // `Sec-Fetch-Site`/`Origin`. apps/web's calls over the internal network
    // carry neither header and pass. See `manage_our_home_http_guard::origin`.
    let origin_guard = manage_our_home_http_guard::OriginGuard {
        trusted_origin: manage_our_home_http_guard::origin_of(&state.frontend_base_url)
            .map(Into::into),
        render: cross_origin_response,
    };

    Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/verify-email", get(auth::verify_email))
        .route("/auth/verify-email/resend", post(auth::resend_verification))
        .route("/auth/login", post(auth::login))
        .route("/auth/me", get(auth::me))
        .route(
            "/auth/age-declaration",
            post(auth::age_declaration::declare),
        )
        .route(
            "/auth/terms-acceptance",
            post(auth::terms_acceptance::accept),
        )
        .route("/auth/google/start", get(auth::oauth_google::start))
        .route("/auth/google/callback", get(auth::oauth_google::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/sessions", get(auth::list_sessions))
        .route("/auth/sessions/:id/revoke", post(auth::revoke_one_session))
        .route(
            "/auth/sessions/revoke-all",
            post(auth::revoke_every_session),
        )
        .route("/auth/password/forgot", post(auth::forgot_password))
        .route("/auth/password/reset", post(auth::reset_password))
        .route("/settings/password/change", post(auth::change_password))
        .route("/settings/password/set", post(auth::set_password))
        .route("/account/delete", post(auth::delete_account))
        .route("/account/delete/cancel", post(auth::cancel_delete_account))
        .route("/account/export", get(rgpd::export_account))
        .route(
            "/me/avatar",
            // The body limit on `PUT` only: `MethodRouter::layer` applies to
            // the methods registered before it.
            put(avatar::put_avatar)
                .layer(DefaultBodyLimit::max(avatar::MAX_AVATAR_BODY_BYTES))
                .get(avatar::get_own_avatar)
                .delete(avatar::delete_avatar),
        )
        .route(
            "/account/notifications",
            get(notifications::preferences::get_settings)
                .put(notifications::preferences::update_settings),
        )
        .route(
            "/notifications",
            get(notifications::center::list_notifications),
        )
        .route(
            "/notifications/unread_count",
            get(notifications::center::unread_count),
        )
        .route(
            "/notifications/read",
            post(notifications::center::mark_all_read),
        )
        .route(
            "/notifications/:id/read",
            post(notifications::center::mark_read),
        )
        .route(
            "/account/push-subscriptions",
            post(notifications::preferences::subscribe)
                .delete(notifications::preferences::unsubscribe_all),
        )
        .route(
            "/account/push-subscriptions/received",
            post(notifications::preferences::received),
        )
        // With `/auth/logout`, the only routes a restricted session opens
        // (#289).
        .route("/account/deactivated", get(auth::deactivated::status))
        .route(
            "/account/deactivated/reactivation-request",
            post(auth::deactivated::request_reactivation),
        )
        .route("/privacy-policy", get(rgpd::privacy_policy))
        .route("/legal-notice", get(rgpd::legal_notice))
        .route("/terms-of-service", get(rgpd::terms_of_service))
        .route(
            "/terms-of-service/announced",
            get(rgpd::announced_terms_of_service),
        )
        .route(
            "/groups",
            post(groups::create_group).get(groups::list_groups),
        )
        .route(
            "/groups/:id",
            get(groups::get_group)
                .patch(groups::rename_group)
                .delete(groups::delete_group),
        )
        .route(
            "/groups/:id/transfer-ownership",
            post(groups::transfer_ownership),
        )
        .route("/groups/:id/invitations", post(groups::create_invitation))
        .route(
            "/groups/invitations/:token/accept",
            post(groups::accept_invitation),
        )
        .route(
            "/groups/:id/members/:user_id/role",
            post(groups::change_role),
        )
        .route(
            "/groups/:id/members/:user_id",
            get(groups::get_member).delete(groups::remove_member),
        )
        .route(
            "/groups/:id/members/:user_id/avatar",
            get(avatar::get_member_avatar),
        )
        .route("/groups/:id/leave", post(groups::leave_group))
        .route(
            "/groups/:id/ownership-notice/seen",
            post(groups::acknowledge_ownership_notice),
        )
        .route(
            "/groups/:id/events",
            post(agenda::events::create_event).get(agenda::events::list_events),
        )
        .route(
            "/groups/:id/events/:event_id",
            get(agenda::events::get_event)
                .patch(agenda::events::update_event)
                .delete(agenda::events::delete_event),
        )
        .route(
            "/groups/:id/events/:event_id/reminders",
            post(agenda::reminders::create_reminder),
        )
        .route(
            "/groups/:id/events/:event_id/reminders/:reminder_id",
            delete(agenda::reminders::delete_reminder),
        )
        .route(
            "/groups/:id/events/:event_id/attachments",
            // The body limit is raised on the upload method only, and
            // `MethodRouter::layer` applies to the methods registered before
            // it — hence `.get(...)` after, which needs no body at all.
            post(agenda::attachments::upload_attachment)
                .layer(DefaultBodyLimit::max(storage::MAX_UPLOAD_BODY_BYTES))
                .get(agenda::attachments::list_attachments),
        )
        .route(
            "/groups/:id/events/:event_id/attachments/:attachment_id/download",
            get(agenda::attachments::download_attachment),
        )
        .route(
            "/groups/:id/events/:event_id/attachments/:attachment_id",
            delete(agenda::attachments::delete_attachment),
        )
        .route(
            "/groups/:id/stock-items",
            post(stocks::items::create_stock_item).get(stocks::items::list_stock_items),
        )
        .route(
            "/groups/:id/stock-items/scan",
            post(stocks::scan::scan_stock_item),
        )
        .route(
            "/groups/:id/stock-items/reception",
            get(stocks::reception::get_reception).post(stocks::reception::receive),
        )
        .route(
            "/groups/:id/stock-items/:item_id",
            get(stocks::items::get_stock_item)
                .patch(stocks::items::update_stock_item)
                .delete(stocks::items::delete_stock_item),
        )
        .route(
            "/groups/:id/recipes",
            post(recipes::crud::create_recipe).get(recipes::crud::list_recipes),
        )
        .route(
            "/groups/:id/recipes/import",
            post(recipes::import::import_recipe),
        )
        .route(
            "/groups/:id/recipes/suggestions",
            get(recipes::suggestions::suggest_recipes),
        )
        .route(
            "/groups/:id/recipes/meal-history",
            get(recipes::meal_history::list_meal_history),
        )
        .route(
            "/groups/:id/recipes/selections",
            get(recipes::selections::list_selections),
        )
        .route(
            "/groups/:id/recipes/:recipe_id",
            get(recipes::crud::get_recipe)
                .patch(recipes::crud::update_recipe)
                .delete(recipes::crud::delete_recipe),
        )
        .route(
            "/groups/:id/recipes/:recipe_id/meal-history",
            post(recipes::meal_history::log_meal),
        )
        .route(
            "/groups/:id/recipes/:recipe_id/selection",
            post(recipes::selections::select_recipe)
                .patch(recipes::selections::update_selection)
                .delete(recipes::selections::deselect_recipe),
        )
        .route(
            "/groups/:id/recipes/:recipe_id/selection/done",
            get(recipes::outcome::preview_done).post(recipes::outcome::apply_done),
        )
        .route(
            "/groups/:id/recipes/:recipe_id/selection/abandoned",
            get(recipes::outcome::preview_abandoned).post(recipes::outcome::apply_abandoned),
        )
        .route(
            "/groups/:id/grocery-items",
            post(grocery_list::items::create_grocery_item)
                .get(grocery_list::items::list_grocery_items),
        )
        .route(
            "/groups/:id/grocery-items/generate",
            post(grocery_list::items::generate_grocery_items),
        )
        .route(
            "/groups/:id/grocery-items/:item_id",
            get(grocery_list::items::get_grocery_item)
                .patch(grocery_list::items::update_grocery_item)
                .delete(grocery_list::items::delete_grocery_item),
        )
        .route(
            "/groups/:id/grocery-items/:item_id/check",
            post(grocery_list::items::check_grocery_item),
        )
        .route(
            "/groups/:id/grocery-items/:item_id/price",
            post(budget::entries::set_grocery_item_price),
        )
        .route(
            "/groups/:id/budget-entries",
            post(budget::entries::create_budget_entry).get(budget::entries::list_budget_entries),
        )
        .route(
            "/groups/:id/budget-entries/summary",
            get(budget::entries::budget_summary),
        )
        .route(
            "/groups/:id/budget-entries/:entry_id",
            get(budget::entries::get_budget_entry)
                .patch(budget::entries::update_budget_entry)
                .delete(budget::entries::delete_budget_entry),
        )
        .route(
            "/groups/:id/budget-categories",
            post(budget::categories::create_budget_category)
                .get(budget::categories::list_budget_categories),
        )
        .route(
            "/groups/:id/budget-categories/:category_id",
            axum::routing::patch(budget::categories::update_budget_category)
                .delete(budget::categories::delete_budget_category),
        )
        .route(
            "/groups/:id/budget-settings",
            get(budget::categories::get_budget_settings)
                .put(budget::categories::put_budget_settings),
        )
        .route(
            "/groups/:id/budget-alert-settings",
            get(budget::alerts::get_alert_settings).patch(budget::alerts::patch_alert_settings),
        )
        .route(
            "/groups/:id/messages",
            post(messagerie::messages::create_message).get(messagerie::messages::list_messages),
        )
        .route("/groups/:id/messages/ws", get(messagerie::ws::message_ws))
        .route(
            "/groups/:id/messages/read",
            post(messagerie::messages::mark_messages_read),
        )
        .route(
            "/groups/:id/messages/unread_count",
            get(messagerie::messages::unread_count),
        )
        .route(
            "/groups/:id/messages/:message_id",
            axum::routing::patch(messagerie::messages::update_message)
                .delete(messagerie::messages::delete_message),
        )
        .route(
            "/groups/:id/calendar-imports",
            post(google_calendar::imports::create_calendar_import)
                .get(google_calendar::imports::list_calendar_imports),
        )
        .route(
            "/groups/:id/calendar-imports/:import_id",
            delete(google_calendar::imports::delete_calendar_import),
        )
        .route(
            "/groups/:id/calendar-imports/:import_id/import",
            post(google_calendar::imports::trigger_calendar_import),
        )
        .route("/admin/groups", get(user_admin::admin::list_groups))
        .route(
            "/admin/groups/:id/members",
            get(user_admin::admin::ownerless_group_members),
        )
        .route(
            "/admin/groups/:id/owner",
            post(user_admin::admin::designate_owner),
        )
        .route("/admin/users", get(user_admin::admin::list_users))
        .route(
            "/admin/users/:id/deactivate",
            post(user_admin::admin::deactivate_user),
        )
        .route(
            "/admin/users/:id/reactivate",
            post(user_admin::admin::reactivate_user),
        )
        .route(
            "/admin/users/:id/reactivation-request/refuse",
            post(user_admin::admin::refuse_reactivation),
        )
        .layer(CookieManagerLayer::new())
        .layer(axum::middleware::from_fn_with_state(
            body_guard,
            manage_our_home_http_guard::guard_request_body,
        ))
        // Outermost, so a forged request is refused before its body is read.
        .layer(axum::middleware::from_fn_with_state(
            origin_guard,
            manage_our_home_http_guard::guard_cross_origin,
        ))
        // Content-Security-Policy reports (#325), added after the origin
        // guard so that it does not apply: a forged report adds at most
        // `csp_report::MAX_VIOLATIONS_PER_REQUEST` log lines and changes
        // nothing else — which any client can do anyway, origin guard or
        // not, since it only judges browser headers — and how a
        // browser's report request fills `Origin`/`Sec-Fetch-Site` is not
        // something to bet the reports on. The body guard, a small body
        // limit and a per-address limit (#375) still do. A refused request
        // never reaches the handler, the only thing that reads the body, so
        // its body is not read wherever that limit sits; outermost, it also
        // skips the body guard's wrapping.
        .route(
            "/csp-report",
            post(csp_report::receive)
                .layer(DefaultBodyLimit::max(csp_report::MAX_REPORT_BODY_BYTES))
                .layer(axum::middleware::from_fn_with_state(
                    body_guard,
                    manage_our_home_http_guard::guard_request_body,
                ))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    csp_report::throttle,
                )),
        )
        // The orchestrator's probes (#424), after every layer so that none
        // applies: they read no cookie and no body, and an origin check or
        // a limit has nothing to protect there.
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .with_state(state)
}
