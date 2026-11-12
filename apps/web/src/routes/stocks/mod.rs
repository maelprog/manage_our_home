//! Stocks screens (front epic #4, issue #19): the family inventory list with
//! a low-stock indicator, manual add, and per-item edit/quantity-adjust/delete
//! behind a permission bar. Same SSR pattern as `routes/agenda/*` and
//! `routes/groups/*` — plain `<form method=post>` submissions, per-page error
//! tables mapping `apps/api/src/stocks/`'s exact status/error codes to French
//! copy, PRG (`?notice=`/`?error=` codes) after every mutation.
//!
//! Permission bar (#19 + follow-up #39): the backend runs a two-tier bar. A
//! **quantity-only** PATCH is open to any family member (shared inventory), so
//! the adjust form renders for everyone; the full-record edit and delete stay
//! behind `can_modify` (creator or group admin/owner), so the edit link and
//! delete button render only for those users. The backend stays the authority
//! (a forged full-edit/delete is still 403'd, mapped defensively here). An
//! adjustment to 0 of an article without a reorder threshold removes it
//! (#800) — the consequence of the adjustment, open to every member — and
//! lands on the list with `?notice=item_depleted`, never on its gone page.
//!
//! Expiry date (#401): one date per article, the nearest. Its status is
//! derived by the backend (`StockItemResponse::expiry_status`, today in
//! Europe/Paris) and shown here as a worded badge — "Périmé", "À consommer
//! bientôt" — never as a colour alone. `/stocks?sort=expires_on` lists soonest
//! first.

pub mod detail;
pub mod edit;
pub mod list;
pub mod new;
pub mod photo;

use axum::http::HeaderMap;
use axum::response::Html;
use chrono::NaiveDate;
use leptos::prelude::*;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::dto::stocks::PlannedUsage;
use manage_our_home_shared::validation::stocks::{format_quantity, ExpiryStatus};
use uuid::Uuid;

use crate::app::{shell, Width};
use crate::family::{active_group_id_from_headers, resolve_active_group};
use crate::routes::groups::{cookie_of, header_with_groups};
use crate::state::AppState;

/// Resolved active-family context for a Stocks page: the family id every
/// `/groups/:gid/…` API call is scoped to, the caller's role in it (for the
/// permission bar), and the shared authenticated header. `None` means the user
/// has no group yet — callers redirect to `/groups/new`.
pub(crate) struct FamilyContext {
    pub gid: Uuid,
    /// The family's name, which the add sheet is headed with (#527).
    pub name: String,
    pub role: String,
    pub header: String,
}

pub(crate) async fn family_context(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    redirect_to: &str,
) -> Option<FamilyContext> {
    let (groups, header) = header_with_groups(state, headers, me, redirect_to).await;
    let preferred = active_group_id_from_headers(headers);
    let active: Option<&GroupSummary> = resolve_active_group(&groups, preferred);
    active.map(|g| FamilyContext {
        gid: g.group_id,
        name: g.name.clone(),
        role: g.role.clone(),
        header,
    })
}

/// Mirror of `apps/api/src/stocks/mod.rs::can_modify`: the item's creator, or a
/// group owner/admin, may edit the full record or delete it. Used to decide
/// whether to render the edit/delete controls (the backend still 403s a forged
/// request). Quantity adjustment is *not* gated by this — it's open to any
/// member (issue #39).
pub(crate) fn can_modify(role: &str, is_creator: bool) -> bool {
    is_creator || role == "owner" || role == "admin"
}

/// Fetches `cookie_of` for a Stocks handler (re-exported so the submodules
/// don't each reach into `routes::groups`).
pub(crate) fn stocks_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_of(headers)
}

// -- shared error/landing pages ---------------------------------------------

pub(crate) fn item_not_found_page() -> Html<String> {
    let body = view! {
        <h1>"Article introuvable"</h1>
        <p>"Cet article n'existe pas ou vous n'y avez pas accès."</p>
        <a class="btn secondary" href="/stocks">"Retour aux stocks"</a>
    };
    Html(shell(Width::Form, "Article introuvable", &body.to_html()))
}

pub(crate) fn service_unavailable_page() -> Html<String> {
    let body = view! {
        <h1>"Service momentanément indisponible"</h1>
        <p>"Merci de réessayer dans quelques instants."</p>
        <a class="btn secondary" href="/stocks">"Retour aux stocks"</a>
    };
    Html(shell(Width::Form, "Service indisponible", &body.to_html()))
}

