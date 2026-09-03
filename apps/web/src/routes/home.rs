use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use leptos::prelude::*;
use manage_our_home_shared::dto::agenda::{OccurrenceList, OccurrenceResponse};
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::budget::{BudgetPeriodTotal, BudgetSummary};
use manage_our_home_shared::dto::grocery_list::{GroceryItemList, GroceryItemResponse};
use manage_our_home_shared::dto::groups::{GroupDetailResponse, GroupMember, GroupSummary};
use manage_our_home_shared::dto::messagerie::{MessageList, MessageResponse};
use manage_our_home_shared::dto::stocks::{StockItemList, StockItemResponse};
use manage_our_home_shared::validation::budget::{format_euros, month_name_fr};
use manage_our_home_shared::validation::messagerie::author_name;
use uuid::Uuid;

use crate::app::{
    combined_member_colour, html_escape, member_colour, member_initial, shell, shell_with_header,
    Width,
};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::layout::CurrentUser;
use crate::routes::agenda::{fmt_paris, paris_local_to_utc, today_paris};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::{api_request_auth, AppState};

/// "Quelques jours" (issue #73): today plus the next two civil days — enough
/// to answer "what's coming up", not a second calendar next to `/agenda`.
const DASHBOARD_AGENDA_WINDOW_DAYS: i64 = 3;
/// The dashboard shows what's soonest, not everything the window holds — the
/// full picture is one click away on `/agenda`.
const DASHBOARD_AGENDA_CAP: usize = 5;
/// Same trade for stock: a taste of what's low, `/stocks?low_stock=1` for
/// the rest.
const DASHBOARD_STOCK_CAP: usize = 5;
/// Requested straight from the API (`?limit=&unread=true`), newest-unread
/// first — no client-side capping needed for messages the way the other
/// blocks need it.
const DASHBOARD_MESSAGES_LIMIT: i64 = 5;
/// A message preview is not the thread: long messages are cut to this many
/// characters so one verbose message can't push the rest of the card (and the
/// blocks below it) off screen.
const DASHBOARD_MESSAGE_SNIPPET_MAX_CHARS: usize = 120;

// -- pure logic (TDD'd below) ------------------------------------------------

/// Sorts occurrences chronologically and keeps the earliest `cap` that have
/// not yet started — the dashboard answers "what's coming up next", not
/// "what happened today". The API window the caller fetches starts at
/// midnight Paris (same civil day the calendar page uses), so the raw list
/// still holds this morning's already-finished events; filtering on `now`
/// here (not on the query's lower bound) is what keeps a 17:00 page load
/// from surfacing an 09:00 event as "coming up" (#98 verification finding).
/// An API response is not guaranteed to arrive in start-time order either,
/// hence the sort.
fn soonest_occurrences(
    occurrences: &[OccurrenceResponse],
    now: DateTime<Utc>,
    cap: usize,
) -> Vec<&OccurrenceResponse> {
    let mut refs: Vec<&OccurrenceResponse> = occurrences
        .iter()
        .filter(|o| o.occurrence_starts_at >= now)
        .collect();
    refs.sort_by_key(|o| o.occurrence_starts_at);
    refs.truncate(cap);
    refs
}

/// How many grocery-list items are still unchecked — the count the dashboard
/// shows without listing every item.
fn count_unchecked(items: &[GroceryItemResponse]) -> usize {
    items.iter().filter(|i| !i.checked).count()
}

/// The current month's cumulated spend from a `BudgetSummary`'s periods
/// (each `period` is the first day of its month). `None` when the family has
/// no entry at all this month — distinct from "0,00 €", which would claim
/// spending was recorded and happened to be nil.
fn current_month_total(periods: &[BudgetPeriodTotal], today: NaiveDate) -> Option<f64> {
    periods
        .iter()
        .find(|p| p.period.year() == today.year() && p.period.month() == today.month())
        .map(|p| p.total)
}

/// Cuts a message preview to `max_chars`, appending an ellipsis when it did.
/// Splits on `char` boundaries (never mid-UTF-8-sequence) and trims the
/// trailing whitespace an arbitrary cut can leave before the ellipsis.
fn truncate_message(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        content.to_string()
    } else {
        let head: String = content.chars().take(max_chars).collect();
        format!("{}…", head.trim_end())
    }
}

