//! `/agenda/:id/edit` — edit an event/task's fields and recurrence. Same
//! permission bar as delete (`can_modify`): a non-editor never sees the
//! form, and the backend 403 is mapped to `?error=forbidden` on the detail
//! page defensively. `is_task` is shown read-only — the backend's
//! `UpdateEventRequest` has no `is_task` field, so it can't change after
//! creation. Choosing "Aucune" recurrence clears an existing rule (sent as
//! the empty string, which the backend maps to `NULL`). The « Privé » box
//! (#621) is always sent: unchecked, the event becomes shared again.
//! Opened on an occurrence of a recurring event (`?occ=`, #709), the form
//! shows that occurrence and asks what the change applies to: the whole
//! series, this occurrence and the following ones, or this one alone.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{DateTime, Utc};
use manage_our_home_shared::dto::agenda::{EditScope, EventResponse, UpdateEventRequest};
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::agenda::{
    parse_rrule, validate_event_form, EventFormError,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::lenient_query::{lenient, LenientQuery};
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::new::{
    assignee_checkboxes, assignee_ids_from_raw_form, error_message, is_form_urlencoded,
    private_checkbox, raw_is_private, recurrence_picker, rrule_from_form, schedule_fields,
    EventForm, RecurrenceFields,
};
use super::{
    agenda_cookie, all_day_field_values, can_modify, date_only, event_not_found_page,
    family_context_for, fmt_paris, forbidden_page, form_bounds, service_unavailable_page,
    to_datetime_local, with_family, EventFamily,
};

/// The line under the assignee picker explaining what an empty selection
/// means — and nothing at all when the roster failed to load.
///
/// On that degraded path `assignee_checkboxes` emits no
/// `ASSIGNEES_PRESENT_FIELD`, so `post` below sends no `assignee_ids` and
/// the backend leaves the assignment exactly as it was. The fieldset says so
/// ("l'assignation reste inchangée") — keeping the "no selection = the
/// creator" hint next to it announced a fallback that will not happen (#99).
/// `new.rs` keeps its own hint unconditionally on purpose: creating with no
/// `assignee_ids` really does fall back to the creator (`resolve_assignees`),
/// roster or no roster.
fn assignment_hint(members: &[GroupMember]) -> &'static str {
    if members.is_empty() {
        ""
    } else {
        r#"<p class="muted">Aucune sélection = assigné au créateur de l'événement.</p>"#
    }
}

#[derive(serde::Deserialize)]
pub struct EditQuery {
    /// The occurrence the form was opened from, as the calendar's links and
    /// the detail's « Modifier » write it (#709). Read as absent when it
    /// does not convert (#580).
    #[serde(default, deserialize_with = "lenient")]
    occ: Option<DateTime<Utc>>,
}

/// The occurrence the edit form was opened on, and what the change applies
/// to (#709). Only a recurring event opened from one of its occurrences
/// carries one; any other edit changes the event as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScopeChoice {
    pub occurrence_at: DateTime<Utc>,
    pub scope: EditScope,
}

