//! `/notifications` — the notification centre (#535), over the API of #534
//! (`apps/api/src/notifications/center.rs`).
//!
//! The list, newest first, in three groups: « Aujourd'hui », « Cette
//! semaine » (the six days before), and « Plus tôt » for the rest of the
//! 30 days the API keeps them. One filter per kind, « Tout marquer comme
//! lu », and a panel that leads to the push preferences of
//! `/account/notifications`.
//!
//! Without JavaScript, as every page: each entry is the button of its own
//! `POST` form. Opening one marks it read, then goes to what it is about
//! (`target`) — in its own family, made the active one when the caller
//! belongs to it, so that a reminder from another family does not land on
//! a « introuvable » page. An invitation has nowhere to go in the
//! application (its token is only in the e-mail): opening it marks it read
//! and comes back here.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::dto::notifications::{NotificationPage, NotificationResponse};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::family::set_active_group_cookie;
use crate::icons::{icon, Icon};
use crate::layout::CurrentUser;
use crate::routes::agenda::{today_paris, DISPLAY_TZ};
use crate::routes::groups::{cookie_of, header_parts};
use crate::routes::stocks::list::WEEKDAYS;
use crate::state::{api_request_auth, fetch_groups, AppState};

/// How many entries a page asks the API for. Thirty days of a busy family
/// can hold more: the page then ends on a link to the older ones.
const PAGE_LIMIT: usize = 30;

/// The filters, in the order the row shows them: the API's `kind` (`None`
/// for every kind) and the label of its pastille.
const FILTERS: [(Option<&str>, &str); 7] = [
    (None, "Toutes"),
    (Some("agenda_reminder"), "Agenda"),
    (Some("stock_expiry"), "Stocks"),
    (Some("message"), "Messages"),
    (Some("invitation"), "Familles"),
    (Some("grocery_item"), "Courses"),
    (Some("budget_alert"), "Budget"),
];

// -- pure logic (TDD'd below) ------------------------------------------------

/// The `?kind=` of the page, kept only when it names a kind: anything else
/// shows every kind rather than an error, and never reaches the API.
fn kind_filter(raw: Option<&str>) -> Option<&'static str> {
    let raw = raw?;
    FILTERS
        .iter()
        .find_map(|(kind, _)| kind.filter(|k| *k == raw))
}

/// Where an entry comes from, under its text: « Agenda · 18:30 ».
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "agenda_reminder" => "Agenda",
        "stock_expiry" => "Stocks",
        "message" => "Messages",
        "invitation" => "Familles",
        "grocery_item" => "Courses",
        "budget_alert" => "Budget",
        _ => "Notification",
    }
}

/// The family an opened entry makes active: its own, when the caller is
/// still a member of it (re-checked, as `POST /groups/switch` does — the
/// form's `group_id` is the caller's to forge), otherwise none.
fn family_to_activate(groups: &[GroupSummary], group_id: Uuid) -> Option<Uuid> {
    groups
        .iter()
        .any(|g| g.group_id == group_id)
        .then_some(group_id)
}

fn kind_icon(kind: &str) -> Icon {
    match kind {
        "agenda_reminder" => Icon::Calendar,
        "stock_expiry" => Icon::Stocks,
        "message" => Icon::Messages,
        "invitation" => Icon::Members,
        "grocery_item" => Icon::Groceries,
        "budget_alert" => Icon::Budget,
        _ => Icon::Bell,
    }
}

/// The group an entry is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Today,
    ThisWeek,
    Earlier,
}

impl Bucket {
    fn heading(self) -> &'static str {
        match self {
            Bucket::Today => "Aujourd’hui",
            Bucket::ThisWeek => "Cette semaine",
            Bucket::Earlier => "Plus tôt",
        }
    }
}

/// `day` and `today` are Paris days. A day after today — a server clock
/// behind the API's — is today.
fn bucket(day: NaiveDate, today: NaiveDate) -> Bucket {
    match (today - day).num_days() {
        ..=0 => Bucket::Today,
        1..=6 => Bucket::ThisWeek,
        _ => Bucket::Earlier,
    }
}