// -- the dashboard proper -----------------------------------------------

/// Resolved active-family context for the home dashboard: the family id
/// every `/groups/:gid/…` API call is scoped to, plus the shared
/// authenticated header (nav + family switcher, #17). Built the same way as
/// every other family-scoped route (`routes/agenda`, `routes/stocks`, …);
/// `None` means the user has no group yet, handled separately by `get`
/// before this is even called.
struct FamilyContext {
    gid: Uuid,
    header: String,
}

async fn family_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
) -> (Option<FamilyContext>, Vec<GroupSummary>) {
    let (groups, header) = header_with_groups(state, headers, me, "/").await;
    let preferred = active_group_id_from_headers(headers);
    let active: Option<&GroupSummary> = resolve_active_group(&groups, preferred);
    let fam = active.map(|g| FamilyContext {
        gid: g.group_id,
        header: header.clone(),
    });
    (fam, groups)
}

fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
    };
    Html(shell(Width::Form, "Service indisponible", &body.to_html()))
}

/// The family's members, for resolving a name behind a colour/id — a
/// dashboard failure here degrades to an empty roster (the blocks that need
/// it fall back to "Membre") rather than taking the whole page down over
/// decoration, same call as `routes::messagerie::thread::fetch_members`.
async fn fetch_members(state: &AppState, gid: Uuid, cookie: Option<&str>) -> Vec<GroupMember> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<GroupDetailResponse>(resp.body)
                .map(|g| g.members)
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// One upcoming-event row: the assigned member(s)' coloured initial (their
/// own colour for one assignee, the running average for several — see
/// `combined_member_colour`), the time, and the title. The ring's letter is
/// the first assignee's initial; with several assignees the names beside it
/// (not just the ring) are what actually says who, matching WCAG 1.4.1
/// (colour is never the only carrier — see `agenda/calendar.rs`'s doc
/// comment on the same constraint).
fn agenda_row(occ: &OccurrenceResponse, members: &[GroupMember]) -> String {
    let e = &occ.event;
    let assignee_names: Vec<&str> = e
        .assignee_ids
        .iter()
        .map(|id| author_name(members, *id))
        .collect();
    let assignees = assignee_names.join(", ");
    let colour_tokens: Vec<&str> = e.assignee_ids.iter().copied().map(member_colour).collect();
    let colour = combined_member_colour(&colour_tokens);
    let initial = assignee_names
        .first()
        .map(|n| member_initial(n))
        .unwrap_or_else(|| "?".to_string());
    let time = if e.all_day {
        "journée".to_string()
    } else {
        // No weekday name here: chrono's `%a` has no locale support and
        // would render the English abbreviation (`Wed`) in an
        // otherwise all-French page — see `agenda/detail.rs`'s
        // pre-existing use of the same specifier, not this issue's to fix.
        fmt_paris(occ.occurrence_starts_at, "%d/%m %H:%M")
    };
    format!(
        r#"<li class="list-row"><span><span class="avatar" style="color:{colour}" aria-hidden="true">{initial}</span> <strong>{time}</strong> {title} <span class="muted">— {assignees}</span></span></li>"#,
        colour = colour,
        initial = html_escape(&initial),
        time = html_escape(&time),
        title = html_escape(&e.title),
        assignees = html_escape(&assignees),
    )
}

