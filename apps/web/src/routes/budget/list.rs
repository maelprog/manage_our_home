//! `/budget` — one month of the family's spend, as the mock-up « Budget »
//! draws it (#533): a month selector (`?month=YYYY-MM`, the current month
//! without it), what the month spent against the family's cap and what is
//! left, a bar per category with « n % de [plafond] » and « Dépassé », then
//! the month's expenses. An owner or an admin reads it (#531); per-row a
//! `Modifier` link renders only for entries the caller may modify. Manual
//! entry lives on `/budget/new`, the caps and categories on
//! `/budget/settings`; the price-on-checkout path is on the grocery list.
//! PRG banners after create/update/delete. See `docs/front-epic-7-budget.md`.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use chrono::{Datelike, NaiveDate};
use manage_our_home_shared::dto::budget::{
    BudgetCategoryUsage, BudgetEntryList, BudgetEntryResponse, BudgetMonthSummary, BudgetSummary,
};
use manage_our_home_shared::validation::budget::{
    can_modify, cap_share_label, format_euros, format_period, in_month, month_param, next_month,
    parse_month, previous_month, remaining,
};

use crate::app::{html_escape, shell_with_header, Width};
use crate::icons::{icon, icon_link, Icon};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{budget_cookie, category_class, family_context, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct ListQuery {
    notice: Option<String>,
    error: Option<String>,
    month: Option<String>,
}

fn notice_html(notice: Option<&str>) -> String {
    let text = match notice {
        Some("entry_created") => "Dépense ajoutée.",
        Some("entry_updated") => "Dépense mise à jour.",
        Some("entry_deleted") => "Dépense supprimée.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice success">{}</p>"#, html_escape(text))
}

fn error_html(error: Option<&str>) -> String {
    let text = match error {
        Some("name_required") => "Le nom est obligatoire.",
        Some("amount_must_be_non_negative") => "Le montant ne peut pas être négatif.",
        Some("forbidden") => "Vous n'avez pas les droits nécessaires pour cette action.",
        Some("unavailable") => "Service momentanément indisponible, merci de réessayer.",
        _ => return String::new(),
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

/// The month selector: the month before, the month shown, the month after.
/// The arrows hold an icon alone, so their name is what they step to.
fn month_nav(month: NaiveDate) -> String {
    let arrow = |to: Option<NaiveDate>, label: &str, glyph: Icon| {
        to.map(|to| {
            icon_link(
                "btn secondary",
                &format!("/budget?month={}", month_param(to)),
                label,
                glyph,
            )
        })
        .unwrap_or_default()
    };
    format!(
        r#"<div class="split">
{prev}
<h2>{label}</h2>
{next}
</div>"#,
        prev = arrow(previous_month(month), "Mois précédent", Icon::Previous),
        label = html_escape(&format_period(month)),
        next = arrow(next_month(month), "Mois suivant", Icon::Next),
    )
}

/// A bar's share of its cap in words, and « Dépassé » past it. The bar
/// itself is `aria-hidden`: this line says what it draws.
fn share_line(percent: Option<i64>, cap: f64, over: bool) -> String {
    let over = if over {
        r#" <span class="badge late">Dépassé</span>"#
    } else {
        ""
    };
    format!(
        r#"<span class="muted">{share}{over}</span>"#,
        share = html_escape(&cap_share_label(percent.unwrap_or(0), cap)),
    )
}

/// A `<progress>` of `percent` out of 100, held at 100 past the cap: the
/// element draws no more than full, and the caption tells by how much.
fn bar(class: &str, percent: Option<i64>) -> String {
    let class = if class.is_empty() {
        String::new()
    } else {
        format!(r#" class="{class}""#)
    };
    format!(
        r#"<progress{class} max="100" value="{value}" aria-hidden="true"></progress>"#,
        value = percent.unwrap_or(0).clamp(0, 100),
    )
}

/// Spent, cap and what is left of it for the month, and the family's bar.
/// Without a cap, only what was spent, and the way to set one.
fn summary_card(m: &BudgetMonthSummary) -> String {
    let spent = format!(
        r#"<div><dt>Dépensé</dt><dd>{}</dd></div>"#,
        html_escape(&format_euros(m.total))
    );
    let body = match m.monthly_cap {
        Some(cap) => {
            let left = remaining(m.total, cap);
            let left = if left < 0.0 {
                format!(
                    r#"<div><dt>Dépassement</dt><dd>{}</dd></div>"#,
                    html_escape(&format_euros(-left))
                )
            } else {
                format!(
                    r#"<div><dt>Reste</dt><dd>{}</dd></div>"#,
                    html_escape(&format_euros(left))
                )
            };
            format!(
                r#"<dl class="figures">
{spent}
<div><dt>Plafond</dt><dd>{cap}</dd></div>
{left}
</dl>
{bar}
<p>{share}</p>"#,
                cap = html_escape(&format_euros(cap)),
                bar = bar(if m.over_cap { "over" } else { "" }, m.percent),
                share = share_line(m.percent, cap, m.over_cap),
            )
        }
        None => format!(
            r#"<dl class="figures">
{spent}
</dl>
<p class="muted">Aucun plafond mensuel. <a href="/budget/settings">Fixer un plafond</a></p>"#
        ),
    };
    format!(
        r#"<section class="card" id="monthly-summary" aria-label="Résumé du mois">
{body}
</section>"#
    )
}

/// One category's line: its swatch and name, what it spent, then its bar
/// against its own cap, or « Sans plafond ».
fn category_row(c: &BudgetCategoryUsage) -> String {
    let class = category_class(&c.color);
    let usage = match c.monthly_cap {
        Some(cap) => format!(
            "{bar}\n{share}",
            bar = bar(class, c.percent),
            share = share_line(c.percent, cap, c.over_cap),
        ),
        None => r#"<span class="muted">Sans plafond</span>"#.to_string(),
    };
    format!(
        r#"<li class="list-row stacked">
<span class="split"><span><span class="swatch {class}" aria-hidden="true"></span> <strong>{name}</strong></span><span>{total}</span></span>
{usage}
</li>"#,
        name = html_escape(&c.name),
        total = html_escape(&format_euros(c.total)),
    )
}

fn categories_card(m: &BudgetMonthSummary) -> String {
    let mut rows: String = m.categories.iter().map(category_row).collect();
    if m.uncategorized > 0.0 {
        rows.push_str(&format!(
            r#"<li class="list-row"><span>Sans catégorie</span><span>{}</span></li>"#,
            html_escape(&format_euros(m.uncategorized)),
        ));
    }
    let body = if rows.is_empty() {
        r#"<p class="muted">Aucune catégorie pour le moment. <a href="/budget/settings">Créer une catégorie</a></p>"#
            .to_string()
    } else {
        format!(r#"<ul class="list">{rows}</ul>"#)
    };
    format!(
        r#"<section class="card">
<h2>Par catégorie</h2>
{body}
</section>"#
    )
}

/// Renders one budget entry: date, name, an optional `Courses` badge (set
/// when the entry links to grocery items, as one priced on checkout does),
/// its category in a badge with its swatch, the amount, and — for permitted
/// users — an edit link.
fn entry_row(
    entry: &BudgetEntryResponse,
    category: Option<(&str, &str)>,
    can_edit: bool,
) -> String {
    let date = entry.spent_at.format("%d/%m/%Y").to_string();
    let badge_html = if !entry.grocery_items.is_empty() {
        r#" <span class="badge">Courses</span>"#
    } else {
        ""
    };
    let category_html = category
        .map(|(name, color)| {
            format!(
                r#" <span class="badge"><span class="swatch {class}" aria-hidden="true"></span> {name}</span>"#,
                class = category_class(color),
                name = html_escape(name),
            )
        })
        .unwrap_or_default();
    let edit_link = if can_edit {
        format!(
            r#"<a class="btn secondary" href="/budget/{id}">Modifier</a>"#,
            id = entry.id
        )
    } else {
        String::new()
    };
    format!(
        r#"<li class="list-row">
<span><span class="muted">{date}</span> <strong>{name}</strong>{badge_html}{category_html} <span class="muted">— {amount}</span></span>
{edit_link}
</li>"#,
        date = html_escape(&date),
        name = html_escape(&entry.name),
        amount = html_escape(&format_euros(entry.amount)),
    )
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);

    // A month the selector did not write is the current one, as without it.
    let asked = query.month.as_deref().and_then(parse_month);
    let summary_path = match asked {
        Some(month) => format!(
            "/groups/{}/budget-entries/summary?month={}",
            fam.gid,
            month_param(month)
        ),
        None => format!("/groups/{}/budget-entries/summary", fam.gid),
    };

    // The month against the caps (computed server-side). A transport error
    // takes down the whole page; any non-200 renders without it rather than
    // leaking JSON.
    let summary: BudgetSummary = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &summary_path,
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<BudgetSummary>(resp.body).unwrap_or_default()
        }
        Ok(_) => BudgetSummary::default(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    let entries: Vec<BudgetEntryResponse> = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/budget-entries", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<BudgetEntryList>(resp.body)
                .map(|l| l.entries)
                .unwrap_or_default()
        }
        Ok(_) => Vec::new(),
        Err(_) => return service_unavailable_page().into_response(),
    };

    // The month the API answered for; the one asked, or today's, without it.
    let month = summary
        .month
        .as_ref()
        .map(|m| m.period)
        .or(asked)
        .unwrap_or_else(|| {
            let today = chrono::Utc::now().date_naive();
            today.with_day(1).unwrap_or(today)
        });

    let notice = notice_html(query.notice.as_deref());
    let error = error_html(query.error.as_deref());
    let (summary_section, categories_section) = match &summary.month {
        Some(m) => (summary_card(m), categories_card(m)),
        None => (
            r#"<section class="card" id="monthly-summary"><p class="muted">Le résumé du mois n'a pas pu être chargé.</p></section>"#
                .to_string(),
            String::new(),
        ),
    };

    let category_of = |entry: &BudgetEntryResponse| {
        let id = entry.category_id?;
        let c = summary
            .month
            .as_ref()?
            .categories
            .iter()
            .find(|c| c.id == id)?;
        Some((c.name.as_str(), c.color.as_str()))
    };
    let rows: String = entries
        .iter()
        .filter(|e| in_month(e.spent_at, month))
        .map(|e| {
            entry_row(
                e,
                category_of(e),
                can_modify(&fam.role, e.created_by == me.user_id),
            )
        })
        .collect();
    let entries_section = if rows.is_empty() {
        format!(
            r#"<p class="muted">Aucune dépense en {}.</p>"#,
            html_escape(&format_period(month))
        )
    } else {
        format!(r#"<ul class="list">{rows}</ul>"#)
    };

    let body = format!(
        r#"<div class="page-header">
<h1>Budget</h1>
<span class="actions">
<a class="btn secondary" href="/budget/settings">{settings} Réglages</a>
<a class="btn" href="/budget/new">Ajouter une dépense</a>
</span>
</div>
{notice}{error}
{nav}
<div class="budget">
<div>
{summary_section}
{categories_section}
</div>
<section class="card">
<h2>Dépenses</h2>
{entries_section}
</section>
</div>"#,
        settings = icon(Icon::Settings),
        nav = month_nav(month),
    );
    Html(shell_with_header(Width::Full, "Budget", &fam.header, &body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn month(cap: Option<f64>, total: f64, percent: Option<i64>, over: bool) -> BudgetMonthSummary {
        BudgetMonthSummary {
            period: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            total,
            monthly_cap: cap,
            percent,
            over_cap: over,
            categories: Vec::new(),
            uncategorized: 0.0,
        }
    }

    fn usage(
        cap: Option<f64>,
        total: f64,
        percent: Option<i64>,
        over: bool,
    ) -> BudgetCategoryUsage {
        BudgetCategoryUsage {
            id: Uuid::from_u128(1),
            name: "Alimentation".to_string(),
            color: "blue".to_string(),
            total,
            monthly_cap: cap,
            percent,
            over_cap: over,
        }
    }

    #[test]
    fn the_month_selector_steps_a_month_either_way() {
        let html = month_nav(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert!(html.contains(r#"href="/budget?month=2025-12""#), "{html}");
        assert!(html.contains(r#"href="/budget?month=2026-02""#), "{html}");
        assert!(html.contains(r#"aria-label="Mois précédent""#), "{html}");
        assert!(html.contains(r#"aria-label="Mois suivant""#), "{html}");
        assert!(html.contains("<h2>janvier 2026</h2>"), "{html}");
    }

    #[test]
    fn a_capped_month_shows_spent_cap_and_what_is_left() {
        let html = summary_card(&month(Some(1000.0), 250.0, Some(25), false));
        assert!(html.contains("<dt>Dépensé</dt><dd>250,00 €</dd>"), "{html}");
        assert!(
            html.contains("<dt>Plafond</dt><dd>1000,00 €</dd>"),
            "{html}"
        );
        assert!(html.contains("<dt>Reste</dt><dd>750,00 €</dd>"), "{html}");
        assert!(
            html.contains(r#"<progress max="100" value="25" aria-hidden="true">"#),
            "{html}"
        );
        assert!(html.contains("25 % de 1000,00 €"), "{html}");
        assert!(!html.contains("Dépassé"), "{html}");
    }

    #[test]
    fn an_exceeded_cap_says_by_how_much_and_fills_the_bar_in_red() {
        let html = summary_card(&month(Some(100.0), 112.5, Some(112), true));
        assert!(
            html.contains("<dt>Dépassement</dt><dd>12,50 €</dd>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<progress class="over" max="100" value="100""#),
            "{html}"
        );
        assert!(html.contains("112 % de 100,00 €"), "{html}");
        assert!(
            html.contains(r#"<span class="badge late">Dépassé</span>"#),
            "{html}"
        );
    }

    #[test]
    fn a_month_without_a_cap_shows_what_was_spent_and_the_way_to_set_one() {
        let html = summary_card(&month(None, 40.0, None, false));
        assert!(html.contains("<dd>40,00 €</dd>"), "{html}");
        assert!(!html.contains("<progress"), "{html}");
        assert!(html.contains(r#"href="/budget/settings""#), "{html}");
    }

    #[test]
    fn a_category_bar_is_in_its_colour_with_its_share_of_its_cap() {
        let html = category_row(&usage(Some(200.0), 50.0, Some(25), false));
        assert!(
            html.contains(r#"<span class="swatch cat-blue" aria-hidden="true">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<progress class="cat-blue" max="100" value="25""#),
            "{html}"
        );
        assert!(html.contains("25 % de 200,00 €"), "{html}");
        assert!(html.contains("<strong>Alimentation</strong>"), "{html}");
    }

    #[test]
    fn an_exceeded_category_says_so_in_words() {
        let html = category_row(&usage(Some(200.0), 210.0, Some(105), true));
        assert!(html.contains(r#"value="100""#), "{html}");
        assert!(html.contains("Dépassé"), "{html}");
    }

    #[test]
    fn a_category_without_a_cap_has_no_bar() {
        let html = category_row(&usage(None, 30.0, None, false));
        assert!(!html.contains("<progress"), "{html}");
        assert!(html.contains("Sans plafond"), "{html}");
    }

    #[test]
    fn uncategorized_spend_has_its_own_line_only_when_there_is_some() {
        let mut m = month(None, 12.0, None, false);
        m.categories.push(usage(None, 0.0, None, false));
        assert!(!categories_card(&m).contains("Sans catégorie"));
        m.uncategorized = 12.0;
        let html = categories_card(&m);
        assert!(html.contains("Sans catégorie"), "{html}");
        assert!(html.contains("12,00 €"), "{html}");
    }

    #[test]
    fn a_category_name_is_escaped() {
        let mut c = usage(None, 0.0, None, false);
        c.name = "<b>Maison</b>".to_string();
        let html = category_row(&c);
        assert!(html.contains("&lt;b&gt;Maison&lt;/b&gt;"), "{html}");
    }
}
