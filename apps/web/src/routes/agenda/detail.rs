//! `/agenda/:id` — event/task detail, with per-occurrence completion for
//! recurring tasks (the crux of this epic: completion lives in
//! `event_occurrence_completions`, never on the shared `events` row, so the
//! detail lists occurrences and toggles them one at a time), plus the
//! delete action and the add-reminder form. Edit lives in `edit.rs`.
//!
//! Error table: 404 (`get_event`) unknown/foreign event → introuvable page;
//! 403 (`update_event`/`delete_event` permission bar) → `?error=forbidden`;
//! 400 `completed_only_valid_for_tasks` / `occurrence_at_required_for_recurring_task`
//! mapped defensively (the UI only renders those forms where valid).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{DateTime, Duration, Utc};
use manage_our_home_shared::dto::agenda::{
    AttachmentResponse, EventResponse, OccurrenceResponse, UpdateEventRequest,
};
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::{
    format_reminder_offset, parse_rrule, Freq, RecurrenceEnd,
};
use manage_our_home_shared::validation::messagerie::author_name;
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::lenient_query::lenient;
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::new::{reminder_push_block, reminder_select};
use super::{
    agenda_cookie, can_modify, event_not_found_page, family_context, fmt_paris, forbidden_page,
    json_from_response, occurrences_from_response, read_or_log, service_unavailable_page,
    today_paris,
};

#[derive(serde::Deserialize)]
pub struct DetailQuery {
    occ: Option<String>,
    notice: Option<String>,
    error: Option<String>,
    /// Set after a reminder is created, so the (non-enumerable) reminder can
    /// be shown once with a delete link. Both read as absent when they do
    /// not convert (#580): no banner, offset 0.
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
        "unsupported_file_type" => Some("Type de fichier non autorisé (png, jpg, webp, pdf)."),
        "file_too_large" => Some("Fichier trop volumineux (max 20 Mo)."),
        "upload_failed" => Some("L'envoi de la pièce jointe a échoué, merci de réessayer."),
        "reminder_not_found" => Some("Ce rappel n'existe plus."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
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
fn assignees_html(assignee_ids: &[Uuid], members: &[GroupMember]) -> String {
    if assignee_ids.is_empty() || members.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = assignee_ids
        .iter()
        .map(|id| author_name(members, *id))
        .collect();
    format!(
        "<p><strong>Assigné à :</strong> {}</p>",
        html_escape(&names.join(", ")),
    )
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
    Query(query): Query<DetailQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, &format!("/agenda/{event_id}")).await
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
    ))
    .into_response()
}

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
) -> String {
    let id = event.id;
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
    let assignees = assignees_html(&event.assignee_ids, members);
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
            render_occurrence_completions(id, occurrences, focus_occ)
        } else {
            render_oneoff_completion(id, event)
        }
    } else {
        String::new()
    };

    // Edit/delete controls (creator or owner/admin only).
    let controls_html = if can_edit {
        format!(
            r#"<div class="actions">
<a class="btn secondary" href="/agenda/{id}/edit">Modifier</a>
<form method="post" action="/agenda/{id}/delete">
<button type="submit" class="secondary danger">Supprimer</button>
</form>
</div>"#
        )
    } else {
        r#"<p class="muted">Seul le créateur ou un administrateur peut modifier ou supprimer cet événement.</p>"#.to_string()
    };

    // Reminders: add form + one-shot display of a just-created reminder
    // (backend exposes no list endpoint — documented v1 limitation).
    let just_created = reminder
        .map(|(rid, offset)| render_reminder_created(id, rid, offset))
        .unwrap_or_default();
    let reminder_select_html = reminder_select("reminder", None);
    let reminders_html = format!(
        r#"<h2>Rappels</h2>
<p class="muted">Ajoute un rappel avant l'événement. Il est envoyé au créateur de l'événement, par notification ou par email selon son choix (le vôtre se règle dans <a href="/account/notifications">Notifications de rappel</a>). La liste des rappels existants n'est pas affichable (limitation v1).</p>
{just_created}
<form method="post" action="/agenda/{id}/reminders">
<label>Nouveau rappel {reminder_select_html}</label>
{push}
<button type="submit">Ajouter le rappel</button>
</form>"#
    );

    // Attachments.
    let attachments_html = render_attachments(id, attachments, can_edit);

    let body = format!(
        r#"<h1>{title}</h1>
{notice_html}{error_html}
<p class="muted">{kind}</p>
<p><strong>Quand :</strong> {when}</p>
{assignees}
{location_html}{description_html}{recurrence_html}
{completion_html}
{controls_html}
{reminders_html}
{attachments_html}
<div class="links"><a href="/agenda">Retour à l'agenda</a></div>"#,
        title = html_escape(&event.title),
    );
    shell_with_header(Width::Read, &event.title, header, &body)
}

