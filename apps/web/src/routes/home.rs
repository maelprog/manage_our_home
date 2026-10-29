use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use leptos::prelude::*;
use manage_our_home_shared::dto::agenda::OccurrenceResponse;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::budget::{BudgetPeriodTotal, BudgetSummary};
use manage_our_home_shared::dto::grocery_list::{GroceryItemList, GroceryItemResponse};
use manage_our_home_shared::dto::groups::{GroupDetailResponse, GroupMember, GroupSummary};
use manage_our_home_shared::dto::messagerie::{MessageList, MessageResponse};
use manage_our_home_shared::dto::stocks::{StockItemList, StockItemResponse};
use manage_our_home_shared::validation::budget::{can_access_budget, format_euros, month_name_fr};
use manage_our_home_shared::validation::messagerie::author_name;
use uuid::Uuid;

use crate::app::{
    avatar_photo, combined_member_colour, html_escape, member_colour, member_initial,
    member_photo_url, shell, shell_with_header, Width,
};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::icons::{icon, icon_link, Icon};
use crate::layout::CurrentUser;
use crate::routes::account::terms::update_notice;
use crate::routes::agenda::calendar::{render_chips, FR_WEEKDAYS, MONTH_CHIP_CAP};
use crate::routes::agenda::{
    fmt_paris, occurrences_from_response, paris_local_to_utc, read_or_log, today_paris, DISPLAY_TZ,
};
use crate::routes::groups::ownership::ownership_notices;
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::{api_request_auth, AppState};

/// How far ahead the agenda card reads: today plus this many civil days
/// (Paris). The card shows the next `DASHBOARD_AGENDA_CAP` occurrences, so
/// this is not what it displays but where it stops looking.
///
/// It used to be three days in all (#73), which turned "the next five" into
/// "the next five *before the day after tomorrow*": an event at J+3 was never
/// read, and the card said "nothing planned" (#455). Sixty days leaves room
/// for a sparse agenda while keeping a recurring series' unroll bounded — a
/// daily series costs at most sixty-two occurrences here: sixty-one civil
/// days, plus one when it sits on midnight or 23:59:59, since the API widens
/// an hour-bound window by a second either side (pinned by
/// `a_daily_series_at_paris_midnight_costs_the_home_horizon_62_occurrences`
/// in apps/api's `recurrence.rs`). The API also caps any series at its own
/// `MAX_OCCURRENCES` whatever the window.
const DASHBOARD_AGENDA_HORIZON_DAYS: i64 = 60;
/// The dashboard shows what's soonest, not everything the horizon holds —
/// the full picture is one click away on `/agenda`.
const DASHBOARD_AGENDA_CAP: usize = 5;
/// Same trade for stock: a taste of what's low, `/stocks?low_stock=1` for
/// the rest.
const DASHBOARD_STOCK_CAP: usize = 5;
/// The expiry tile counts what expires from today to this many days out:
/// seven days in all, the "cette semaine" of the mock-up (#524).
const DASHBOARD_EXPIRY_DAYS: i64 = 6;
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

