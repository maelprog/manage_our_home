//! The API's OpenAPI documentation (#626): the spec, generated from the
//! handlers' `#[utoipa::path]` annotations, and a Swagger UI to read it.
//!
//! Off by default, on with `API_DOCS_ENABLED=true` (OWASP API9, inventory
//! of the API): `build_router` then merges [`router`]. Off, its routes are
//! not registered at all, so they answer exactly what an unknown route
//! answers.
//!
//! - `GET /openapi.json`: the spec, OpenAPI 3.1.
//! - `GET /docs/`: Swagger UI, served from the binary — the archive comes
//!   from the `utoipa-swagger-ui-vendored` crate at build time, nothing is
//!   fetched at runtime. Its initializer is a file
//!   (`swagger-initializer.js`), so the page runs under Caddy's
//!   `script-src 'self'`. Every URL it holds is relative, so the page
//!   works both on the API directly and behind Caddy's `/api` prefix.

pub mod schemas;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use manage_our_home_shared::dto::auth::ErrorResponse;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::openapi::{ContentBuilder, Ref, RefOr, ResponseBuilder, ServerBuilder};
use utoipa::{IntoResponses, OpenApi};

/// The variable that turns the documentation on.
pub const API_DOCS_ENABLED_VAR: &str = "API_DOCS_ENABLED";

/// Whether `API_DOCS_ENABLED` turns the documentation on: `true`, exactly,
/// and nothing else. Off is the default, so a typo (`True`, `1`, `yes`)
/// leaves it off rather than publishing the inventory of the routes.
pub fn docs_enabled(value: Option<&str>) -> bool {
    value == Some("true")
}

/// The name of the security scheme every authenticated operation names.
pub const SESSION_SCHEME: &str = "session_cookie";

