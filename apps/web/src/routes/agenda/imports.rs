//! `/agenda/imports` — front epic F11 (issue #52): connect a Google calendar
//! to the family agenda, pull it on demand, disconnect it.
//!
//! **The model the copy has to make legible.** Import is one-way and
//! *pull-on-demand*: a per-calendar private ICS feed URL ("Adresse secrète au
//! format iCal"), fetched only when someone presses the button. No OAuth, no
//! background polling, no webhooks, and Google's own ICS cache lags by up to a
//! few hours. Every screen here says so rather than implying a live sync.
//!
//! **Placement.** Under `/agenda/*` and not `/groups/:id/settings` because what
//! it produces is agenda data and the read/trigger bar is "any member", like the
//! rest of Agenda; the family-settings page carries a cross-link for people who
//! go looking under settings.
//!
//! **Permission bar** (`validation::google_calendar::can_configure`, mirror of
//! `apps/api/src/google_calendar/mod.rs`): connecting and removing a calendar is
//! admin/owner only — the feed URL is a bearer credential for a member's Google
//! account, not household data. Listing and triggering an import stay open to
//! any member. The controls are hidden for a standard member and the handlers
//! bounce them defensively; the backend stays the authority.
//!
//! **The feed URL is a credential.** It is submitted once, by POST body only. It
//! never enters a query string or a PRG parameter, is never re-rendered into a
//! form value after an error (the form re-asks for it), and is never logged. The
//! backend's `feed_fetch_failed: {reqwest error}` message can embed the URL, so
//! the handlers match on the code prefix and render fixed copy — the tail of
//! that message never reaches the page.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::google_calendar::{
    CalendarImportResponse, CalendarImportsResponse, CreateCalendarImportRequest,
    DeleteCalendarImportResponse, ImportRunResponse,
};
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::google_calendar::{
    can_configure, format_last_imported, import_deleted_notice, import_run_summary,
    validate_import_form, ImportFormError,
};
// Same member-id → display-name resolution F8 introduced (with its "Membre"
// fallback for someone who has since left the family) — the "créé par" column
// needs exactly that, so it is reused rather than duplicated.
use manage_our_home_shared::validation::messagerie::author_name;
use uuid::Uuid;

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::{agenda_cookie, family_context, fmt_paris, service_unavailable_page, FamilyContext};

/// Google's own help page for the private iCal address — linked rather than
/// paraphrased, because Google moves the setting around and a stale click-path
/// in our copy would be worse than no click-path.
const GOOGLE_HELP_URL: &str = "https://support.google.com/calendar/answer/37648";

#[derive(serde::Deserialize)]
pub struct ImportsQuery {
    notice: Option<String>,
    error: Option<String>,
    /// The three counters of the run that just finished, carried through the PRG
    /// so a refresh doesn't re-import. Safe in the URL — unlike the feed URL,
    /// they are not a credential.
    imported: Option<usize>,
    updated: Option<usize>,
    skipped: Option<usize>,
    /// How many imported events the delete took with the connection (#55).
    /// Present only when that branch ran — absent is "the events were
    /// kept", which is a different sentence, not a count of zero.
    deleted: Option<usize>,
}

fn notice_text(query: &ImportsQuery) -> Option<String> {
    match query.notice.as_deref()? {
        "import_created" => Some(
            "Agenda Google connecté. Lancez un import pour récupérer ses événements.".to_string(),
        ),
        "import_deleted" => Some(import_deleted_notice(query.deleted)),
        "imported" => Some(format!(
            "Import terminé : {}.",
            import_run_summary(
                query.imported.unwrap_or(0),
                query.updated.unwrap_or(0),
                query.skipped.unwrap_or(0),
            )
        )),
        _ => None,
    }
}