fn agenda_block(
    occurrences: &[OccurrenceResponse],
    now: DateTime<Utc>,
    members: &[GroupMember],
) -> String {
    let picked = soonest_occurrences(occurrences, now, DASHBOARD_AGENDA_CAP);
    let body = if picked.is_empty() {
        r#"<p class="muted">Rien de prévu dans les prochains jours.</p>"#.to_string()
    } else {
        let rows: String = picked.iter().map(|o| agenda_row(o, members)).collect();
        format!(r#"<ul class="list">{rows}</ul>"#)
    };
    format!(
        r#"<section class="card">
<h2>Prochains événements</h2>
{body}
<a class="btn secondary" href="/agenda">Voir l'agenda</a>
</section>"#
    )
}

fn stocks_block(items: &[StockItemResponse]) -> String {
    let shown = &items[..items.len().min(DASHBOARD_STOCK_CAP)];
    let hidden = items.len().saturating_sub(shown.len());
    let body = if items.is_empty() {
        r#"<p class="muted">Aucun article en stock bas.</p>"#.to_string()
    } else {
        let rows: String = shown
            .iter()
            .map(|item| {
                format!(
                    r#"<li class="list-row"><span><strong>{name}</strong></span> <span class="badge warn">Stock bas</span></li>"#,
                    name = html_escape(&item.name),
                )
            })
            .collect();
        let more = if hidden > 0 {
            format!(r#"<p class="muted">+{hidden} autre(s).</p>"#)
        } else {
            String::new()
        };
        format!(r#"<ul class="list">{rows}</ul>{more}"#)
    };
    format!(
        r#"<section class="card">
<h2>Stock bas</h2>
{body}
<a class="btn secondary" href="/stocks?low_stock=1">Voir le stock</a>
</section>"#
    )
}

fn grocery_block(unchecked: usize) -> String {
    let text = match unchecked {
        0 => "Aucun article à acheter pour le moment.".to_string(),
        1 => "1 article à acheter.".to_string(),
        n => format!("{n} articles à acheter."),
    };
    format!(
        r#"<section class="card">
<h2>Liste de courses</h2>
<p>{text}</p>
<a class="btn secondary" href="/grocery-list">Voir la liste</a>
</section>"#,
        text = html_escape(&text),
    )
}

fn budget_block(total: Option<f64>, today: NaiveDate) -> String {
    let month = html_escape(month_name_fr(today.month()));
    let body = match total {
        Some(total) => format!(
            r#"<p><strong>{amount}</strong> dépensés en {month}.</p>"#,
            amount = html_escape(&format_euros(total)),
        ),
        None => format!(r#"<p class="muted">Aucune dépense enregistrée en {month}.</p>"#),
    };
    format!(
        r#"<section class="card">
<h2>Budget</h2>
{body}
<a class="btn secondary" href="/budget">Voir le budget</a>
</section>"#
    )
}

/// One message-preview row. Since #73, `messages` really is the unread set
/// (`GET …/messages?unread=true`, backed by `message_read_state` — see
/// `apps/api/src/messagerie/messages.rs::unread_messages`), not just the
/// most recent few: this card only shows what the caller hasn't read yet.
fn message_row(msg: &MessageResponse, members: &[GroupMember]) -> String {
    let author = author_name(members, msg.created_by);
    let snippet = truncate_message(&msg.content, DASHBOARD_MESSAGE_SNIPPET_MAX_CHARS);
    format!(
        r#"<li class="list-row stacked"><span class="muted"><strong>{author}</strong></span><span>{snippet}</span></li>"#,
        author = html_escape(author),
        snippet = html_escape(&snippet),
    )
}

/// `messages` is already the unread page (see `message_row`'s doc comment):
/// an empty list here means "nothing unread", not "no messages ever" — same
/// distinction `grocery_block` draws between zero-unchecked and an empty
/// list, and the same reason the empty copy says "tout est lu" rather than
/// "aucun message".
fn messages_block(messages: &[MessageResponse], members: &[GroupMember]) -> String {
    let body = if messages.is_empty() {
        r#"<p class="muted">Tout est lu.</p>"#.to_string()
    } else {
        let rows: String = messages.iter().map(|m| message_row(m, members)).collect();
        format!(r#"<ul class="list">{rows}</ul>"#)
    };
    format!(
        r#"<section class="card">
<h2>Messages non lus</h2>
{body}
<a class="btn secondary" href="/messagerie">Voir la messagerie</a>
</section>"#
    )
}

/// Authenticated home page. Since the Groups epic (#17) it carries the
/// shared header with the active-family switcher. Since #73 it carries the
/// active family's dashboard: what's coming up on the agenda, what's low in
/// stock, how much is left to buy, this month's spend, and the unread chat —
/// one card per domain, each behind its own read of the same repositories
/// the domain's own page uses (no bespoke summary tables, no invented
/// fields). With no family yet, the pre-#73 empty-state hint
/// (`/groups/new`, `/groups`) is unchanged.
///
/// Each card is one more API call to apps/api at render time — this app is
/// SSR-only, with no client-side hydration to defer work to (see
/// `Cargo.toml`'s doc comment and the issue's own "Attention" note). Five
/// domain calls plus one shared members lookup (for the agenda/messages
/// author names) is the accepted cost of five cards; if that ever needs
/// trimming, the fix is fewer cards, not async loading.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let (fam, _groups) = family_context(&state, &headers, &me).await;

    let Some(fam) = fam else {
        let no_group_hint = view! {
            <p>
                "Commencez par "
                <a href="/groups/new">"créer votre famille"</a>
                " ou rejoignez-en une via une invitation depuis "
                <a href="/groups">"Mes groupes"</a>
                "."
            </p>
        };
        let (_, header) = header_with_groups(&state, &headers, &me, "/").await;
        let body = view! {
            <h1>"Bienvenue"</h1>
            <p>"Vous êtes connecté."</p>
            {no_group_hint}
        };
        return Html(shell_with_header(
            Width::Read,
            "Accueil",
            &header,
            &body.to_html(),
        ))
        .into_response();
    };

    let cookie = cookie_of(&headers);
    let today = today_paris();

    // Agenda: today .. today + (WINDOW_DAYS - 1), same Paris-civil-day ->
    // UTC-instant conversion `/agenda` itself uses.
    let last_day = today + Duration::days(DASHBOARD_AGENDA_WINDOW_DAYS - 1);
    let (Some(from), Some(to)) = (
        paris_local_to_utc(&format!("{today}T00:00")),
        paris_local_to_utc(&format!("{last_day}T23:59:59")),
    ) else {
        return service_unavailable_page().into_response();
    };
    let occurrences: Vec<OccurrenceResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!(
            "/groups/{}/events?from={}&to={}",
            fam.gid,
            from.format("%Y-%m-%dT%H:%M:%SZ"),
            to.format("%Y-%m-%dT%H:%M:%SZ"),
        ),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<OccurrenceList>(resp.body)
                .map(|l| l.occurrences)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let low_stock: Vec<StockItemResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/stock-items?low_stock=true", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<StockItemList>(resp.body)
                .map(|l| l.items)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let grocery_items: Vec<GroceryItemResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/grocery-items", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<GroceryItemList>(resp.body)
                .map(|l| l.items)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let budget_summary: BudgetSummary = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/budget-entries/summary", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<BudgetSummary>(resp.body).unwrap_or(BudgetSummary {
                periods: Vec::new(),
            })
        }
        Ok(_) => BudgetSummary {
            periods: Vec::new(),
        },
        Err(_) => return service_unavailable_page().into_response(),
    };

    let messages: Vec<MessageResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!(
            "/groups/{}/messages?limit={}&unread=true",
            fam.gid, DASHBOARD_MESSAGES_LIMIT
        ),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<MessageList>(resp.body)
                .map(|l| l.messages)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let members = fetch_members(&state, fam.gid, cookie.as_deref()).await;

    let body = format!(
        r#"<h1>Accueil</h1>
{agenda}
{stocks}
{grocery}
{budget}
{messages}"#,
        agenda = agenda_block(&occurrences, chrono::Utc::now(), &members),
        stocks = stocks_block(&low_stock),
        grocery = grocery_block(count_unchecked(&grocery_items)),
        budget = budget_block(current_month_total(&budget_summary.periods, today), today),
        messages = messages_block(&messages, &members),
    );

    Html(shell_with_header(
        Width::Full,
        "Accueil",
        &fam.header,
        &body,
    ))
    .into_response()
}