/// The `[from, to]` instants the agenda card asks the API for: from Paris
/// midnight `today` to the last second of the Paris civil day
/// `DASHBOARD_AGENDA_HORIZON_DAYS` later — the same civil-day -> UTC
/// conversion `/agenda` uses, so a DST change inside the horizon lands on the
/// right offset. `None` only if a bound is not a valid Paris local time.
fn agenda_horizon(today: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let last_day = today + Duration::days(DASHBOARD_AGENDA_HORIZON_DAYS);
    Some((
        paris_local_to_utc(&format!("{today}T00:00"))?,
        paris_local_to_utc(&format!("{last_day}T23:59:59"))?,
    ))
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
/// before this is even called. `role` is the caller's in that family, which
/// decides the budget card (#531).
struct FamilyContext {
    gid: Uuid,
    role: String,
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
        role: g.role.clone(),
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
/// transitional measure. `0013_backfill_event_assignees.sql` repairs the
/// stored rows that exist at the moment it runs, and nothing else — it used
/// to repair none at all under the role `apps/api/README.md` prescribes for
/// `DATABASE_URL`, which #105 closed by moving migrations onto their own
/// `BYPASSRLS` role (see that migration's header) — and a migration runs
/// once either way, so it cannot reach an event created later with no
/// assignment. The Google Calendar import used to create exactly those, on
/// every deployment, on every import (issue #106); it now assigns the account
/// that ran the sync, and repairs the rows it wrote before. That closes the
/// one source that was still producing them, but it does not make this
/// fallback transitional: it is the last thing standing between a row whose
/// assignees failed to arrive — for any reason — and a bare "?" ring.
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

/// The Paris civil day an occurrence starts on.
fn paris_start_day(occ: &OccurrenceResponse) -> NaiveDate {
    occ.occurrence_starts_at
        .with_timezone(&DISPLAY_TZ)
        .date_naive()
}

/// The pill at the head of an agenda row (#524): what the eye looks for
/// first. Today's time, or "Journée"; for a later day its date — a weekday
/// name would need chrono's `%a`, which has no locale and would print
/// `Wed` in a French page; and "En cours" for an event begun on an earlier
/// day and not over, whose start time would say something false about
/// today.
fn when_label(occ: &OccurrenceResponse, today: NaiveDate) -> String {
    let day = paris_start_day(occ);
    if day > today {
        fmt_paris(occ.occurrence_starts_at, "%d/%m")
    } else if occ.event.all_day {
        "Journée".to_string()
    } else if day < today {
        "En cours".to_string()
    } else {
        fmt_paris(occ.occurrence_starts_at, "%H:%M")
    }
}

/// Splits the card's occurrences, already in start order, between today —
/// anything that started today or before and is not over, which is all
/// `soonest_occurrences` keeps — and the days after.
fn split_today(
    picked: Vec<&OccurrenceResponse>,
    today: NaiveDate,
) -> (Vec<&OccurrenceResponse>, Vec<&OccurrenceResponse>) {
    picked
        .into_iter()
        .partition(|o| paris_start_day(o) <= today)
}

/// One row of the "Aujourd'hui" card: the time in a pill, the title, who it
/// is for, and that member's avatar (their own colour for one assignee, the
/// running average for several — see `combined_member_colour`, and
/// `row_assignee_ids` for who counts as assigned on an event carrying no
/// assignee at all). The ring's letter is the first assignee's initial;
/// with several assignees the names under the title (not just the ring) are
/// what actually says who, matching WCAG 1.4.1 (colour is never the only
/// carrier — see `agenda/calendar.rs`'s doc comment on the same
/// constraint). A later day's pill holds its date, so its time moves to the
/// names' line.
///
/// The avatar is a flex item of the row, centred on it: the baseline gap
/// that lifted a photo 6–7 px above an initial in the inline avatar this
/// row used to carry (#468) has no line to sit on here.
fn agenda_row(occ: &OccurrenceResponse, today: NaiveDate, members: &[GroupMember]) -> String {
    let e = &occ.event;
    let shown = row_assignee_ids(&e.assignee_ids, &e.created_by);
    let assignee_names: Vec<&str> = shown.iter().map(|id| author_name(members, *id)).collect();
    let assignees = assignee_names.join(", ");
    let colour_tokens: Vec<&str> = shown.iter().copied().map(member_colour).collect();
    let colour = combined_member_colour(&colour_tokens);
    // `shown` is never empty, so this always names somebody; `member_initial`
    // keeps its own `?` for a name with no usable letter in it.
    let initial = member_initial(assignee_names.first().copied().unwrap_or_default());
    // #468: one assignee's photo, when they have one, takes the initial's
    // place. Several keep the initial: one photo would name only one of them.
    let icon = match shown {
        [only] if members.iter().any(|m| m.user_id == *only && m.has_avatar) => {
            avatar_photo(&member_photo_url(e.group_id, *only))
        }
        _ => html_escape(&initial),
    };
    let detail = if paris_start_day(occ) > today {
        let time = if e.all_day {
            "Journée".to_string()
        } else {
            fmt_paris(occ.occurrence_starts_at, "%H:%M")
        };
        format!("{time} · {assignees}")
    } else {
        assignees
    };
    format!(
        r#"<li class="list-row"><span class="when">{when}</span><span class="what"><strong>{title}</strong><span class="muted">{detail}</span></span><span class="avatar" style="color:{colour}" aria-hidden="true">{icon}</span></li>"#,
        when = html_escape(&when_label(occ, today)),
        title = html_escape(&e.title),
        detail = html_escape(&detail),
    )
}

/// A card's head: its title, and the link that leads to the whole of it —
/// `.more`, 48px (--s12), over the 44px of any target (DESIGN.md → Espacement).
fn card_head(title: &str, href: &str, label: &str) -> String {
    format!(
        r#"<div class="split"><h2>{title}</h2><a class="more" href="{href}">{label}</a></div>"#,
        title = html_escape(title),
        label = html_escape(label),
    )
}

fn agenda_list(rows: &[&OccurrenceResponse], today: NaiveDate, members: &[GroupMember]) -> String {
    let rows: String = rows.iter().map(|o| agenda_row(o, today, members)).collect();
    format!(r#"<ul class="list">{rows}</ul>"#)
}

/// The "Aujourd'hui" card of the mock-up (#524). It still reads the next
/// `DASHBOARD_AGENDA_CAP` occurrences of the horizon, as the card it
/// replaces did, and lists those of the days after under "Ensuite": a phone
/// has no week view, and a card that only knew today would say nothing of
/// an appointment on Tuesday — the gap #455 closed.
///
/// `occurrences` is `None` when the API's answer could not be read: that
/// renders as an error, never as the "nothing planned" an empty list means
/// (#455).
fn agenda_block(
    occurrences: Option<&[OccurrenceResponse]>,
    now: DateTime<Utc>,
    today: NaiveDate,
    members: &[GroupMember],
) -> String {
    let body = match occurrences {
        None => {
            r#"<p class="notice error">Impossible de charger les événements pour le moment.</p>"#
                .to_string()
        }
        Some(occurrences) => {
            let picked = soonest_occurrences(occurrences, now, DASHBOARD_AGENDA_CAP);
            if picked.is_empty() {
                format!(
                    r#"<p class="muted">Rien de prévu dans les {DASHBOARD_AGENDA_HORIZON_DAYS} prochains jours.</p>"#
                )
            } else {
                let (now_rows, later) = split_today(picked, today);
                let now_html = if now_rows.is_empty() {
                    r#"<p class="muted">Rien aujourd'hui.</p>"#.to_string()
                } else {
                    agenda_list(&now_rows, today, members)
                };
                let later_html = if later.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n<h3>Ensuite</h3>\n{}",
                        agenda_list(&later, today, members)
                    )
                };
                format!("{now_html}{later_html}")
            }
        }
    };
    format!(
        "<section class=\"card\">\n{head}\n{body}\n</section>",
        head = card_head("Aujourd'hui", "/agenda", "Agenda"),
    )
}

/// The seven days the PC's week card shows (#524): today and the six after
/// it. Not Monday to Sunday as the mock-up draws them: the card reads the
/// same horizon as "Aujourd'hui", which starts at today's midnight, and
/// days already gone would show what that read never asked for.
fn week_days(today: NaiveDate) -> Vec<NaiveDate> {
    (0..7).map(|d| today + Duration::days(d)).collect()
}

/// The week card, drawn on a PC only (`.pc`): one column a day, each
/// occurrence a chip of the agenda (`agenda::calendar::render_chips`,
/// capped at the month grid's three rows, the last one counting the rest
/// and leading to the week view). Today's column wears `.current` and says
/// "aujourd'hui" in words — the tint is never the only carrier.
///
/// `None` — the agenda could not be read — draws nothing: the
/// "Aujourd'hui" card already says so.
fn week_block(occurrences: Option<&[OccurrenceResponse]>, today: NaiveDate) -> String {
    let Some(occurrences) = occurrences else {
        return String::new();
    };
    let days = week_days(today);
    let mut by_day: BTreeMap<NaiveDate, Vec<&OccurrenceResponse>> = BTreeMap::new();
    for occ in occurrences {
        let day = paris_start_day(occ);
        if days.contains(&day) {
            by_day.entry(day).or_default().push(occ);
        }
    }
    let body = if by_day.is_empty() {
        r#"<p class="muted">Rien de prévu ces sept prochains jours.</p>"#.to_string()
    } else {
        let columns: String = days
            .iter()
            .map(|day| {
                let mut occs = by_day.remove(day).unwrap_or_default();
                occs.sort_by_key(|o| o.occurrence_starts_at);
                let weekday = FR_WEEKDAYS[day.weekday().num_days_from_monday() as usize];
                let (class, note) = if *day == today {
                    (r#" class="current""#, " · aujourd'hui")
                } else {
                    ("", "")
                };
                format!(
                    r#"<li{class}><p class="eyebrow">{weekday} {num}{note}</p>{chips}</li>"#,
                    num = day.day(),
                    chips = render_chips(&occs, Some(MONTH_CHIP_CAP), *day, &[]),
                )
            })
            .collect();
        format!(r#"<ol class="days">{columns}</ol>"#)
    };
    format!(
        "<section class=\"card pc\">\n{head}\n{body}\n</section>",
        head = card_head(
            "Les sept prochains jours",
            "/agenda?view=week",
            "Ouvrir l'agenda"
        ),
    )
}

/// The date over the greeting, lower case as French writes it — the sheet
/// sets it in capitals (`.eyebrow`): "mercredi 28 octobre", "dimanche 1er
/// novembre".
fn long_date_fr(day: NaiveDate) -> String {
    const WEEKDAYS: [&str; 7] = [
        "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
    ];
    let weekday = WEEKDAYS[day.weekday().num_days_from_monday() as usize];
    let num = match day.day() {
        1 => "1er".to_string(),
        n => n.to_string(),
    };
    format!("{weekday} {num} {}", month_name_fr(day.month()))
}

/// The head of the page: the date and "Bonjour", and on a PC the two
/// actions the mock-up puts beside them. A phone has the floating button
/// instead (`add_event_button`), and its grocery tile leads to the list.
fn greeting(today: NaiveDate) -> String {
    format!(
        r#"<div class="page-header"><hgroup><p class="eyebrow">{date}</p><h1>Bonjour</h1></hgroup><div class="actions pc"><a class="btn secondary" href="/grocery-list">Ajouter aux courses</a><a class="btn" href="/agenda/new">{plus} Nouvel événement</a></div></div>"#,
        date = html_escape(&long_date_fr(today)),
        plus = icon(Icon::Plus),
    )
}

/// The phone's floating "+" (#524), above the tab bar: an icon alone, so
/// the link is named by its label (`icon_link`). `.phone`: a PC has
/// "Nouvel événement" in the page's head.
fn add_event_button() -> String {
    icon_link(
        "btn fab phone",
        "/agenda/new",
        "Ajouter un événement",
        Icon::Plus,
    )
}

/// The grocery tile: what is left to buy, and a gauge of what is already
/// ticked off out of the whole list. The gauge is `aria-hidden` — a
/// `<progress>` inside a link would be read as a second control — and the
/// line under it says the same in words. An empty list has neither.
fn grocery_tile(unchecked: usize, total: usize) -> String {
    let label = if unchecked > 1 {
        "articles à acheter"
    } else {
        "article à acheter"
    };
    let gauge = if total == 0 {
        String::new()
    } else {
        let ticked = total.saturating_sub(unchecked);
        format!(
            r#"<progress max="{total}" value="{ticked}" aria-hidden="true"></progress><span class="muted">{ticked} sur {total} cochés</span>"#
        )
    };
    format!(
        r#"<a class="card tile" href="/grocery-list">{cart}<strong>{unchecked}</strong><span class="muted">{label}</span>{gauge}</a>"#,
        cart = icon(Icon::Groceries),
    )
}

/// What the stock holds that expires this week: today to six days out,
/// soonest first. Already past is not "this week" — the stock page shows
/// it as expired — and an article with no date is not counted.
fn expiring_this_week(items: &[StockItemResponse], today: NaiveDate) -> Vec<&StockItemResponse> {
    let last = today + Duration::days(DASHBOARD_EXPIRY_DAYS);
    let mut out: Vec<&StockItemResponse> = items
        .iter()
        .filter(|i| i.expires_on.is_some_and(|d| today <= d && d <= last))
        .collect();
    out.sort_by_key(|i| i.expires_on);
    out
}

/// The expiry tile, in the warning tint: how many articles expire this
/// week, leading to the stock sorted by date ("à consommer en premier").
fn expiry_tile(expiring: &[&StockItemResponse]) -> String {
    let label = match expiring.len() {
        0 => "rien ne périme cette semaine",
        1 => "produit périme cette semaine",
        _ => "produits périment cette semaine",
    };
    format!(
        r#"<a class="card tile warn" href="/stocks?sort=expires_on">{clock}<strong>{count}</strong><span>{label}</span></a>"#,
        clock = icon(Icon::Clock),
        count = expiring.len(),
    )
}

/// The articles at or under their threshold, in the order the API lists
/// the stock (by name). One read of the stock feeds both this card and the
/// expiry tile.
fn low_stock_items(items: &[StockItemResponse]) -> Vec<&StockItemResponse> {
    items.iter().filter(|i| i.low_stock).collect()
}

fn stocks_block(items: &[&StockItemResponse]) -> String {
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
        "<section class=\"card\">\n{head}\n{body}\n</section>",
        head = card_head("Stock bas", "/stocks?low_stock=1", "Voir le stock"),
    )
}

/// "Budget d'octobre", "Budget de mars": French elides `de` before a vowel,
/// and three months start with one.
fn budget_title(month: u32) -> String {
    let name = month_name_fr(month);
    if name.starts_with(['a', 'o']) {
        format!("Budget d'{name}")
    } else {
        format!("Budget de {name}")
    }
}

/// This month's spend, for an owner or an admin: a standard member's home
/// page has no budget card (#531), and its summary is not even asked for.
fn budget_block(total: Option<f64>, today: NaiveDate) -> String {
    let body = match total {
        Some(total) => format!(
            r#"<p><strong>{amount}</strong> dépensés.</p>"#,
            amount = html_escape(&format_euros(total)),
        ),
        None => r#"<p class="muted">Aucune dépense enregistrée ce mois-ci.</p>"#.to_string(),
    };
    format!(
        "<section class=\"card\">\n{head}\n{body}\n</section>",
        head = card_head(&budget_title(today.month()), "/budget", "Voir le budget"),
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
/// distinction `grocery_tile` draws between zero-unchecked and an empty
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
        "<section class=\"card\">\n{head}\n{body}\n</section>",
        head = card_head("Messages non lus", "/messagerie", "Voir la messagerie"),
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
/// Since #524 it is laid out as the mock-ups draw it: the date and
/// "Bonjour", the PC's week, then the cards — "Aujourd'hui", the grocery
/// and expiry tiles, the budget, the low stock and the unread chat. The
/// week and "Aujourd'hui" are two readings of the one agenda call, and the
/// low-stock card and the expiry tile two readings of the one stock call.
///
/// Each domain is one more API call to apps/api at render time — this app is
/// SSR-only, with no client-side hydration to defer work to (see
/// `Cargo.toml`'s doc comment and the issue's own "Attention" note). Five
/// domain calls plus one shared members lookup (for the agenda/messages
/// author names) is the accepted cost; if that ever needs trimming, the fix
/// is fewer cards, not async loading.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // #462: where the password login, Google's callback and the age and
    // terms pages land — so where a visitor turned away from an invitation
    // before having a session is sent back to it, once. A cookie that names
    // no allowed page is cleared in the answer that shows the home page:
    // never by sending back to `/`, which looped on a cookie the clearing
    // did not reach (#576).
    let cookies = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok());
    let Some(value) = crate::return_to::return_cookie(cookies) else {
        return dashboard(me, state, headers).await;
    };
    let clear = (
        axum::http::header::SET_COOKIE,
        crate::return_to::clear_return_cookie(),
    );
    match crate::return_to::safe_return_path(value) {
        Some(to) => ([clear], Redirect::to(to)).into_response(),
        None => ([clear], dashboard(me, state, headers).await).into_response(),
    }
}

/// The home page itself, once `get` has dealt with the return cookie.
async fn dashboard(me: MeResponse, state: AppState, headers: HeaderMap) -> Response {
    let (fam, groups) = family_context(&state, &headers, &me).await;

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
            <p>"Vous êtes connecté."</p>
            {no_group_hint}
        };
        return Html(shell_with_header(
            Width::Read,
            "Accueil",
            &header,
            &format!(
                "<h1>Bienvenue</h1>\n{terms}{body}",
                terms = update_notice(&me),
                body = body.to_html(),
            ),
        ))
        .into_response();
    };

    let cookie = cookie_of(&headers);
    let today = today_paris();

    let Some((from, to)) = agenda_horizon(today) else {
        return service_unavailable_page().into_response();
    };
    // `None`: the answer could not be read, which the card shows as an error
    // rather than as an empty agenda (#455).
    let occurrences: Option<Vec<OccurrenceResponse>> = match api_request_auth(
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
        Ok(resp) => read_or_log(
            occurrences_from_response(resp.status, resp.body),
            fam.gid,
            "home",
        ),
        Err(_) => return service_unavailable_page().into_response(),
    };

    // The whole stock, once: the low-stock card and the expiry tile are two
    // readings of it (#524), where the card alone used to ask the API for
    // `?low_stock=true`.
    let stock: Vec<StockItemResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/stock-items", fam.gid),
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

    let budget = if can_access_budget(&fam.role) {
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
        budget_block(current_month_total(&budget_summary.periods, today), today)
    } else {
        String::new()
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
        r#"{greeting}
{terms}{ownership}{week}
<div class="dash">
{agenda}
{grocery}
{expiry}
{budget}
{stocks}
{messages}
</div>
{add}"#,
        greeting = greeting(today),
        // #319: a member who accepted an earlier version of the CGU is told
        // here, first thing after logging in, until they acknowledge it.
        terms = update_notice(&me),
        // #323: a group that came to the member without their asking, until
        // they acknowledge it — from the same `GET /groups` the header uses.
        ownership = ownership_notices(&groups),
        week = week_block(occurrences.as_deref(), today),
        agenda = agenda_block(occurrences.as_deref(), chrono::Utc::now(), today, &members),
        grocery = grocery_tile(count_unchecked(&grocery_items), grocery_items.len()),
        expiry = expiry_tile(&expiring_this_week(&stock, today)),
        stocks = stocks_block(&low_stock_items(&stock)),
        messages = messages_block(&messages.messages, messages.unread_total, &members),
        add = add_event_button(),
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
    logout_to(&state, &headers, "/login").await
}

/// [`logout`]'s work, landing on `to`: the `/admin` pages send a superadmin
/// whose session the admin routes no longer accept to `/login` with a
/// notice (#226).
pub(crate) async fn logout_to(state: &AppState, headers: &HeaderMap, to: &str) -> Response {
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
    (response_headers, Redirect::to(to)).into_response()
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

    /// The use case behind #455: the family's next event is ten days out,
    /// with nothing before it but one appointment tomorrow. Both are among
    /// the next five, so both are picked — the selection has no day bound of
    /// its own, only the cap.
    #[test]
    fn an_occurrence_ten_days_out_is_picked_when_it_is_among_the_next_five() {
        let now = late_afternoon();
        let day = |d: i64| now + Duration::days(d);
        let occs = vec![
            occurrence_between(day(10), day(10) + Duration::hours(1), false, "J+10"),
            occurrence_between(day(1), day(1) + Duration::hours(1), false, "J+1"),
        ];
        let picked = soonest_occurrences(&occs, now, 5);
        let titles: Vec<&str> = picked.iter().map(|o| o.event.title.as_str()).collect();
        assert_eq!(titles, ["J+1", "J+10"]);
    }

    /// And when five occurrences come first, the far one is the one cut.
    #[test]
    fn a_far_occurrence_gives_way_to_five_nearer_ones() {
        let now = late_afternoon();
        let mut occs: Vec<OccurrenceResponse> = (1..=5)
            .map(|d| {
                let s = now + Duration::days(d);
                occurrence_between(s, s + Duration::hours(1), false, &format!("J+{d}"))
            })
            .collect();
        let far = now + Duration::days(10);
        occs.push(occurrence_between(
            far,
            far + Duration::hours(1),
            false,
            "J+10",
        ));
        let picked = soonest_occurrences(&occs, now, 5);
        assert_eq!(picked.len(), 5);
        assert!(picked.iter().all(|o| o.event.title != "J+10"));
    }

    // -- agenda_horizon (#455) ------------------------------------------

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Starts at Paris midnight today: this morning's events are still read,
    /// `soonest_occurrences` is what drops the finished ones.
    #[test]
    fn the_horizon_starts_at_paris_midnight_today() {
        let (from, _) = agenda_horizon(ymd(2026, 9, 3)).unwrap();
        assert_eq!(from, Utc.with_ymd_and_hms(2026, 9, 2, 22, 0, 0).unwrap());
    }

    /// Ends on the last second of the Paris day `HORIZON_DAYS` later — here
    /// across the October DST change, so on the winter offset (UTC+1).
    #[test]
    fn the_horizon_ends_on_the_last_second_of_its_last_paris_day() {
        let today = ymd(2026, 10, 14);
        let last_day = today + Duration::days(DASHBOARD_AGENDA_HORIZON_DAYS);
        assert_eq!(last_day, ymd(2026, 12, 13));
        let (_, to) = agenda_horizon(today).unwrap();
        assert_eq!(to, Utc.with_ymd_and_hms(2026, 12, 13, 22, 59, 59).unwrap());
    }

    /// The acceptance criterion of #455: an event ten days out is read at
    /// all — the old three-day window never asked the API for it.
    #[test]
    fn an_event_ten_days_out_falls_inside_the_horizon() {
        let today = ymd(2026, 10, 14);
        let (from, to) = agenda_horizon(today).unwrap();
        let ten_days_out = Utc.with_ymd_and_hms(2026, 10, 24, 18, 0, 0).unwrap();
        assert!(from <= ten_days_out && ten_days_out <= to);
    }

    // -- agenda_block (#455) --------------------------------------------

    /// An API failure is not "nothing planned".
    #[test]
    fn an_unreadable_agenda_is_an_error_not_an_empty_card() {
        let html = agenda_block(None, late_afternoon(), fixture_day(), &[]);
        assert!(!html.contains("Rien de prévu"), "{html}");
        assert!(html.contains(r#"class="notice error""#), "{html}");
        assert!(html.contains("Impossible de charger"), "{html}");
    }

    /// The empty copy names the horizon it actually searched.
    #[test]
    fn an_empty_horizon_says_how_far_it_looked() {
        let html = agenda_block(Some(&[]), late_afternoon(), fixture_day(), &[]);
        assert!(
            html.contains(&format!(
                "Rien de prévu dans les {DASHBOARD_AGENDA_HORIZON_DAYS} prochains jours."
            )),
            "{html}"
        );
        assert!(!html.contains("notice error"), "{html}");
    }

    /// Everything in the list is over: the card is empty, not in error.
    #[test]
    fn a_horizon_holding_only_finished_events_is_empty_not_an_error() {
        let occs = vec![occurrence_span(8, 9, "Ce matin")];
        let html = agenda_block(Some(&occs), late_afternoon(), fixture_day(), &[]);
        assert!(html.contains("Rien de prévu"), "{html}");
        assert!(!html.contains("notice error"), "{html}");
    }

    /// The exact shape of the verification finding on #98: six occurrences
    /// this morning (08h-13h), `now` at 17h16 — every one of them is
    /// already over, so none should be picked, even though the API window
    /// (Paris midnight .. `DASHBOARD_AGENDA_HORIZON_DAYS` later) still
    /// returns all six.
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
            has_avatar: false,
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
        let html = agenda_row(
            &occurrence_assigned_to(creator, &[]),
            fixture_day(),
            &members,
        );
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
        let legacy = agenda_row(
            &occurrence_assigned_to(creator, &[]),
            fixture_day(),
            &members,
        );
        let explicit = agenda_row(
            &occurrence_assigned_to(creator, &[creator]),
            fixture_day(),
            &members,
        );
        assert_eq!(legacy, explicit);
    }

    /// An event assigned to someone other than its creator keeps naming the
    /// assignee — the fallback only fires on an empty list.
    #[test]
    fn an_explicit_assignee_is_not_overridden_by_the_creator() {
        let creator = Uuid::from_u128(7);
        let other = Uuid::from_u128(8);
        let members = vec![member(creator, "Camille"), member(other, "Robin")];
        let html = agenda_row(
            &occurrence_assigned_to(creator, &[other]),
            fixture_day(),
            &members,
        );
        assert!(html.contains("Robin"), "{html}");
        assert!(!html.contains("Camille"), "{html}");
    }

    /// #468: a row assigned to one member who has a photo shows it in place
    /// of their initial, in their own hue.
    #[test]
    fn a_row_with_one_assignee_shows_their_photo() {
        let robin = Uuid::from_u128(8);
        let mut with_photo = member(robin, "Robin");
        with_photo.has_avatar = true;
        let occ = occurrence_assigned_to(robin, &[robin]);
        let html = agenda_row(&occ, fixture_day(), &[with_photo]);
        let photo = crate::app::member_photo_url(occ.event.group_id, robin);
        assert!(html.contains(&crate::app::avatar_photo(&photo)), "{html}");
        assert!(!html.contains(">R</span>"), "{html}");
        assert!(
            html.contains(&format!("color:var({})", member_colour(robin))),
            "{html}"
        );
    }

    /// Several assignees keep the mixed hue and the first one's initial:
    /// one member's photo would name only one of them.
    #[test]
    fn a_row_with_several_assignees_keeps_the_initial() {
        let (camille, robin) = (Uuid::from_u128(7), Uuid::from_u128(8));
        let mut with_photo = member(robin, "Robin");
        with_photo.has_avatar = true;
        let members = vec![member(camille, "Camille"), with_photo];
        let html = agenda_row(
            &occurrence_assigned_to(camille, &[robin, camille]),
            fixture_day(),
            &members,
        );
        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains(">R</span>"), "{html}");
    }

    // -- the mock-up's home (#524) -----------------------------------------

    /// 2026-09-03, the day the fixtures above live on (UTC+2 in Paris).
    fn fixture_day() -> NaiveDate {
        ymd(2026, 9, 3)
    }

    #[test]
    fn the_date_line_names_the_weekday_and_the_month_in_french() {
        assert_eq!(long_date_fr(ymd(2026, 10, 28)), "mercredi 28 octobre");
        assert_eq!(long_date_fr(ymd(2026, 10, 26)), "lundi 26 octobre");
        assert_eq!(long_date_fr(ymd(2026, 11, 8)), "dimanche 8 novembre");
    }

    /// French writes the first of the month as an ordinal.
    #[test]
    fn the_first_of_the_month_is_written_1er() {
        assert_eq!(long_date_fr(ymd(2026, 11, 1)), "dimanche 1er novembre");
    }

    #[test]
    fn the_greeting_carries_the_date_the_heading_and_the_pc_actions() {
        let html = greeting(ymd(2026, 10, 28));
        assert!(
            html.contains(r#"<p class="eyebrow">mercredi 28 octobre</p>"#),
            "{html}"
        );
        assert!(html.contains("<h1>Bonjour</h1>"), "{html}");
        // The PC draws two buttons beside the greeting; the phone has the
        // floating one instead.
        assert!(html.contains(r#"class="actions pc""#), "{html}");
        assert!(html.contains(r#"href="/agenda/new""#), "{html}");
        assert!(html.contains("Nouvel événement"), "{html}");
        assert!(html.contains(r#"href="/grocery-list""#), "{html}");
    }

    #[test]
    fn the_phone_gets_a_floating_button_that_adds_an_event() {
        let html = add_event_button();
        assert!(html.contains(r#"class="btn fab phone""#), "{html}");
        assert!(html.contains(r#"href="/agenda/new""#), "{html}");
        assert!(
            html.contains(r#"aria-label="Ajouter un événement""#),
            "{html}"
        );
    }

    // -- the "Aujourd'hui" card ------------------------------------------

    /// An occurrence of `title` starting at `starts`, one hour long.
    fn timed_at(starts: DateTime<Utc>, title: &str) -> OccurrenceResponse {
        occurrence_between(starts, starts + Duration::hours(1), false, title)
    }

    #[test]
    fn a_timed_event_today_shows_its_paris_time() {
        // 08:15 UTC is 10:15 in Paris in September.
        let occ = timed_at(Utc.with_ymd_and_hms(2026, 9, 3, 8, 15, 0).unwrap(), "École");
        assert_eq!(when_label(&occ, fixture_day()), "10:15");
    }

    #[test]
    fn an_all_day_event_today_says_journee() {
        assert_eq!(
            when_label(&all_day_occurrence("Anniversaire"), fixture_day()),
            "Journée"
        );
    }

    #[test]
    fn a_later_event_shows_its_paris_date() {
        let occ = timed_at(Utc.with_ymd_and_hms(2026, 9, 8, 16, 0, 0).unwrap(), "Judo");
        assert_eq!(when_label(&occ, fixture_day()), "08/09");
    }

    /// Started on an earlier day and not over: it belongs to today, but its
    /// start time would say something false about today.
    #[test]
    fn an_event_begun_on_an_earlier_day_is_en_cours() {
        let occ = occurrence_between(
            Utc.with_ymd_and_hms(2026, 9, 1, 8, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 9, 4, 8, 0, 0).unwrap(),
            false,
            "Séjour",
        );
        assert_eq!(when_label(&occ, fixture_day()), "En cours");
    }

    #[test]
    fn today_takes_what_starts_today_or_before_and_later_the_rest() {
        let early = timed_at(
            Utc.with_ymd_and_hms(2026, 9, 3, 8, 0, 0).unwrap(),
            "Ce matin",
        );
        // 22:30 UTC on the 3rd is 00:30 on the 4th in Paris: tomorrow.
        let tomorrow = timed_at(
            Utc.with_ymd_and_hms(2026, 9, 3, 22, 30, 0).unwrap(),
            "Demain",
        );
        let birthday = all_day_occurrence("Anniversaire");
        let picked = vec![&birthday, &early, &tomorrow];
        let (today, later) = split_today(picked, fixture_day());
        let titles = |v: &[&OccurrenceResponse]| -> Vec<String> {
            v.iter().map(|o| o.event.title.clone()).collect()
        };
        assert_eq!(titles(&today), ["Anniversaire", "Ce matin"]);
        assert_eq!(titles(&later), ["Demain"]);
    }

    #[test]
    fn the_agenda_card_is_titled_today() {
        let html = agenda_block(Some(&[]), late_afternoon(), fixture_day(), &[]);
        assert!(html.contains("<h2>Aujourd'hui</h2>"), "{html}");
        assert!(html.contains(r#"href="/agenda""#), "{html}");
    }

    #[test]
    fn a_row_puts_its_time_in_a_pill_and_names_who_it_is_for() {
        let camille = Uuid::from_u128(7);
        let mut occ = occurrence_assigned_to(camille, &[camille]);
        occ.event.title = "Piscine".to_string();
        let html = agenda_row(&occ, fixture_day(), &[member(camille, "Camille")]);
        assert!(
            html.contains(r#"<span class="when">20:00</span>"#),
            "{html}"
        );
        assert!(html.contains("<strong>Piscine</strong>"), "{html}");
        assert!(
            html.contains(r#"<span class="muted">Camille</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"class="avatar""#), "{html}");
    }

    /// A later row's pill holds its date, so its time moves to the line
    /// under the title, beside who it is for.
    #[test]
    fn a_later_row_says_its_time_under_the_title() {
        let camille = Uuid::from_u128(7);
        let mut occ = occurrence_assigned_to(camille, &[camille]);
        let starts = Utc.with_ymd_and_hms(2026, 9, 8, 16, 0, 0).unwrap();
        occ.occurrence_starts_at = starts;
        occ.occurrence_ends_at = starts + Duration::hours(1);
        let html = agenda_row(&occ, fixture_day(), &[member(camille, "Camille")]);
        assert!(
            html.contains(r#"<span class="when">08/09</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="muted">18:00 · Camille</span>"#),
            "{html}"
        );
    }

    #[test]
    fn events_after_today_are_listed_under_ensuite() {
        let now = late_afternoon();
        let tonight = timed_at(Utc.with_ymd_and_hms(2026, 9, 3, 18, 0, 0).unwrap(), "Dîner");
        let later = timed_at(now + Duration::days(5), "Contrôle technique");
        let html = agenda_block(Some(&[later, tonight]), now, fixture_day(), &[]);
        let (head, tail) = html.split_once("<h3>Ensuite</h3>").expect(&html);
        assert!(head.contains("Dîner"), "{html}");
        assert!(tail.contains("Contrôle technique"), "{html}");
        assert!(!html.contains("Rien"), "{html}");
    }

    /// Nothing left today, something on Tuesday: the card says so, and
    /// still shows Tuesday — the concern of #455.
    #[test]
    fn an_empty_today_says_so_and_still_shows_what_comes_next() {
        let now = late_afternoon();
        let later = timed_at(now + Duration::days(5), "Contrôle technique");
        let html = agenda_block(Some(&[later]), now, fixture_day(), &[]);
        assert!(
            html.contains(r#"<p class="muted">Rien aujourd'hui.</p>"#),
            "{html}"
        );
        assert!(html.contains("<h3>Ensuite</h3>"), "{html}");
        assert!(html.contains("Contrôle technique"), "{html}");
        assert!(!html.contains("Rien de prévu"), "{html}");
    }

    #[test]
    fn a_today_with_nothing_after_it_has_no_ensuite_heading() {
        let tonight = timed_at(Utc.with_ymd_and_hms(2026, 9, 3, 18, 0, 0).unwrap(), "Dîner");
        let html = agenda_block(Some(&[tonight]), late_afternoon(), fixture_day(), &[]);
        assert!(!html.contains("Ensuite"), "{html}");
        assert!(!html.contains("Rien"), "{html}");
    }

    // -- the week (PC) -----------------------------------------------------

    #[test]
    fn the_week_is_today_and_the_six_days_after_it() {
        let days = week_days(ymd(2026, 10, 28));
        assert_eq!(days.len(), 7);
        assert_eq!(days[0], ymd(2026, 10, 28));
        assert_eq!(days[6], ymd(2026, 11, 3));
    }

    #[test]
    fn the_week_is_drawn_on_a_pc_only_with_today_marked_in_words() {
        let tonight = timed_at(Utc.with_ymd_and_hms(2026, 9, 3, 18, 0, 0).unwrap(), "Dîner");
        let html = week_block(Some(&[tonight]), fixture_day());
        assert!(html.contains(r#"<section class="card pc">"#), "{html}");
        assert_eq!(html.matches("<li").count(), 7, "{html}");
        assert!(
            html.contains(r#"<li class="current"><p class="eyebrow">Jeu 3 · aujourd'hui</p>"#),
            "{html}"
        );
        assert!(html.contains(r#"<p class="eyebrow">Mer 9</p>"#), "{html}");
        assert!(html.contains(r#"class="chip""#), "{html}");
        assert!(html.contains("Dîner"), "{html}");
    }

    /// Each occurrence lands on its own Paris day: 22:30 UTC on the 3rd is
    /// the 4th in Paris.
    #[test]
    fn an_occurrence_lands_on_its_paris_day() {
        let late = timed_at(
            Utc.with_ymd_and_hms(2026, 9, 3, 22, 30, 0).unwrap(),
            "Minuit et demi",
        );
        let html = week_block(Some(&[late]), fixture_day());
        let (_, friday) = html.split_once("Ven 4").expect(&html);
        let friday = friday.split("</li>").next().unwrap();
        assert!(friday.contains("Minuit et demi"), "{html}");
    }

    /// A day holds three rows, the third counting the rest, as the agenda's
    /// month grid does.
    #[test]
    fn a_crowded_day_counts_what_it_does_not_list() {
        let occs: Vec<OccurrenceResponse> = (10..15).map(|h| occurrence(h, "Réunion")).collect();
        let html = week_block(Some(&occs), fixture_day());
        assert_eq!(html.matches("Réunion").count(), 2, "{html}");
        assert!(html.contains("+3 autres"), "{html}");
    }

    #[test]
    fn an_empty_week_says_so_instead_of_seven_empty_columns() {
        let html = week_block(Some(&[]), fixture_day());
        assert!(
            html.contains("Rien de prévu ces sept prochains jours."),
            "{html}"
        );
        assert!(!html.contains("<li"), "{html}");
    }

    /// The agenda's horizon reaches past the week: an occurrence ten days
    /// out belongs to "Aujourd'hui", not to any of the seven columns.
    #[test]
    fn an_occurrence_past_the_seventh_day_leaves_the_week_empty() {
        let later = timed_at(
            Utc.with_ymd_and_hms(2026, 9, 13, 10, 0, 0).unwrap(),
            "Plus tard",
        );
        let html = week_block(Some(&[later]), fixture_day());
        assert!(
            html.contains("Rien de prévu ces sept prochains jours."),
            "{html}"
        );
        assert!(!html.contains("Plus tard"), "{html}");
        assert!(!html.contains("<li"), "{html}");
    }

    /// The agenda card already says the read failed; the week says nothing.
    #[test]
    fn an_unreadable_agenda_draws_no_week() {
        assert_eq!(week_block(None, fixture_day()), "");
    }

    // -- the tiles ---------------------------------------------------------

    #[test]
    fn the_grocery_tile_counts_what_is_left_and_gauges_what_is_ticked() {
        let html = grocery_tile(3, 5);
        assert!(
            html.contains(r#"<a class="card tile" href="/grocery-list">"#),
            "{html}"
        );
        assert!(html.contains("<strong>3</strong>"), "{html}");
        assert!(html.contains("articles à acheter"), "{html}");
        assert!(
            html.contains(r#"<progress max="5" value="2" aria-hidden="true"></progress>"#),
            "{html}"
        );
        // The gauge is drawn for the eye; the words say it for everyone.
        assert!(html.contains("2 sur 5 cochés"), "{html}");
    }

    #[test]
    fn one_article_left_is_singular() {
        let html = grocery_tile(1, 1);
        assert!(html.contains("article à acheter"), "{html}");
        assert!(!html.contains("articles"), "{html}");
    }

    #[test]
    fn an_empty_list_has_no_gauge() {
        let html = grocery_tile(0, 0);
        assert!(html.contains("<strong>0</strong>"), "{html}");
        assert!(html.contains("article à acheter"), "{html}");
        assert!(!html.contains("<progress"), "{html}");
        assert!(!html.contains("sur 0"), "{html}");
    }

    fn stock(name: &str, low: bool, expires_on: Option<NaiveDate>) -> StockItemResponse {
        StockItemResponse {
            id: Uuid::nil(),
            group_id: Uuid::nil(),
            created_by: Uuid::nil(),
            name: name.to_string(),
            category: None,
            quantity: 1.0,
            unit: "u".to_string(),
            reorder_threshold: None,
            low_stock: low,
            expires_on,
            expiry_status: manage_our_home_shared::validation::stocks::ExpiryStatus::Unknown,
            barcode: None,
        }
    }

    #[test]
    fn what_expires_this_week_is_today_to_six_days_out_soonest_first() {
        let today = fixture_day();
        let items = vec![
            stock("J+6", false, Some(today + Duration::days(6))),
            stock("Périmé", false, Some(today - Duration::days(1))),
            stock("Aujourd'hui", false, Some(today)),
            stock("J+7", false, Some(today + Duration::days(7))),
            stock("Sans date", false, None),
        ];
        let names: Vec<&str> = expiring_this_week(&items, today)
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, ["Aujourd'hui", "J+6"]);
    }

    #[test]
    fn the_expiry_tile_counts_and_leads_to_the_stock_by_date() {
        let today = fixture_day();
        let items = vec![
            stock("Crème", false, Some(today)),
            stock("Jambon", false, Some(today)),
        ];
        let html = expiry_tile(&expiring_this_week(&items, today));
        assert!(
            html.contains(r#"<a class="card tile warn" href="/stocks?sort=expires_on">"#),
            "{html}"
        );
        assert!(html.contains("<strong>2</strong>"), "{html}");
        assert!(html.contains("produits périment cette semaine"), "{html}");
    }

    #[test]
    fn one_product_expiring_is_singular() {
        let today = fixture_day();
        let items = vec![stock("Crème", false, Some(today))];
        let html = expiry_tile(&expiring_this_week(&items, today));
        assert!(html.contains("produit périme cette semaine"), "{html}");
    }

    #[test]
    fn nothing_expiring_says_so() {
        let html = expiry_tile(&[]);
        assert!(html.contains("<strong>0</strong>"), "{html}");
        assert!(html.contains("rien ne périme cette semaine"), "{html}");
    }

    // -- the other cards ---------------------------------------------------

    #[test]
    fn only_low_items_reach_the_low_stock_card_in_their_order() {
        let items = vec![
            stock("Farine", true, None),
            stock("Lait", false, None),
            stock("Sucre", true, None),
        ];
        let names: Vec<&str> = low_stock_items(&items)
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, ["Farine", "Sucre"]);
    }

    #[test]
    fn an_empty_low_stock_card_says_so() {
        let html = stocks_block(&[]);
        assert!(html.contains("<h2>Stock bas</h2>"), "{html}");
        assert!(html.contains("Aucun article en stock bas."), "{html}");
        assert!(html.contains(r#"href="/stocks?low_stock=1""#), "{html}");
    }

    #[test]
    fn the_budget_card_is_titled_by_its_month_with_the_elision() {
        assert_eq!(budget_title(10), "Budget d'octobre");
        assert_eq!(budget_title(4), "Budget d'avril");
        assert_eq!(budget_title(8), "Budget d'août");
        assert_eq!(budget_title(3), "Budget de mars");
    }

    #[test]
    fn the_budget_card_shows_the_month_s_spend() {
        let html = budget_block(Some(42.5), ymd(2026, 10, 28));
        assert!(html.contains("<h2>Budget d'octobre</h2>"), "{html}");
        assert!(html.contains("42,50"), "{html}");
        assert!(html.contains(r#"href="/budget""#), "{html}");
    }

    #[test]
    fn an_empty_budget_card_says_nothing_was_spent() {
        let html = budget_block(None, ymd(2026, 10, 28));
        assert!(
            html.contains("Aucune dépense enregistrée ce mois-ci."),
            "{html}"
        );
    }

    #[test]
    fn an_empty_messages_card_says_everything_is_read() {
        let html = messages_block(&[], None, &[]);
        assert!(html.contains("<h2>Messages non lus</h2>"), "{html}");
        assert!(html.contains("Tout est lu."), "{html}");
        assert!(html.contains(r#"href="/messagerie""#), "{html}");
    }

    /// Each card's way out is a link in its head, 44 px tall (`.more`).
    #[test]
    fn a_card_head_holds_its_title_and_its_link() {
        assert_eq!(
            card_head("Stock bas", "/stocks?low_stock=1", "Voir le stock"),
            r#"<div class="split"><h2>Stock bas</h2><a class="more" href="/stocks?low_stock=1">Voir le stock</a></div>"#
        );
    }
}