/// Every error the screens surface, mapped to fixed French copy. `apps/api`'s
/// `feed_fetch_failed` carries a `reqwest` message that can contain the feed
/// URL; only the bare code ever reaches this table.
fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "label_required" => Some("Donnez un nom à cet agenda."),
        "feed_url_required" => Some("L'adresse iCal est obligatoire."),
        "feed_url_must_be_http_or_https" => Some(
            "L'adresse doit commencer par https:// — copiez l'adresse secrète au format iCal depuis Google Agenda.",
        ),
        "forbidden" => Some(
            "Seul un administrateur ou le propriétaire de la famille peut connecter ou retirer un agenda Google.",
        ),
        "not_found" => Some("Cet agenda connecté n'existe plus."),
        "feed_fetch_failed" => Some(
            "Google n'a pas répondu, ou l'adresse n'est plus valide. Vérifiez que l'agenda est toujours partagé ; sinon supprimez cette connexion et reconnectez-la.",
        ),
        "feed_too_large" => Some("Cet agenda dépasse la taille maximale acceptée (5 Mo)."),
        "invalid_ics" => Some("Le contenu récupéré n'est pas un agenda iCal valide."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        _ => None,
    }
}

fn error_code(err: ImportFormError) -> &'static str {
    match err {
        ImportFormError::LabelRequired => "label_required",
        ImportFormError::FeedUrlRequired => "feed_url_required",
        ImportFormError::FeedUrlMustBeHttpOrHttps => "feed_url_must_be_http_or_https",
    }
}

/// Turns one of `trigger_calendar_import`'s 422 bodies into a bare code. The
/// backend formats `feed_fetch_failed: {e}`, so the prefix is what identifies
/// it — and the interpolated tail (which can contain the feed URL) is dropped
/// here rather than travelling any further.
fn import_error_code(body: &serde_json::Value) -> &'static str {
    let raw = body.get("error").and_then(|e| e.as_str()).unwrap_or("");
    if raw.starts_with("feed_fetch_failed") {
        "feed_fetch_failed"
    } else if raw.starts_with("feed_too_large") {
        "feed_too_large"
    } else if raw.starts_with("invalid_ics") {
        "invalid_ics"
    } else {
        "unavailable"
    }
}

fn banner(query: &ImportsQuery) -> String {
    let notice = notice_text(query)
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(&n)))
        .unwrap_or_default();
    let error = query
        .error
        .as_deref()
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    format!("{notice}{error}")
}

/// The one-way, pull-on-demand model, restated wherever it matters.
const MODEL_EXPLAINER: &str = r#"<p class="muted">L'import est <strong>à sens unique</strong> (Google → cette application) et se déclenche <strong>à la demande</strong> : rien n'est synchronisé en arrière-plan. Google met par ailleurs son adresse iCal en cache, donc une modification faite dans Google Agenda peut mettre quelques heures à apparaître ici. Ce que vous modifiez de ce côté n'est jamais renvoyé à Google.</p>"#;

async fn fetch_imports(
    state: &AppState,
    cookie: Option<&str>,
    gid: Uuid,
) -> Result<Vec<CalendarImportResponse>, ()> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/calendar-imports"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Ok(serde_json::from_value::<CalendarImportsResponse>(resp.body)
                .map(|l| l.imports)
                .unwrap_or_default())
        }
        Ok(_) | Err(_) => Err(()),
    }
}

// -- GET /agenda/imports ----------------------------------------------------

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ImportsQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/imports").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = agenda_cookie(&headers);
    let Ok(imports) = fetch_imports(&state, cookie.as_deref(), fam.gid).await else {
        return service_unavailable_page().into_response();
    };
    // Member names for the "créé par" column. A failure here is not worth
    // failing the page over — `author_name` already falls back to "Membre".
    let members: Vec<GroupMember> = fetch_group_detail(&state, cookie.as_deref(), fam.gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members)
        .unwrap_or_default();

    let may_configure = can_configure(&fam.role);
    let body = format!(
        r#"{header}
<div style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;flex-wrap:wrap;margin-bottom:1rem;">
<h1 style="margin:0;">Agendas Google</h1>
<span style="display:flex;gap:0.5rem;flex-wrap:wrap;">
<a class="button secondary" href="/agenda">Retour à l'agenda</a>
{add_button}
</span>
</div>
{banner}
{explainer}
{content}"#,
        header = fam.header,
        add_button = if may_configure {
            r#"<a class="button" href="/agenda/imports/new">Ajouter un agenda Google</a>"#
        } else {
            ""
        },
        banner = banner(&query),
        explainer = MODEL_EXPLAINER,
        content = if imports.is_empty() {
            empty_state(may_configure)
        } else {
            table(&imports, &members, may_configure)
        },
    );
    Html(shell("Agendas Google", &body)).into_response()
}

