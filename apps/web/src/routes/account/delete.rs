//! `/account/delete` — RGPD Art. 17 (effacement), self-service. A **grace
//! period** flow: `POST /account/delete` only stamps
//! `users.deletion_requested_at`; the account keeps working and the purge job
//! anonymizes it 30 days later, so the page sells "programmée + annulable", never
//! "supprimé immédiatement" (that is F9's superadmin `deactivate`, a different
//! action with different copy).
//!
//! Two guards before anything is sent, both mirrored from the backend in
//! `validation::rgpd::validate_deletion_confirmation`: an account with a password
//! must retype it (apps/api verifies it and 401s otherwise), and every account
//! must tick an explicit consent box — apps/api's `delete_account` states that
//! for Google-only accounts "re-consent is validated on the frontend flow", and
//! for password accounts it keeps an irreversible action off a single stray
//! click.
//!
//! The 409 `owner_of_groups` block is the one API error body carrying more than a
//! code: it lists the groups the caller still owns. The page turns that into the
//! two ways out (transfer ownership, or delete the group) with a direct link to
//! each group's settings screen, rather than showing a raw error.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::{DeleteAccountRequest, MeResponse};
use manage_our_home_shared::dto::rgpd::{BlockingGroup, OwnerOfGroupsError};

use crate::app::{html_escape, password_field, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{
    account_cookie, account_header, can_cancel_deletion, can_request_deletion,
    pending_deletion_panel, service_unavailable_page, validate_deletion_confirmation,
    GRACE_PERIOD_DAYS,
};

#[derive(serde::Deserialize)]
pub struct DeleteForm {
    #[serde(default)]
    current_password: String,
    /// Unchecked checkboxes are simply absent from a form POST, so the presence
    /// of the field *is* the consent.
    consent: Option<String>,
}

/// French message for a `validate_deletion_confirmation` code, plus the
/// backend's own 401 (wrong password) mapped the same way so the page has one
/// error slot.
fn error_message(code: &str) -> &'static str {
    match code {
        "password_required" => "Saisissez votre mot de passe actuel pour confirmer la suppression.",
        "consent_required" => {
            "Cochez la case de confirmation pour demander la suppression de votre compte."
        }
        "invalid_password" => "Mot de passe incorrect.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "La demande de suppression n'a pas pu être enregistrée.",
    }
}

/// The request form. Rendered only while no deletion is pending — otherwise the
/// page shows the pending panel (with the cancel action) instead.
fn request_form(me: &MeResponse, error: Option<&str>) -> String {
    let error_html = error
        .map(|code| {
            format!(
                r#"<p class="notice error">{}</p>"#,
                html_escape(error_message(code))
            )
        })
        .unwrap_or_default();

    // A Google-only account has no password to verify: apps/api skips the check
    // for it, so asking would be a field nobody can fill.
    let password = if me.has_password {
        password_field(
            "Mot de passe actuel",
            "current_password",
            "current-password",
            false,
        )
    } else {
        r#"<p class="muted">Votre compte utilise la connexion Google : aucun mot de passe à saisir, la confirmation ci-dessous suffit.</p>"#.to_string()
    };

    format!(
        r#"{error_html}
<form method="post" action="/account/delete" onsubmit="return confirm('Demander la suppression de votre compte ?');">
{password}
<label style="flex-direction:row;gap:0.5rem;align-items:flex-start;">
<input type="checkbox" name="consent" value="1" style="width:auto;margin-top:0.15rem;"/>
<span>Je demande la suppression de mon compte et je comprends que mes identifiants seront supprimés au bout de {days} jours.</span>
</label>
<button type="submit" class="danger">Demander la suppression de mon compte</button>
</form>"#,
        days = GRACE_PERIOD_DAYS,
    )
}

/// The full page: explanation, then either the request form or the pending
/// panel. `blocked` renders the 409 `owner_of_groups` guidance above the form.
fn page(me: &MeResponse, header: &str, error: Option<&str>, blocked: &[BlockingGroup]) -> String {
    let action = if can_request_deletion(me.deletion_requested_at) {
        format!(
            "{blocked}{form}",
            blocked = blocked_html(blocked),
            form = request_form(me, error),
        )
    } else {
        pending_deletion_panel(me)
    };

    format!(
        r#"{header}
<p><a href="/account">← Retour à mon compte</a></p>
<h1>Supprimer mon compte</h1>
<p>La suppression est <strong>programmée</strong>, pas immédiate : votre compte reste utilisable pendant un délai de grâce de {days} jours, pendant lequel vous pouvez annuler la demande. Passé ce délai, vos identifiants de connexion sont supprimés et votre compte est anonymisé.</p>
<p class="muted">Le contenu que vous avez créé dans une famille (messages, événements, dépenses…) reste visible par les autres membres de cette famille, mais n'est plus rattaché à votre identité — c'est le fonctionnement documenté d'un espace partagé. Pour en récupérer une copie avant la suppression, <a href="/account/export">exportez vos données</a>.</p>
{action}"#,
        days = GRACE_PERIOD_DAYS,
    )
}