/// The one-shot notice shown after a reminder is created, with its delete
/// button. `offset` is the minutes-before value echoed in the redirect.
/// The API refuses a negative offset; a forged one in the query reads as 0.
fn render_reminder_created(id: Uuid, rid: Uuid, offset: i32) -> String {
    let minutes = u32::try_from(offset).unwrap_or(0);
    let when = if minutes == 0 {
        format_reminder_offset(0)
    } else {
        format!("{} avant", format_reminder_offset(minutes))
    };
    format!(
        r#"<p class="notice success split">
<span>Rappel actif : {when}.</span>
<form method="post" action="/agenda/{id}/reminders/{rid}/delete">
<button type="submit" class="secondary">Supprimer ce rappel</button>
</form></p>"#
    )
}

fn render_oneoff_completion(id: Uuid, event: &EventResponse) -> String {
    let done = event.completed_at.is_some();
    let (label, next, status) = if done {
        ("Marquer à faire", "false", "✅ Fait")
    } else {
        ("Marquer faite", "true", "⬜ À faire")
    };
    format!(
        r#"<div class="actions">
<span><strong>{status}</strong></span>
<form method="post" action="/agenda/{id}/complete">
<input type="hidden" name="completed" value="{next}"/>
<button type="submit" class="secondary">{label}</button>
</form></div>"#
    )
}

/// `occurrences` is `None` when the API's answer could not be read: that
/// renders as an error, never as "no upcoming occurrence" (#455).
fn render_occurrence_completions(
    id: Uuid,
    occurrences: Option<&[OccurrenceResponse]>,
    focus_occ: Option<&str>,
) -> String {
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
<form method="post" action="/agenda/{id}/complete">
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
    id: Uuid,
    attachments: Option<&[AttachmentResponse]>,
    can_edit: bool,
) -> String {
    let rows: String = attachments
        .unwrap_or_default()
        .iter()
        .map(|a| {
            let size_kb = (a.size_bytes as f64 / 1024.0).round() as i64;
            let delete = if can_edit {
                format!(
                    r#"<form method="post" action="/agenda/{id}/attachments/{aid}/delete">
<button type="submit" class="secondary">Supprimer</button></form>"#,
                    aid = a.id,
                )
            } else {
                String::new()
            };
            format!(
                r#"<li class="list-row">
<a href="/agenda/{id}/attachments/{aid}/download">{name}</a>
<span class="muted">{size_kb} Ko</span>
{delete}</li>"#,
                aid = a.id,
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
        r#"<form method="post" action="/agenda/{id}/attachments" enctype="multipart/form-data">
<label>Ajouter une pièce jointe (png, jpg, webp, pdf — max 20 Mo)
<input type="file" name="file" accept="image/png,image/jpeg,image/webp,application/pdf" required/></label>
<button type="submit">Envoyer</button>
</form>"#
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
    Form(form): Form<CompleteForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &_me, &format!("/agenda/{event_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let detail = format!("/agenda/{event_id}");
    let completed = form.completed == "true";
    let occurrence_at: Option<DateTime<Utc>> = if form.occurrence_at.trim().is_empty() {
        None
    } else {
        match DateTime::parse_from_rfc3339(form.occurrence_at.trim()) {
            Ok(dt) => Some(dt.with_timezone(&Utc)),
            Err(_) => {
                return Redirect::to(&format!("{detail}?error=occurrence_required")).into_response()
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
    Redirect::to(&target).into_response()
}

pub async fn delete(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &_me, &format!("/agenda/{event_id}")).await
    else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = agenda_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/events/{}", fam.gid, event_id),
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
        serde_urlencoded::from_str(raw).expect("a malformed parameter must not reject the page")
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

    fn member(id: Uuid, name: &str) -> GroupMember {
        GroupMember {
            user_id: id,
            display_name: name.to_string(),
            email: format!("{name}@example.test"),
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
        let html = assignees_html(&[uid(1)], &[member(uid(1), "Alice")]);
        assert!(html.contains("Assigné à"), "{html}");
        assert!(html.contains("Alice"), "{html}");
    }

    /// The case the verifier reproduced: an event assigned to two members
    /// showed neither of their names anywhere on its own page.
    #[test]
    fn several_assignees_are_all_named_in_order() {
        let members = vec![member(uid(1), "Alice"), member(uid(2), "Bob")];
        let html = assignees_html(&[uid(2), uid(1)], &members);
        assert!(html.contains("Bob, Alice"), "{html}");
    }

    #[test]
    fn an_event_with_no_assignee_renders_no_line() {
        assert_eq!(assignees_html(&[], &[member(uid(1), "Alice")]), "");
    }

    /// A roster that failed to load must not turn the line into UUIDs.
    #[test]
    fn an_unloadable_roster_renders_no_line() {
        assert_eq!(assignees_html(&[uid(1)], &[]), "");
    }

    #[test]
    fn a_display_name_is_escaped() {
        let html = assignees_html(&[uid(1)], &[member(uid(1), "<script>")]);
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>"), "{html}");
    }
}