/// What the feature does, for a family that has connected nothing yet. A
/// standard member gets the same explanation minus the call to action they
/// cannot follow.
fn empty_state(may_configure: bool) -> String {
    let call_to_action = if may_configure {
        format!(
            r#"<p>Pour en connecter un : dans Google Agenda, ouvrez <strong>Paramètres</strong> → l'agenda concerné → <strong>Intégrer l'agenda</strong>, puis copiez son <strong>adresse secrète au format iCal</strong> (<a href="{GOOGLE_HELP_URL}" target="_blank" rel="noopener noreferrer">aide Google</a>). Collez-la ensuite dans <a href="/agenda/imports/new">Ajouter un agenda Google</a>.</p>"#
        )
    } else {
        r#"<p class="muted">Seul un administrateur ou le propriétaire de la famille peut connecter un agenda Google.</p>"#.to_string()
    };
    format!(
        r#"<section class="notice">
<p><strong>Aucun agenda Google connecté.</strong> Connecter un agenda permet de recopier ses événements dans l'agenda de la famille, où ils apparaissent comme n'importe quel autre événement.</p>
{call_to_action}
</section>"#
    )
}

fn table(
    imports: &[CalendarImportResponse],
    members: &[GroupMember],
    may_configure: bool,
) -> String {
    let rows: String = imports
        .iter()
        .map(|i| {
            let actions = format!(
                r#"<form method="post" action="/agenda/imports/{id}/import" style="display:inline;margin:0;"><button type="submit">Importer maintenant</button></form>{delete}"#,
                id = i.id,
                delete = if may_configure {
                    format!(
                        r#" <a class="button secondary" href="/agenda/imports/{id}/delete">Supprimer</a>"#,
                        id = i.id,
                    )
                } else {
                    String::new()
                },
            );
            format!(
                r#"<tr>
<td style="padding:0.4rem;border-bottom:1px solid var(--border);">{label}</td>
<td style="padding:0.4rem;border-bottom:1px solid var(--border);">{last}</td>
<td style="padding:0.4rem;border-bottom:1px solid var(--border);">{by} — {created}</td>
<td style="padding:0.4rem;border-bottom:1px solid var(--border);">{actions}</td>
</tr>"#,
                label = html_escape(&i.label),
                last = html_escape(&format_last_imported(i.last_imported_at)),
                by = html_escape(author_name(members, i.created_by)),
                created = fmt_paris(i.created_at, "%d/%m/%Y"),
                actions = actions,
            )
        })
        .collect();
    format!(
        r#"<table style="width:100%;border-collapse:collapse;">
<thead><tr>
<th style="text-align:left;padding:0.4rem;">Agenda</th>
<th style="text-align:left;padding:0.4rem;">Dernier import</th>
<th style="text-align:left;padding:0.4rem;">Ajouté par</th>
<th style="text-align:left;padding:0.4rem;">Actions</th>
</tr></thead>
<tbody>{rows}</tbody></table>"#
    )
}

// -- GET /agenda/imports/new + POST /agenda/imports -------------------------

#[derive(serde::Deserialize, Default)]
pub struct NewImportForm {
    #[serde(default)]
    label: String,
    /// Submitted once, by POST body only. Never echoed back into the re-rendered
    /// form — see the module header.
    #[serde(default)]
    feed_url: String,
}

