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

/// Sorts occurrences chronologically and keeps the earliest `cap` that are
/// **not over yet** — the dashboard answers "what's still ahead of me
/// today", not "what happened this morning". The API window the caller
/// fetches starts at midnight Paris (same civil day the calendar page
/// uses), so the raw list still holds this morning's finished events;
/// filtering here (not on the query's lower bound) is what keeps a 17:00
/// page load from surfacing an 09:00 event as "coming up" (#98
/// verification, round 1).
///
/// The bound is `occurrence_ends_at`, not `occurrence_starts_at`, and that
/// distinction is the whole point (#98 verification, round 2). An `all_day`
/// event starts at Paris midnight, so a start-time filter dropped every
/// birthday, holiday and school break from the moment the day began — the
/// most useful class of entry on a family dashboard, and it also left
/// `agenda_row`'s "journée" branch unreachable for today. The same filter
/// dropped an event merely *in progress*: a 19:00-22:00 dinner vanished at
/// 19:01. Ending the window on the end instant fixes both, and still drops
/// what is genuinely finished.
///
/// An API response is not guaranteed to arrive in start-time order either,
/// hence the sort.
fn soonest_occurrences(
    occurrences: &[OccurrenceResponse],
    now: DateTime<Utc>,
    cap: usize,
) -> Vec<&OccurrenceResponse> {
    let mut refs: Vec<&OccurrenceResponse> = occurrences
        .iter()
        .filter(|o| o.occurrence_ends_at >= now)
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

/// The member ids a dashboard row actually names: the event's assignees, or
/// — when it has none — its creator.
///
/// The `/agenda` write paths keep at least the creator on every event they
/// touch (`resolve_assignees`), but they are not the only way a row enters
/// `events`, and `EventResponse::assignee_ids` can arrive empty — its own
/// doc comment lists the ways known so far. Before this fallback, such a row
/// rendered a bare "?" ring followed by an em dash and nothing (#99).
///
/// **This fallback, not the backfill, is what fixes #99**, and it is not a
/// transitional measure. `0013_backfill_event_assignees.sql` repairs stored
/// rows only on a deployment whose migration role bypasses RLS — under the
/// role `apps/api/README.md` prescribes for `DATABASE_URL` it inserts
/// nothing at all (measured; see that migration's header) — and a migration
/// runs once either way, so it cannot reach an event created later with no
/// assignment. The Google Calendar import creates exactly those, on every
/// deployment, on every import (issue #106). The row therefore has to be
/// right without the database's help, indefinitely.
///
/// `agenda/detail.rs::assignees_html` takes the other way out on the same
/// data — it hides its line entirely — because a detail page has no ring to
/// leave blank.
fn row_assignee_ids<'a>(assignee_ids: &'a [Uuid], created_by: &'a Uuid) -> &'a [Uuid] {
    if assignee_ids.is_empty() {
        std::slice::from_ref(created_by)
    } else {
        assignee_ids
    }
}

/// One upcoming-event row: the assigned member(s)' coloured initial (their
/// own colour for one assignee, the running average for several — see
/// `combined_member_colour`, and `row_assignee_ids` for who counts as
/// assigned on an event carrying no assignee at all), the time, and the
/// title. The ring's letter is the first assignee's initial; with several
/// assignees the names beside it (not just the ring) are what actually says
/// who, matching WCAG 1.4.1 (colour is never the only carrier — see
/// `agenda/calendar.rs`'s doc comment on the same constraint).
fn agenda_row(occ: &OccurrenceResponse, members: &[GroupMember]) -> String {
    let e = &occ.event;
    let shown = row_assignee_ids(&e.assignee_ids, &e.created_by);
    let assignee_names: Vec<&str> = shown.iter().map(|id| author_name(members, *id)).collect();
    let assignees = assignee_names.join(", ");
    let colour_tokens: Vec<&str> = shown.iter().copied().map(member_colour).collect();
    let colour = combined_member_colour(&colour_tokens);
    // `shown` is never empty, so this always names somebody; `member_initial`
    // keeps its own `?` for a name with no usable letter in it.
    let initial = member_initial(assignee_names.first().copied().unwrap_or_default());
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

/// How many unread messages the card is not showing, given the page it
/// received and the family-wide unread count the API reports
/// (`MessageList::unread_total`, `None` on a backend that didn't send it).
/// Saturating: a total that lags the page — a message posted between the
/// two queries of the same request — reads as "nothing more", never as a
/// negative count.
fn hidden_unread(shown: usize, unread_total: Option<i64>) -> usize {
    unread_total
        .map(|total| (total.max(0) as usize).saturating_sub(shown))
        .unwrap_or(0)
}

/// `messages` is already the unread page (see `message_row`'s doc comment):
/// an empty list here means "nothing unread", not "no messages ever" — same
/// distinction `grocery_block` draws between zero-unchecked and an empty
/// list, and the same reason the empty copy says "tout est lu" rather than
/// "aucun message".
///
/// `unread_total` is what turns the cap into an honest one: the page is at
/// most `DASHBOARD_MESSAGES_LIMIT` long, so without it a member with twenty
/// unread messages saw five and no sign of the rest (#98 verification,
/// round 2). Rendered as the "+N autre(s)" line `stocks_block` already used.
fn messages_block(
    messages: &[MessageResponse],
    unread_total: Option<i64>,
    members: &[GroupMember],
) -> String {
    let body = if messages.is_empty() {
        r#"<p class="muted">Tout est lu.</p>"#.to_string()
    } else {
        let rows: String = messages.iter().map(|m| message_row(m, members)).collect();
        let hidden = hidden_unread(messages.len(), unread_total);
        let more = if hidden > 0 {
            format!(r#"<p class="muted">+{hidden} autre(s).</p>"#)
        } else {
            String::new()
        };
        format!(r#"<ul class="list">{rows}</ul>{more}"#)
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

    let empty_messages = || MessageList {
        messages: Vec::new(),
        has_more: false,
        unread_total: None,
    };
    let messages: MessageList = match api_request_auth(
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
            serde_json::from_value::<MessageList>(resp.body).unwrap_or_else(|_| empty_messages())
        }
        Ok(_) => empty_messages(),
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
        messages = messages_block(&messages.messages, messages.unread_total, &members),
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

    /// An occurrence spanning `starts` → `ends`, flagged `all_day` or not.
    /// The two are separate parameters on purpose: a fixture that always
    /// sets `ends == starts` (what this block shipped with) cannot tell a
    /// finished event from one still running, and one that never sets
    /// `all_day` cannot tell a birthday from a meeting — both blind spots
    /// hid a real dashboard bug (#98 verification, round 2).
    fn occurrence_between(
        starts: DateTime<Utc>,
        ends: DateTime<Utc>,
        all_day: bool,
        title: &str,
    ) -> OccurrenceResponse {
        OccurrenceResponse {
            event: EventResponse {
                id: Uuid::from_u128(starts.timestamp() as u128),
                group_id: Uuid::nil(),
                created_by: Uuid::nil(),
                title: title.to_string(),
                description: None,
                location: None,
                starts_at: starts,
                ends_at: ends,
                all_day,
                is_task: false,
                completed_at: None,
                rrule: None,
                assignee_ids: vec![Uuid::nil()],
            },
            occurrence_starts_at: starts,
            occurrence_ends_at: ends,
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 3, hour, 0, 0).unwrap()
    }

    /// A zero-length occurrence at `hour` — the shape most of the ordering
    /// and capping cases below only need.
    fn occurrence(hour: u32, title: &str) -> OccurrenceResponse {
        occurrence_between(at(hour), at(hour), false, title)
    }

    /// An occurrence running from `start_hour` to `end_hour` (UTC, same day).
    fn occurrence_span(start_hour: u32, end_hour: u32, title: &str) -> OccurrenceResponse {
        occurrence_between(at(start_hour), at(end_hour), false, title)
    }

    /// A whole-day occurrence for 2026-09-03 in Paris (UTC+2 in September),
    /// i.e. 2026-09-02T22:00Z → 2026-09-03T22:00Z, and the reason a filter
    /// on `occurrence_starts_at` hid this whole class of entry — its start
    /// is in the past from one minute past midnight onwards.
    ///
    /// This block used to claim these bounds were "exactly what the API
    /// returns for a birthday or a public holiday". They were not, and #101
    /// is the bug that claim hid: until then nothing normalized an `all_day`
    /// event, so the app's own form stored whatever its two
    /// `datetime-local` fields held — 08:00 → 09:00 for a birthday added in
    /// the morning, which is *finished* by 09:01. The API now stores whole
    /// Paris civil days (`validation::agenda::normalize_all_day`), so the
    /// claim is true of anything created or edited through it — but still
    /// not of the Google Calendar mirror, which writes to `events` directly
    /// and anchors an all-day VEVENT on **UTC** midnight
    /// (`apps/api/src/google_calendar/parse.rs`), collapsing it to a
    /// zero-length instant when the feed carries no DTEND.
    fn all_day_occurrence(title: &str) -> OccurrenceResponse {
        occurrence_between(
            Utc.with_ymd_and_hms(2026, 9, 2, 22, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 9, 3, 22, 0, 0).unwrap(),
            true,
            title,
        )
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

    /// An all-day event today (birthday, holiday, school break) starts at
    /// Paris midnight, so it is *always* in the past by the time anyone
    /// loads the dashboard. Filtering on the start instant made the single
    /// most useful class of family entry structurally invisible — and left
    /// `agenda_row`'s "journée" branch dead for the current day (#98
    /// verification, round 2).
    #[test]
    fn an_all_day_event_today_is_still_upcoming_in_the_evening() {
        // 21:00 UTC = 23:00 Paris, the last hour of the same civil day.
        let now = at(21);
        let occs = vec![all_day_occurrence("Anniversaire de Camille")];
        let picked = soonest_occurrences(&occs, now, 5);
        assert_eq!(picked.len(), 1, "{picked:#?}");
        assert!(picked[0].event.all_day);
    }

    /// A dinner from 19:00 to 22:00 read at 19:01 has started but is not
    /// over: "Prochains événements" must still carry it, the way a paper
    /// agenda still shows the slot you are sitting in.
    #[test]
    fn an_event_in_progress_is_still_shown() {
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 19, 1, 0).unwrap();
        let occs = vec![occurrence_span(19, 22, "Dîner")];
        let picked = soonest_occurrences(&occs, now, 5);
        assert_eq!(picked.len(), 1, "{picked:#?}");
    }

    /// The symmetric guard: an event with a real duration that is genuinely
    /// over stays out. Without it, "filter on the end instant" would be
    /// satisfied by not filtering at all.
    #[test]
    fn an_event_that_ended_this_morning_is_not_shown() {
        let occs = vec![occurrence_span(8, 9, "Petit-déjeuner")];
        assert!(
            soonest_occurrences(&occs, late_afternoon(), 5).is_empty(),
            "an event over since 09:00 is not coming up at 17:16"
        );
    }

    /// The mix a real family day produces: something finished, something
    /// running, something later, and the all-day entry that covers all of
    /// them.
    #[test]
    fn a_realistic_day_keeps_exactly_the_all_day_the_running_and_the_later() {
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 19, 1, 0).unwrap();
        let occs = vec![
            occurrence_span(8, 9, "Petit-déjeuner"),
            occurrence_span(19, 22, "Dîner"),
            occurrence_span(21, 23, "Film"),
            all_day_occurrence("Jour férié"),
        ];
        let picked = soonest_occurrences(&occs, now, 5);
        let titles: Vec<&str> = picked.iter().map(|o| o.event.title.as_str()).collect();
        assert_eq!(titles, vec!["Jour férié", "Dîner", "Film"], "{picked:#?}");
    }

    // -- hidden_unread (#98 round-2, Mi3) ---------------------------------

    #[test]
    fn a_backend_without_the_total_hides_the_line() {
        assert_eq!(hidden_unread(5, None), 0);
    }

    #[test]
    fn a_page_holding_everything_unread_hides_the_line() {
        assert_eq!(hidden_unread(3, Some(3)), 0);
    }

    /// The case the card was silently swallowing: twenty unread, five shown.
    #[test]
    fn the_rest_of_the_unread_set_is_counted() {
        assert_eq!(hidden_unread(5, Some(20)), 15);
    }

    /// A total that lags its own page (a message landing between the two
    /// queries) must not underflow into a huge count.
    #[test]
    fn a_total_smaller_than_the_page_does_not_underflow() {
        assert_eq!(hidden_unread(5, Some(2)), 0);
        assert_eq!(hidden_unread(5, Some(-1)), 0);
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

    // -- row_assignee_ids / agenda_row (#99) -------------------------------

    fn member(user_id: Uuid, display_name: &str) -> GroupMember {
        GroupMember {
            user_id,
            role: "member".to_string(),
            display_name: display_name.to_string(),
            email: format!("{display_name}@example.test").to_lowercase(),
        }
    }

    /// An occurrence created by `created_by` and assigned to `assignees` —
    /// `assignees` empty is the shape every event predating #73 has in the
    /// database, since `0011_event_assignees.sql` created the table empty.
    fn occurrence_assigned_to(created_by: Uuid, assignees: &[Uuid]) -> OccurrenceResponse {
        let mut occ = occurrence(18, "Dîner");
        occ.event.created_by = created_by;
        occ.event.assignee_ids = assignees.to_vec();
        occ
    }

    #[test]
    fn an_event_with_assignees_names_exactly_them() {
        let creator = Uuid::from_u128(1);
        let assignees = [Uuid::from_u128(2), Uuid::from_u128(3)];
        assert_eq!(row_assignee_ids(&assignees, &creator), &assignees);
    }

    /// The #99 bug: `0011_event_assignees.sql` created the junction table
    /// empty, so every event that predates #73 comes back with no assignee
    /// at all. Without a fallback the dashboard row rendered a bare "?" ring
    /// and an em dash followed by nothing.
    #[test]
    fn an_event_with_no_assignee_falls_back_to_its_creator() {
        let creator = Uuid::from_u128(1);
        assert_eq!(row_assignee_ids(&[], &creator), &[creator]);
    }

    #[test]
    fn a_legacy_row_names_its_creator_instead_of_a_question_mark() {
        let creator = Uuid::from_u128(7);
        let members = vec![member(creator, "Camille")];
        let html = agenda_row(&occurrence_assigned_to(creator, &[]), &members);
        assert!(html.contains("Camille"), "{html}");
        assert!(html.contains(">C</span>"), "{html}");
        assert!(!html.contains('?'), "{html}");
        assert!(!html.contains("— </span>"), "{html}");
    }

    /// The fallback must not paint the ring with a colour that belongs to
    /// nobody: a legacy row is shown in its creator's own ramp token, the
    /// same one the event gets the moment someone re-edits it.
    #[test]
    fn a_legacy_row_wears_its_creator_s_own_colour() {
        let creator = Uuid::from_u128(7);
        let members = vec![member(creator, "Camille")];
        let legacy = agenda_row(&occurrence_assigned_to(creator, &[]), &members);
        let explicit = agenda_row(&occurrence_assigned_to(creator, &[creator]), &members);
        assert_eq!(legacy, explicit);
    }

    /// An event assigned to someone other than its creator keeps naming the
    /// assignee — the fallback only fires on an empty list.
    #[test]
    fn an_explicit_assignee_is_not_overridden_by_the_creator() {
        let creator = Uuid::from_u128(7);
        let other = Uuid::from_u128(8);
        let members = vec![member(creator, "Camille"), member(other, "Robin")];
        let html = agenda_row(&occurrence_assigned_to(creator, &[other]), &members);
        assert!(html.contains("Robin"), "{html}");
        assert!(!html.contains("Camille"), "{html}");
    }
}