/// An occurrence as the `occ` parameter of the agenda's links writes it.
pub(super) fn occ_param(occurrence_at: DateTime<Utc>) -> String {
    occurrence_at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The `scope` radio's value as the API's `EditScope`; `None` for anything
/// the form does not offer.
fn scope_from_form(raw: &str) -> Option<EditScope> {
    match raw {
        "all" => Some(EditScope::All),
        "this_and_following" => Some(EditScope::ThisAndFollowing),
        "this" => Some(EditScope::This),
        _ => None,
    }
}

/// The `Début`/`Fin` values an event is edited with. An all-day event is
/// edited as two dates, Fin being its last day: the stored exclusive end
/// shown as `datetime-local` read as one day too many, and "correcting" it
/// lost a day (#117).
fn field_values(
    all_day: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> (String, String) {
    if all_day {
        all_day_field_values(starts_at, ends_at)
    } else {
        (to_datetime_local(starts_at), to_datetime_local(ends_at))
    }
}

/// The `Début`/`Fin` values of the series' occurrence starting at
/// `occurrence_at`: the series' duration, from that instant.
fn occurrence_field_values(
    series: &EventResponse,
    occurrence_at: DateTime<Utc>,
) -> (String, String) {
    let duration = series.ends_at - series.starts_at;
    field_values(series.all_day, occurrence_at, occurrence_at + duration)
}

/// A field moved on the occurrence, carried to the series: the series' day
/// moved by as many days as the field's, at the field's time of day — the
/// time part of a `datetime-local` value, nothing for a date. Days rather
/// than a duration, so a move across a change of Paris time keeps its
/// wall-clock time. `None` when a value does not start with a date.
fn rebased(submitted: &str, occurrence: &str, series: &str) -> Option<String> {
    let day = |value: &str| value.get(..10).and_then(date_only);
    let moved_by = day(submitted)? - day(occurrence)?;
    let series_day = day(series)? + moved_by;
    Some(format!(
        "{}{}",
        series_day.format("%Y-%m-%d"),
        &submitted[10..]
    ))
}

/// The request a submitted form makes of the event as a whole, or the code
/// of the error the form comes back under.
fn update_request(
    form: &EventForm,
    assignee_ids: &[Uuid],
) -> Result<UpdateEventRequest, &'static str> {
    let (starts_at, ends_at) =
        form_bounds(&form.starts_at, &form.ends_at).ok_or("ends_before_starts")?;
    match validate_event_form(&form.title, starts_at, ends_at) {
        Err(EventFormError::TitleRequired) => return Err("title_required"),
        Err(EventFormError::EndsBeforeStarts) => return Err("ends_before_starts"),
        Ok(()) => {}
    }
    let rrule_opt = rrule_from_form(form).map_err(|()| "invalid_rrule")?;
    // "Aucune" clears an existing rule: the backend treats an empty string
    // as NULL; `None` would instead leave the rule unchanged.
    let rrule = Some(rrule_opt.unwrap_or_default());

    Ok(UpdateEventRequest {
        title: Some(form.title.trim().to_string()),
        description: Some(form.description.trim().to_string()),
        location: Some(form.location.trim().to_string()),
        starts_at: Some(starts_at),
        ends_at: Some(ends_at),
        all_day: Some(form.all_day.is_some()),
        rrule,
        completed: None,
        occurrence_at: None,
        // A plain HTML form has no "untouched" signal, so the fieldset
        // carries one: `ASSIGNEES_PRESENT_FIELD` is emitted only when the
        // picker actually listed the family's members. With it, the
        // submitted checkbox state replaces the assignee set (and
        // `resolve_assignees` on the backend falls back to the creator if
        // that leaves it empty). Without it — `fetch_group_detail` failed
        // and the picker rendered blind — the field is omitted entirely and
        // the backend leaves the assignment alone. Sending `Some(vec![])`
        // there silently reassigned the event to its creator: a degradation
        // chosen as decorative was in fact destroying data (#98
        // verification, round 2).
        assignee_ids: form
            .assignees_present
            .is_some()
            .then(|| assignee_ids.to_vec()),
        // The box is always on the form, so its state is always an answer:
        // unchecked makes the event shared again (#621). apps/api refuses
        // (403) to make private an event its author would not take part in.
        is_private: Some(form.is_private.is_some()),
        // The whole event; `scoped_request` narrows it to an occurrence
        // when the form was opened on one (#709).
        scope: None,
    })
}

/// The request of an edit opened on an occurrence: `body` as built from
/// the form, reshaped for the chosen scope. `None` when a moved field cannot
/// be carried to the series.
fn scoped_request(
    body: UpdateEventRequest,
    choice: ScopeChoice,
    form: &EventForm,
    series: &EventResponse,
) -> Option<UpdateEventRequest> {
    let mut body = body;
    // The form shows the occurrence. Its times untouched, none is sent: the
    // series keeps its own, the occurrence its own.
    let (occurrence_starts, occurrence_ends) =
        occurrence_field_values(series, choice.occurrence_at);
    let untouched = form.starts_at == occurrence_starts
        && form.ends_at == occurrence_ends
        && form.all_day.is_some() == series.all_day;
    if untouched {
        body.starts_at = None;
        body.ends_at = None;
    } else if choice.scope == EditScope::All {
        // Moved on the occurrence, the series moves by as much from its own
        // start; sent as they are, the occurrence's times would move the
        // series' start onto the occurrence.
        let (series_starts, series_ends) =
            field_values(series.all_day, series.starts_at, series.ends_at);
        let starts = rebased(&form.starts_at, &occurrence_starts, &series_starts)?;
        let ends = rebased(&form.ends_at, &occurrence_ends, &series_ends)?;
        let (starts_at, ends_at) = form_bounds(&starts, &ends)?;
        body.starts_at = Some(starts_at);
        body.ends_at = Some(ends_at);
    }
    match choice.scope {
        // The occurrence becomes a one-off event, whatever the rule says.
        EditScope::This => body.rrule = None,
        // The series' rule untouched, none is sent: the API carries on
        // with what is left of it — `COUNT` less what the series already
        // produced, where the form's `COUNT` would start over.
        EditScope::ThisAndFollowing if same_rule(body.rrule.as_deref(), series) => {
            body.rrule = None
        }
        EditScope::ThisAndFollowing | EditScope::All => {}
    }
    body.scope = Some(choice.scope);
    body.occurrence_at = (choice.scope != EditScope::All).then_some(choice.occurrence_at);
    Some(body)
}

/// Whether the form's rule (`""` for none) is the series' own.
fn same_rule(submitted: Option<&str>, series: &EventResponse) -> bool {
    let submitted = submitted.filter(|r| !r.is_empty());
    match (submitted, series.rrule.as_deref()) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a == b || matches!((parse_rrule(a), parse_rrule(b)), (Some(x), Some(y)) if x == y)
        }
        _ => false,
    }
}

