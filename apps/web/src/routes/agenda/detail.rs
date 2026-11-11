//! `/agenda/:id` — event/task detail, with per-occurrence completion for
//! recurring tasks (the crux of this epic: completion lives in
//! `event_occurrence_completions`, never on the shared `events` row, so the
//! detail lists occurrences and toggles them one at a time), plus the
//! delete action and the add-reminder form. Edit lives in `edit.rs`.
//! Opened on an occurrence of a recurring event (`?occ=`), « Supprimer »
//! leads to `/agenda/:id/delete?occ=`, which asks what goes: the whole
//! series, this occurrence and the following ones, or this one alone (#710).
//!
//! Error table: 404 (`get_event`) unknown/foreign event → introuvable page;
//! 403 (`update_event`/`delete_event` permission bar) → `?error=forbidden`;
//! 400 `completed_only_valid_for_tasks` / `occurrence_at_required_for_recurring_task`
//! mapped defensively (the UI only renders those forms where valid).

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{DateTime, Duration, Utc};
use manage_our_home_shared::dto::agenda::{
    AttachmentResponse, EditScope, EventResponse, OccurrenceResponse, UpdateEventRequest,
};
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::{
    format_reminder_offset, parse_rrule, Freq, RecurrenceEnd,
};
use manage_our_home_shared::validation::messagerie::author_name;
use uuid::Uuid;

use crate::app::{html_escape, member_link, shell_with_header, Icon, Width};
use crate::layout::CurrentUser;
use crate::lenient_query::{lenient, LenientQuery};
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::edit::{occ_param, occurrence_label, read_scope, scope_radios, ScopeChoice};
use super::new::{reminder_push_block, reminder_select};
use super::{
    agenda_cookie, can_modify, event_not_found_page, family_context_for, fmt_paris, forbidden_page,
    json_from_response, occurrences_from_response, private_line, read_or_log,
    service_unavailable_page, today_paris, with_family, EventFamily,
};

#[derive(serde::Deserialize)]
pub struct DetailQuery {
    occ: Option<String>,
    notice: Option<String>,
    error: Option<String>,
    /// Set after a reminder is created, so the (non-enumerable) reminder can
    /// be shown once with a delete link. Both read as absent when they do
    /// not convert (#580): no banner, offset 0. Any repeated parameter keeps
    /// its first occurrence (#582, [`LenientQuery`]).
    #[serde(default, deserialize_with = "lenient")]
    rid: Option<Uuid>,
    #[serde(default, deserialize_with = "lenient")]
    offset: Option<i32>,
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "completion_updated" => Some("Complétion mise à jour."),
        "reminder_added" => Some("Rappel ajouté."),
        "reminder_deleted" => Some("Rappel supprimé."),
        "attachment_added" => Some("Pièce jointe ajoutée."),
        "attachment_deleted" => Some("Pièce jointe supprimée."),
        "updated" => Some("Événement mis à jour."),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "forbidden" => Some("Vous n'avez pas les droits nécessaires pour cette action."),
        "completed_only_valid_for_tasks" => Some("Seules les tâches peuvent être complétées."),
        "occurrence_required" => Some("Occurrence manquante pour cette tâche récurrente."),
        "unsupported_file_type" => {
            Some("Type de fichier non autorisé (png, jpg, webp, pdf, mp4, webm).")
        }
        "file_too_large" => Some("Fichier trop volumineux (max 20 Mo)."),
        "upload_failed" => Some("L'envoi de la pièce jointe a échoué, merci de réessayer."),
        "reminder_not_found" => Some("Ce rappel n'existe plus."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        // The refusals of a delete scoped to an occurrence (#710).
        "invalid_form" => Some("Formulaire incomplet ou illisible, merci de réessayer."),
        "occurrence_not_in_series" => {
            Some("Cette occurrence ne fait plus partie de la série : rouvrez-la depuis l'agenda.")
        }
        "scope_not_allowed_for_imported_event" => {
            Some("Un événement importé ne se supprime que pour toute la série.")
        }
        "scope_requires_recurring_event" => {
            Some("Cet événement n'est plus récurrent : choisissez « Toute la série ».")
        }
        _ => None,
    }
}

/// The "Assigné à" line: the display names behind an event's
/// `assignee_ids`, in the order the API returned them.
///
/// The assignment introduced by #73 was visible only on the dashboard: a
/// member could assign an event to two people, open its detail page, and
/// find no trace of it anywhere (#98 verification, round 2). Names only, no
/// coloured avatar: the dashboard's ring needs an inline `style=` and this
/// app's inline-style budget (`app.rs`, `INLINE_STYLE_CEILING`) is full —
/// and on a page that shows one event at a time the name *is* the answer,
/// the colour would only repeat it.
///
/// Empty when the event has no assignee, or when the member roster couldn't
/// be loaded — in which case a line of raw UUIDs would be worse than no line.
///
/// "Historical rows only" is what this used to say, on the strength of
/// `resolve_assignees` keeping at least the creator. That covered the
/// `/agenda` write paths and missed the Google Calendar mirror, which wrote
/// `events` directly and never wrote `event_assignees` at all (#106) — a
/// permanent source of assignee-less events, not a leftover. The import now
/// assigns the account that ran it and repairs the rows it wrote before, so
/// the remaining ways in are genuinely historical again; the branch stays
/// because a detail page that guesses at who an event is for would be worse
/// than one that says nothing.
///
/// Each name is a link to the assignee's profile in the event's group
/// (#466), still without the avatar. An assignee no longer in the roster has
/// no profile to open, and keeps the plain fallback name.
fn assignees_html(group_id: Uuid, assignee_ids: &[Uuid], members: &[GroupMember]) -> String {
    if assignee_ids.is_empty() || members.is_empty() {
        return String::new();
    }
    let names: Vec<String> = assignee_ids
        .iter()
        .map(|id| {
            let name = author_name(members, *id);
            if members.iter().any(|m| m.user_id == *id) {
                member_link(group_id, *id, name, Icon::None)
            } else {
                html_escape(name)
            }
        })
        .collect();
    format!("<p><strong>Assigné à :</strong> {}</p>", names.join(", "))
}

