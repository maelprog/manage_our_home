mod app;
mod assets;
mod body_bounds;
mod client_ip;
// The CSP guard (#141, #325). Test-only: it embeds infra/Caddyfile, which
// sets the policy, and holds the markup emitted here to it — no inline script.
#[cfg(test)]
mod csp;
// The DESIGN.md journal guard (#95). Test-only: it embeds the document
// and its lock file, neither of which belongs in the shipped binary.
#[cfg(test)]
mod design_journal;
mod family;
mod layout;
mod routes;
mod state;

use axum::routing::{get, post};
use axum::Router;

use state::AppState;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let api_internal_base_url = std::env::var("API_INTERNAL_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let api_public_base_url =
        std::env::var("API_PUBLIC_BASE_URL").unwrap_or_else(|_| "/api".to_string());
    let bind_addr = std::env::var("WEB_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let state = AppState {
        http: reqwest::Client::new(),
        api_internal_base_url,
        api_public_base_url,
        body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
        upload_gate: manage_our_home_http_guard::UploadGate::production(),
    };

    let app = build_router(state);

    tracing::info!(%bind_addr, "starting manage_our_home_web");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    // `into_make_service_with_connect_info` is what puts the peer address in
    // each request's extensions. `/login` appends it to the `X-Forwarded-For`
    // it relays to apps/api, which is the only way apps/api can tell one
    // browser from another behind this SSR layer (#178).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}

/// Every route of apps/web, with its state. A function of its own so the
/// tests can drive the real router, middleware included.
fn build_router(state: AppState) -> Router {
    let body_guard = manage_our_home_http_guard::BodyGuard {
        limits: state.body_read_limits,
        render: body_bounds::body_read_timeout_page,
    };
    // Every form post (#223), `/login` and `/register` first: they need no
    // session, so `SameSite` cannot stop a login CSRF. No trusted origin:
    // apps/web does not know its public URL, and needs none — a browser's
    // own pages are `Sec-Fetch-Site: same-origin`, or, on a browser too old
    // for it, carry an `Origin` whose host is the request's `Host`.
    let origin_guard = manage_our_home_http_guard::OriginGuard {
        trusted_origin: None,
        render: body_bounds::cross_origin_page,
    };

    Router::new()
        // The self-hosted fonts (#67). Merged first because it is the one
        // group of routes that needs no session and no state.
        .merge(assets::router())
        .route("/", get(routes::home::get))
        .route("/logout", post(routes::home::logout))
        .route("/auth/google/callback", get(routes::home::google_callback))
        .route(
            "/register",
            get(routes::auth::register::get).post(routes::auth::register::post),
        )
        .route(
            "/register/check-email",
            get(routes::auth::register::check_email),
        )
        .route("/verify-email", get(routes::auth::verify_email::get))
        .route(
            "/login",
            get(routes::auth::login::get).post(routes::auth::login::post),
        )
        .route(
            "/forgot-password",
            get(routes::auth::forgot_password::get).post(routes::auth::forgot_password::post),
        )
        .route(
            "/reset-password",
            get(routes::auth::reset_password::get).post(routes::auth::reset_password::post),
        )
        // RGPD self-service (front epic F10): the account hub, the export
        // download, and the grace-period deletion flow. The three public legal
        // documents need no session — they must be readable before registering,
        // and the CGU are what registering accepts (linked from the login and
        // register footers).
        .route("/privacy-policy", get(routes::privacy::get))
        .route("/legal-notice", get(routes::legal::legal_notice))
        .route("/terms-of-service", get(routes::legal::terms_of_service))
        .route(
            routes::legal::ANNOUNCED_TERMS_PAGE,
            get(routes::legal::announced_terms_of_service),
        )
        .route("/account", get(routes::account::get))
        .route("/account/export", get(routes::account::export::get))
        .route(
            "/account/export/download",
            get(routes::account::export::download),
        )
        .route(
            "/account/delete",
            get(routes::account::delete::get).post(routes::account::delete::post),
        )
        .route(
            "/account/delete/cancel",
            post(routes::account::delete::cancel),
        )
        // Reminder channel and this device's notifications (#306), and
        // the service worker, at the root so its scope is the whole site.
        .route(
            "/account/notifications",
            get(routes::account::notifications::get).post(routes::account::notifications::post),
        )
        .route(
            "/account/notifications/subscription",
            post(routes::account::notifications::subscription),
        )
        .route(
            "/account/notifications/devices/remove",
            post(routes::account::notifications::remove_devices),
        )
        // The member's live sessions (#225).
        .route("/account/sessions", get(routes::account::sessions::get))
        .route(
            "/account/sessions/:id/revoke",
            post(routes::account::sessions::revoke),
        )
        .route(
            "/account/sessions/revoke-all",
            post(routes::account::sessions::revoke_all),
        )
        .route(
            "/sw.js",
            get(routes::account::notifications::service_worker),
        )
        // The one page the session of an account without age declaration
        // opens (#318).
        .route(
            "/account/age",
            get(routes::account::age::get).post(routes::account::age::post),
        )
        // The one page the session of an account without acceptance of the
        // CGU opens (#319); its POST also takes a member's acknowledgement
        // of a new version.
        .route(
            "/account/terms",
            get(routes::account::terms::get).post(routes::account::terms::post),
        )
        // The one page a restricted session opens (#289).
        .route(
            "/account/deactivated",
            get(routes::account::deactivated::get),
        )
        .route(
            "/account/deactivated/reactivation-request",
            post(routes::account::deactivated::request),
        )
        .route("/agenda", get(routes::agenda::calendar::get))
        .route(
            "/agenda/new",
            get(routes::agenda::new::get).post(routes::agenda::new::post),
        )
        // Google Calendar import (front epic F11): registered before the
        // `/agenda/:id` family below purely for readability — the router
        // matches the static `imports` segment ahead of the `:id` param
        // regardless of declaration order.
        .route(
            "/agenda/imports",
            get(routes::agenda::imports::get).post(routes::agenda::imports::create),
        )
        .route("/agenda/imports/new", get(routes::agenda::imports::new_get))
        .route(
            "/agenda/imports/:id/import",
            post(routes::agenda::imports::run),
        )
        .route(
            "/agenda/imports/:id/delete",
            get(routes::agenda::imports::delete_get).post(routes::agenda::imports::delete_post),
        )
        .route("/agenda/:id", get(routes::agenda::detail::get))
        .route(
            "/agenda/:id/edit",
            get(routes::agenda::edit::get).post(routes::agenda::edit::post),
        )
        .route("/agenda/:id/delete", post(routes::agenda::detail::delete))
        .route(
            "/agenda/:id/complete",
            post(routes::agenda::detail::complete),
        )
        .route(
            "/agenda/:id/reminders",
            post(routes::agenda::reminders::add),
        )
        .route(
            "/agenda/:id/reminders/:rid/delete",
            post(routes::agenda::reminders::delete),
        )
        .route(
            "/agenda/:id/attachments",
            // The page reads the whole file before relaying it: without a
            // limit of its own it had axum's 2 MiB default, where apps/api
            // and Caddy take 20 MiB plus framing (#243).
            post(routes::agenda::attachments::upload).layer(axum::extract::DefaultBodyLimit::max(
                manage_our_home_http_guard::MAX_UPLOAD_BODY_BYTES,
            )),
        )
        .route(
            "/agenda/:id/attachments/:aid/download",
            get(routes::agenda::attachments::download),
        )
        .route(
            "/agenda/:id/attachments/:aid/delete",
            post(routes::agenda::attachments::delete),
        )
        .route("/stocks", get(routes::stocks::list::get))
        .route(
            "/stocks/new",
            get(routes::stocks::new::get).post(routes::stocks::new::post),
        )
        .route("/stocks/:id", get(routes::stocks::detail::get))
        .route(
            "/stocks/:id/edit",
            get(routes::stocks::edit::get).post(routes::stocks::edit::post),
        )
        .route("/stocks/:id/adjust", post(routes::stocks::detail::adjust))
        .route("/stocks/:id/delete", post(routes::stocks::detail::delete))
        .route("/recipes", get(routes::recipes::list::get))
        .route(
            "/recipes/new",
            get(routes::recipes::new::get).post(routes::recipes::new::post),
        )
        .route("/recipes/:id", get(routes::recipes::detail::get))
        .route(
            "/recipes/:id/edit",
            get(routes::recipes::edit::get).post(routes::recipes::edit::post),
        )
        .route("/recipes/:id/log", post(routes::recipes::detail::log))
        .route("/recipes/:id/delete", post(routes::recipes::detail::delete))
        .route("/grocery-list", get(routes::grocery_list::list::get))
        .route("/grocery-list/add", post(routes::grocery_list::list::add))
        .route(
            "/grocery-list/generate",
            post(routes::grocery_list::list::generate),
        )
        .route("/grocery-list/:id", get(routes::grocery_list::edit::get))
        .route(
            "/grocery-list/:id/check",
            post(routes::grocery_list::list::check),
        )
        .route(
            "/grocery-list/:id/edit",
            post(routes::grocery_list::edit::post),
        )
        .route(
            "/grocery-list/:id/delete",
            post(routes::grocery_list::edit::delete),
        )
        .route(
            "/grocery-list/:id/price",
            post(routes::grocery_list::list::price),
        )
        .route("/budget", get(routes::budget::list::get))
        .route(
            "/budget/new",
            get(routes::budget::new::get).post(routes::budget::new::post),
        )
        .route("/budget/:id", get(routes::budget::edit::get))
        .route("/budget/:id/edit", post(routes::budget::edit::post))
        .route("/budget/:id/delete", post(routes::budget::edit::delete))
        .route(
            "/messagerie",
            get(routes::messagerie::thread::get).post(routes::messagerie::thread::post),
        )
        .route(
            "/messagerie/:id/edit",
            post(routes::messagerie::thread::edit),
        )
        .route(
            "/messagerie/:id/delete",
            post(routes::messagerie::thread::delete),
        )
        .route("/admin/groups", get(routes::admin::groups::get))
        .route("/admin/groups/:id", get(routes::admin::groups::detail))
        .route(
            "/admin/groups/:id/owner",
            post(routes::admin::groups::designate_owner),
        )
        .route("/admin/users", get(routes::admin::users::get))
        .route("/admin/users/:id", get(routes::admin::users::detail))
        .route(
            "/admin/users/:id/deactivate",
            post(routes::admin::users::deactivate),
        )
        .route(
            "/admin/users/:id/reactivate",
            post(routes::admin::users::reactivate),
        )
        .route(
            "/admin/users/:id/reactivation-request/refuse",
            post(routes::admin::users::refuse_reactivation),
        )
        .route("/groups", get(routes::groups::list::get))
        .route("/groups/join", post(routes::groups::list::join))
        .route("/groups/switch", post(routes::groups::switch))
        .route(
            "/groups/:id/ownership-notice",
            post(routes::groups::ownership::acknowledge),
        )
        .route(
            "/groups/new",
            get(routes::groups::new::get).post(routes::groups::new::post),
        )
        .route(
            "/groups/invitations/:token/accept",
            get(routes::groups::invitations::get).post(routes::groups::invitations::post),
        )
        .route("/groups/:id/members", get(routes::groups::members::get))
        .route(
            "/groups/:id/members/invite",
            post(routes::groups::members::invite),
        )
        .route(
            "/groups/:id/members/:user_id/role",
            post(routes::groups::members::change_role),
        )
        .route(
            "/groups/:id/members/:user_id/remove",
            post(routes::groups::members::remove),
        )
        .route("/groups/:id/settings", get(routes::groups::settings::get))
        .route(
            "/groups/:id/settings/rename",
            post(routes::groups::settings::rename),
        )
        .route(
            "/groups/:id/settings/transfer",
            post(routes::groups::settings::transfer),
        )
        .route(
            "/groups/:id/settings/leave",
            post(routes::groups::settings::leave),
        )
        .route(
            "/groups/:id/settings/delete",
            post(routes::groups::settings::delete),
        )
        // Every route (#219): a form can be dripped as slowly as an upload.
        // It sits outside the routes so no handler sees an unguarded body.
        .layer(axum::middleware::from_fn_with_state(
            body_guard,
            manage_our_home_http_guard::guard_request_body,
        ))
        // Outermost, so a forged request is refused before its body is read.
        .layer(axum::middleware::from_fn_with_state(
            origin_guard,
            manage_our_home_http_guard::guard_cross_origin,
        ))
        .with_state(state)
}

/// The cross-origin guard on the real router (#223). The decision itself
/// is covered by `manage_our_home_http_guard::origin`'s own tests; these
/// check that every form post goes through it, `/login` first, and that
/// a refused one never reaches apps/api.
#[cfg(test)]
mod cross_origin_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use tower::ServiceExt;

    use super::*;

    /// An apps/api that has no session to report and accepts every login,
    /// counting them.
    async fn fake_api(logins: Arc<AtomicUsize>) -> String {
        let app = Router::new()
            .route("/auth/me", get(|| async { StatusCode::UNAUTHORIZED }))
            .route(
                "/auth/login",
                post(move || async move {
                    logins.fetch_add(1, Ordering::SeqCst);
                    (
                        [(header::SET_COOKIE, "session=attacker; Path=/; HttpOnly")],
                        axum::Json(serde_json::json!({})),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn web() -> (Router, Arc<AtomicUsize>) {
        let logins = Arc::new(AtomicUsize::new(0));
        let api = fake_api(logins.clone()).await;
        let router = build_router(AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: api,
            api_public_base_url: "/api".into(),
            body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
            upload_gate: manage_our_home_http_guard::UploadGate::production(),
        });
        (router, logins)
    }

    fn login(headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/login")
            .header(header::HOST, "maison.test")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder
            .body(Body::from(
                "email=attacker%40evil.test&password=attacker-password",
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn a_cross_site_login_is_refused_before_apps_api() {
        let (router, logins) = web().await;
        for headers in [
            &[
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.test"),
            ][..],
            // A sibling subdomain: same site, another origin.
            &[
                ("sec-fetch-site", "same-site"),
                ("origin", "https://other.maison.test"),
            ][..],
            // A browser without Fetch Metadata.
            &[("origin", "https://evil.test")][..],
        ] {
            let resp = router.clone().oneshot(login(headers)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{headers:?}");
            assert!(resp.headers().get(header::SET_COOKIE).is_none());
        }
        assert_eq!(logins.load(Ordering::SeqCst), 0, "apps/api was reached");
    }

    #[tokio::test]
    async fn a_same_origin_login_goes_through() {
        let (router, logins) = web().await;
        for headers in [
            &[
                ("sec-fetch-site", "same-origin"),
                ("origin", "https://maison.test"),
            ][..],
            &[("origin", "https://maison.test")][..],
            // Not a browser.
            &[][..],
        ] {
            let resp = router.clone().oneshot(login(headers)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::SEE_OTHER, "{headers:?}");
            assert!(resp.headers().get(header::SET_COOKIE).is_some());
        }
        assert_eq!(logins.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_cross_site_register_is_refused() {
        let (router, _) = web().await;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/register")
            .header("sec-fetch-site", "cross-site")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("email=a%40b.test"))
            .unwrap();
        let resp = router.oneshot(request).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_cross_site_get_still_renders() {
        // A link from another site to the login page is a navigation.
        let (router, _) = web().await;
        let request = Request::builder()
            .uri("/login")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(request).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