pub(crate) fn forbidden_page() -> Html<String> {
    let body = view! {
        <h1>"Action non autorisée"</h1>
        <p>"Vous n'avez pas les droits nécessaires sur cet article."</p>
        <a class="btn secondary" href="/stocks">"Retour aux stocks"</a>
    };
    Html(shell(Width::Form, "Action non autorisée", &body.to_html()))
}

/// The worded badge for an expiry status, as `(class, text)`: the text is what
/// carries the status, the `warn` fill only doubles it (DESIGN.md — colour is
/// never the sole signal). A far-off or missing date gets no badge.
pub(crate) fn expiry_badge(status: ExpiryStatus) -> Option<(&'static str, &'static str)> {
    match status {
        ExpiryStatus::Expired => Some(("badge warn", "Périmé")),
        ExpiryStatus::Soon => Some(("badge", "À consommer bientôt")),
        ExpiryStatus::Ok | ExpiryStatus::Unknown => None,
    }
}

/// An expiry date as the French civil date the rest of the app writes
/// (`09/10/2026`, same as budget and recipes).
pub(crate) fn fmt_date(d: NaiveDate) -> String {
    d.format("%d/%m/%Y").to_string()
}

/// Formats an `f64` quantity/threshold for display and form pre-fill without a
/// trailing `.0` on whole numbers (`2.0` → `"2"`, `0.5` → `"0.5"`).
pub(crate) fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        // Trim trailing zeros from a fixed rendering.
        let s = format!("{n}");
        s
    }
}

/// « Prévu : n recette(s) » on a list card (#712): how many planned recipes
/// take from the article (`planned_usage`, one entry per recipe). `None`
/// when none does, or when the response does not carry the field.
pub(crate) fn planned_count(usage: Option<&[PlannedUsage]>) -> Option<String> {
    match usage.map(<[PlannedUsage]>::len) {
        None | Some(0) => None,
        Some(1) => Some("Prévu : 1 recette".to_string()),
        Some(n) => Some(format!("Prévu : {n} recettes")),
    }
}

/// What the planned recipes take from an article in all, against its stock
/// (#712), each written by `format_quantity` in the article's unit — the
/// unit `planned_usage` is given in. `short_by` is what the stock lacks,
/// `None` when it covers the total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedTotal {
    pub total: String,
    pub stock: String,
    pub short_by: Option<String>,
}

/// The total of `usage` against `quantity`, `None` when no planned recipe
/// uses the article. Optional ingredients count: a recipe listed takes its
/// share. The comparison is made on the quantities as written — rounded to
/// three decimals, like `format_quantity` — so a total that reads equal to
/// the stock is never said to exceed it.
///
/// apps/api caps each recipe's share at what the line holds
/// (`recipes::outcome`, `cap(still, left)`): one recipe alone never makes
/// `short_by` appear, and with several the shortfall is a lower bound —
/// two recipes wanting 600 g each from 500 g read « Il manque 500 g » where
/// 700 g are missing.
pub(crate) fn planned_total(
    usage: &[PlannedUsage],
    quantity: f64,
    unit: &str,
) -> Option<PlannedTotal> {
    if usage.is_empty() {
        return None;
    }
    // Thousandths, as `format_quantity` writes them.
    let thousandths = |q: f64| (q * 1000.0).round() as i64;
    let total: f64 = usage.iter().map(|u| u.quantity).sum();
    let missing = thousandths(total) - thousandths(quantity);
    Some(PlannedTotal {
        total: format_quantity(total, unit),
        stock: format_quantity(quantity, unit),
        short_by: (missing > 0).then(|| format_quantity(missing as f64 / 1000.0, unit)),
    })
}

/// The message for `quantity_too_large` on the stock forms and the adjust
/// action (#649). The bound is written out in French; a test ties it to
/// `MAX_QUANTITY`.
pub(crate) const QUANTITY_TOO_LARGE_MESSAGE: &str =
    "La quantité doit être un nombre d'au plus 1 000 000 000.";