/// A human-readable French description of a v1 RRULE (falls back to a
/// generic label for a rule outside the v1 subset).
fn describe_recurrence(rrule: &str) -> String {
    let Some(r) = parse_rrule(rrule) else {
        return "Récurrence avancée".to_string();
    };
    let base = match r.freq {
        Freq::Daily if r.interval == 1 => "Tous les jours".to_string(),
        Freq::Daily => format!("Tous les {} jours", r.interval),
        Freq::Weekly if r.interval == 1 => "Chaque semaine".to_string(),
        Freq::Weekly => format!("Toutes les {} semaines", r.interval),
        Freq::Monthly if r.interval == 1 => "Chaque mois".to_string(),
        Freq::Monthly => format!("Tous les {} mois", r.interval),
        Freq::Yearly if r.interval == 1 => "Chaque année".to_string(),
        Freq::Yearly => format!("Tous les {} ans", r.interval),
    };
    let suffix = match r.end {
        RecurrenceEnd::Never => String::new(),
        RecurrenceEnd::Count(n) => format!(", {n} fois"),
        RecurrenceEnd::Until(d) => format!(", jusqu'au {}", d.format("%d/%m/%Y")),
    };
    format!("{base}{suffix}")
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    EventFamily(requested): EventFamily,
    LenientQuery(query): LenientQuery<DetailQuery>,
) -> Response {
    let Some(fam) = family_context_for(
        &state,
        &headers,
        &me,
        &format!("/agenda/{event_id}"),
        requested,
    )
    .await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = agenda_cookie(&headers);

    let event = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/events/{}", fam.gid, event_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<EventResponse>(resp.body) {
                Ok(e) => e,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return event_not_found_page().into_response()
        }
        Ok(_) => return service_unavailable_page().into_response(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    // `None`: the list could not be read, which renders as an error rather
    // than as "no attachment" (#455).
    let attachments: Option<Vec<AttachmentResponse>> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/events/{}/attachments", fam.gid, event_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) => read_or_log(
            json_from_response(resp.status, resp.body),
            fam.gid,
            "agenda detail attachments",
        ),
        Err(error) => read_or_log::<Vec<_>, _>(Err(error), fam.gid, "agenda detail attachments"),
    };

    // Recurring task: fetch the upcoming occurrences (next 60 days) so each
    // one can carry its own completion toggle. `None`: the answer could not
    // be read, which renders as an error rather than as "no upcoming
    // occurrence" (#455).
    let recurring_task = event.is_task && event.rrule.is_some();
    let occurrences: Option<Vec<OccurrenceResponse>> = if recurring_task {
        let from = today_paris();
        let to = from + Duration::days(60);
        let path = format!(
            "/groups/{}/events?from={}T00:00:00Z&to={}T23:59:59Z",
            fam.gid, from, to
        );
        let answer =
            api_request_auth(&state, reqwest::Method::GET, &path, cookie.as_deref(), None).await;
        let read = match answer {
            Ok(resp) => read_or_log(
                occurrences_from_response(resp.status, resp.body),
                fam.gid,
                "agenda detail",
            ),
            Err(error) => read_or_log::<Vec<_>, _>(Err(error), fam.gid, "agenda detail"),
        };
        read.map(|occs| {
            occs.into_iter()
                .filter(|o| o.event.id == event_id)
                .collect()
        })
    } else {
        Some(Vec::new())
    };

    let members = fetch_group_detail(&state, cookie.as_deref(), fam.gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members)
        .unwrap_or_default();

    let can_edit = can_modify(&fam.role, event.created_by == me.user_id);
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    let reminder = query.rid.map(|rid| (rid, query.offset.unwrap_or(0)));
    // A reminder goes to the event's creator (#291): whether it will reach
    // them is only the viewer's business when the viewer is the creator.
    let push = if event.created_by == me.user_id {
        reminder_push_block(&state, cookie.as_deref()).await
    } else {
        String::new()
    };

    Html(page(
        &fam.header,
        &event,
        occurrences.as_deref(),
        attachments.as_deref(),
        &members,
        can_edit,
        query.occ.as_deref(),
        notice,
        error,
        reminder,
        &push,
        fam.carried.map(|gid| (gid, fam.name.as_str())),
    ))
    .into_response()
}

/// Where the page's forms and links lead: the event's own URLs, in the
/// family it was opened in (#622). The active family's carry nothing.
#[derive(Clone, Copy)]
struct EventUrls {
    id: Uuid,
    family: Option<Uuid>,
}

impl EventUrls {
    /// `/agenda/<id><rest>`, carrying the family.
    fn to(self, rest: &str) -> String {
        with_family(&format!("/agenda/{}{rest}", self.id), self.family)
    }
}

/// An event of the active family.
impl From<Uuid> for EventUrls {
    fn from(id: Uuid) -> Self {
        Self { id, family: None }
    }
}