/// The three choices, « Toute la série » first and checked by default.
/// Native radios in a `fieldset` named by its `legend`: one Tab stop, the
/// arrows move between them.
fn scope_fieldset(choice: &ScopeChoice, all_day: bool) -> String {
    let when = if all_day {
        fmt_paris(choice.occurrence_at, "%d/%m/%Y")
    } else {
        fmt_paris(choice.occurrence_at, "%d/%m/%Y à %H:%M")
    };
    let radios: String = [
        (EditScope::All, "all", "Toute la série"),
        (
            EditScope::ThisAndFollowing,
            "this_and_following",
            "Cet événement et les suivants",
        ),
        (EditScope::This, "this", "Cet événement uniquement"),
    ]
    .iter()
    .map(|(scope, value, label)| {
        let checked = if *scope == choice.scope { " checked" } else { "" };
        format!(
            r#"<label class="field inline"><input type="radio" name="scope" value="{value}"{checked}/> {label}</label>
"#
        )
    })
    .collect();
    format!(
        r#"<fieldset class="card">
<legend>Appliquer la modification à</legend>
<input type="hidden" name="occurrence_at" value="{occ}"/>
<p class="muted">Les dates affichées sont celles de l'occurrence du {when}. Pour toute la série, un changement de jour ou d'heure décale chaque occurrence d'autant.</p>
{radios}</fieldset>"#,
        occ = occ_param(choice.occurrence_at),
    )
}

