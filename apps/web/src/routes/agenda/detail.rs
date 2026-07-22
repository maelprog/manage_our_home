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
    AttachmentResponse, EventResponse, OccurrenceList, OccurrenceResponse, UpdateEventRequest,
};
use manage_our_home_shared::validation::agenda::{parse_rrule, Freq, RecurrenceEnd};
use uuid::Uuid;

use crate::app::{html_escape, shell};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::new::reminder_select;
use super::{
    agenda_cookie, can_modify, event_not_found_page, family_context, fmt_paris, forbidden_page,
    service_unavailable_page, today_paris,
};

#[derive(serde::Deserialize)]
pub struct DetailQuery {
    occ: Option<String>,
    notice: Option<String>,
    error: Option<String>,
    /// Set after a reminder is created, so the (non-enumerable) reminder can
    /// be shown once with a delete link.
    rid: Option<Uuid>,
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

    let attachments: Vec<AttachmentResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/events/{}/attachments", fam.gid, event_id),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value(resp.body).unwrap_or_default()
        }
        _ => Vec::new(),
    };

    // Recurring task: fetch the upcoming occurrences (next 60 days) so each
    // one can carry its own completion toggle.
    let recurring_task = event.is_task && event.rrule.is_some();
    let occurrences: Vec<OccurrenceResponse> = if recurring_task {
        let from = today_paris();
        let to = from + Duration::days(60);
        let path = format!(
            "/groups/{}/events?from={}T00:00:00Z&to={}T23:59:59Z",
            fam.gid, from, to
        );
        match api_request_auth(&state, reqwest::Method::GET, &path, cookie.as_deref(), None).await {
            Ok(resp) if resp.status == reqwest::StatusCode::OK => {
                serde_json::from_value::<OccurrenceList>(resp.body)
                    .map(|l| l.occurrences)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|o| o.event.id == event_id)
                    .collect()
            }
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let can_edit = can_modify(&fam.role, event.created_by == me.user_id);
    let notice = query.notice.as_deref().and_then(notice_text);
    let error = query.error.as_deref().and_then(error_text);
    let reminder = query.rid.map(|rid| (rid, query.offset.unwrap_or(0)));

    Html(page(
        &fam.header,
        &event,
        &occurrences,
        &attachments,
        can_edit,
        query.occ.as_deref(),
        notice,
        error,
        reminder,
    ))
    .into_response()
}

#[allow(clippy::too_many_arguments)]
fn page(
    header: &str,
    event: &EventResponse,
    occurrences: &[OccurrenceResponse],
    attachments: &[AttachmentResponse],
    can_edit: bool,
    focus_occ: Option<&str>,
    notice: Option<&str>,
    error: Option<&str>,
    reminder: Option<(Uuid, i32)>,
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
            r#"<div style="display:flex;gap:0.5rem;margin-top:1rem;">
<a class="button secondary" href="/agenda/{id}/edit">Modifier</a>
<form method="post" action="/agenda/{id}/delete" style="margin:0;">
<button type="submit" class="secondary" style="color:var(--error);">Supprimer</button>
</form>
</div>"#
        )
    } else {
        r#"<p class="muted" style="margin-top:1rem;">Seul le créateur ou un administrateur peut modifier ou supprimer cet événement.</p>"#.to_string()
    };

    // Reminders: add form + one-shot display of a just-created reminder
    // (backend exposes no list endpoint — documented v1 limitation).
    let just_created = reminder
        .map(|(rid, offset)| {
            format!(
                r#"<p class="notice success" style="display:flex;justify-content:space-between;align-items:center;gap:0.5rem;">
<span>Rappel actif : {offset} min avant.</span>
<form method="post" action="/agenda/{id}/reminders/{rid}/delete" style="margin:0;">
<button type="submit" class="secondary">Supprimer ce rappel</button>
</form></p>"#
            )
        })
        .unwrap_or_default();
    let reminder_select_html = reminder_select("reminder", None);
    let reminders_html = format!(
        r#"<h2 style="font-size:1.1rem;margin-top:1.5rem;">Rappels</h2>
<p class="muted">Ajoute un rappel par email avant l'événement. La liste des rappels existants n'est pas affichable (limitation v1).</p>
{just_created}
<form method="post" action="/agenda/{id}/reminders">
<label>Nouveau rappel {reminder_select_html}</label>
<button type="submit">Ajouter le rappel</button>
</form>"#
    );

    // Attachments.
    let attachments_html = render_attachments(id, attachments, can_edit);

    let body = format!(
        r#"{header}
<h1>{title}</h1>
{notice_html}{error_html}
<p class="muted">{kind}</p>
<p><strong>Quand :</strong> {when}</p>
{location_html}{description_html}{recurrence_html}
{completion_html}
{controls_html}
{reminders_html}
{attachments_html}
<div class="links"><a href="/agenda">Retour à l'agenda</a></div>"#,
        title = html_escape(&event.title),
    );
    shell(&event.title, &body)
}