/// The 409 `owner_of_groups` guidance: what blocks the deletion and the two ways
/// out, one link per blocking group.
fn blocked_html(groups: &[BlockingGroup]) -> String {
    if groups.is_empty() {
        return String::new();
    }
    let items: String = groups
        .iter()
        .map(|g| {
            format!(
                r#"<li>{name} — <a href="/groups/{id}/settings">gérer cette famille</a></li>"#,
                name = html_escape(&g.name),
                id = g.id,
            )
        })
        .collect();
    format!(
        r#"<section class="notice error">
<p><strong>Suppression impossible pour le moment :</strong> vous êtes propriétaire {count}, et une famille ne peut pas rester sans propriétaire.</p>
<ul>{items}</ul>
<p>Depuis les paramètres de chaque famille, <strong>transférez la propriété</strong> à un autre membre — ou <strong>supprimez la famille</strong> si elle n'a plus lieu d'être. Revenez ensuite ici.</p>
</section>"#,
        count = if groups.len() == 1 {
            "d'une famille".to_string()
        } else {
            format!("de {} familles", groups.len())
        },
    )
}

/// `GET /account/delete`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let header = account_header(&state, &headers, &me, "/account/delete").await;
    Html(shell(
        "Supprimer mon compte",
        &page(&me, &header, None, &[]),
    ))
    .into_response()
}

/// `POST /account/delete` — validates the confirmation locally, then relays to
/// `POST /account/delete` on apps/api. 200 → PRG to `/account` with the pending
/// banner; 401 → wrong password on the form; 409 `owner_of_groups` → the blocking
/// guidance; any other status → the generic banner; a transport error → the
/// service-unavailable page.
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeleteForm>,
) -> Response {
    let render = |error: Option<&str>, blocked: &[BlockingGroup], header: String| {
        Html(shell(
            "Supprimer mon compte",
            &page(&me, &header, error, blocked),
        ))
        .into_response()
    };

    let current_password = match validate_deletion_confirmation(
        me.has_password,
        &form.current_password,
        form.consent.is_some(),
    ) {
        Ok(password) => password,
        Err(code) => {
            let header = account_header(&state, &headers, &me, "/account/delete").await;
            return render(Some(code), &[], header);
        }
    };

    let cookie = account_cookie(&headers);
    let body = serde_json::to_value(DeleteAccountRequest { current_password }).unwrap_or_default();
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        "/account/delete",
        cookie.as_deref(),
        Some(body),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to("/account?notice=deletion_requested").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            let header = account_header(&state, &headers, &me, "/account/delete").await;
            render(Some("invalid_password"), &[], header)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            let blocked = serde_json::from_value::<OwnerOfGroupsError>(resp.body)
                .map(|e| e.groups)
                .unwrap_or_default();
            let header = account_header(&state, &headers, &me, "/account/delete").await;
            render(None, &blocked, header)
        }
        Ok(_) => {
            let header = account_header(&state, &headers, &me, "/account/delete").await;
            render(Some("unavailable"), &[], header)
        }
        Err(_) => service_unavailable_page().into_response(),
    }
}

/// `POST /account/delete/cancel` — relays to `POST /account/delete/cancel`. 200 →
/// PRG to `/account` with the cancelled banner; 404 → the "nothing to cancel"
/// banner (the backend 404s when no request is pending, e.g. a double submit);
/// any other status → the generic banner; a transport error → the
/// service-unavailable page.
pub async fn cancel(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // Mirrors the backend's guard: nothing pending, nothing to cancel.
    if !can_cancel_deletion(me.deletion_requested_at) {
        return Redirect::to("/account?error=no_pending_deletion").into_response();
    }

    let cookie = account_cookie(&headers);
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        "/account/delete/cancel",
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to("/account?notice=deletion_cancelled").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            Redirect::to("/account?error=no_pending_deletion").into_response()
        }
        Ok(_) => Redirect::to("/account?error=unavailable").into_response(),
        Err(_) => service_unavailable_page().into_response(),
    }
}