/// `other_family` is the family the event was opened in when it is not the
/// active one (#622): its id, which every URL of the page carries, and its
/// name, which the page says — the header names the active family.
#[allow(clippy::too_many_arguments)]
fn page(
    header: &str,
    event: &EventResponse,
    occurrences: Option<&[OccurrenceResponse]>,
    attachments: Option<&[AttachmentResponse]>,
    members: &[GroupMember],
    can_edit: bool,
    focus_occ: Option<&str>,
    notice: Option<&str>,
    error: Option<&str>,
    reminder: Option<(Uuid, i32)>,
    push: &str,
    other_family: Option<(Uuid, &str)>,
) -> String {
    let urls = EventUrls {
        id: event.id,
        family: other_family.map(|(gid, _)| gid),
    };
    let family_html = other_family
        .map(|(_, name)| format!("<p><strong>Famille :</strong> {}</p>", html_escape(name)))
        .unwrap_or_default();
    let notice_html = notice
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();

    let when = if event.all_day {
        format!(
            "{} — journée entière",
            fmt_paris(event.starts_at, "%d/%m/%Y")
        )
    } else {
        format!(
            "{} → {}",
            fmt_paris(event.starts_at, "%d/%m/%Y %H:%M"),
            fmt_paris(event.ends_at, "%d/%m/%Y %H:%M"),
        )
    };
    let kind = if event.is_task {
        "Tâche"
    } else {
        "Événement"
    };
    let location_html = event
        .location
        .as_deref()
        .filter(|l| !l.is_empty())
        .map(|l| format!("<p><strong>Lieu :</strong> {}</p>", html_escape(l)))
        .unwrap_or_default();
    let description_html = event
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(|d| format!("<p>{}</p>", html_escape(d)))
        .unwrap_or_default();
    let assignees = assignees_html(event.group_id, &event.assignee_ids, members);
    let recurrence_html = event
        .rrule
        .as_deref()
        .map(|r| {
            format!(
                "<p><strong>Récurrence :</strong> {}</p>",
                html_escape(&describe_recurrence(r))
            )
        })
        .unwrap_or_default();

    // Completion UI.
    let completion_html = if event.is_task {
        if event.rrule.is_some() {
            render_occurrence_completions(urls, occurrences, focus_occ)
        } else {
            render_oneoff_completion(urls, event)
        }
    } else {
        String::new()
    };

    // Edit/delete controls (creator or owner/admin only).
    let controls_html = if can_edit {
        format!(
            r#"<div class="actions">
<a class="btn secondary" href="{edit}">Modifier</a>
{delete}
</div>"#,
            edit = edit_href(urls, event, focus_occ),
            delete = delete_control(urls, event, focus_occ),
        )
    } else {
        r#"<p class="muted">Seul le créateur ou un administrateur peut modifier ou supprimer cet événement.</p>"#.to_string()
    };

    // Reminders: add form + one-shot display of a just-created reminder
    // (backend exposes no list endpoint — documented v1 limitation).
    let just_created = reminder
        .map(|(rid, offset)| render_reminder_created(urls, rid, offset))
        .unwrap_or_default();
    let reminder_select_html = reminder_select("reminder", None);
    let add_reminder = urls.to("/reminders");
    let reminders_html = format!(
        r#"<h2>Rappels</h2>
<p class="muted">Ajoute un rappel avant l'événement. Il est envoyé au créateur de l'événement, par notification ou par email selon son choix (le vôtre se règle dans <a href="/account/notifications">Notifications de rappel</a>). La liste des rappels existants n'est pas affichable (limitation v1).</p>
{just_created}
<form method="post" action="{add_reminder}">
<label>Nouveau rappel {reminder_select_html}</label>
{push}
<button type="submit">Ajouter le rappel</button>
</form>"#
    );

    // Attachments.
    let attachments_html = render_attachments(urls, attachments, can_edit);

    let body = format!(
        r#"<h1>{title}</h1>
{notice_html}{error_html}
<p class="muted">{kind}</p>
{private}
{family_html}
<p><strong>Quand :</strong> {when}</p>
{assignees}
{location_html}{description_html}{recurrence_html}
{completion_html}
{controls_html}
{reminders_html}
{attachments_html}
<div class="links"><a href="/agenda">Retour à l'agenda</a></div>"#,
        title = html_escape(&event.title),
        private = private_line(event.is_private),
    );
    shell_with_header(Width::Read, &event.title, header, &body)
}

/// The « Modifier » link (#709): on the occurrence the detail was opened
/// from (`occ`), for a recurring event, so the edit form can offer to change
/// that occurrence alone or from it on.
fn edit_href(urls: EventUrls, event: &EventResponse, focus_occ: Option<&str>) -> String {
    match focused_occurrence(event, focus_occ) {
        Some(at) => urls.to(&format!("/edit?occ={}", occ_param(at))),
        None => urls.to("/edit"),
    }
}

/// The occurrence of a recurring event the detail was opened from (`occ`),
/// when it reads as an instant.
fn focused_occurrence(event: &EventResponse, focus_occ: Option<&str>) -> Option<DateTime<Utc>> {
    focus_occ
        .filter(|_| event.rrule.is_some())
        .and_then(|occ| occ.parse::<DateTime<Utc>>().ok())
}

/// « Supprimer » (#710): on an occurrence of a recurring event, a link to
/// the screen that asks which occurrences go; otherwise the form that
/// deletes the event whole, as before — a series opened without an
/// occurrence has none to offer the choice on.
fn delete_control(urls: EventUrls, event: &EventResponse, focus_occ: Option<&str>) -> String {
    match focused_occurrence(event, focus_occ) {
        Some(at) => format!(
            r#"<a class="btn secondary danger" href="{href}">Supprimer</a>"#,
            href = urls.to(&format!("/delete?occ={}", occ_param(at))),
        ),
        None => format!(
            r#"<form method="post" action="{delete}">
<button type="submit" class="secondary danger">Supprimer</button>
</form>"#,
            delete = urls.to("/delete"),
        ),
    }
}

/// The delete screen of an occurrence of a series (#710): the three
/// choices of the edit form (#709), « Toute la série » checked — the
/// screen picks it, whoever opens it.
fn delete_page(
    header: &str,
    event: &EventResponse,
    occurrence_at: DateTime<Utc>,
    family: Option<Uuid>,
) -> String {
    let urls = EventUrls {
        id: event.id,
        family,
    };
    let body = format!(
        r#"<h1>Supprimer — {title}</h1>
<p>Cet événement se répète. Vous l'avez ouvert sur l'occurrence du {when}.</p>
<form method="post" action="{action}">
<fieldset class="card">
<legend>Supprimer</legend>
<input type="hidden" name="occurrence_at" value="{occ}"/>
{radios}</fieldset>
<button type="submit" class="danger">Supprimer</button>
</form>
<div class="links"><a href="{back}">Annuler</a></div>"#,
        title = html_escape(&event.title),
        when = occurrence_label(occurrence_at, event.all_day),
        action = urls.to("/delete"),
        occ = occ_param(occurrence_at),
        radios = scope_radios(EditScope::All),
        back = urls.to(&format!("?occ={}", occ_param(occurrence_at))),
    );
    shell_with_header(Width::Form, "Supprimer l'événement", header, &body)
}