#[derive(OpenApi)]
#[openapi(
    info(
        title = "manage_our_home API",
        description = "The JSON API apps/web calls. Behind Caddy it is served under `/api`.\n\n\
            Errors answer `{\"error\": \"<code>\"}` (`ErrorResponse`); a few 409s add fields \
            next to `error`, named on their operation. A JSON body that cannot be read at \
            all (malformed, wrong `Content-Type`, missing field) is refused by axum's \
            extractor with a plain-text 400, 415 or 422, before the handler runs.\n\n\
            Every unsafe method is refused 403 `cross_origin_request` when the browser \
            says it comes from another site, and a request body that is sent too slowly \
            is answered 408 `request_timeout`; neither is repeated on each operation.",
    ),
    paths(
        crate::agenda::attachments::delete_attachment,
        crate::agenda::attachments::download_attachment,
        crate::agenda::attachments::list_attachments,
        crate::agenda::attachments::upload_attachment,
        crate::agenda::events::create_event,
        crate::agenda::events::delete_event,
        crate::agenda::events::get_event,
        crate::agenda::events::list_events,
        crate::agenda::events::update_event,
        crate::agenda::national_days::list_national_days,
        crate::agenda::reminders::create_reminder,
        crate::agenda::reminders::delete_reminder,
        crate::auth::age_declaration::declare,
        crate::auth::cancel_delete_account,
        crate::auth::change_password,
        crate::auth::deactivated::request_reactivation,
        crate::auth::deactivated::status,
        crate::auth::delete_account,
        crate::auth::forgot_password,
        crate::auth::google_id_token::issue_nonce,
        crate::auth::google_id_token::sign_in,
        crate::auth::list_sessions,
        crate::auth::login,
        crate::auth::logout,
        crate::auth::me,
        crate::auth::oauth_google::callback,
        crate::auth::oauth_google::start,
        crate::auth::register,
        crate::auth::resend_verification,
        crate::auth::reset_password,
        crate::auth::revoke_every_session,
        crate::auth::set_password,
        crate::auth::terms_acceptance::accept,
        crate::auth::verify_email,
        crate::avatar::delete_avatar,
        crate::avatar::get_member_avatar,
        crate::avatar::get_own_avatar,
        crate::avatar::put_avatar,
        crate::budget::alerts::get_alert_settings,
        crate::budget::alerts::patch_alert_settings,
        crate::budget::categories::create_budget_category,
        crate::budget::categories::delete_budget_category,
        crate::budget::categories::get_budget_settings,
        crate::budget::categories::list_budget_categories,
        crate::budget::categories::put_budget_settings,
        crate::budget::categories::update_budget_category,
        crate::budget::entries::budget_summary,
        crate::budget::entries::create_budget_entry,
        crate::budget::entries::delete_budget_entry,
        crate::budget::entries::get_budget_entry,
        crate::budget::entries::list_budget_entries,
        crate::budget::entries::set_grocery_item_price,
        crate::budget::entries::update_budget_entry,
        crate::google_calendar::imports::create_calendar_import,
        crate::google_calendar::imports::delete_calendar_import,
        crate::google_calendar::imports::list_calendar_imports,
        crate::google_calendar::imports::trigger_calendar_import,
        crate::grocery_list::items::add_missing_grocery_items,
        crate::grocery_list::items::check_grocery_item,
        crate::grocery_list::items::create_grocery_item,
        crate::grocery_list::items::delete_grocery_item,
        crate::grocery_list::items::generate_grocery_items,
        crate::grocery_list::items::get_grocery_item,
        crate::grocery_list::items::list_grocery_items,
        crate::grocery_list::items::update_grocery_item,
        crate::groups::accept_invitation,
        crate::groups::acknowledge_ownership_notice,
        crate::groups::change_role,
        crate::groups::create_group,
        crate::groups::create_invitation,
        crate::groups::delete_group,
        crate::groups::get_group,
        crate::groups::get_member,
        crate::groups::leave_group,
        crate::groups::list_groups,
        crate::groups::remove_member,
        crate::groups::rename_group,
        crate::groups::transfer_ownership,
        crate::messagerie::messages::create_message,
        crate::messagerie::messages::delete_message,
        crate::messagerie::messages::list_messages,
        crate::messagerie::messages::mark_messages_read,
        crate::messagerie::messages::unread_count,
        crate::messagerie::messages::update_message,
        crate::notifications::center::list_notifications,
        crate::notifications::center::mark_all_read,
        crate::notifications::center::mark_read,
        crate::notifications::center::unread_count,
        crate::notifications::preferences::get_settings,
        crate::notifications::preferences::received,
        crate::notifications::preferences::subscribe,
        crate::notifications::preferences::unsubscribe_all,
        crate::notifications::preferences::update_settings,
        crate::notifications::upcoming::list_upcoming,
        crate::recipes::crud::create_recipe,
        crate::recipes::crud::delete_recipe,
        crate::recipes::crud::get_recipe,
        crate::recipes::crud::list_recipes,
        crate::recipes::crud::update_recipe,
        crate::recipes::import::import_recipe,
        crate::recipes::meal_history::list_meal_history,
        crate::recipes::meal_history::log_meal,
        crate::recipes::outcome::apply_abandoned,
        crate::recipes::outcome::apply_done,
        crate::recipes::outcome::preview_abandoned,
        crate::recipes::outcome::preview_done,
        crate::recipes::selections::deselect_recipe,
        crate::recipes::selections::list_selections,
        crate::recipes::selections::select_recipe,
        crate::recipes::selections::update_selection,
        crate::recipes::suggestions::suggest_recipes,
        crate::rgpd::announced_terms_of_service,
        crate::rgpd::export_account,
        crate::rgpd::legal_notice,
        crate::rgpd::privacy_policy,
        crate::rgpd::terms_of_service,
        crate::stocks::items::create_stock_item,
        crate::stocks::items::delete_stock_item,
        crate::stocks::items::expiring_count,
        crate::stocks::items::get_stock_item,
        crate::stocks::items::list_stock_items,
        crate::stocks::items::update_stock_item,
        crate::stocks::reception::get_reception,
        crate::stocks::reception::receive,
        crate::stocks::scan::scan_stock_item,
        crate::user_admin::admin::deactivate_user,
        crate::user_admin::admin::designate_owner,
        crate::user_admin::admin::list_groups,
        crate::user_admin::admin::list_users,
        crate::user_admin::admin::ownerless_group_members,
        crate::user_admin::admin::reactivate_user,
        crate::user_admin::admin::refuse_reactivation,
    ),
    components(schemas(ErrorResponse))
)]
struct ApiDoc;