/// When an entry was written, as its group needs it: the hour today, the
/// weekday this week, the date before.
fn when_label(at: DateTime<Utc>, today: NaiveDate) -> String {
    let local = at.with_timezone(&DISPLAY_TZ);
    match bucket(local.date_naive(), today) {
        Bucket::Today => local.format("%H:%M").to_string(),
        Bucket::ThisWeek => WEEKDAYS[local.weekday().num_days_from_monday() as usize].to_string(),
        Bucket::Earlier => local.format("%d/%m").to_string(),
    }
}

/// Where opening an entry leads, `None` for nowhere but back here.
fn target(kind: &str, object_id: Option<Uuid>) -> Option<String> {
    match (kind, object_id) {
        ("agenda_reminder", Some(id)) => Some(format!("/agenda/{id}")),
        ("agenda_reminder", None) => Some("/agenda".into()),
        ("stock_expiry", Some(id)) => Some(format!("/stocks/{id}")),
        ("stock_expiry", None) => Some("/stocks".into()),
        ("message", _) => Some("/messagerie".into()),
        ("grocery_item", _) => Some("/grocery-list".into()),
        ("budget_alert", _) => Some("/budget".into()),
        _ => None,
    }
}

/// The count over the title.
fn unread_line(unread: u64) -> String {
    match unread {
        0 => "Tout est lu".into(),
        1 => "1 non lue".into(),
        n => format!("{n} non lues"),
    }
}

/// The `before_created_at`/`before_id` cursor of the page, both valid or
/// none: half a cursor or a malformed one shows the first page.
fn cursor(created_at: Option<&str>, id: Option<&str>) -> Option<(DateTime<Utc>, Uuid)> {
    let created_at = created_at?.parse::<DateTime<Utc>>().ok()?;
    let id = id?.parse::<Uuid>().ok()?;
    Some((created_at, id))
}