/// `POST /logout` on apps/web itself: forwards to `POST /auth/logout` on
/// apps/api (revoking the session server-side), then always redirects to
/// `/login` — matching AC #7 ("Logout clears the session cookie and
/// redirects to /login").
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok());

    let mut req = state
        .http
        .post(format!("{}/auth/logout", state.api_internal_base_url));
    if let Some(cookie) = cookie_header {
        req = req.header("cookie", cookie);
    }
    let api_resp = req.send().await.ok();

    let mut response_headers = HeaderMap::new();
    if let Some(resp) = api_resp {
        if let Some(set_cookie) = resp.headers().get(axum::http::header::SET_COOKIE) {
            response_headers.insert(axum::http::header::SET_COOKIE, set_cookie.clone());
        }
    }
    (response_headers, Redirect::to("/login")).into_response()
}

/// Landing page for the post-Google-OAuth redirect. In practice
/// apps/api's `/auth/google/callback` already redirects straight to `/`
/// once the session cookie is set (`state.frontend_base_url`, see
/// `apps/api/src/auth/oauth_google.rs::callback`), so the root layout's
/// own `GET /auth/me` check is what actually confirms the session. This
/// route exists as a defensive landing spot matching issue #15's route
/// table (`/auth/google/callback` — "confirms session cookie present,
/// redirects to /") in case that redirect target ever points here
/// instead.
pub async fn google_callback(CurrentUserOpt(me): CurrentUserOpt) -> impl IntoResponse {
    if me.is_some() {
        Redirect::to("/")
    } else {
        Redirect::to("/login")
    }
}