/// The connect form. `label` is pre-filled after a failed submit (it is not a
/// secret); `feed_url` deliberately is not — the page asks for it again rather
/// than putting a credential back into the HTML.
fn new_page(fam: &FamilyContext, error: Option<&str>, label: &str) -> String {
    let error_html = error
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    format!(
        r#"{header}
<h1>Connecter un agenda Google</h1>
{error_html}
{explainer}
<section class="notice">
<p><strong>Où trouver l'adresse ?</strong></p>
<ol>
<li>Ouvrez Google Agenda sur un ordinateur.</li>
<li>Cliquez sur l'engrenage <strong>Paramètres</strong>.</li>
<li>Dans la colonne de gauche, sélectionnez l'agenda à connecter.</li>
<li>Descendez jusqu'à <strong>Intégrer l'agenda</strong>.</li>
<li>Copiez l'<strong>adresse secrète au format iCal</strong> (elle se termine par <code>.ics</code>).</li>
</ol>
<p class="muted"><a href="{help}" target="_blank" rel="noopener noreferrer">Aide Google : intégrer un agenda</a></p>
</section>
<p class="notice error"><strong>Cette adresse est un secret.</strong> Toute personne qui la détient peut lire cet agenda, sans mot de passe. Elle est enregistrée chiffrée et ne pourra plus être réaffichée ensuite : pour la changer, il faudra supprimer cette connexion et en créer une nouvelle. Si vous pensez l'avoir divulguée, réinitialisez-la depuis Google Agenda.</p>
<form method="post" action="/agenda/imports">
<label>Nom de l'agenda <input type="text" name="label" required value="{label}" placeholder="Agenda de Marie"/></label>
<label>Adresse secrète au format iCal
<input type="password" name="feed_url" required autocomplete="off" spellcheck="false" placeholder="https://calendar.google.com/calendar/ical/…/basic.ics"/>
<span class="muted">Commence par https:// et se termine par .ics.</span>
</label>
<button type="submit">Connecter cet agenda</button>
</form>
<div class="links"><a href="/agenda/imports">Retour aux agendas Google</a></div>"#,
        header = fam.header,
        error_html = error_html,
        explainer = MODEL_EXPLAINER,
        help = GOOGLE_HELP_URL,
        label = html_escape(label),
    )
}

pub async fn new_get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/imports/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    if !can_configure(&fam.role) {
        return Redirect::to("/agenda/imports?error=forbidden").into_response();
    }
    Html(shell(
        "Connecter un agenda Google",
        &new_page(&fam, None, ""),
    ))
    .into_response()
}

pub async fn create(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NewImportForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/imports/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    if !can_configure(&fam.role) {
        return Redirect::to("/agenda/imports?error=forbidden").into_response();
    }

    let render_error = |code: &str| {
        Html(shell(
            "Connecter un agenda Google",
            &new_page(&fam, Some(code), &form.label),
        ))
        .into_response()
    };

    if let Err(err) = validate_import_form(&form.label, &form.feed_url) {
        return render_error(error_code(err));
    }

    let req = CreateCalendarImportRequest {
        label: form.label.trim().to_string(),
        feed_url: form.feed_url.trim().to_string(),
    };
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/calendar-imports", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap_or_default()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/agenda/imports?notice=import_created").into_response()
        }
        // Defensive: `validate_import_form` mirrors these, so a 400 here means a
        // forged or drifted request. Map the backend's own code back onto the
        // form rather than showing a generic failure.
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable")
                .to_string();
            render_error(&code)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to("/agenda/imports?error=forbidden").into_response()
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}

// -- POST /agenda/imports/:id/import ----------------------------------------