/// A cursor instant as the next page's address and the API's query carry
/// it: no `+`, nothing to encode.
fn cursor_instant(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// One entry: the button of its form. Unread, it is a card with its text in
/// bold, a dot for the eye and « Non lue » for the ear.
fn row_html(n: &NotificationResponse, today: NaiveDate) -> String {
    let unread = n.read_at.is_none();
    let object = n.object_id.map_or(String::new(), |id| {
        format!(r#"<input type="hidden" name="object_id" value="{id}"/>"#)
    });
    let (class, text, mark) = if unread {
        (
            "secondary notif unread",
            "strong",
            r#"<span class="dot" aria-hidden="true"></span> <span class="sr-only">Non lue</span>"#,
        )
    } else {
        ("secondary notif", "span", "")
    };
    format!(
        r#"<li><form method="post" action="/notifications/{id}/open"><input type="hidden" name="group_id" value="{group}"/><input type="hidden" name="kind" value="{kind}"/>{object}<button class="{class}">{icon}<span class="what"><{text}>{body}</{text}> <span class="muted">{label} · {when}</span></span>{mark}</button></form></li>"#,
        id = n.id,
        group = n.group_id,
        kind = html_escape(&n.kind),
        icon = icon(kind_icon(&n.kind)),
        body = html_escape(&n.body),
        label = kind_label(&n.kind),
        when = when_label(n.created_at, today),
    )
}

/// The entries under their group headings, in the order given (newest
/// first). Nothing for an empty list.
fn groups_html(notifications: &[NotificationResponse], today: NaiveDate) -> String {
    let mut html = String::new();
    let mut open: Option<Bucket> = None;
    for n in notifications {
        let here = bucket(n.created_at.with_timezone(&DISPLAY_TZ).date_naive(), today);
        if open != Some(here) {
            if open.is_some() {
                html.push_str("</ul>");
            }
            html.push_str(&format!(
                r#"<h2 class="eyebrow">{}</h2><ul class="list notifs">"#,
                here.heading()
            ));
            open = Some(here);
        }
        html.push_str(&row_html(n, today));
    }
    if open.is_some() {
        html.push_str("</ul>");
    }
    html
}

/// The row of filters, the current one in `aria-current`.
fn filters_html(current: Option<&str>) -> String {
    let links: String = FILTERS
        .iter()
        .map(|&(kind, label)| {
            let href = kind.map_or("/notifications".to_string(), |k| {
                format!("/notifications?kind={k}")
            });
            let lit = if kind == current {
                r#" aria-current="true""#
            } else {
                ""
            };
            format!(r#"<a href="{href}"{lit}>{label}</a>"#)
        })
        .collect();
    format!(r#"<nav class="filters" aria-label="Filtres">{links}</nav>"#)
}

/// The address of the next page, after `last`.
fn older_href(kind: Option<&str>, last: &NotificationResponse) -> String {
    let kind = kind.map_or(String::new(), |k| format!("kind={k}&amp;"));
    format!(
        "/notifications?{kind}before_created_at={at}&amp;before_id={id}",
        at = cursor_instant(last.created_at),
        id = last.id,
    )
}

/// The notice a failed form comes back with.
fn error_html(code: Option<&str>) -> &'static str {
    match code {
        Some("mark_failed") => {
            r#"<p class="notice error">Les notifications n'ont pas pu être marquées comme lues. Réessayez dans un instant.</p>"#
        }
        Some("open_failed") => {
            r#"<p class="notice error">Cette notification n'a pas pu être ouverte. Elle a peut-être expiré.</p>"#
        }
        _ => "",
    }
}

/// The side panel: where the notifications that reach a device are set.
const SETTINGS_PANEL: &str = r#"<section class="card"><h2>Réglages</h2><p>Choisissez comment vos rappels vous parviennent, et activez les notifications sur cet appareil.</p><a class="more" href="/account/notifications">Régler les notifications</a></section>"#;

/// What the page shows of its list: the entries, or why there are none.
enum Listing {
    Page(NotificationPage),
    Unavailable,
}

fn page_body(
    listing: &Listing,
    kind: Option<&str>,
    paged: bool,
    unread: Option<u64>,
    error: Option<&str>,
    today: NaiveDate,
) -> String {
    let count = unread.map_or(String::new(), |n| {
        format!(r#"<p class="eyebrow">{}</p>"#, unread_line(n))
    });
    let mark_all = if unread.unwrap_or(1) > 0 {
        r#"<form method="post" action="/notifications/read"><button class="secondary">Tout marquer comme lu</button></form>"#
    } else {
        ""
    };
    let list = match listing {
        Listing::Unavailable => {
            r#"<p class="notice error">Les notifications sont momentanément indisponibles.</p>"#
                .to_string()
        }
        Listing::Page(page) if page.notifications.is_empty() => {
            let words = if kind.is_some() || paged {
                "Aucune notification ici."
            } else {
                "Aucune notification pour le moment."
            };
            format!(r#"<p class="muted">{words}</p>"#)
        }
        Listing::Page(page) => {
            let older = match page.notifications.last() {
                Some(last) if page.has_more => format!(
                    r#"<p><a class="more" href="{}">Notifications plus anciennes</a></p>"#,
                    older_href(kind, last)
                ),
                _ => String::new(),
            };
            format!("{}{older}", groups_html(&page.notifications, today))
        }
    };
    let newest = if paged {
        let href = kind.map_or("/notifications".to_string(), |k| {
            format!("/notifications?kind={k}")
        });
        format!(r#"<p><a class="more" href="{href}">Revenir aux plus récentes</a></p>"#)
    } else {
        String::new()
    };
    format!(
        r#"<div class="page-header"><hgroup>{count}<h1>Notifications</h1></hgroup>{mark_all}</div>
{error}<div class="settings">
<div>{filters}{newest}{list}</div>
{SETTINGS_PANEL}
</div>"#,
        error = error_html(error),
        filters = filters_html(kind),
    )
}

// -- handlers ----------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
pub struct PageQuery {
    kind: Option<String>,
    before_created_at: Option<String>,
    before_id: Option<String>,
    error: Option<String>,
}

/// `GET /notifications`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    let cookie = cookie_of(&headers);
    let kind = kind_filter(query.kind.as_deref());
    let before = cursor(
        query.before_created_at.as_deref(),
        query.before_id.as_deref(),
    );
    let mut path = format!("/notifications?limit={PAGE_LIMIT}");
    if let Some(kind) = kind {
        path.push_str(&format!("&kind={kind}"));
    }
    if let Some((at, id)) = before {
        path.push_str(&format!(
            "&before_created_at={}&before_id={id}",
            cursor_instant(at)
        ));
    }
    let (header, listing) = tokio::join!(
        header_parts(&state, &headers, &me, "/notifications"),
        async {
            match api_request_auth(&state, reqwest::Method::GET, &path, cookie.as_deref(), None)
                .await
            {
                Ok(resp) if resp.status == reqwest::StatusCode::OK => {
                    serde_json::from_value::<NotificationPage>(resp.body)
                        .map_or(Listing::Unavailable, Listing::Page)
                }
                _ => Listing::Unavailable,
            }
        }
    );
    let (_, header, unread) = header;
    let body = page_body(
        &listing,
        kind,
        before.is_some(),
        unread,
        query.error.as_deref(),
        today_paris(),
    );
    Html(shell_with_header(
        Width::Full,
        "Notifications",
        &header,
        &body,
    ))
    .into_response()
}

/// `POST /notifications/read` — « Tout marquer comme lu ».
pub async fn mark_all_read(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = cookie_of(&headers);
    match api_request_auth(
        &state,
        reqwest::Method::POST,
        "/notifications/read",
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => {
            Redirect::to("/notifications").into_response()
        }
        _ => Redirect::to("/notifications?error=mark_failed").into_response(),
    }
}

/// What an entry's form posts: enough to find where it leads, never the
/// address itself — `target` builds it, so the form cannot send the caller
/// anywhere else.
#[derive(Debug, serde::Deserialize)]
pub struct OpenForm {
    group_id: Uuid,
    kind: String,
    object_id: Option<Uuid>,
}

/// `POST /notifications/:id/open` — marks the entry read, then goes to
/// what it is about, in its family.
pub async fn open(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Form(form): Form<OpenForm>,
) -> Response {
    let cookie = cookie_of(&headers);
    let marked = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/notifications/{id}/read"),
        cookie.as_deref(),
        None,
    )
    .await;
    if !matches!(marked, Ok(ref resp) if resp.status == reqwest::StatusCode::NO_CONTENT) {
        return Redirect::to("/notifications?error=open_failed").into_response();
    }
    let Some(to) = target(&form.kind, form.object_id) else {
        return Redirect::to("/notifications").into_response();
    };
    // The family it happened in becomes the active one.
    let groups = fetch_groups(&state, cookie.as_deref())
        .await
        .unwrap_or_default();
    let mut response_headers = HeaderMap::new();
    if let Some(group_id) = family_to_activate(&groups, form.group_id) {
        if let Ok(v) = set_active_group_cookie(group_id).parse() {
            response_headers.insert(axum::http::header::SET_COOKIE, v);
        }
    }
    (response_headers, Redirect::to(&to)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn notification(kind: &str, created_at: DateTime<Utc>, read: bool) -> NotificationResponse {
        NotificationResponse {
            id: Uuid::from_u128(7),
            group_id: Uuid::from_u128(1),
            kind: kind.to_string(),
            object_id: Some(Uuid::from_u128(9)),
            body: "Rappel : « Piscine » le 30/10/2026 à 18:30.".to_string(),
            read_at: read.then_some(created_at),
            created_at,
        }
    }

    // 2026-10-30 is a Friday; Paris is UTC+1 from 2026-10-25.
    fn friday() -> NaiveDate {
        day(2026, 10, 30)
    }

    #[test]
    fn a_known_kind_filters_and_anything_else_shows_every_kind() {
        assert_eq!(
            kind_filter(Some("agenda_reminder")),
            Some("agenda_reminder")
        );
        assert_eq!(kind_filter(Some("budget_alert")), Some("budget_alert"));
        for raw in [
            None,
            Some(""),
            Some("Agenda"),
            Some("unread"),
            Some("message "),
        ] {
            assert_eq!(kind_filter(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn every_filter_but_the_first_is_a_kind_the_page_accepts() {
        assert_eq!(FILTERS[0], (None, "Toutes"));
        for (kind, _) in &FILTERS[1..] {
            assert_eq!(kind_filter(*kind), *kind);
        }
    }

    #[test]
    fn each_kind_says_where_it_comes_from() {
        assert_eq!(kind_label("agenda_reminder"), "Agenda");
        assert_eq!(kind_label("stock_expiry"), "Stocks");
        assert_eq!(kind_label("message"), "Messages");
        assert_eq!(kind_label("invitation"), "Familles");
        assert_eq!(kind_label("grocery_item"), "Courses");
        assert_eq!(kind_label("budget_alert"), "Budget");
        // A kind a newer API adds still lists, under a neutral word.
        assert_eq!(kind_label("something_new"), "Notification");
        assert_eq!(kind_icon("something_new"), Icon::Bell);
        assert_eq!(kind_icon("agenda_reminder"), Icon::Calendar);
        // An entry is named as its filter is.
        for (kind, label) in FILTERS.iter().skip(1) {
            assert_eq!(kind_label(kind.unwrap()), *label);
        }
    }

    #[test]
    fn today_the_six_days_before_and_the_rest() {
        let today = friday();
        assert_eq!(bucket(today, today), Bucket::Today);
        assert_eq!(bucket(day(2026, 10, 29), today), Bucket::ThisWeek);
        assert_eq!(bucket(day(2026, 10, 24), today), Bucket::ThisWeek);
        assert_eq!(bucket(day(2026, 10, 23), today), Bucket::Earlier);
        assert_eq!(bucket(day(2026, 10, 1), today), Bucket::Earlier);
        // A clock a little ahead of the server's is still today.
        assert_eq!(bucket(day(2026, 10, 31), today), Bucket::Today);
    }

    #[test]
    fn the_hour_today_the_weekday_this_week_the_date_before() {
        let today = friday();
        // 17:30 UTC is 18:30 in Paris.
        let at = Utc.with_ymd_and_hms(2026, 10, 30, 17, 30, 0).unwrap();
        assert_eq!(when_label(at, today), "18:30");
        let monday = Utc.with_ymd_and_hms(2026, 10, 26, 9, 0, 0).unwrap();
        assert_eq!(when_label(monday, today), "lundi");
        let earlier = Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        assert_eq!(when_label(earlier, today), "12/10");
    }

    #[test]
    fn the_day_is_the_paris_one_not_the_utc_one() {
        // 23:30 UTC on Thursday is 00:30 on Friday in Paris: today.
        let at = Utc.with_ymd_and_hms(2026, 10, 29, 23, 30, 0).unwrap();
        assert_eq!(when_label(at, friday()), "00:30");
    }

    #[test]
    fn opening_an_entry_leads_to_what_it_is_about() {
        let id = Uuid::from_u128(9);
        assert_eq!(
            target("agenda_reminder", Some(id)).as_deref(),
            Some(format!("/agenda/{id}").as_str())
        );
        assert_eq!(
            target("stock_expiry", Some(id)).as_deref(),
            Some(format!("/stocks/{id}").as_str())
        );
        assert_eq!(target("message", Some(id)).as_deref(), Some("/messagerie"));
        assert_eq!(
            target("grocery_item", Some(id)).as_deref(),
            Some("/grocery-list")
        );
        assert_eq!(target("budget_alert", None).as_deref(), Some("/budget"));
        // An invitation is accepted from its e-mail only.
        assert_eq!(target("invitation", Some(id)), None);
        // Without its object, a reminder or an expiry has its section.
        assert_eq!(target("agenda_reminder", None).as_deref(), Some("/agenda"));
        assert_eq!(target("stock_expiry", None).as_deref(), Some("/stocks"));
        assert_eq!(target("something_new", Some(id)), None);
    }

    #[test]
    fn the_count_over_the_title_agrees_with_its_number() {
        assert_eq!(unread_line(0), "Tout est lu");
        assert_eq!(unread_line(1), "1 non lue");
        assert_eq!(unread_line(3), "3 non lues");
    }

    #[test]
    fn a_cursor_is_both_halves_or_nothing() {
        let id = Uuid::from_u128(7);
        let at = "2026-10-30T17:30:00.123456Z";
        let parsed = cursor(Some(at), Some(&id.to_string())).expect("a cursor");
        assert_eq!(parsed.1, id);
        assert_eq!(parsed.0, at.parse::<DateTime<Utc>>().unwrap());
        assert_eq!(cursor(Some(at), None), None);
        assert_eq!(cursor(None, Some(&id.to_string())), None);
        assert_eq!(cursor(Some("hier"), Some(&id.to_string())), None);
        assert_eq!(cursor(Some(at), Some("x")), None);
    }

    #[test]
    fn an_unread_entry_is_marked_for_the_eye_and_the_ear() {
        let at = Utc.with_ymd_and_hms(2026, 10, 30, 17, 30, 0).unwrap();
        let html = row_html(&notification("agenda_reminder", at, false), friday());
        let id = Uuid::from_u128(7);
        assert!(
            html.contains(&format!(
                r#"<form method="post" action="/notifications/{id}/open">"#
            )),
            "{html}"
        );
        assert!(html.contains(r#"class="secondary notif unread""#), "{html}");
        assert!(
            html.contains("Rappel : « Piscine » le 30/10/2026 à 18:30."),
            "{html}"
        );
        assert!(html.contains("Agenda · 18:30"), "{html}");
        assert!(
            html.contains(r#"<span class="sr-only">Non lue</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="dot" aria-hidden="true"></span>"#),
            "{html}"
        );
    }

    #[test]
    fn a_read_entry_carries_no_mark() {
        let at = Utc.with_ymd_and_hms(2026, 10, 30, 17, 30, 0).unwrap();
        let html = row_html(&notification("message", at, true), friday());
        assert!(html.contains(r#"class="secondary notif""#), "{html}");
        assert!(!html.contains("unread"), "{html}");
        assert!(!html.contains("Non lue"), "{html}");
        assert!(!html.contains("dot"), "{html}");
    }

    #[test]
    fn the_text_of_an_entry_is_escaped() {
        let at = Utc.with_ymd_and_hms(2026, 10, 30, 17, 30, 0).unwrap();
        let mut n = notification("grocery_item", at, false);
        n.body = "Sam a ajouté « <b>pain</b> » à la liste de courses.".to_string();
        let html = row_html(&n, friday());
        assert!(html.contains("&lt;b&gt;pain&lt;/b&gt;"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
    }

    #[test]
    fn entries_fall_under_their_group_in_order() {
        let today = friday();
        let list = [
            notification(
                "message",
                Utc.with_ymd_and_hms(2026, 10, 30, 8, 0, 0).unwrap(),
                false,
            ),
            notification(
                "stock_expiry",
                Utc.with_ymd_and_hms(2026, 10, 30, 7, 0, 0).unwrap(),
                true,
            ),
            notification(
                "invitation",
                Utc.with_ymd_and_hms(2026, 10, 26, 9, 0, 0).unwrap(),
                true,
            ),
            notification(
                "grocery_item",
                Utc.with_ymd_and_hms(2026, 10, 2, 9, 0, 0).unwrap(),
                true,
            ),
        ];
        let html = groups_html(&list, today);
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {html}"))
        };
        assert!(at("<h2 class=\"eyebrow\">Aujourd’hui</h2>") < at("Messages · 09:00"));
        assert!(at("Messages · 09:00") < at("Stocks · 08:00"));
        assert!(at("Stocks · 08:00") < at("<h2 class=\"eyebrow\">Cette semaine</h2>"));
        assert!(at("<h2 class=\"eyebrow\">Cette semaine</h2>") < at("Familles · lundi"));
        assert!(at("Familles · lundi") < at("<h2 class=\"eyebrow\">Plus tôt</h2>"));
        assert!(at("<h2 class=\"eyebrow\">Plus tôt</h2>") < at("Courses · 02/10"));
        assert_eq!(html.matches("<h2").count(), 3);
    }

    #[test]
    fn an_empty_group_has_no_heading() {
        let list = [notification(
            "message",
            Utc.with_ymd_and_hms(2026, 10, 26, 9, 0, 0).unwrap(),
            false,
        )];
        let html = groups_html(&list, friday());
        assert!(!html.contains("Aujourd’hui"), "{html}");
        assert!(html.contains("Cette semaine"), "{html}");
        assert!(!html.contains("Plus tôt"), "{html}");
        assert_eq!(groups_html(&[], friday()), "");
    }

    #[test]
    fn the_current_filter_is_lit_and_every_filter_is_a_link() {
        let html = filters_html(Some("stock_expiry"));
        assert!(
            html.starts_with(r#"<nav class="filters" aria-label="Filtres">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<a href="/notifications?kind=stock_expiry" aria-current="true">Stocks</a>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/notifications">Toutes</a>"#),
            "{html}"
        );
        assert_eq!(html.matches("<a ").count(), FILTERS.len());
        assert_eq!(html.matches("aria-current").count(), 1);
        let html = filters_html(None);
        assert!(
            html.contains(r#"<a href="/notifications" aria-current="true">Toutes</a>"#),
            "{html}"
        );
    }

    #[test]
    fn the_next_page_keeps_the_filter_and_starts_after_the_last_entry() {
        let at = Utc.with_ymd_and_hms(2026, 10, 30, 17, 30, 0).unwrap();
        let last = notification("message", at, true);
        let id = Uuid::from_u128(7);
        assert_eq!(
            older_href(Some("message"), &last),
            format!(
                "/notifications?kind=message&amp;before_created_at=2026-10-30T17:30:00.000000Z&amp;before_id={id}"
            )
        );
        assert_eq!(
            older_href(None, &last),
            format!(
                "/notifications?before_created_at=2026-10-30T17:30:00.000000Z&amp;before_id={id}"
            )
        );
        // What it writes, `cursor` reads back.
        let (created, before) =
            cursor(Some("2026-10-30T17:30:00.000000Z"), Some(&id.to_string())).expect("a cursor");
        assert_eq!((created, before), (at, id));
    }

    fn summary(group_id: Uuid) -> GroupSummary {
        GroupSummary {
            group_id,
            name: "Martin".to_string(),
            role: "member".to_string(),
            accent: "sarcelle".to_string(),
            ownership_notice: None,
        }
    }

    #[test]
    fn opening_an_entry_switches_only_to_a_family_the_caller_still_belongs_to() {
        let kept = Uuid::from_u128(1);
        let left = Uuid::from_u128(2);
        let groups = [summary(kept), summary(Uuid::from_u128(3))];
        assert_eq!(family_to_activate(&groups, kept), Some(kept));
        // Left since, or forged in the form: the active family stays.
        assert_eq!(family_to_activate(&groups, left), None);
        // The groups could not be read: nothing is switched.
        assert_eq!(family_to_activate(&[], kept), None);
    }
}