/// The message for `reorder_threshold_too_large` (#649).
pub(crate) const THRESHOLD_TOO_LARGE_MESSAGE: &str =
    "Le seuil de réappro doit être un nombre d'au plus 1 000 000 000.";

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_too_large_messages_write_the_shared_bound() {
        use manage_our_home_shared::validation::stocks::MAX_QUANTITY;
        // 1000000000 → « 1 000 000 000 ».
        let digits = (MAX_QUANTITY as u64).to_string();
        let mut grouped = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                grouped.push(' ');
            }
            grouped.push(c);
        }
        let bound = format!("au plus {grouped}.");
        assert!(QUANTITY_TOO_LARGE_MESSAGE.ends_with(&bound), "{bound}");
        assert!(THRESHOLD_TOO_LARGE_MESSAGE.ends_with(&bound), "{bound}");
    }

    #[test]
    fn an_expired_or_close_date_is_worded_not_only_coloured() {
        assert_eq!(
            expiry_badge(ExpiryStatus::Expired),
            Some(("badge warn", "Périmé"))
        );
        assert_eq!(
            expiry_badge(ExpiryStatus::Soon),
            Some(("badge", "À consommer bientôt"))
        );
    }

    #[test]
    fn a_far_or_missing_date_gets_no_badge() {
        assert_eq!(expiry_badge(ExpiryStatus::Ok), None);
        assert_eq!(expiry_badge(ExpiryStatus::Unknown), None);
    }

    // -- planned_count / planned_total (#712) --------------------------------

    pub(crate) fn usage(n: u128, name: &str, quantity: f64, unit: &str) -> PlannedUsage {
        PlannedUsage {
            recipe_id: Uuid::from_u128(n),
            recipe_name: name.to_string(),
            quantity,
            unit: unit.to_string(),
            is_optional: false,
        }
    }

    #[test]
    fn no_planned_recipe_counts_nothing() {
        assert_eq!(planned_count(None), None);
        assert_eq!(planned_count(Some(&[])), None);
    }

    #[test]
    fn the_planned_recipes_are_counted_singular_then_plural() {
        let one = [usage(1, "Gratin", 200.0, "g")];
        assert_eq!(
            planned_count(Some(&one)).as_deref(),
            Some("Prévu : 1 recette")
        );
        let three = [
            usage(1, "Gratin", 200.0, "g"),
            usage(2, "Quiche", 100.0, "g"),
            usage(3, "Soupe", 0.0, "g"),
        ];
        assert_eq!(
            planned_count(Some(&three)).as_deref(),
            Some("Prévu : 3 recettes")
        );
    }

    #[test]
    fn no_usage_has_no_total() {
        assert_eq!(planned_total(&[], 3.0, "paquet"), None);
    }

    #[test]
    fn one_recipe_within_the_stock_lacks_nothing() {
        assert_eq!(
            planned_total(&[usage(1, "Pâtes bolo", 1.0, "paquet")], 3.0, "paquet"),
            Some(PlannedTotal {
                total: "1 paquet".to_string(),
                stock: "3 paquets".to_string(),
                short_by: None,
            })
        );
    }

    #[test]
    fn several_recipes_add_up_optional_ones_included() {
        let mut optional = usage(3, "Salade", 50.0, "g");
        optional.is_optional = true;
        let total = planned_total(
            &[
                usage(1, "Gratin", 200.0, "g"),
                usage(2, "Quiche", 150.0, "g"),
                optional,
            ],
            400.0,
            "g",
        )
        .unwrap();
        assert_eq!(total.total, "400 g");
        assert_eq!(total.stock, "400 g");
        // Equal to the stock: it covers the total.
        assert_eq!(total.short_by, None);
    }

    #[test]
    fn a_total_above_the_stock_says_what_is_missing() {
        let total = planned_total(
            &[
                usage(1, "Pâtes bolo", 2.0, "paquet"),
                usage(2, "Gratin", 1.5, "paquet"),
            ],
            2.0,
            "paquet",
        )
        .unwrap();
        assert_eq!(total.total, "3,5 paquets");
        assert_eq!(total.stock, "2 paquets");
        assert_eq!(total.short_by.as_deref(), Some("1,5 paquet"));
        // Nothing in stock: all of it is missing.
        let total = planned_total(&[usage(1, "Crêpes", 0.5, "L")], 0.0, "L").unwrap();
        assert_eq!(total.short_by.as_deref(), Some("0,5 L"));
    }

    #[test]
    fn a_total_that_reads_equal_to_the_stock_does_not_exceed_it() {
        // 0.1 + 0.2 is 0.30000000000000004 in f64.
        let total = planned_total(
            &[usage(1, "A", 0.1, "kg"), usage(2, "B", 0.2, "kg")],
            0.3,
            "kg",
        )
        .unwrap();
        assert_eq!(total.total, "0,3 kg");
        assert_eq!(total.short_by, None);
        // Under a thousandth over still reads equal, and lacks nothing.
        let total = planned_total(&[usage(1, "A", 1.0004, "kg")], 1.0, "kg").unwrap();
        assert_eq!(total.short_by, None);
        let total = planned_total(&[usage(1, "A", 1.001, "kg")], 1.0, "kg").unwrap();
        assert_eq!(total.short_by.as_deref(), Some("0,001 kg"));
    }

    #[test]
    fn an_expiry_date_reads_day_first() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        assert_eq!(fmt_date(d), "09/10/2026");
    }
}