/// The API path a delete calls: the event whole without parameters, as
/// before #710; one occurrence, or one and the following, with the scope and
/// the occurrence in the query.
fn delete_api_path(group_id: Uuid, event_id: Uuid, choice: Option<ScopeChoice>) -> String {
    let path = format!("/groups/{group_id}/events/{event_id}");
    let scope = match choice.map(|c| (c.scope, c.occurrence_at)) {
        None | Some((EditScope::All, _)) => return path,
        Some((EditScope::This, at)) => ("this", at),
        Some((EditScope::ThisAndFollowing, at)) => ("this_and_following", at),
    };
    format!(
        "{path}?scope={}&occurrence_at={}",
        scope.0,
        occ_param(scope.1)
    )
}

/// The detail error a 400 of a scoped delete comes back under (#710).
fn rejected_delete_code(api_code: Option<&str>) -> &'static str {
    match api_code {
        Some("occurrence_not_in_series") => "occurrence_not_in_series",
        Some("scope_not_allowed_for_imported_event") => "scope_not_allowed_for_imported_event",
        Some("scope_requires_recurring_event") => "scope_requires_recurring_event",
        _ => "unavailable",
    }
}

/// The one-shot notice shown after a reminder is created, with its delete
/// button. `offset` is the minutes-before value echoed in the redirect.
/// The API refuses a negative offset; a forged one in the query reads as 0.
fn render_reminder_created(urls: impl Into<EventUrls>, rid: Uuid, offset: i32) -> String {
    let minutes = u32::try_from(offset).unwrap_or(0);
    let when = if minutes == 0 {
        format_reminder_offset(0)
    } else {
        format!("{} avant", format_reminder_offset(minutes))
    };
    format!(
        r#"<p class="notice success split">
<span>Rappel actif : {when}.</span>
<form method="post" action="{delete}">
<button type="submit" class="secondary">Supprimer ce rappel</button>
</form></p>"#,
        delete = urls.into().to(&format!("/reminders/{rid}/delete")),
    )
}

fn render_oneoff_completion(urls: EventUrls, event: &EventResponse) -> String {
    let done = event.completed_at.is_some();
    let (label, next, status) = if done {
        ("Marquer à faire", "false", "✅ Fait")
    } else {
        ("Marquer faite", "true", "⬜ À faire")
    };
    format!(
        r#"<div class="actions">
<span><strong>{status}</strong></span>
<form method="post" action="{complete}">
<input type="hidden" name="completed" value="{next}"/>
<button type="submit" class="secondary">{label}</button>
</form></div>"#,
        complete = urls.to("/complete"),
    )
}

/// `occurrences` is `None` when the API's answer could not be read: that
/// renders as an error, never as "no upcoming occurrence" (#455).
fn render_occurrence_completions(
    urls: impl Into<EventUrls>,
    occurrences: Option<&[OccurrenceResponse]>,
    focus_occ: Option<&str>,
) -> String {
    let complete = urls.into().to("/complete");
    let Some(occurrences) = occurrences else {
        return r#"<p class="notice error">Impossible de charger les occurrences pour le moment.</p>"#
            .to_string();
    };
    if occurrences.is_empty() {
        return r#"<p class="muted">Aucune occurrence à venir dans les 60 prochains jours.</p>"#
            .to_string();
    }
    let rows: String = occurrences
        .iter()
        .map(|occ| {
            let occ_param = occ
                .occurrence_starts_at
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            let done = occ.event.completed_at.is_some();
            let (label, next, status) = if done {
                ("Marquer à faire", "false", "✅")
            } else {
                ("Marquer faite", "true", "⬜")
            };
            let highlight = if focus_occ == Some(occ_param.as_str()) {
                " current"
            } else {
                ""
            };
            format!(
                r#"<li class="list-row{highlight}">
<span>{status} {when}</span>
<form method="post" action="{complete}">
<input type="hidden" name="completed" value="{next}"/>
<input type="hidden" name="occurrence_at" value="{occ_param}"/>
<button type="submit" class="secondary">{label}</button>
</form></li>"#,
                when = html_escape(&fmt_paris(occ.occurrence_starts_at, "%a %d/%m/%Y %H:%M")),
            )
        })
        .collect();
    format!(
        r#"<h2>Occurrences à venir</h2>
<p class="muted">La complétion est indépendante par occurrence : cocher l'une n'affecte pas les autres.</p>
<ul class="list">{rows}</ul>"#
    )
}

/// `attachments` is `None` when the API's answer could not be read: the
/// list renders as an error, never as "no attachment", and the upload form
/// stays (#455).
fn render_attachments(
    urls: impl Into<EventUrls>,
    attachments: Option<&[AttachmentResponse]>,
    can_edit: bool,
) -> String {
    let urls = urls.into();
    let rows: String = attachments
        .unwrap_or_default()
        .iter()
        .map(|a| {
            let size_kb = (a.size_bytes as f64 / 1024.0).round() as i64;
            let delete = if can_edit {
                format!(
                    r#"<form method="post" action="{delete}">
<button type="submit" class="secondary">Supprimer</button></form>"#,
                    delete = urls.to(&format!("/attachments/{}/delete", a.id)),
                )
            } else {
                String::new()
            };
            format!(
                r#"<li class="list-row">
<a href="{download}">{name}</a>
<span class="muted">{size_kb} Ko</span>
{delete}</li>"#,
                download = urls.to(&format!("/attachments/{}/download", a.id)),
                name = html_escape(&a.filename),
            )
        })
        .collect();
    let list = match attachments {
        None => r#"<p class="notice error">Impossible de charger les pièces jointes pour le moment.</p>"#
            .to_string(),
        Some([]) => r#"<p class="muted">Aucune pièce jointe.</p>"#.to_string(),
        Some(_) => format!(r#"<ul class="list">{rows}</ul>"#),
    };
    let upload = format!(
        r#"<form method="post" action="{action}" enctype="multipart/form-data">
<label>Ajouter une pièce jointe (png, jpg, webp, pdf, mp4, webm — max 20 Mo)
<input type="file" name="file" accept="image/png,image/jpeg,image/webp,application/pdf,video/mp4,video/webm" required/></label>
<button type="submit">Envoyer</button>
</form>"#,
        action = urls.to("/attachments"),
    );
    format!(
        r#"<h2>Pièces jointes</h2>
{list}
{upload}"#
    )
}

// -- mutations --------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct CompleteForm {
    completed: String,
    #[serde(default)]
    occurrence_at: String,
}

