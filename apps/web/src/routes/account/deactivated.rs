//! `/account/deactivated` — the one page a restricted session opens (#289).
//! A correct login, by password or Google, on an account the superadmin
//! deactivated (#256) lands here instead of the app: every other page's
//! extractor (`CurrentUser`, `CurrentSuperAdmin`, `RedirectIfAuthenticated`)
//! sends such a session here, and apps/api refuses it everywhere else with
//! 403 `account_deactivated`. A wrong password still gets the login page's
//! generic message.
//!
//! The page says what the deactivation means, and offers the holder one
//! reactivation request at a time, with an optional note, for the superadmin
//! to decide on (`/admin/users/:id`), and says when the account will be
//! purged. No app header: none of its links would open; the logout button
//! posts to `/logout`, which apps/api's logout takes from a restricted
//! session too.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::{DeactivatedAccountResponse, ReactivationRequestBody};
use manage_our_home_shared::validation::user_admin::{
    format_admin_datetime, purge_outlook, validate_reactivation_message, PurgeOutlook,
    MAX_REACTIVATION_MESSAGE_CHARS,
};

use crate::app::{html_escape, shell, Width};
use crate::state::{api_request_auth, AppState};

use super::{account_cookie, service_unavailable_page};

const TITLE: &str = "Compte désactivé";

#[derive(serde::Deserialize)]
pub struct PageQuery {
    notice: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct RequestForm {
    #[serde(default)]
    message: String,
}

fn notice_html(notice: Option<&str>) -> String {
    match notice {
        Some("reactivation_requested") => format!(
            r#"<p class="notice success">{}</p>"#,
            html_escape(
                "Votre demande de réactivation a été transmise à l'administrateur du service."
            )
        ),
        _ => String::new(),
    }
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("reactivation_message_too_long") => {
            "Votre message est trop long : 1000 caractères au plus."
        }
        Some("reactivation_already_requested") => {
            "Une demande de réactivation est déjà en attente."
        }
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// When the account will be purged, in the holder's words: the purge rule
/// (`jobs::account_purge::purge_due`) as [`purge_outlook`] mirrors it.
fn purge_note(status: &DeactivatedAccountResponse) -> String {
    const TWO_YEARS: &str = "Sans réactivation, votre compte sera supprimé au bout de 2 ans de désactivation. Un email vous en prévient à l'adresse du compte, et la suppression n'a jamais lieu moins de 30 jours après cet email.";
    match purge_outlook(
        status.deletion_requested_at,
        status.reactivation_requested_at,
        status.reactivation_refused_at,
    ) {
        PurgeOutlook::DeletionRequested { from } => format!(
            "Vous avez demandé la suppression de votre compte : il sera supprimé à partir du {}, sans autre email. Tant qu'il est désactivé, cette demande ne peut pas être annulée, et une demande de réactivation ne suspend pas la suppression.",
            format_admin_datetime(from)
        ),
        PurgeOutlook::Suspended => format!(
            "{TWO_YEARS} Votre demande de réactivation en attente suspend cette échéance."
        ),
        PurgeOutlook::Runs {
            after_refusal: false,
        } => format!(
            "{TWO_YEARS} Une demande de réactivation suspend cette échéance tant qu'elle est en attente."
        ),
        PurgeOutlook::Runs {
            after_refusal: true,
        } => format!(
            "{TWO_YEARS} Une demande de réactivation a déjà été refusée : vous pouvez en faire une nouvelle, mais elle ne suspend plus cette échéance."
        ),
    }
}

fn page(status: &DeactivatedAccountResponse, notice: Option<&str>, error: Option<&str>) -> String {
    let deactivated_on = html_escape(&format_admin_datetime(status.deactivated_at));
    let request = match status.reactivation_requested_at {
        Some(at) => format!(
            r#"<section class="card">
<h2>Demande de réactivation</h2>
<p>Votre demande du {on} attend la décision de l'administrateur du service.</p>
</section>"#,
            on = html_escape(&format_admin_datetime(at)),
        ),
        None => format!(
            r#"<section class="card">
<h2>Demander la réactivation</h2>
<form method="post" action="/account/deactivated/reactivation-request">
<label class="field">Message pour l'administrateur (facultatif)
<textarea name="message" rows="4" maxlength="{max}"></textarea>
</label>
<button type="submit">Demander la réactivation</button>
</form>
</section>"#,
            max = MAX_REACTIVATION_MESSAGE_CHARS,
        ),
    };

    format!(
        r#"<h1>{title}</h1>
{notice}{error}
<p>Votre compte a été désactivé par l'administrateur du service le {deactivated_on}. Tant qu'il l'est, aucune page de l'application ne vous est ouverte ; rien n'a été effacé.</p>
<p class="muted">{purge}</p>
{request}
<form method="post" action="/logout">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
<div class="links">
<a href="/privacy-policy">Politique de confidentialité</a>
</div>"#,
        title = TITLE,
        purge = html_escape(&purge_note(status)),
        notice = notice_html(notice),
        error = error_html(error),
    )
}

/// `GET /account/deactivated`. Reads the account's state with the caller's
/// session: anything but a restricted session (apps/api's 401) is sent to
/// `/`, whose own extractor then tells an active session from none.
pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = account_cookie(&headers);
    match api_request_auth(
        &state,
        reqwest::Method::GET,
        "/account/deactivated",
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<DeactivatedAccountResponse>(resp.body) {
                Ok(status) => Html(shell(
                    Width::Form,
                    TITLE,
                    &page(&status, query.notice.as_deref(), query.error.as_deref()),
                ))
                .into_response(),
                Err(_) => service_unavailable_page().into_response(),
            }
        }
        Ok(_) => Redirect::to("/").into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}

/// `POST /account/deactivated/reactivation-request` — checks the note
/// locally, then relays to the same apps/api route. 201 → PRG with the
/// success banner; 409 / 422 → the page with that error; 401 (not, or no
/// longer, a restricted session) → `/`; anything else → the unavailable
/// banner.
pub async fn request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<RequestForm>,
) -> Response {
    let message = match validate_reactivation_message(&form.message) {
        Ok(message) => message,
        Err(code) => {
            return Redirect::to(&format!("/account/deactivated?error={code}")).into_response()
        }
    };
    let cookie = account_cookie(&headers);
    let body = serde_json::to_value(ReactivationRequestBody { message }).unwrap_or_default();
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        "/account/deactivated/reactivation-request",
        cookie.as_deref(),
        Some(body),
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/account/deactivated?notice=reactivation_requested").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            Redirect::to("/account/deactivated?error=reactivation_already_requested")
                .into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            Redirect::to("/account/deactivated?error=reactivation_message_too_long").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            Redirect::to("/").into_response()
        }
        Ok(_) => Redirect::to("/account/deactivated?error=unavailable").into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}