/// The spec, with the session cookie's name as this process sets it
/// (`__Host-session_id` behind `SECURE_COOKIES`, `session_id` without —
/// `auth::session::session_cookie_name`).
pub fn openapi(secure_cookies: bool) -> utoipa::openapi::OpenApi {
    let mut spec = ApiDoc::openapi();
    // Relative: resolved against the spec's own URL, so "Try it out" goes
    // to `/api/...` behind Caddy.
    spec.servers = Some(vec![ServerBuilder::new()
        .url("/api")
        .description(Some("Behind Caddy (infra/Caddyfile)"))
        .build()]);
    let components = spec.components.get_or_insert_with(Default::default);
    components.add_security_scheme(
        SESSION_SCHEME,
        SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
            crate::auth::session::session_cookie_name(secure_cookies),
            "The session cookie `POST /auth/login` sets (HttpOnly, SameSite=Lax).",
        ))),
    );
    spec
}

/// The documentation's routes, for `build_router` to merge when
/// `API_DOCS_ENABLED=true`.
pub fn router<S>(spec: &utoipa::openapi::OpenApi) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // Serialized once: the spec does not change while the process runs.
    let spec_json: Arc<str> = spec.to_json().expect("the OpenAPI spec serializes").into();
    // `../openapi.json` from `/docs/` is `/openapi.json` on the API itself
    // and `/api/openapi.json` behind Caddy. No validator: Swagger UI's
    // default sends the spec's URL to validator.swagger.io and shows its
    // badge as an image from there — a third party, and blocked by the CSP.
    // `BaseLayout`, not the default `StandaloneLayout`: the latter's top bar
    // draws its logo as an SVG holding a `<style>` element, which
    // `style-src 'self'` refuses (one violation report per page view), and
    // offers a field to load any other spec URL into the page.
    let config = Arc::new(
        utoipa_swagger_ui::Config::new(["../openapi.json"])
            .validator_url("none")
            .use_base_layout(),
    );
    Router::new()
        .route(
            "/openapi.json",
            get(move || {
                let spec_json = spec_json.clone();
                async move {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        spec_json.to_string(),
                    )
                }
            }),
        )
        // Relative, unlike `utoipa_swagger_ui`'s own router (`/docs/`): an
        // absolute path would leave the `/api` prefix behind Caddy.
        .route("/docs", get(|| async { Redirect::to("docs/") }))
        .route("/docs/", {
            let config = config.clone();
            get(move || swagger_file(String::new(), config.clone()))
        })
        .route(
            "/docs/{*rest}",
            get(move |Path(rest): Path<String>| swagger_file(rest, config.clone())),
        )
}

/// Swagger UI's own `index.css`, in place of the archive's: the same rules,
/// then the ones that take Swagger UI's `data:` images off (see the file).
const INDEX_CSS: &str = include_str!("index.css");

async fn swagger_file(path: String, config: Arc<utoipa_swagger_ui::Config<'static>>) -> Response {
    if path == "index.css" {
        return ([(header::CONTENT_TYPE, "text/css")], INDEX_CSS).into_response();
    }
    match utoipa_swagger_ui::serve(&path, config) {
        Ok(Some(file)) => (
            [(header::CONTENT_TYPE, file.content_type)],
            file.bytes.into_owned(),
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, "swagger ui file");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn error_response(description: &str) -> RefOr<utoipa::openapi::Response> {
    ResponseBuilder::new()
        .description(description)
        .content(
            "application/json",
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("ErrorResponse")))
                .build(),
        )
        .build()
        .into()
}