/// Any member may trigger a run — same bar as `apps/api`'s
/// `trigger_calendar_import`. PRG carries the three counters back to the list.
pub async fn run(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(import_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/imports").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/calendar-imports/{}/import", fam.gid, import_id),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let run = serde_json::from_value::<ImportRunResponse>(resp.body).unwrap_or(
                ImportRunResponse {
                    imported: 0,
                    updated: 0,
                    skipped: 0,
                },
            );
            Redirect::to(&format!(
                "/agenda/imports?notice=imported&imported={}&updated={}&skipped={}",
                run.imported, run.updated, run.skipped
            ))
            .into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            Redirect::to("/agenda/imports?error=not_found").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => Redirect::to(
            &format!("/agenda/imports?error={}", import_error_code(&resp.body)),
        )
        .into_response(),
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to("/agenda/imports?error=forbidden").into_response()
        }
        Ok(_) | Err(_) => Redirect::to("/agenda/imports?error=unavailable").into_response(),
    }
}

// -- GET/POST /agenda/imports/:id/delete ------------------------------------

/// The confirmation page. What happens to the already-imported events is a
/// *choice* (#55), not a consequence to endure: `calendar_import_events`
/// cascades from `calendar_imports` but `events` does not
/// (`0010_google_calendar_import.sql`), so the connection can go with or
/// without them.
///
/// The checkbox ships **unticked** — keeping them stays the default, because an
/// imported event may since have picked up local work that has no Google
/// counterpart (a reminder and its queued notifications, an attachment, a
/// completion), all of which would cascade away with it. So the page states
/// both outcomes and lets the reader pick, rather than asserting one.
///
/// The duplicate warning below is unconditional: with the UID→event mapping
/// gone, re-adding the same calendar has nothing to match on and re-inserts
/// everything: alongside the survivors in the kept branch, on a clean agenda in
/// the other. It is also why the backend exposes no `PATCH` for the label or
/// the URL.
fn delete_page(fam: &FamilyContext, import: &CalendarImportResponse) -> String {
    format!(
        r#"{header}
<h1>Retirer « {label} » ?</h1>
<p>Cette connexion ne sera plus utilisable et son adresse iCal sera effacée.</p>
<form method="post" action="/agenda/imports/{id}/delete">
<section class="notice">
<p><strong>Que faire des événements déjà importés ?</strong></p>
<ul>
<li>Par défaut, ils <strong>restent dans l'agenda</strong> de la famille : ils deviennent des événements ordinaires, que vous pouvez modifier ou supprimer un par un.</li>
<li>Si vous cochez la case ci-dessous, ils sont <strong>supprimés en même temps</strong> que la connexion — avec, pour chacun, ses rappels, ses pièces jointes et ce qui a été coché comme fait. Cela ne se limite pas à ce qui vient de Google : c'est aussi ce que votre famille a ajouté sur ces événements depuis l'import.</li>
</ul>
<p><label><input type="checkbox" name="delete_events" value="true"> Supprimer aussi les événements que cet agenda a importés</label></p>
</section>
<section class="notice error">
<p>Dans les deux cas : si vous reconnectez le même agenda plus tard, ses événements seront <strong>ré-importés en double</strong> à côté de ceux qui restent — le lien qui permettait de les reconnaître aura disparu.</p>
<p>Vous vouliez seulement corriger le nom ou l'adresse ? Il n'existe pas de modification en place : supprimer puis recréer implique ce ré-import complet.</p>
</section>
<button type="submit" class="danger">Retirer cet agenda Google</button>
</form>
<div class="links"><a href="/agenda/imports">Annuler</a></div>"#,
        header = fam.header,
        label = html_escape(&import.label),
        id = import.id,
    )
}

/// The confirmation's checkbox, as submitted. Unticked checkboxes are not
/// submitted at all, so absence is the default (keep the events); a ticked one
/// arrives as the form's `value`, with `on` accepted too since that is what a
/// browser sends for a valueless checkbox. Anything else reads as "not asked
/// for": the destructive branch takes a positive signal only.
#[derive(serde::Deserialize, Default)]
pub struct DeleteImportForm {
    delete_events: Option<String>,
}

fn wants_event_deletion(field: Option<&str>) -> bool {
    matches!(field, Some("true") | Some("on"))
}