#[allow(clippy::too_many_arguments)]
fn page(
    header: &str,
    id: Uuid,
    is_task: bool,
    title: &str,
    description: &str,
    location: &str,
    starts_local: &str,
    ends_local: &str,
    all_day: bool,
    recurrence: &RecurrenceFields,
    error: Option<&str>,
    members: &[GroupMember],
    selected_assignees: &[Uuid],
    is_private: bool,
    family: Option<Uuid>,
    scope: Option<&ScopeChoice>,
) -> String {
    // The family the event was opened in, when it is not the active one
    // (#622): the form posts to it and the way back returns to it — to the
    // occurrence the form was opened on, if any (#709).
    let action = with_family(&format!("/agenda/{id}/edit"), family);
    let back = match scope {
        Some(choice) => format!("/agenda/{id}?occ={}", occ_param(choice.occurrence_at)),
        None => format!("/agenda/{id}"),
    };
    let back = with_family(&back, family);
    let scope_html = scope
        .map(|choice| scope_fieldset(choice, all_day))
        .unwrap_or_default();
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let picker = recurrence_picker(recurrence);
    let assignees = assignee_checkboxes(members, selected_assignees);
    let hint = assignment_hint(members);
    let private = private_checkbox(is_private);
    let schedule = schedule_fields(all_day, starts_local, ends_local);
    let kind = if is_task { "Tâche" } else { "Événement" };
    let body = format!(
        r#"<h1>Modifier — {title_esc}</h1>
<p class="muted">{kind} (le type ne peut pas être changé après création)</p>
{error_html}
<form method="post" action="{action}">
<label>Titre <input type="text" name="title" required value="{title_attr}"/></label>
{schedule}
<label>Lieu <input type="text" name="location" value="{location_attr}"/></label>
<label>Description <textarea name="description" rows="3">{description_esc}</textarea></label>
{picker}
{assignees}
{hint}
{private}
{scope_html}
<button type="submit">Enregistrer</button>
</form>
<div class="links"><a href="{back}">Retour au détail</a></div>"#,
        title_esc = html_escape(title),
        title_attr = html_escape(title),
        location_attr = html_escape(location),
        description_esc = html_escape(description),
    );
    shell_with_header(Width::Form, "Modifier l'événement", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    EventFamily(requested): EventFamily,
    LenientQuery(query): LenientQuery<EditQuery>,
) -> Response {
    let Some(fam) = family_context_for(
        &state,
        &headers,
        &me,
        &format!("/agenda/{event_id}/edit"),
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

    let members = fetch_group_detail(&state, cookie.as_deref(), fam.gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members)
        .unwrap_or_default();

    let recurrence = event
        .rrule
        .as_deref()
        .and_then(parse_rrule)
        .map(|r| RecurrenceFields::from_recurrence(&r))
        .unwrap_or_default();
    // Opened on an occurrence of a recurring event (#709): the form shows
    // that occurrence and asks what the change applies to.
    let scope = query
        .occ
        .filter(|_| event.rrule.is_some())
        .map(|occurrence_at| ScopeChoice {
            occurrence_at,
            scope: EditScope::All,
        });
    let (starts_value, ends_value) = match scope {
        Some(choice) => occurrence_field_values(&event, choice.occurrence_at),
        None => field_values(event.all_day, event.starts_at, event.ends_at),
    };
    Html(page(
        &fam.header,
        event_id,
        event.is_task,
        &event.title,
        event.description.as_deref().unwrap_or(""),
        event.location.as_deref().unwrap_or(""),
        &starts_value,
        &ends_value,
        event.all_day,
        &recurrence,
        None,
        &members,
        &event.assignee_ids,
        event.is_private,
        fam.carried,
        scope.as_ref(),
    ))
    .into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(event_id): Path<Uuid>,
    EventFamily(requested): EventFamily,
    raw_body: axum::body::Bytes,
) -> Response {
    if !is_form_urlencoded(&headers) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Some(fam) = family_context_for(
        &state,
        &headers,
        &me,
        &format!("/agenda/{event_id}/edit"),
        requested,
    )
    .await
    else {
        return Redirect::to("/groups/new").into_response();
    };

    let raw = String::from_utf8_lossy(&raw_body);
    let assignee_ids = assignee_ids_from_raw_form(&raw);
    let cookie = agenda_cookie(&headers);
    let members = fetch_group_detail(&state, cookie.as_deref(), fam.gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members)
        .unwrap_or_default();

    // Same reason as `new.rs`: an unreadable body is a 422, not a 200
    // carrying an unrelated validation message (#98 verification, round 2).
    let Ok(form) = serde_urlencoded::from_bytes::<EventForm>(&raw_body) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Html(page(
                &fam.header,
                event_id,
                false,
                "",
                "",
                "",
                "",
                "",
                false,
                &RecurrenceFields::default(),
                Some(error_message("invalid_form")),
                &members,
                &assignee_ids,
                raw_is_private(&raw),
                fam.carried,
                None,
            )),
        )
            .into_response();
    };
    // The occurrence the form was opened on and the scope chosen (#709).
    let scope = read_scope(&raw_body);
    let shown_scope = scope.ok().flatten();

    // An inline error gives the picker back as it was submitted, a value
    // the rule refused included: read through the rule, it fell back to
    // « Aucune » and the recurrence had to be chosen again (#696).
    let rec_for_render = RecurrenceFields::from_form(&form);

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            event_id,
            // is_task display: unknown here without a fetch; the field is
            // read-only anyway, default to false label rather than a round
            // trip on an error path.
            false,
            &form.title,
            &form.description,
            &form.location,
            &form.starts_at,
            &form.ends_at,
            form.all_day.is_some(),
            &rec_for_render,
            Some(error_message(code)),
            &members,
            &assignee_ids,
            form.is_private.is_some(),
            fam.carried,
            shown_scope.as_ref(),
        ))
        .into_response()
    };

    // A forged occurrence or scope is the same unreadable form as above.
    let Ok(scope) = scope else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            render_error("invalid_form"),
        )
            .into_response();
    };
    let body = match update_request(&form, &assignee_ids) {
        Ok(body) => body,
        Err(code) => return render_error(code),
    };
    let body = match scope {
        None => body,
        Some(choice) => {
            // The series as it stands: the form shows the occurrence, and
            // what it sends depends on the series' own times and rule.
            let series = match api_request_auth(
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
                        Err(_) => return render_error("unavailable"),
                    }
                }
                Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
                    return event_not_found_page().into_response()
                }
                _ => return render_error("unavailable"),
            };
            match scoped_request(body, choice, &form, &series) {
                Some(body) => body,
                None => return render_error("ends_before_starts"),
            }
        }
    };

    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/events/{}", fam.gid, event_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;

    match result {
        // Changed alone or from it on, the occurrence is now another
        // event: the detail it leads to is that one's (#709).
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            let id = serde_json::from_value::<EventResponse>(resp.body)
                .map(|e| e.id)
                .unwrap_or(event_id);
            Redirect::to(&fam.url(&format!("/agenda/{id}?notice=updated"))).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            Redirect::to(&fam.url(&format!("/agenda/{event_id}?error=forbidden"))).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            event_not_found_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            let code = resp.body.get("error").and_then(|v| v.as_str());
            render_error(rejected_edit_code(code))
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}