const UNAUTHORIZED: &str = "No valid session: `unauthorized`.";
const SESSION_REFUSED: &str = "`account_deactivated`, `age_not_declared` or \
    `terms_not_accepted`: the session cannot open this route until the account is \
    reactivated, its age declared or the CGU accepted.";
const INTERNAL: &str = "`internal_error`.";

/// The errors of a route any full session opens (`AuthUser`).
pub struct SessionErrors;

impl IntoResponses for SessionErrors {
    fn responses() -> BTreeMap<String, RefOr<utoipa::openapi::Response>> {
        BTreeMap::from([
            ("401".to_string(), error_response(UNAUTHORIZED)),
            ("403".to_string(), error_response(SESSION_REFUSED)),
            ("500".to_string(), error_response(INTERNAL)),
        ])
    }
}

/// The errors of a route under `/groups/{id}`: those of [`SessionErrors`],
/// and `forbidden` for a caller who is not a member of the group or whose
/// role does not allow the action.
pub struct MemberErrors;

impl IntoResponses for MemberErrors {
    fn responses() -> BTreeMap<String, RefOr<utoipa::openapi::Response>> {
        BTreeMap::from([
            ("401".to_string(), error_response(UNAUTHORIZED)),
            (
                "403".to_string(),
                error_response(&format!(
                    "`forbidden`: not a member of the group, or a role that does not \
                     allow this. Or {SESSION_REFUSED}"
                )),
            ),
            ("500".to_string(), error_response(INTERNAL)),
        ])
    }
}

/// The errors of a superadmin route (`SuperAdminUser`).
pub struct AdminErrors;

impl IntoResponses for AdminErrors {
    fn responses() -> BTreeMap<String, RefOr<utoipa::openapi::Response>> {
        BTreeMap::from([
            (
                "401".to_string(),
                error_response(
                    "`unauthorized`: no valid session, or a superadmin session past its \
                     freshness bound.",
                ),
            ),
            (
                "403".to_string(),
                error_response(&format!(
                    "`forbidden`: not a superadmin. Or {SESSION_REFUSED}"
                )),
            ),
            ("500".to_string(), error_response(INTERNAL)),
        ])
    }
}

/// The errors of a route a restricted session opens too (`AnySession`,
/// `DeactivatedSession`, `AgeUndeclaredSession`, `TermsSession`): only the
/// session kind it needs, or 401.
pub struct RestrictedSessionErrors;

impl IntoResponses for RestrictedSessionErrors {
    fn responses() -> BTreeMap<String, RefOr<utoipa::openapi::Response>> {
        BTreeMap::from([
            (
                "401".to_string(),
                error_response("`unauthorized`: no session of the kind this route takes."),
            ),
            ("500".to_string(), error_response(INTERNAL)),
        ])
    }
}

/// The errors of a route that takes no session.
pub struct PublicErrors;

impl IntoResponses for PublicErrors {
    fn responses() -> BTreeMap<String, RefOr<utoipa::openapi::Response>> {
        BTreeMap::from([("500".to_string(), error_response(INTERNAL))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_means_disabled() {
        assert!(!docs_enabled(None));
    }

    #[test]
    fn false_means_disabled() {
        assert!(!docs_enabled(Some("false")));
    }

    #[test]
    fn any_other_value_means_disabled() {
        for value in [
            "", "TRUE", "True", "1", "yes", "on", " true", "true ", "enabled",
        ] {
            assert!(!docs_enabled(Some(value)), "{value:?} must not enable");
        }
    }

    #[test]
    fn true_means_enabled() {
        assert!(docs_enabled(Some("true")));
    }
}