/// Resolves the connection the confirmation page is about. There is no
/// single-import GET on `apps/api`, so it is found in the list — same approach
/// F9's user detail page takes.
async fn find_import(
    state: &AppState,
    cookie: Option<&str>,
    gid: Uuid,
    import_id: Uuid,
) -> Result<Option<CalendarImportResponse>, ()> {
    let imports = fetch_imports(state, cookie, gid).await?;
    Ok(imports.into_iter().find(|i| i.id == import_id))
}

pub async fn delete_get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(import_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/imports").await else {
        return Redirect::to("/groups/new").into_response();
    };
    if !can_configure(&fam.role) {
        return Redirect::to("/agenda/imports?error=forbidden").into_response();
    }
    let cookie = agenda_cookie(&headers);
    match find_import(&state, cookie.as_deref(), fam.gid, import_id).await {
        Ok(Some(import)) => Html(shell(
            &format!("Retirer {}", import.label),
            &delete_page(&fam, &import),
        ))
        .into_response(),
        Ok(None) => Redirect::to("/agenda/imports?error=not_found").into_response(),
        Err(()) => service_unavailable_page().into_response(),
    }
}

/// `Form` comes last: it consumes the request body, so no extractor may follow
/// it (axum's rule for `FromRequest`).
pub async fn delete_post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(import_id): Path<Uuid>,
    Form(form): Form<DeleteImportForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/agenda/imports").await else {
        return Redirect::to("/groups/new").into_response();
    };
    if !can_configure(&fam.role) {
        return Redirect::to("/agenda/imports?error=forbidden").into_response();
    }
    let delete_events = wants_event_deletion(form.delete_events.as_deref());
    let cookie = agenda_cookie(&headers);
    match api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!(
            "/groups/{}/calendar-imports/{}?delete_events={delete_events}",
            fam.gid, import_id
        ),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            // The count travels only on the branch that ran, so the banner
            // never says "0 événement supprimé" about events it kept.
            if delete_events {
                let deleted = serde_json::from_value::<DeleteCalendarImportResponse>(resp.body)
                    .map(|r| r.deleted_events)
                    .unwrap_or(0);
                Redirect::to(&format!(
                    "/agenda/imports?notice=import_deleted&deleted={deleted}"
                ))
                .into_response()
            } else {
                Redirect::to("/agenda/imports?notice=import_deleted").into_response()
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            Redirect::to("/agenda/imports?error=not_found").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to("/agenda/imports?error=forbidden").into_response()
        }
        Ok(_) | Err(_) => Redirect::to("/agenda/imports?error=unavailable").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- import_error_code ---------------------------------------------------

    fn err_body(message: &str) -> serde_json::Value {
        serde_json::json!({ "error": message })
    }

    #[test]
    fn maps_each_422_code_the_backend_can_return() {
        assert_eq!(
            import_error_code(&err_body("feed_too_large")),
            "feed_too_large"
        );
        assert_eq!(import_error_code(&err_body("invalid_ics")), "invalid_ics");
    }

    #[test]
    fn feed_fetch_failed_is_matched_on_its_prefix() {
        // The backend interpolates reqwest's error after the code.
        assert_eq!(
            import_error_code(&err_body("feed_fetch_failed: status 404")),
            "feed_fetch_failed"
        );
    }

    #[test]
    fn the_feed_url_inside_a_fetch_error_never_leaves_this_function() {
        // reqwest's Display can embed the URL it was given — the code returned
        // here is a fixed string, so nothing of it reaches a page or a redirect.
        let leaky = "feed_fetch_failed: error sending request for url (https://calendar.google.com/calendar/ical/secret-token/basic.ics)";
        let code = import_error_code(&err_body(leaky));
        assert_eq!(code, "feed_fetch_failed");
        assert!(!code.contains("secret-token"));
        assert!(!error_text(code).unwrap().contains("secret-token"));
    }

    #[test]
    fn an_unrecognised_body_falls_back_to_unavailable() {
        assert_eq!(
            import_error_code(&err_body("something_else")),
            "unavailable"
        );
        assert_eq!(import_error_code(&serde_json::json!({})), "unavailable");
    }

    // -- notice_text ---------------------------------------------------------

    fn query(notice: Option<&str>, counts: Option<(usize, usize, usize)>) -> ImportsQuery {
        ImportsQuery {
            notice: notice.map(|s| s.to_string()),
            error: None,
            imported: counts.map(|c| c.0),
            updated: counts.map(|c| c.1),
            skipped: counts.map(|c| c.2),
            deleted: None,
        }
    }

    #[test]
    fn the_import_notice_renders_the_run_counters() {
        assert_eq!(
            notice_text(&query(Some("imported"), Some((3, 1, 12)))).unwrap(),
            "Import terminé : 3 événements importés, 1 mis à jour, 12 inchangés."
        );
    }

    #[test]
    fn missing_counters_read_as_zero_rather_than_breaking_the_banner() {
        assert_eq!(
            notice_text(&query(Some("imported"), None)).unwrap(),
            "Import terminé : 0 événement importé, 0 mis à jour, 0 inchangé."
        );
    }

    #[test]
    fn the_delete_notice_states_that_imported_events_remain() {
        let text = notice_text(&query(Some("import_deleted"), None)).unwrap();
        assert!(text.contains("restent dans l'agenda"));
    }

    // -- the delete-events branch (#55) --------------------------------------

    fn delete_query(deleted: Option<usize>) -> ImportsQuery {
        ImportsQuery {
            notice: Some("import_deleted".to_string()),
            error: None,
            imported: None,
            updated: None,
            skipped: None,
            deleted,
        }
    }

    /// The count only reaches the banner when the branch actually ran, so
    /// the two outcomes never wear each other's sentence.
    #[test]
    fn the_delete_notice_names_the_count_when_the_events_went_too() {
        let text = notice_text(&delete_query(Some(3))).unwrap();
        assert!(text.contains('3'), "{text}");
        assert!(!text.contains("restent dans l'agenda"), "{text}");
    }

    #[test]
    fn the_delete_notice_without_a_count_still_says_the_events_remain() {
        let text = notice_text(&delete_query(None)).unwrap();
        assert!(text.contains("restent dans l'agenda"), "{text}");
    }

    /// An unticked checkbox is not submitted at all; a ticked one arrives
    /// as this form's `value`. Anything else is treated as "not asked for"
    /// — the destructive branch requires a positive signal, never a
    /// default or a typo.
    #[test]
    fn only_a_ticked_checkbox_asks_for_the_events_to_go() {
        assert!(wants_event_deletion(Some("true")));
        // Browsers submit a valueless checkbox as `on`.
        assert!(wants_event_deletion(Some("on")));
        assert!(!wants_event_deletion(None));
        assert!(!wants_event_deletion(Some("false")));
        assert!(!wants_event_deletion(Some("")));
    }

    /// The confirmation must present both outcomes as a choice, with the
    /// checkbox unticked: keeping the events is still the default.
    #[test]
    fn the_confirmation_offers_the_deletion_unticked() {
        let import = CalendarImportResponse {
            id: Uuid::nil(),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            label: "Agenda de Marie".to_string(),
            last_imported_at: None,
            created_at: chrono::Utc::now(),
        };
        let fam = FamilyContext {
            gid: Uuid::nil(),
            role: "owner".to_string(),
            header: String::new(),
        };
        let page = delete_page(&fam, &import);

        assert!(page.contains(r#"name="delete_events""#), "{page}");
        assert!(page.contains(r#"type="checkbox""#), "{page}");
        assert!(
            !page.contains("checked"),
            "the destructive branch must not be pre-ticked"
        );
        // Both outcomes stated, so neither is a surprise after the click.
        assert!(page.contains("restent dans l'agenda"), "{page}");
        assert!(page.contains("ré-importés en double"), "{page}");
    }

    #[test]
    fn an_unknown_notice_code_renders_nothing() {
        assert!(notice_text(&query(Some("nope"), None)).is_none());
        assert!(notice_text(&query(None, None)).is_none());
    }
}