pub use crate::layout::CurrentUserOpt;

#[cfg(test)]
mod pure_logic_tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use manage_our_home_shared::dto::agenda::EventResponse;

    fn occurrence(hour: u32, title: &str) -> OccurrenceResponse {
        let starts = Utc.with_ymd_and_hms(2026, 9, 3, hour, 0, 0).unwrap();
        OccurrenceResponse {
            event: EventResponse {
                id: Uuid::from_u128(u128::from(hour)),
                group_id: Uuid::nil(),
                created_by: Uuid::nil(),
                title: title.to_string(),
                description: None,
                location: None,
                starts_at: starts,
                ends_at: starts,
                all_day: false,
                is_task: false,
                completed_at: None,
                rrule: None,
                assignee_ids: vec![Uuid::nil()],
            },
            occurrence_starts_at: starts,
            occurrence_ends_at: starts,
        }
    }

    // -- soonest_occurrences --------------------------------------------

    /// A `now` late enough that every fixture in this block (hours 8-14) is
    /// unambiguously in the past, except where a test picks a later `now`
    /// on purpose.
    fn late_afternoon() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 3, 17, 16, 0).unwrap()
    }

    #[test]
    fn occurrences_are_sorted_chronologically_regardless_of_input_order() {
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 8, 0, 0).unwrap();
        let occs = vec![occurrence(14, "Après-midi"), occurrence(9, "Matin")];
        let picked = soonest_occurrences(&occs, now, 5);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].event.title, "Matin");
        assert_eq!(picked[1].event.title, "Après-midi");
    }

    #[test]
    fn more_occurrences_than_the_cap_keeps_only_the_soonest() {
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 0, 0, 0).unwrap();
        let occs: Vec<OccurrenceResponse> = (0..5)
            .map(|i| occurrence(8 + i, &format!("Événement {i}")))
            .collect();
        let picked = soonest_occurrences(&occs, now, 3);
        assert_eq!(picked.len(), 3);
        assert_eq!(picked[0].event.title, "Événement 0");
        assert_eq!(picked[2].event.title, "Événement 2");
    }

    #[test]
    fn an_empty_window_picks_nothing() {
        assert!(soonest_occurrences(&[], late_afternoon(), 5).is_empty());
    }

    /// The exact shape of the verification finding on #98: six occurrences
    /// this morning (08h-13h), `now` at 17h16 — every one of them is
    /// already over, so none should be picked, even though the API window
    /// (Paris midnight .. midnight+WINDOW_DAYS) still returns all six.
    #[test]
    fn occurrences_earlier_today_than_now_are_excluded() {
        let now = late_afternoon();
        let occs: Vec<OccurrenceResponse> = (0..6).map(|i| occurrence(8 + i, "Matin")).collect();
        let picked = soonest_occurrences(&occs, now, 5);
        assert!(picked.is_empty(), "{picked:#?}");
    }

    /// A mix of already-past and still-upcoming occurrences today: only the
    /// ones at or after `now` are "coming up".
    #[test]
    fn only_occurrences_at_or_after_now_are_picked() {
        let now = late_afternoon();
        let occs = vec![
            occurrence(9, "Petit-déjeuner"),   // past
            occurrence(14, "Déjeuner tardif"), // past
            occurrence(18, "Dîner"),           // future
            occurrence(20, "Soirée"),          // future
        ];
        let picked = soonest_occurrences(&occs, now, 5);
        assert_eq!(picked.len(), 2, "{picked:#?}");
        assert_eq!(picked[0].event.title, "Dîner");
        assert_eq!(picked[1].event.title, "Soirée");
    }

    /// An occurrence starting at exactly `now` is still upcoming, not past
    /// — the filter is inclusive on its lower bound.
    #[test]
    fn an_occurrence_starting_exactly_now_is_kept() {
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 12, 0, 0).unwrap();
        let occs = vec![occurrence(12, "Pile à l'heure")];
        let picked = soonest_occurrences(&occs, now, 5);
        assert_eq!(picked.len(), 1);
    }

    // -- count_unchecked --------------------------------------------------

    fn grocery_item(checked: bool) -> GroceryItemResponse {
        GroceryItemResponse {
            id: Uuid::nil(),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            name: "Lait".to_string(),
            quantity: None,
            unit: None,
            checked,
            source: "manual".to_string(),
            source_recipe_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn an_empty_list_has_nothing_unchecked() {
        assert_eq!(count_unchecked(&[]), 0);
    }

    #[test]
    fn only_unchecked_items_are_counted() {
        let items = vec![grocery_item(false), grocery_item(true), grocery_item(false)];
        assert_eq!(count_unchecked(&items), 2);
    }

    #[test]
    fn a_fully_checked_list_counts_zero() {
        let items = vec![grocery_item(true), grocery_item(true)];
        assert_eq!(count_unchecked(&items), 0);
    }

    // -- current_month_total ----------------------------------------------

    fn period(year: i32, month: u32, total: f64) -> BudgetPeriodTotal {
        BudgetPeriodTotal {
            period: NaiveDate::from_ymd_opt(year, month, 1).unwrap(),
            total,
        }
    }

    #[test]
    fn the_period_matching_the_current_month_is_picked() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let periods = vec![period(2026, 9, 42.5), period(2026, 8, 100.0)];
        assert_eq!(current_month_total(&periods, today), Some(42.5));
    }

    #[test]
    fn no_entry_this_month_is_none_not_zero() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let periods = vec![period(2026, 8, 100.0), period(2025, 9, 30.0)];
        assert_eq!(current_month_total(&periods, today), None);
    }

    #[test]
    fn an_empty_summary_has_no_current_month() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        assert_eq!(current_month_total(&[], today), None);
    }

    // -- truncate_message ---------------------------------------------------

    #[test]
    fn a_short_message_is_returned_unchanged() {
        assert_eq!(truncate_message("Bonjour", 120), "Bonjour");
    }

    #[test]
    fn a_message_at_exactly_the_limit_is_unchanged() {
        let content = "a".repeat(10);
        assert_eq!(truncate_message(&content, 10), content);
    }

    #[test]
    fn a_long_message_is_cut_with_an_ellipsis() {
        let content = "a".repeat(15);
        let out = truncate_message(&content, 10);
        assert_eq!(out, format!("{}…", "a".repeat(10)));
    }

    #[test]
    fn trailing_whitespace_left_by_the_cut_is_trimmed_before_the_ellipsis() {
        let content = "dix lettres puis un mot plus long";
        // Cut lands right after "dix lettres" (11 chars) plus a space.
        let out = truncate_message(content, 12);
        assert!(!out.contains(" …"), "{out}");
        assert!(out.ends_with('…'), "{out}");
    }

    #[test]
    fn multi_byte_characters_are_not_split_mid_codepoint() {
        // Every character here is multi-byte in UTF-8; a byte-indexed cut
        // would panic or produce invalid UTF-8.
        let content = "éèàçùâêîôûëïüœ".repeat(3);
        let out = truncate_message(&content, 10);
        assert_eq!(out.chars().count(), 11); // 10 + the ellipsis
    }
}