/// The occurrence and scope fields of the edit form (#709). Both are absent
/// from the form of an event opened as a whole.
#[derive(serde::Deserialize)]
struct ScopeForm {
    #[serde(default)]
    occurrence_at: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// The scope the submitted form chose: `None` without an occurrence, `Err`
/// for an occurrence or a scope the form cannot have sent.
fn read_scope(raw_body: &[u8]) -> Result<Option<ScopeChoice>, ()> {
    let form = serde_urlencoded::from_bytes::<ScopeForm>(raw_body).map_err(|_| ())?;
    let Some(occurrence_at) = form.occurrence_at else {
        return Ok(None);
    };
    let occurrence_at = occurrence_at.trim().parse().map_err(|_| ())?;
    let scope = scope_from_form(form.scope.as_deref().unwrap_or("all")).ok_or(())?;
    Ok(Some(ScopeChoice {
        occurrence_at,
        scope,
    }))
}

/// The form error a 400 of the `PATCH` comes back under. The scope's own
/// refusals (#708) say what went wrong; anything else reads as before.
fn rejected_edit_code(api_code: Option<&str>) -> &'static str {
    match api_code {
        Some("occurrence_not_in_series") => "occurrence_not_in_series",
        Some("scope_not_allowed_for_imported_event") => "scope_not_allowed_for_imported_event",
        Some("scope_requires_recurring_event") => "scope_requires_recurring_event",
        _ => "ends_before_starts",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(display_name: &str) -> GroupMember {
        GroupMember {
            user_id: Uuid::from_u128(1),
            role: "member".to_string(),
            display_name: display_name.to_string(),
            email: "camille@example.test".to_string(),
            has_avatar: false,
        }
    }

    fn edit_page(is_private: bool) -> String {
        page(
            "",
            Uuid::from_u128(9),
            false,
            "Dentiste",
            "",
            "",
            "2026-09-05T10:00",
            "2026-09-05T11:00",
            false,
            &RecurrenceFields::default(),
            None,
            &[member("Camille")],
            &[],
            is_private,
            None,
            None,
        )
    }

    /// #622: another family's event is edited in that family — the form
    /// posts to it and the way back returns to it.
    #[test]
    fn another_familys_edit_form_posts_in_that_family() {
        let g = Uuid::from_u128(2);
        let html = page(
            "",
            Uuid::from_u128(9),
            false,
            "Dentiste",
            "",
            "",
            "2026-09-05T10:00",
            "2026-09-05T11:00",
            false,
            &RecurrenceFields::default(),
            None,
            &[member("Camille")],
            &[],
            false,
            Some(g),
            None,
        );
        let id = Uuid::from_u128(9);
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/agenda/{id}/edit?famille={g}">"#
            )),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"<a href="/agenda/{id}?famille={g}">Retour"#)),
            "{html}"
        );
        let own = edit_page(false);
        assert!(!own.contains("famille="), "{own}");
    }

    /// #696: under an error the edit form gives back what was submitted —
    /// the place, the description and the recurrence, the value the rule
    /// refused (`invalid_rrule`) included.
    #[test]
    fn the_edit_form_comes_back_with_what_was_submitted() {
        let form: EventForm = serde_urlencoded::from_str(
            "title=D%C3%AEner&location=Chez+L%C3%A9a&description=Apporter+le+vin\
             &starts_at=2026-09-05T20:00&ends_at=2026-09-05T21:00\
             &freq=weekly&interval=2&byday_mo=on&byday_we=on&end_kind=count&count=0",
        )
        .unwrap();
        assert!(rrule_from_form(&form).is_err());
        let html = page(
            "",
            Uuid::from_u128(9),
            false,
            &form.title,
            &form.description,
            &form.location,
            &form.starts_at,
            &form.ends_at,
            false,
            &RecurrenceFields::from_form(&form),
            Some(error_message("invalid_rrule")),
            &[member("Camille")],
            &[],
            false,
            None,
            None,
        );
        assert!(
            html.contains(r#"name="title" required value="Dîner"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="location" value="Chez Léa"/>"#),
            "{html}"
        );
        assert!(html.contains(">Apporter le vin</textarea>"), "{html}");
        assert!(
            html.contains(r#"<option value="weekly" selected>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="interval" min="1" max="99" value="2""#),
            "{html}"
        );
        assert!(html.contains(r#"name="byday_mo" checked/>"#), "{html}");
        assert!(html.contains(r#"name="byday_we" checked/>"#), "{html}");
        assert!(
            html.contains(r#"<option value="count" selected>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="count" min="1" max="730" value="0""#),
            "{html}"
        );
    }

    /// #698: an event with a rule opens its edit form with the « Événement
    /// récurrent » box checked over its settings; a one-off one, unchecked.
    #[test]
    fn the_edit_form_checks_the_recurring_box_of_a_recurring_event() {
        let rule = parse_rrule("FREQ=WEEKLY;BYDAY=TU").unwrap();
        let html = page(
            "",
            Uuid::from_u128(9),
            false,
            "Piscine",
            "",
            "",
            "2026-09-05T10:00",
            "2026-09-05T11:00",
            false,
            &RecurrenceFields::from_recurrence(&rule),
            None,
            &[member("Camille")],
            &[],
            false,
            None,
            None,
        );
        assert!(
            html.contains(r#"name="recurring" checked aria-expanded="true""#),
            "{html}"
        );
        assert!(html.contains(r#"name="byday_tu" checked/>"#), "{html}");
        assert!(
            edit_page(false).contains(r#"name="recurring" aria-expanded="false""#),
            "{}",
            edit_page(false)
        );
    }

    /// #621: the edit form carries the « Privé » box under who it is for,
    /// checked as the event is.
    #[test]
    fn the_edit_form_shows_the_private_box_under_who_it_is_for() {
        let html = edit_page(false);
        let boxed = html.find(r#"name="is_private""#).expect(&html);
        assert!(
            html.find(r#"name="assignee_ids""#).unwrap() < boxed,
            "{html}"
        );
        assert!(html.find("Aucune sélection").unwrap() < boxed, "{html}");
        assert!(boxed < html.find("Enregistrer").unwrap(), "{html}");
        assert!(html.contains(&private_checkbox(false)), "{html}");
        assert!(edit_page(true).contains(&private_checkbox(true)));
    }

    #[test]
    fn a_loaded_roster_still_explains_the_empty_selection_default() {
        let hint = assignment_hint(&[member("Camille")]);
        assert!(hint.contains("créateur de l'événement"), "{hint}");
    }

    /// #99: on the degraded path the fieldset already says the assignment
    /// stays unchanged, and the form carries no opinion about it at all
    /// (`ASSIGNEES_PRESENT_FIELD` is absent, so the handler sends no
    /// `assignee_ids`). Promising a fallback to the creator there announced
    /// a write that never happens.
    #[test]
    fn a_roster_that_failed_to_load_promises_no_fallback() {
        assert_eq!(assignment_hint(&[]), "");
    }

    // -- the scope of an edit opened on an occurrence (#709) ---------------

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    /// A weekly series on Tuesdays at 17:00 in Paris (16:00 UTC in
    /// November), an hour long, ten times, from 2026-11-03.
    fn series() -> EventResponse {
        EventResponse {
            id: Uuid::from_u128(7),
            group_id: Uuid::from_u128(2),
            created_by: Uuid::from_u128(1),
            title: "Piscine".to_string(),
            description: None,
            location: None,
            starts_at: at("2026-11-03T16:00:00Z"),
            ends_at: at("2026-11-03T17:00:00Z"),
            all_day: false,
            is_task: false,
            completed_at: None,
            rrule: Some("FREQ=WEEKLY;BYDAY=TU;COUNT=10".to_string()),
            assignee_ids: vec![Uuid::from_u128(1)],
            is_private: false,
        }
    }

    /// Its third occurrence, 2026-11-17.
    fn third(scope: EditScope) -> ScopeChoice {
        ScopeChoice {
            occurrence_at: at("2026-11-17T16:00:00Z"),
            scope,
        }
    }

    /// The edit form as it is sent from the third occurrence, its rule
    /// untouched, with `extra` replacing or adding fields.
    fn submitted(starts: &str, ends: &str, rule: &str) -> EventForm {
        serde_urlencoded::from_str(&format!(
            "title=Piscine&starts_at={starts}&ends_at={ends}\
             &recurring_box=1&recurring=on&{rule}"
        ))
        .unwrap()
    }

    const SAME_RULE: &str = "freq=weekly&interval=1&byday_tu=on&end_kind=count&count=10";

    /// The request the form makes under `choice`.
    fn request(form: &EventForm, choice: ScopeChoice) -> UpdateEventRequest {
        let body = update_request(form, &[]).unwrap();
        scoped_request(body, choice, form, &series()).unwrap()
    }

    /// A 400 of the scope says what was refused; any other reads as before.
    #[test]
    fn a_refused_scope_comes_back_under_its_own_message() {
        for code in [
            "occurrence_not_in_series",
            "scope_not_allowed_for_imported_event",
            "scope_requires_recurring_event",
        ] {
            assert_eq!(rejected_edit_code(Some(code)), code);
            assert_ne!(error_message(code), error_message("unknown"), "{code}");
        }
        assert_eq!(
            rejected_edit_code(Some("title_required")),
            "ends_before_starts"
        );
        assert_eq!(rejected_edit_code(None), "ends_before_starts");
    }

    /// A forged occurrence or scope is an unreadable form; no occurrence is
    /// an edit of the whole event.
    #[test]
    fn the_scope_fields_are_read_from_the_body() {
        assert_eq!(read_scope(b"title=A"), Ok(None));
        assert_eq!(
            read_scope(b"title=A&occurrence_at=2026-11-17T16%3A00%3A00Z&scope=this"),
            Ok(Some(third(EditScope::This)))
        );
        assert_eq!(
            read_scope(b"occurrence_at=2026-11-17T16%3A00%3A00Z"),
            Ok(Some(third(EditScope::All)))
        );
        assert_eq!(
            read_scope(b"occurrence_at=2026-11-17T16%3A00%3A00Z&scope=both"),
            Err(())
        );
        assert_eq!(read_scope(b"occurrence_at=demain&scope=this"), Err(()));
    }

    #[test]
    fn the_scope_radio_reads_as_the_apis_scope() {
        assert_eq!(scope_from_form("all"), Some(EditScope::All));
        assert_eq!(
            scope_from_form("this_and_following"),
            Some(EditScope::ThisAndFollowing)
        );
        assert_eq!(scope_from_form("this"), Some(EditScope::This));
        assert_eq!(scope_from_form(""), None);
        assert_eq!(scope_from_form("everything"), None);
    }

    #[test]
    fn an_occurrence_is_shown_at_its_own_day_for_the_series_duration() {
        assert_eq!(
            occurrence_field_values(&series(), at("2026-11-17T16:00:00Z")),
            (
                "2026-11-17T17:00".to_string(),
                "2026-11-17T18:00".to_string()
            )
        );
        let mut all_day = series();
        all_day.all_day = true;
        all_day.starts_at = at("2026-11-02T23:00:00Z");
        all_day.ends_at = at("2026-11-03T23:00:00Z");
        assert_eq!(
            field_values(true, all_day.starts_at, all_day.ends_at),
            ("2026-11-03".to_string(), "2026-11-03".to_string())
        );
        assert_eq!(
            occurrence_field_values(&all_day, at("2026-11-16T23:00:00Z")),
            ("2026-11-17".to_string(), "2026-11-17".to_string())
        );
    }

    #[test]
    fn a_field_moved_on_the_occurrence_moves_the_series_by_as_many_days() {
        assert_eq!(
            rebased("2026-11-18T09:30", "2026-11-17T17:00", "2026-11-03T17:00").as_deref(),
            Some("2026-11-04T09:30")
        );
        // Made all-day: the series' day, without a time.
        assert_eq!(
            rebased("2026-11-17", "2026-11-17T17:00", "2026-11-03T17:00").as_deref(),
            Some("2026-11-03")
        );
        // Across a month.
        assert_eq!(
            rebased("2026-12-01", "2026-11-17", "2026-11-03").as_deref(),
            Some("2026-11-17")
        );
        assert_eq!(rebased("demain", "2026-11-17", "2026-11-03"), None);
    }

    /// Without an occurrence, the form edits the whole series, as before.
    #[test]
    fn an_edit_without_occurrence_sends_no_scope() {
        let form = submitted("2026-11-03T17:00", "2026-11-03T18:00", SAME_RULE);
        let body = update_request(&form, &[]).unwrap();
        assert_eq!(body.scope, None);
        assert_eq!(body.occurrence_at, None);
        assert_eq!(body.starts_at, Some(at("2026-11-03T16:00:00Z")));
        assert_eq!(body.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=TU;COUNT=10"));
    }

    /// « Toute la série » with the occurrence's times untouched leaves the
    /// series' times alone: the form shows the occurrence's day, and sent
    /// as it is it would move the series' start there.
    #[test]
    fn the_whole_series_keeps_its_times_when_they_are_untouched() {
        let form = submitted("2026-11-17T17:00", "2026-11-17T18:00", SAME_RULE);
        let body = request(&form, third(EditScope::All));
        assert_eq!(body.scope, Some(EditScope::All));
        assert_eq!(body.occurrence_at, None);
        assert_eq!((body.starts_at, body.ends_at), (None, None));
        assert_eq!(body.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=TU;COUNT=10"));
        assert_eq!(body.title.as_deref(), Some("Piscine"));
    }

    /// Moved on the occurrence, the whole series moves by as much.
    #[test]
    fn the_whole_series_moves_as_the_occurrence_was_moved() {
        let form = submitted("2026-11-18T09:30", "2026-11-18T10:30", SAME_RULE);
        let body = request(&form, third(EditScope::All));
        assert_eq!(body.starts_at, Some(at("2026-11-04T08:30:00Z")));
        assert_eq!(body.ends_at, Some(at("2026-11-04T09:30:00Z")));
    }

    #[test]
    fn this_one_only_sends_the_occurrence_and_no_rule() {
        let form = submitted("2026-11-17T17:00", "2026-11-17T18:00", SAME_RULE);
        let body = request(&form, third(EditScope::This));
        assert_eq!(body.scope, Some(EditScope::This));
        assert_eq!(body.occurrence_at, Some(at("2026-11-17T16:00:00Z")));
        assert_eq!((body.starts_at, body.ends_at), (None, None));
        assert_eq!(body.rrule, None);

        let moved = submitted("2026-11-18T09:30", "2026-11-18T10:30", SAME_RULE);
        let body = request(&moved, third(EditScope::This));
        assert_eq!(body.starts_at, Some(at("2026-11-18T08:30:00Z")));
        assert_eq!(body.ends_at, Some(at("2026-11-18T09:30:00Z")));
    }

    /// The series' rule untouched is not sent: the API carries on with what
    /// is left of it (`COUNT` less what the series already produced), where
    /// the form's `COUNT=10` would start ten more.
    #[test]
    fn this_and_following_sends_a_rule_only_when_it_changed() {
        let form = submitted("2026-11-17T17:00", "2026-11-17T18:00", SAME_RULE);
        let body = request(&form, third(EditScope::ThisAndFollowing));
        assert_eq!(body.scope, Some(EditScope::ThisAndFollowing));
        assert_eq!(body.occurrence_at, Some(at("2026-11-17T16:00:00Z")));
        assert_eq!((body.starts_at, body.ends_at), (None, None));
        assert_eq!(body.rrule, None);

        let daily = submitted(
            "2026-11-17T18:00",
            "2026-11-17T19:00",
            "freq=daily&interval=1&end_kind=never",
        );
        let body = request(&daily, third(EditScope::ThisAndFollowing));
        assert_eq!(body.rrule.as_deref(), Some("FREQ=DAILY"));
        assert_eq!(body.starts_at, Some(at("2026-11-17T17:00:00Z")));

        // « Événement récurrent » unchecked: the rest becomes one event.
        let once: EventForm = serde_urlencoded::from_str(
            "title=Piscine&starts_at=2026-11-17T17:00&ends_at=2026-11-17T18:00&recurring_box=1",
        )
        .unwrap();
        let body = request(&once, third(EditScope::ThisAndFollowing));
        assert_eq!(body.rrule.as_deref(), Some(""));
    }

    #[test]
    fn the_scope_fieldset_offers_three_choices_the_whole_series_first() {
        let html = scope_fieldset(&third(EditScope::All), false);
        assert!(html.starts_with(r#"<fieldset class="card">"#), "{html}");
        assert!(html.contains("<legend>"), "{html}");
        assert!(
            html.contains(
                r#"<input type="hidden" name="occurrence_at" value="2026-11-17T16:00:00Z"/>"#
            ),
            "{html}"
        );
        assert!(html.contains("17/11/2026 à 17:00"), "{html}");
        let all = html
            .find(
                r#"<input type="radio" name="scope" value="all" checked/> Toute la série</label>"#,
            )
            .expect(&html);
        let following = html
            .find(r#"<input type="radio" name="scope" value="this_and_following"/> Cet événement et les suivants</label>"#)
            .expect(&html);
        let this = html
            .find(r#"<input type="radio" name="scope" value="this"/> Cet événement uniquement</label>"#)
            .expect(&html);
        assert!(all < following && following < this, "{html}");
        assert_eq!(html.matches(" checked").count(), 1, "{html}");

        // Back under an error, the choice made stays made.
        let html = scope_fieldset(&third(EditScope::This), true);
        assert!(html.contains(r#"value="this" checked/>"#), "{html}");
        assert_eq!(html.matches(" checked").count(), 1, "{html}");
        assert!(html.contains("17/11/2026"), "{html}");
        assert!(!html.contains("17:00"), "{html}");
    }

    #[test]
    fn the_scope_is_asked_just_before_saving_and_only_on_an_occurrence() {
        assert!(!edit_page(false).contains(r#"name="scope""#));
        let html = page(
            "",
            Uuid::from_u128(7),
            false,
            "Piscine",
            "",
            "",
            "2026-11-17T17:00",
            "2026-11-17T18:00",
            false,
            &RecurrenceFields::default(),
            None,
            &[member("Camille")],
            &[],
            false,
            None,
            Some(&third(EditScope::All)),
        );
        let scope = html.find(r#"name="scope""#).expect(&html);
        assert!(html.find(r#"name="is_private""#).unwrap() < scope, "{html}");
        assert!(scope < html.find("Enregistrer").unwrap(), "{html}");
        // The way back returns to the occurrence it came from.
        assert!(
            html.contains(&format!(
                r#"<a href="/agenda/{}?occ=2026-11-17T16:00:00Z">Retour"#,
                Uuid::from_u128(7)
            )),
            "{html}"
        );
    }
}