pub async fn complete(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    EventFamily(requested): EventFamily,
    Form(form): Form<CompleteForm>,
) -> Response {
    let detail = format!("/agenda/{event_id}");
    let Some(fam) = family_context_for(&state, &headers, &_me, &detail, requested).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let completed = form.completed == "true";
    let occurrence_at: Option<DateTime<Utc>> = if form.occurrence_at.trim().is_empty() {
        None
    } else {
        match DateTime::parse_from_rfc3339(form.occurrence_at.trim()) {
            Ok(dt) => Some(dt.with_timezone(&Utc)),
            Err(_) => {
                return Redirect::to(&fam.url(&format!("{detail}?error=occurrence_required")))
                    .into_response()
            }
        }
    };

    let body = UpdateEventRequest {
        completed: Some(completed),
        occurrence_at,
        ..Default::default()
    };
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/events/{}", fam.gid, event_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    let target = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            format!("{detail}?notice=completion_updated")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            format!("{detail}?error=forbidden")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return event_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            format!("{detail}?error=occurrence_required")
        }
        Ok(_) | Err(_) => format!("{detail}?error=unavailable"),
    };
    Redirect::to(&fam.url(&target)).into_response()
}

#[derive(serde::Deserialize)]
pub struct DeleteQuery {
    /// The occurrence « Supprimer » was pressed on (#710). Read as absent
    /// when it does not convert (#580).
    #[serde(default, deserialize_with = "lenient")]
    occ: Option<DateTime<Utc>>,
}

/// `GET /agenda/:id/delete?occ=` (#710): the screen that asks which
/// occurrences of a series go. Without an occurrence of a recurring event
/// there is nothing to ask, and the detail deletes the event whole.
pub async fn delete_confirm(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    EventFamily(requested): EventFamily,
    LenientQuery(query): LenientQuery<DeleteQuery>,
) -> Response {
    let detail = format!("/agenda/{event_id}");
    let Some(fam) = family_context_for(
        &state,
        &headers,
        &me,
        &format!("{detail}/delete"),
        requested,
    )
    .await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = agenda_cookie(&headers);
    let event = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/events/{}", fam.gid, event_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<EventResponse>(resp.body) {
                Ok(e) => e,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return event_not_found_page().into_response()
        }
        _ => return service_unavailable_page().into_response(),
    };
    if !can_modify(&fam.role, event.created_by == me.user_id) {
        return forbidden_page().into_response();
    }
    let Some(occurrence_at) = query.occ.filter(|_| event.rrule.is_some()) else {
        return Redirect::to(&fam.url(&detail)).into_response();
    };
    Html(delete_page(&fam.header, &event, occurrence_at, fam.carried)).into_response()
}