fn render_oneoff_completion(id: Uuid, event: &EventResponse) -> String {
    let done = event.completed_at.is_some();
    let (label, next, status) = if done {
        ("Marquer à faire", "false", "✅ Fait")
    } else {
        ("Marquer faite", "true", "⬜ À faire")
    };
    format!(
        r#"<div style="margin-top:1rem;display:flex;gap:0.75rem;align-items:center;">
<span><strong>{status}</strong></span>
<form method="post" action="/agenda/{id}/complete" style="margin:0;">
<input type="hidden" name="completed" value="{next}"/>
<button type="submit" class="secondary">{label}</button>
</form></div>"#
    )
}

fn render_occurrence_completions(
    id: Uuid,
    occurrences: &[OccurrenceResponse],
    focus_occ: Option<&str>,
) -> String {
    if occurrences.is_empty() {
        return r#"<p class="muted" style="margin-top:1rem;">Aucune occurrence à venir dans les 60 prochains jours.</p>"#.to_string();
    }
    let rows: String = occurrences
        .iter()
        .map(|occ| {
            let occ_param = occ.occurrence_starts_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let done = occ.event.completed_at.is_some();
            let (label, next, status) = if done {
                ("Marquer à faire", "false", "✅")
            } else {
                ("Marquer faite", "true", "⬜")
            };
            let highlight = if focus_occ == Some(occ_param.as_str()) {
                "background:var(--accent-bg,#eef);"
            } else {
                ""
            };
            format!(
                r#"<li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.5rem;border-bottom:1px solid var(--border);{highlight}">
<span>{status} {when}</span>
<form method="post" action="/agenda/{id}/complete" style="margin:0;">
<input type="hidden" name="completed" value="{next}"/>
<input type="hidden" name="occurrence_at" value="{occ_param}"/>
<button type="submit" class="secondary">{label}</button>
</form></li>"#,
                when = html_escape(&fmt_paris(occ.occurrence_starts_at, "%a %d/%m/%Y %H:%M")),
            )
        })
        .collect();
    format!(
        r#"<h2 style="font-size:1.1rem;margin-top:1.5rem;">Occurrences à venir</h2>
<p class="muted">La complétion est indépendante par occurrence : cocher l'une n'affecte pas les autres.</p>
<ul style="list-style:none;padding:0;margin:0;">{rows}</ul>"#
    )
}

fn render_attachments(id: Uuid, attachments: &[AttachmentResponse], can_edit: bool) -> String {
    let rows: String = attachments
        .iter()
        .map(|a| {
            let size_kb = (a.size_bytes as f64 / 1024.0).round() as i64;
            let delete = if can_edit {
                format!(
                    r#"<form method="post" action="/agenda/{id}/attachments/{aid}/delete" style="margin:0;">
<button type="submit" class="secondary">Supprimer</button></form>"#,
                    aid = a.id,
                )
            } else {
                String::new()
            };
            format!(
                r#"<li style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;padding:0.5rem;border-bottom:1px solid var(--border);">
<a href="/agenda/{id}/attachments/{aid}/download">{name}</a>
<span class="muted">{size_kb} Ko</span>
{delete}</li>"#,
                aid = a.id,
                name = html_escape(&a.filename),
            )
        })
        .collect();
    let list = if attachments.is_empty() {
        r#"<p class="muted">Aucune pièce jointe.</p>"#.to_string()
    } else {
        format!(r#"<ul style="list-style:none;padding:0;margin:0;">{rows}</ul>"#)
    };
    let upload = format!(
        r#"<form method="post" action="/agenda/{id}/attachments" enctype="multipart/form-data">
<label>Ajouter une pièce jointe (png, jpg, webp, pdf — max 20 Mo)
<input type="file" name="file" accept="image/png,image/jpeg,image/webp,application/pdf" required/></label>
<button type="submit">Envoyer</button>
</form>"#
    );
    format!(
        r#"<h2 style="font-size:1.1rem;margin-top:1.5rem;">Pièces jointes</h2>
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