/// `POST /agenda/:id/delete`: the event whole from the detail's form, or
/// what the delete screen chose (#710).
pub async fn delete(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    EventFamily(requested): EventFamily,
    raw_body: axum::body::Bytes,
) -> Response {
    let detail = format!("/agenda/{event_id}");
    let Some(fam) = family_context_for(&state, &headers, &_me, &detail, requested).await else {
        return Redirect::to("/groups/new").into_response();
    };
    // A forged occurrence or scope comes back to the detail; nothing is
    // deleted on a guess.
    let Ok(choice) = read_scope(&raw_body) else {
        return Redirect::to(&fam.url(&format!("{detail}?error=invalid_form"))).into_response();
    };
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &delete_api_path(fam.gid, event_id, choice),
        cookie.as_deref(),
        None,
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/agenda?notice=event_deleted").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            event_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = rejected_delete_code(resp.body.get("error").and_then(|v| v.as_str()));
            Redirect::to(&fam.url(&format!("{detail}?error={code}"))).into_response()
        }
        Ok(_) | Err(_) => service_unavailable_page().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct, stable ids — the assertions are about names and order, so
    /// a fixed id per member reads better than a random one.
    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn detail_query(raw: &str) -> DetailQuery {
        crate::lenient_query::from_query_str(raw)
            .expect("a malformed parameter must not reject the page")
    }

    #[test]
    fn absent_rid_and_offset_read_as_none() {
        // Each typed field needs `default` beside `deserialize_with`: without
        // it a missing parameter is a "missing field" 400.
        for raw in ["", "notice=updated"] {
            let q = detail_query(raw);
            assert!(q.rid.is_none(), "{raw}");
            assert!(q.offset.is_none(), "{raw}");
        }
    }

    #[test]
    fn malformed_rid_or_offset_reads_as_absent() {
        // #580: no banner without a valid rid; an unconvertible offset is 0.
        assert!(detail_query("rid=").rid.is_none());
        assert!(detail_query("rid=x&offset=30").rid.is_none());
        let rid = "6f1c1c1e-3b4a-4d2e-9f00-0123456789ab";
        for offset in ["", "abc", "1.5", "99999999999"] {
            let q = detail_query(&format!("rid={rid}&offset={offset}"));
            assert_eq!(q.rid, Some(Uuid::parse_str(rid).unwrap()), "{offset}");
            assert_eq!(q.offset, None, "{offset}");
        }
    }

    #[test]
    fn valid_rid_and_offset_keep_their_effect() {
        let q = detail_query("occ=2026-10-22&rid=6f1c1c1e-3b4a-4d2e-9f00-0123456789ab&offset=30");
        assert_eq!(q.occ.as_deref(), Some("2026-10-22"));
        assert!(q.rid.is_some());
        assert_eq!(q.offset, Some(30));
    }

    #[test]
    fn a_repeated_detail_parameter_keeps_its_first_occurrence() {
        // #582: a repeated key was a "duplicate field" 400.
        let q = detail_query("occ=2026-10-22&occ=2026-10-23&offset=30&offset=60&notice=a&notice=b");
        assert_eq!(q.occ.as_deref(), Some("2026-10-22"));
        assert_eq!(q.offset, Some(30));
        assert_eq!(q.notice.as_deref(), Some("a"));
    }

    fn member(id: Uuid, name: &str) -> GroupMember {
        GroupMember {
            user_id: id,
            display_name: name.to_string(),
            email: format!("{name}@example.test"),
            has_avatar: false,
            role: "member".to_string(),
        }
    }

    // -- render_reminder_created (#457) ------------------------------------

    #[test]
    fn a_one_day_reminder_reads_in_days() {
        let html = render_reminder_created(uid(9), uid(3), 1440);
        assert!(html.contains("Rappel actif : 1 jour avant."), "{html}");
        assert!(!html.contains("1440"), "{html}");
    }

    #[test]
    fn a_mixed_offset_reads_in_days_hours_and_minutes() {
        let html = render_reminder_created(uid(9), uid(3), 1510);
        assert!(
            html.contains("Rappel actif : 1 jour 1 h 10 min avant."),
            "{html}"
        );
    }

    /// "à l'heure de l'événement avant" would not be French.
    #[test]
    fn an_at_event_time_reminder_has_no_avant() {
        let html = render_reminder_created(uid(9), uid(3), 0);
        assert!(
            html.contains("Rappel actif : à l'heure de l'événement."),
            "{html}"
        );
    }

    /// The API refuses a negative offset; one forged in the query string
    /// must not wrap into a delay of thousands of years.
    #[test]
    fn a_negative_offset_reads_as_at_event_time() {
        let html = render_reminder_created(uid(9), uid(3), -1);
        assert!(
            html.contains("Rappel actif : à l'heure de l'événement."),
            "{html}"
        );
        assert!(!html.contains("avant"), "{html}");
    }

    #[test]
    fn the_delete_form_targets_the_created_reminder() {
        let html = render_reminder_created(uid(9), uid(3), 10);
        assert!(
            html.contains(&format!(
                r#"action="/agenda/{}/reminders/{}/delete""#,
                uid(9),
                uid(3)
            )),
            "{html}"
        );
    }

    // -- render_occurrence_completions (#455) ------------------------------

    /// An API failure is not "no upcoming occurrence".
    #[test]
    fn unreadable_occurrences_are_an_error_not_an_empty_list() {
        let html = render_occurrence_completions(uid(9), None, None);
        assert!(!html.contains("Aucune occurrence"), "{html}");
        assert!(html.contains(r#"class="notice error""#), "{html}");
        assert!(
            html.contains("Impossible de charger les occurrences"),
            "{html}"
        );
    }

    #[test]
    fn no_upcoming_occurrence_says_so_without_an_error() {
        let html = render_occurrence_completions(uid(9), Some(&[]), None);
        assert!(html.contains("Aucune occurrence à venir"), "{html}");
        assert!(!html.contains("notice error"), "{html}");
    }

    // -- render_attachments (#455) -------------------------------------------

    /// An API failure is not "no attachment"; uploading stays possible.
    #[test]
    fn unreadable_attachments_are_an_error_not_an_empty_list() {
        let html = render_attachments(uid(9), None, true);
        assert!(!html.contains("Aucune pièce jointe"), "{html}");
        assert!(html.contains(r#"class="notice error""#), "{html}");
        assert!(
            html.contains("Impossible de charger les pièces jointes"),
            "{html}"
        );
        assert!(html.contains(r#"enctype="multipart/form-data""#), "{html}");
    }

    #[test]
    fn no_attachment_says_so_without_an_error() {
        let html = render_attachments(uid(9), Some(&[]), true);
        assert!(html.contains("Aucune pièce jointe."), "{html}");
        assert!(!html.contains("notice error"), "{html}");
    }

    /// The attachments endpoint answers a bare array.
    #[test]
    fn an_attachment_list_is_read_from_a_200() {
        let body = serde_json::json!([{
            "id": uid(1),
            "event_id": uid(9),
            "filename": "scan.pdf",
            "mime_type": "application/pdf",
            "size_bytes": 2048,
        }]);
        let read: Vec<AttachmentResponse> =
            json_from_response(reqwest::StatusCode::OK, body).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].filename, "scan.pdf");
        assert!(json_from_response::<Vec<AttachmentResponse>>(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": "internal" })
        )
        .is_err());
    }

    // -- assignees_html (#98 round-2, Mo2) -------------------------------

    #[test]
    fn a_single_assignee_is_named() {
        let html = assignees_html(uid(9), &[uid(1)], &[member(uid(1), "Alice")]);
        assert!(html.contains("Assigné à"), "{html}");
        assert!(html.contains("Alice"), "{html}");
    }

    /// The case the verifier reproduced: an event assigned to two members
    /// showed neither of their names anywhere on its own page.
    #[test]
    fn several_assignees_are_all_named_in_order() {
        let members = vec![member(uid(1), "Alice"), member(uid(2), "Bob")];
        let html = assignees_html(uid(9), &[uid(2), uid(1)], &members);
        let (bob, alice) = (html.find("Bob"), html.find("Alice"));
        assert!(bob.is_some() && alice.is_some() && bob < alice, "{html}");
        assert!(
            html.contains("</a>, <a "),
            "names are comma-separated: {html}"
        );
    }

    #[test]
    fn an_event_with_no_assignee_renders_no_line() {
        assert_eq!(assignees_html(uid(9), &[], &[member(uid(1), "Alice")]), "");
    }

    /// A roster that failed to load must not turn the line into UUIDs.
    #[test]
    fn an_unloadable_roster_renders_no_line() {
        assert_eq!(assignees_html(uid(9), &[uid(1)], &[]), "");
    }

    /// #466: each assignee's name opens their profile in the event's group.
    #[test]
    fn each_assignee_links_to_their_profile() {
        let members = vec![member(uid(1), "Alice"), member(uid(2), "Bob")];
        let html = assignees_html(uid(9), &[uid(2), uid(1)], &members);
        for (id, name) in [(uid(2), "Bob"), (uid(1), "Alice")] {
            let link = format!(
                r#"<a class="member" href="/groups/{}/members/{id}"><strong>{name}</strong></a>"#,
                uid(9)
            );
            assert!(html.contains(&link), "{html}");
        }
    }

    /// An assignee who has left the group has no profile to open: their
    /// fallback name stays plain text rather than a link to a 404.
    #[test]
    fn an_assignee_who_left_is_named_without_a_link() {
        let html = assignees_html(uid(9), &[uid(3)], &[member(uid(1), "Alice")]);
        assert!(html.contains("Membre"), "{html}");
        assert!(!html.contains("<a "), "{html}");
    }

    #[test]
    fn a_display_name_is_escaped() {
        let html = assignees_html(uid(9), &[uid(1)], &[member(uid(1), "<script>")]);
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>"), "{html}");
    }

    // -- the mark of a private event (#621) ---------------------------------

    fn event(is_private: bool) -> EventResponse {
        let at = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 9, 5, 8, 0, 0).unwrap();
        EventResponse {
            id: uid(7),
            group_id: uid(9),
            created_by: uid(1),
            title: "Dentiste".to_string(),
            description: None,
            location: None,
            starts_at: at,
            ends_at: at + Duration::hours(1),
            all_day: false,
            is_task: false,
            completed_at: None,
            rrule: None,
            assignee_ids: vec![uid(1)],
            is_private,
        }
    }

    fn detail(is_private: bool) -> String {
        page(
            "",
            &event(is_private),
            Some(&[]),
            Some(&[]),
            &[member(uid(1), "Alice")],
            true,
            None,
            None,
            None,
            None,
            "",
            None,
        )
    }

    // -- an event of another family (#622) ----------------------------------

    /// A one-off task with an attachment and a just-created reminder: every
    /// form and link the page can hold.
    fn full_detail(other_family: Option<(Uuid, &str)>) -> String {
        let mut task = event(false);
        task.is_task = true;
        let attachment = AttachmentResponse {
            id: uid(5),
            event_id: uid(7),
            filename: "ordonnance.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size_bytes: 2048,
            has_thumbnail: false,
        };
        page(
            "",
            &task,
            Some(&[]),
            Some(&[attachment]),
            &[member(uid(1), "Alice")],
            true,
            None,
            None,
            None,
            Some((uid(3), 10)),
            "",
            other_family,
        )
    }

    /// Every URL of the page that leads back to the event, in `html`.
    fn event_urls(html: &str) -> Vec<&str> {
        html.split(['"'])
            .filter(|s| s.starts_with("/agenda/"))
            .collect()
    }

    #[test]
    fn every_url_of_another_familys_event_carries_that_family() {
        let html = full_detail(Some((uid(2), "Les Martin")));
        let urls = event_urls(&html);
        // Edit, delete, completion, reminder add and delete, attachment
        // download, delete and upload.
        assert_eq!(urls.len(), 8, "{urls:?}");
        let family = format!("famille={}", uid(2));
        for url in &urls {
            assert!(url.ends_with(&family), "{url}");
        }
    }

    #[test]
    fn another_familys_event_names_its_family() {
        let html = full_detail(Some((uid(2), "Les <Martin>")));
        assert!(
            html.contains("<p><strong>Famille :</strong> Les &lt;Martin&gt;</p>"),
            "{html}"
        );
    }

    /// The active family's event: its URLs as they were, and no line.
    #[test]
    fn the_active_familys_event_carries_no_family() {
        let html = full_detail(None);
        let urls = event_urls(&html);
        assert_eq!(urls.len(), 8, "{urls:?}");
        assert!(urls.iter().all(|u| !u.contains("famille")), "{urls:?}");
        assert!(!html.contains("Famille :"), "{html}");
    }

    /// #774: the videos the API takes are announced and offered.
    #[test]
    fn the_upload_form_lists_every_accepted_format() {
        let html = full_detail(None);
        assert!(
            html.contains("(png, jpg, webp, pdf, mp4, webm — max 20 Mo)"),
            "{html}"
        );
        assert!(
            html.contains(
                r#"accept="image/png,image/jpeg,image/webp,application/pdf,video/mp4,video/webm""#
            ),
            "{html}"
        );
        assert_eq!(
            error_text("unsupported_file_type"),
            Some("Type de fichier non autorisé (png, jpg, webp, pdf, mp4, webm).")
        );
    }

    #[test]
    fn a_private_event_says_so_under_its_kind() {
        let html = detail(true);
        let line = private_line(true);
        assert!(!line.is_empty());
        let kind = html.find(r#"<p class="muted">Événement</p>"#).expect(&html);
        let at = html.find(&line).expect(&html);
        assert!(kind < at, "{html}");
    }

    /// #709: « Modifier » opens the edit form on the occurrence the detail
    /// was opened from, when the event is recurring — and only then, and
    /// only with an instant it could read.
    #[test]
    fn the_edit_link_carries_the_occurrence_of_a_recurring_event() {
        let id = uid(7);
        let mut recurring = event(false);
        recurring.rrule = Some("FREQ=WEEKLY".to_string());
        let occ = Some("2026-09-12T08:00:00Z");
        assert_eq!(
            edit_href(EventUrls::from(id), &recurring, occ),
            format!("/agenda/{id}/edit?occ=2026-09-12T08:00:00Z")
        );
        assert_eq!(
            edit_href(
                EventUrls {
                    id,
                    family: Some(uid(2))
                },
                &recurring,
                occ
            ),
            format!(
                "/agenda/{id}/edit?occ=2026-09-12T08:00:00Z&famille={}",
                uid(2)
            )
        );
        let plain = format!("/agenda/{id}/edit");
        assert_eq!(edit_href(EventUrls::from(id), &event(false), occ), plain);
        assert_eq!(edit_href(EventUrls::from(id), &recurring, None), plain);
        assert_eq!(
            edit_href(EventUrls::from(id), &recurring, Some("demain\"><b>")),
            plain
        );
    }

    // -- the delete of a recurring event (#710) -----------------------------

    fn weekly() -> EventResponse {
        let mut recurring = event(false);
        recurring.rrule = Some("FREQ=WEEKLY".to_string());
        recurring
    }

    fn third_week_at() -> DateTime<Utc> {
        "2026-09-19T08:00:00Z".parse().unwrap()
    }

    fn third_week(scope: EditScope) -> ScopeChoice {
        ScopeChoice {
            occurrence_at: third_week_at(),
            scope,
        }
    }

    /// « Supprimer » on an occurrence of a series leads to the screen that
    /// asks which occurrences go, in the family the detail was opened in.
    #[test]
    fn deleting_an_occurrence_of_a_series_asks_first() {
        let id = uid(7);
        let occ = Some("2026-09-19T08:00:00Z");
        let html = delete_control(EventUrls::from(id), &weekly(), occ);
        assert_eq!(
            html,
            format!(
                r#"<a class="btn secondary danger" href="/agenda/{id}/delete?occ=2026-09-19T08:00:00Z">Supprimer</a>"#
            )
        );
        let other = delete_control(
            EventUrls {
                id,
                family: Some(uid(2)),
            },
            &weekly(),
            occ,
        );
        assert!(
            other.contains(&format!(
                r#"href="/agenda/{id}/delete?occ=2026-09-19T08:00:00Z&famille={}""#,
                uid(2)
            )),
            "{other}"
        );
    }

    /// A one-off event, or a series opened without an occurrence it can
    /// read, is deleted whole, straight from the detail, as before.
    #[test]
    fn deleting_a_one_off_event_or_a_whole_series_posts_at_once() {
        let id = uid(7);
        let direct = format!(
            r#"<form method="post" action="/agenda/{id}/delete">
<button type="submit" class="secondary danger">Supprimer</button>
</form>"#
        );
        let occ = Some("2026-09-19T08:00:00Z");
        assert_eq!(
            delete_control(EventUrls::from(id), &event(false), occ),
            direct
        );
        assert_eq!(delete_control(EventUrls::from(id), &weekly(), None), direct);
        assert_eq!(
            delete_control(EventUrls::from(id), &weekly(), Some("demain\"><b>")),
            direct
        );
    }

    fn confirm(family: Option<Uuid>) -> String {
        delete_page("", &weekly(), third_week_at(), family)
    }

    /// The three choices, « Toute la série » first and checked, before the
    /// button; the occurrence travels with them.
    #[test]
    fn the_delete_screen_offers_three_choices_the_whole_series_by_default() {
        let html = confirm(None);
        let all = html.find(r#"value="all" checked"#).expect(&html);
        let following = html
            .find(r#"value="this_and_following"/> Cet événement et les suivants"#)
            .expect(&html);
        let this = html
            .find(r#"value="this"/> Cet événement uniquement"#)
            .expect(&html);
        assert!(all < following && following < this, "{html}");
        assert_eq!(html.matches(r#"type="radio" name="scope""#).count(), 3);
        assert_eq!(html.matches(" checked").count(), 1, "{html}");
        assert!(html.contains("Toute la série"), "{html}");
        assert!(
            html.contains(
                r#"<input type="hidden" name="occurrence_at" value="2026-09-19T08:00:00Z"/>"#
            ),
            "{html}"
        );
        let button = html
            .find(r#"<button type="submit" class="danger">"#)
            .expect(&html);
        assert!(this < button, "{html}");
        // The occurrence, on the Paris clock.
        assert!(html.contains("19/09/2026 à 10:00"), "{html}");
    }

    #[test]
    fn the_delete_screen_posts_and_goes_back_in_its_family() {
        let id = uid(7);
        let own = confirm(None);
        assert!(
            own.contains(&format!(
                r#"<form method="post" action="/agenda/{id}/delete">"#
            )),
            "{own}"
        );
        assert!(
            own.contains(&format!(
                r#"<a href="/agenda/{id}?occ=2026-09-19T08:00:00Z">Annuler</a>"#
            )),
            "{own}"
        );
        let other = confirm(Some(uid(2)));
        let family = format!("famille={}", uid(2));
        assert!(
            other.contains(&format!(
                r#"<form method="post" action="/agenda/{id}/delete?{family}">"#
            )),
            "{other}"
        );
        assert!(
            other.contains(&format!(
                r#"<a href="/agenda/{id}?occ=2026-09-19T08:00:00Z&{family}">Annuler</a>"#
            )),
            "{other}"
        );
    }

    #[test]
    fn the_delete_screen_escapes_the_title() {
        let mut series = weekly();
        series.title = "<b>Piscine</b>".to_string();
        let html = delete_page("", &series, third_week_at(), None);
        assert!(html.contains("&lt;b&gt;Piscine&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>Piscine"), "{html}");
    }

    /// What the API is asked: the whole event without parameters, as
    /// before; one occurrence, or one and the following, with the scope and
    /// the occurrence in the query.
    #[test]
    fn the_delete_request_carries_the_scope_and_its_occurrence() {
        let (g, e) = (uid(9), uid(7));
        let whole = format!("/groups/{g}/events/{e}");
        assert_eq!(delete_api_path(g, e, None), whole);
        assert_eq!(
            delete_api_path(g, e, Some(third_week(EditScope::All))),
            whole
        );
        assert_eq!(
            delete_api_path(g, e, Some(third_week(EditScope::This))),
            format!("{whole}?scope=this&occurrence_at=2026-09-19T08:00:00Z")
        );
        assert_eq!(
            delete_api_path(g, e, Some(third_week(EditScope::ThisAndFollowing))),
            format!("{whole}?scope=this_and_following&occurrence_at=2026-09-19T08:00:00Z")
        );
    }

    /// The refusals of a scoped delete say what went wrong on the detail;
    /// any other 400 reads as the service failing.
    #[test]
    fn a_refused_scoped_delete_says_why() {
        for code in [
            "occurrence_not_in_series",
            "scope_not_allowed_for_imported_event",
            "scope_requires_recurring_event",
        ] {
            assert_eq!(rejected_delete_code(Some(code)), code);
            assert!(error_text(code).is_some(), "{code}");
        }
        assert_eq!(
            error_text("scope_not_allowed_for_imported_event"),
            Some("Un événement importé ne se supprime que pour toute la série.")
        );
        assert_eq!(rejected_delete_code(Some("not_found")), "unavailable");
        assert_eq!(rejected_delete_code(None), "unavailable");
        assert!(error_text("invalid_form").is_some());
    }

    #[test]
    fn a_shared_event_does_not_say_prive() {
        let html = detail(false);
        assert!(!html.contains("Privé"), "{html}");
        assert!(
            !html.contains(crate::icons::icon(crate::icons::Icon::Lock)),
            "{html}"
        );
    }
}
