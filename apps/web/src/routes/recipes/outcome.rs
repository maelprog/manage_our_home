//! `/recipes/:id/done` and `/recipes/:id/abandon` (#474) — marking a
//! recipe « À réaliser » done or abandoned, through apps/api's
//! `recipes::outcome` (#473).
//!
//! Each action is a confirmation page that needs no JavaScript: `GET` shows
//! apps/api's preview as a form, `POST` applies it with the corrections and
//! goes back to the page the member came from (`back`, as for « À
//! réaliser »).
//!
//! - **Faite**: one line per stock line the preview takes from, its
//!   quantity editable and a « Retirer du stock » box checked: every
//!   ingredient is proposed for removal, the member unchecks those not
//!   used (user precision of 2026-10-21). 0 removes nothing either. The
//!   meal date defaults to today in Paris. « Autre chose utilisé ? » (#540)
//!   searches the group's stock for products the recipe does not call for
//!   — « Chercher » shows the form again with what it finds, unchecked —
//!   and takes those checked, with their quantity. Once one the recipe
//!   lacks is chosen, a member who may change the recipe is asked « Ajouter
//!   X, Y à la recette pour la prochaine fois ? », Oui or Non, before the
//!   form is sent.
//! - **Abandonner**: the unchecked grocery items generated for the recipe,
//!   each with a « Retirer » box checked by default.
//!
//! Before sending a `done`, the form is checked against a fresh preview
//! (`validate_done`): a quantity that does not read, or above what the line
//! holds, is shown on its line with what was typed. A refusal of apps/api
//! for a change made since the page was shown (a stock line lowered or
//! gone, a grocery item checked or deleted), or a form whose lines are no
//! longer the preview's, shows the updated preview with a banner rather
//! than the typed values: they were corrections of another proposal.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::NaiveDate;
use manage_our_home_shared::dto::recipes::{
    AbandonItem, AbandonPreview, AbandonRequest, DoneIngredient, DoneOther, DonePreview,
    DoneRemoval, DoneRequest, RemovalWarning,
};
use manage_our_home_shared::dto::stocks::{StockItemList, StockItemResponse};
use manage_our_home_shared::validation::stocks::{article_key, format_quantity};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::agenda::new::is_form_urlencoded;
use crate::routes::stocks::{fmt_date, fmt_num};
use crate::state::{api_request_auth, AppState};

use super::{
    back_path, back_with, family_context, forbidden_page, recipe_not_found_page, recipes_cookie,
    service_unavailable_page,
};

/// The notice of the list and of the recipe after a `done`.
pub(crate) const DONE_NOTICE: &str =
    "Recette faite : le stock est mis à jour et le repas enregistré.";
/// The notice after an `abandoned`.
pub(crate) const ABANDONED_NOTICE: &str = "Recette abandonnée : elle n'est plus à réaliser.";
/// The error when the recipe is no longer « À réaliser ».
pub(crate) const SELECTION_GONE: &str =
    "Cette recette n'est plus à réaliser : un autre membre l'a peut-être déjà marquée faite ou abandonnée.";

// -- Pure part ----------------------------------------------------------------

/// One stock line of the `done` form, as submitted: the text of its
/// fields, parsed only by `validate_done`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct DoneLine {
    pub stock_item_id: Uuid,
    pub quantity: String,
    pub remove: bool,
}

/// A product of the « Autre chose utilisé ? » section (#540), as submitted.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct OtherLine {
    pub stock_item_id: Uuid,
    pub quantity: String,
    pub used: bool,
}

/// The `done` form as submitted.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct DoneForm {
    pub lines: Vec<DoneLine>,
    /// The other products listed, chosen (`used`) or not.
    pub others: Vec<OtherLine>,
    /// The text of the stock search, as typed.
    pub search: String,
    /// The search button was pressed: show the form again with what it
    /// finds, without sending anything.
    pub searching: bool,
    /// « Ajouter … à la recette ? »: `Some(true)` for Oui, `None` when not
    /// answered.
    pub add_to_recipe: Option<bool>,
    pub eaten_on: String,
    pub back: String,
}

/// What stops a stock line from being taken from as typed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LineError {
    /// Empty, negative, or not a number.
    InvalidQuantity,
    /// More than the line holds, which is `available`.
    TooMuch { available: f64 },
}

/// The fields of a `done` form in error, all of them at once.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Invalid {
    /// One entry per line, `Some` on the lines in error; empty when none.
    pub lines: Vec<Option<LineError>>,
    /// The same for the form's other products; empty when none.
    pub others: Vec<Option<LineError>>,
    /// The meal date does not read.
    pub date: bool,
    /// « Ajouter … à la recette ? » is asked and not answered.
    pub add_to_recipe: bool,
}

/// Why a `done` form is not sent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DoneError {
    /// Fields to correct.
    Invalid(Invalid),
    /// The form's lines are not the preview's any more: the stock or the
    /// recipe changed since the page was shown.
    Stale,
}

/// Reads the `done` form back: lines numbered `stock.i`, `quantity.i`,
/// `remove.i`, and other products numbered `other.i`, `other_quantity.i`,
/// `other_used.i`, each in their numeric order. A line without a valid
/// stock id is dropped; a repeated field keeps its first occurrence. `q` is
/// the search, `intent=search` its button, `add_to_recipe` `yes` or `no`.
pub(crate) fn parse_done_form(pairs: &[(String, String)]) -> DoneForm {
    use std::collections::btree_map::{BTreeMap, Entry};
    let (mut eaten_on, mut back, mut search, mut intent, mut add) = (None, None, None, None, None);
    // line index → field name → value, for the recipe's lines and for the
    // other products.
    let mut fields: BTreeMap<usize, BTreeMap<&str, &str>> = BTreeMap::new();
    let mut other_fields: BTreeMap<usize, BTreeMap<&str, &str>> = BTreeMap::new();
    for (key, value) in pairs {
        let value = value.as_str();
        match key.split_once('.') {
            None => {
                let slot = match key.as_str() {
                    "eaten_on" => &mut eaten_on,
                    "back" => &mut back,
                    "q" => &mut search,
                    "intent" => &mut intent,
                    "add_to_recipe" => &mut add,
                    _ => continue,
                };
                slot.get_or_insert(value);
            }
            Some((field, index)) => {
                let Ok(index) = index.parse::<usize>() else {
                    continue;
                };
                let map = if field.starts_with("other") {
                    &mut other_fields
                } else {
                    &mut fields
                };
                if let Entry::Vacant(slot) = map.entry(index).or_default().entry(field) {
                    slot.insert(value);
                }
            }
        }
    }
    let id = |v: Option<&&str>| v?.trim().parse::<Uuid>().ok();
    DoneForm {
        lines: fields
            .into_values()
            .filter_map(|f| {
                Some(DoneLine {
                    stock_item_id: id(f.get("stock"))?,
                    quantity: f.get("quantity").copied().unwrap_or_default().to_string(),
                    remove: f.contains_key("remove"),
                })
            })
            .collect(),
        others: other_fields
            .into_values()
            .filter_map(|f| {
                Some(OtherLine {
                    stock_item_id: id(f.get("other"))?,
                    quantity: f
                        .get("other_quantity")
                        .copied()
                        .unwrap_or_default()
                        .to_string(),
                    used: f.contains_key("other_used"),
                })
            })
            .collect(),
        search: search.unwrap_or_default().to_string(),
        searching: intent == Some("search"),
        add_to_recipe: match add {
            Some("yes") => Some(true),
            Some("no") => Some(false),
            _ => None,
        },
        eaten_on: eaten_on.unwrap_or_default().to_string(),
        back: back.unwrap_or_default().to_string(),
    }
}

/// `x` without the binary noise of a sum (0.1 + 0.2 is 0.3), not rounded:
/// twelve significant digits, the rule apps/api compares the stock with
/// (`grocery_list::planned::without_binary_noise`, decision of 2026-10-25).
/// A coarser rule would accept here what apps/api refuses.
fn without_noise(x: f64) -> f64 {
    format!("{x:.11e}").parse().unwrap_or(x)
}

/// A typed quantity: 0 or more, a comma read as a decimal point.
fn parse_quantity(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|q| q.is_finite() && *q >= 0.0)
}

/// Checks the form against a fresh preview and builds the request. The
/// lines must be the preview's lots, in order; each line kept for removal
/// needs a quantity of 0 or more, at most what its stock line holds. Two
/// ingredients of the same name take from the same line: their lines are
/// sent as one, the sum of what is kept for removal. Each other product
/// chosen must still be in the group's `stock`, with a quantity above 0
/// and at most what its line holds. « Ajouter … à la recette ? » must be
/// answered when it is asked (`offered_names`, to who may change the
/// recipe); `add_to_recipe` is sent only then, for Oui.
pub(crate) fn validate_done(
    form: &DoneForm,
    preview: &DonePreview,
    stock: &[StockItemResponse],
) -> Result<DoneRequest, DoneError> {
    let lots: Vec<_> = preview.ingredients.iter().flat_map(|i| &i.lots).collect();
    if form.lines.len() != lots.len()
        || form
            .lines
            .iter()
            .zip(&lots)
            .any(|(line, lot)| line.stock_item_id != lot.stock_item.id)
    {
        return Err(DoneError::Stale);
    }
    // stock line → (sum kept for removal, whether a line keeps it), in the
    // order the lines first name them.
    let mut taken: Vec<(Uuid, f64, bool)> = Vec::new();
    let mut errors = Vec::with_capacity(lots.len());
    for (line, lot) in form.lines.iter().zip(&lots) {
        let slot = match taken.iter().position(|t| t.0 == line.stock_item_id) {
            Some(i) => i,
            None => {
                taken.push((line.stock_item_id, 0.0, false));
                taken.len() - 1
            }
        };
        if !line.remove {
            errors.push(None);
            continue;
        }
        let Some(quantity) = parse_quantity(&line.quantity) else {
            errors.push(Some(LineError::InvalidQuantity));
            continue;
        };
        let entry = &mut taken[slot];
        entry.1 = without_noise(entry.1 + quantity);
        entry.2 = true;
        let available = without_noise(lot.stock_item.quantity);
        errors.push((entry.1 > available).then_some(LineError::TooMuch { available }));
    }
    // The other products chosen: each in the group's stock, else the page
    // is stale; above 0, at most what its line holds.
    let mut others = Vec::new();
    let mut used = Vec::new();
    let mut other_errors = Vec::with_capacity(form.others.len());
    for other in &form.others {
        if !other.used {
            other_errors.push(None);
            continue;
        }
        let Some(line) = stock.iter().find(|s| s.id == other.stock_item_id) else {
            return Err(DoneError::Stale);
        };
        let Some(quantity) = parse_quantity(&other.quantity).filter(|q| *q > 0.0) else {
            other_errors.push(Some(LineError::InvalidQuantity));
            continue;
        };
        let available = without_noise(line.quantity);
        if quantity > available {
            other_errors.push(Some(LineError::TooMuch { available }));
            continue;
        }
        other_errors.push(None);
        used.push(line);
        others.push(DoneOther {
            stock_item_id: line.id,
            quantity,
        });
    }
    // Asked only of who may change the recipe, about products it lacks.
    let asked = preview.can_edit_recipe && !offered_names(preview, &used).is_empty();
    let unanswered = asked && form.add_to_recipe.is_none();
    let eaten_on = match form.eaten_on.trim() {
        "" => Ok(None),
        d => d.parse::<NaiveDate>().map(Some),
    };
    // Every field in error at once, the date with the lines.
    let lines_ok = errors.iter().all(Option::is_none);
    let others_ok = other_errors.iter().all(Option::is_none);
    let eaten_on = match eaten_on {
        Ok(d) if lines_ok && others_ok && !unanswered => d,
        date => {
            return Err(DoneError::Invalid(Invalid {
                lines: if lines_ok { vec![] } else { errors },
                others: if others_ok { vec![] } else { other_errors },
                date: date.is_err(),
                add_to_recipe: unanswered,
            }))
        }
    };
    Ok(DoneRequest {
        others,
        add_to_recipe: asked && form.add_to_recipe == Some(true),
        removals: taken
            .into_iter()
            .map(|(stock_item_id, quantity, kept)| DoneRemoval {
                stock_item_id,
                quantity: if kept { quantity } else { 0.0 },
                exclude: !kept,
            })
            .collect(),
        eaten_on,
    })
}

/// The most stock lines a search lists.
pub(crate) const SEARCH_MAX: usize = 10;

/// The stock lines whose name contains `search` (`article_key`: trimmed,
/// whatever the case), but those of `exclude`, by name, at most
/// `SEARCH_MAX`. Nothing for a blank search.
pub(crate) fn search_stock<'a>(
    stock: &'a [StockItemResponse],
    search: &str,
    exclude: &[Uuid],
) -> Vec<&'a StockItemResponse> {
    let search = article_key(search);
    if search.is_empty() {
        return vec![];
    }
    let mut found: Vec<_> = stock
        .iter()
        .filter(|s| !exclude.contains(&s.id) && article_key(&s.name).contains(&search))
        .collect();
    found.sort_by_cached_key(|s| (article_key(&s.name), s.id));
    found.truncate(SEARCH_MAX);
    found
}

/// A product of the « Autre chose utilisé ? » section, as shown.
#[derive(Debug, Clone)]
pub(crate) struct OtherShown {
    pub stock: StockItemResponse,
    pub quantity: String,
    pub used: bool,
    pub error: Option<LineError>,
}

/// The « Autre chose utilisé ? » section as shown.
#[derive(Debug, Clone, Default)]
pub(crate) struct Others {
    pub lines: Vec<OtherShown>,
    /// The search field's text.
    pub search: String,
    /// The lines include what `search` found, which may be nothing.
    pub searched: bool,
}

/// The section of a form shown again: its other products found in `stock`,
/// as typed, with `errors` (one per product of the form, or empty). After
/// a search (`search`), only the products chosen are kept, followed by the
/// lines the search finds, unchecked and empty, but the recipe's lots and
/// the products already listed.
pub(crate) fn others_section(
    form: &DoneForm,
    preview: &DonePreview,
    stock: &[StockItemResponse],
    errors: &[Option<LineError>],
    search: bool,
) -> Others {
    let mut lines: Vec<OtherShown> = form
        .others
        .iter()
        .enumerate()
        .filter(|(_, o)| !search || o.used)
        .filter_map(|(n, o)| {
            Some(OtherShown {
                stock: stock.iter().find(|s| s.id == o.stock_item_id)?.clone(),
                quantity: o.quantity.clone(),
                used: o.used,
                error: errors.get(n).copied().flatten(),
            })
        })
        .collect();
    let searched = search && !article_key(&form.search).is_empty();
    if searched {
        let exclude: Vec<Uuid> = preview
            .ingredients
            .iter()
            .flat_map(|i| i.lots.iter().map(|l| l.stock_item.id))
            .chain(lines.iter().map(|l| l.stock.id))
            .collect();
        lines.extend(
            search_stock(stock, &form.search, &exclude)
                .into_iter()
                .map(|s| OtherShown {
                    stock: s.clone(),
                    quantity: String::new(),
                    used: false,
                    error: None,
                }),
        );
    }
    Others {
        lines,
        search: form.search.clone(),
        searched,
    }
}

/// The names « Ajouter … à la recette ? » asks about: those of the
/// products `used` the recipe has no ingredient of (`article_key`), once
/// each, in order.
pub(crate) fn offered_names(preview: &DonePreview, used: &[&StockItemResponse]) -> Vec<String> {
    let mut keys: Vec<String> = preview
        .ingredients
        .iter()
        .map(|i| article_key(&i.name))
        .collect();
    let mut names = Vec::new();
    for s in used {
        let key = article_key(&s.name);
        if !keys.contains(&key) {
            keys.push(key);
            names.push(s.name.trim().to_string());
        }
    }
    names
}

/// What a 400 or 409 of apps/api to a sent form means for the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Refusal {
    /// Something changed since the preview the form was checked against:
    /// the updated preview is shown.
    Changed,
    /// The request itself is refused: a fault of this page, which showing
    /// the same form again as « changed » would never clear.
    Rejected,
}

/// The refusal of a `done`: a 409 (a stock line lowered or gone) or a 404
/// (an other product gone from the stock, or the selection closed: the
/// preview read again tells them apart) is a change; a 400 is a rejected
/// request.
pub(crate) fn done_refusal(status: reqwest::StatusCode) -> Refusal {
    if status == reqwest::StatusCode::CONFLICT || status == reqwest::StatusCode::NOT_FOUND {
        Refusal::Changed
    } else {
        Refusal::Rejected
    }
}

/// Reads the `abandon` form back: the grocery items to remove (`remove`,
/// once each) and `back`. `None` when an id does not read.
pub(crate) fn parse_abandon_form(pairs: &[(String, String)]) -> Option<(Vec<Uuid>, String)> {
    let mut remove = Vec::new();
    let mut back = None;
    for (key, value) in pairs {
        match key.as_str() {
            "remove" => {
                let id = value.trim().parse::<Uuid>().ok()?;
                if !remove.contains(&id) {
                    remove.push(id);
                }
            }
            "back" => {
                back.get_or_insert(value.as_str());
            }
            _ => {}
        }
    }
    Some((remove, back.unwrap_or_default().to_string()))
}

// -- Rendering ----------------------------------------------------------------

/// `back` as the forms carry it: `list`, `planned` (the « Prévues » tab,
/// #539), or the recipe.
fn back_value(back: &str) -> &'static str {
    match back {
        "list" => "list",
        "planned" => "planned",
        _ => "detail",
    }
}

/// A quantity and its unit, as the page shows them: `400 g`, `2`, or
/// nothing.
fn amount(quantity: Option<f64>, unit: Option<&str>) -> Option<String> {
    match (quantity, unit.map(str::trim).filter(|u| !u.is_empty())) {
        (Some(q), Some(u)) => Some(format!("{} {u}", fmt_num(q))),
        (Some(q), None) => Some(fmt_num(q)),
        (None, _) => None,
    }
}

/// What a stock line holds, and its date.
fn held(stock: &StockItemResponse) -> String {
    let held = format_quantity(stock.quantity, &stock.unit);
    match stock.expires_on {
        Some(d) => format!("{held}, péremption le {}", fmt_date(d)),
        None => held,
    }
}

/// Why the preview proposes 0, or less than the recipe. `first_user` is
/// the label of an earlier ingredient taking from the same stock line:
/// apps/api serves it first, so a capped proposal here is what it left.
fn warning_text(ingredient: &DoneIngredient, first_user: Option<&str>) -> Option<String> {
    let stock_unit = ingredient
        .lots
        .first()
        .map(|l| l.stock_item.unit.as_str())
        .unwrap_or_default();
    match ingredient.warning? {
        RemovalWarning::NoStock => {
            Some("Aucun article de ce nom en stock : rien à retirer.".to_string())
        }
        RemovalWarning::UnitMismatch => Some(format!(
            "Le stock est en {stock_unit} et la recette en {recipe} : saisissez la quantité en {stock_unit}.",
            recipe = ingredient
                .unit
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .unwrap_or("unité"),
        )),
        RemovalWarning::NoQuantity => {
            Some("La recette ne donne pas de quantité : saisissez-la.".to_string())
        }
        RemovalWarning::Capped => Some(match first_user {
            // As the stock list writes a quantity: « 4 », not « 4 unité ».
            Some(first) if ingredient.proposed_quantity > 0.0 => format!(
                "Ce stock sert d'abord à {first} : il en reste {} pour cet ingrédient.",
                format_quantity(ingredient.proposed_quantity, stock_unit)
            ),
            Some(first) => {
                format!("Ce stock sert d'abord à {first} : il n'en reste rien pour cet ingrédient.")
            }
            None => format!(
                "Le stock n'en contient que {} : tout est proposé.",
                format_quantity(ingredient.proposed_quantity, stock_unit)
            ),
        }),
        RemovalWarning::Other => None,
    }
}

/// The message of a line in error, in the unit of its stock line, written
/// as the stock list writes a quantity: « 4 », not « 4 unité ».
/// `sharing` are the labels of the ingredients taking from that line, when
/// there are several: what it holds is for them together.
fn line_error_text(error: LineError, unit: &str, sharing: &[&str]) -> String {
    match error {
        LineError::InvalidQuantity => {
            "Saisissez une quantité de 0 ou plus, ou décochez « Retirer du stock ».".to_string()
        }
        LineError::TooMuch { available } if sharing.len() > 1 => {
            let (last, others) = sharing.split_last().expect("several labels");
            format!(
                "Le stock n'en contient que {} pour {} et {last} ensemble.",
                format_quantity(available, unit),
                others.join(", ")
            )
        }
        LineError::TooMuch { available } => {
            format!(
                "Le stock n'en contient que {}.",
                format_quantity(available, unit)
            )
        }
    }
}

/// How each ingredient is named on the page: its name, numbered (« n° 2 »)
/// when the recipe has several of that name, so that their fields do not
/// carry the same label.
fn ingredient_labels(ingredients: &[DoneIngredient]) -> Vec<String> {
    let keys: Vec<String> = ingredients.iter().map(|i| article_key(&i.name)).collect();
    ingredients
        .iter()
        .enumerate()
        .map(|(n, ingredient)| {
            if keys.iter().filter(|k| **k == keys[n]).count() > 1 {
                let rank = keys[..=n].iter().filter(|k| **k == keys[n]).count();
                format!("{} (n° {rank})", ingredient.name)
            } else {
                ingredient.name.clone()
            }
        })
        .collect()
}

/// The `done` page: the preview as a form, with what was typed (`typed`)
/// and the fields in error (`invalid`) when the form comes back, and the
/// « Autre chose utilisé ? » section (`others`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn done_body(
    recipe_id: Uuid,
    preview: &DonePreview,
    typed: Option<&DoneForm>,
    invalid: &Invalid,
    banner: &str,
    back: &str,
    today: NaiveDate,
    others: &Others,
) -> String {
    let back = back_value(back);
    let cancel = back_path(back, recipe_id);
    let lot_count: usize = preview.ingredients.iter().map(|i| i.lots.len()).sum();
    // What was typed, only when it is a form of these very lots.
    let typed_lines = typed
        .map(|t| t.lines.as_slice())
        .filter(|l| l.len() == lot_count);
    let eaten_on = typed
        .map(|t| t.eaten_on.clone())
        .unwrap_or_else(|| today.to_string());
    let labels = ingredient_labels(&preview.ingredients);
    // The ingredients taking from a stock line, in the recipe's order.
    let users = |stock_item_id: Uuid| -> Vec<&str> {
        preview
            .ingredients
            .iter()
            .zip(&labels)
            .filter(|(i, _)| i.lots.iter().any(|l| l.stock_item.id == stock_item_id))
            .map(|(_, label)| label.as_str())
            .collect()
    };
    let mut i = 0;
    let mut fieldsets = String::new();
    for (ingredient, label) in preview.ingredients.iter().zip(&labels) {
        let name = html_escape(label);
        let mut legend = name.clone();
        if let Some(a) = amount(ingredient.quantity, ingredient.unit.as_deref()) {
            legend.push_str(&format!(" · {} pour la recette", html_escape(&a)));
        }
        if ingredient.is_optional {
            legend.push_str(" · optionnel");
        }
        // An earlier ingredient taking from one of the same lines.
        let first_user = ingredient
            .lots
            .iter()
            .filter_map(|l| users(l.stock_item.id).into_iter().next())
            .find(|first| *first != label.as_str());
        let warning = warning_text(ingredient, first_user)
            .map(|w| format!("<p class=\"notice warning\">{}</p>\n", html_escape(&w)))
            .unwrap_or_default();
        // Two lots of one ingredient would otherwise carry the same label.
        let several = ingredient.lots.len() > 1;
        let mut lines = String::new();
        for lot in &ingredient.lots {
            let stock = &lot.stock_item;
            let (quantity, remove) = match typed_lines.map(|l| &l[i]) {
                Some(t) => (t.quantity.clone(), t.remove),
                None => (fmt_num(lot.quantity), true),
            };
            let (error, aria) = field_error(
                &format!("quantity.{i}"),
                invalid
                    .lines
                    .get(i)
                    .copied()
                    .flatten()
                    .map(|e| line_error_text(e, &stock.unit, &users(stock.id))),
            );
            let lot_label = if several {
                format!(" (lot de {})", html_escape(&held(stock)))
            } else {
                String::new()
            };
            lines.push_str(&format!(
                r#"<input type="hidden" name="stock.{i}" value="{id}"/>
{error}<label>Quantité de {name} à retirer, en {unit} (stock : {held})
<input type="number" name="quantity.{i}" step="any" min="0" value="{quantity}"{aria}/></label>
<label class="field inline"><input type="checkbox" name="remove.{i}"{checked}/> Retirer {name} du stock{lot_label}</label>
"#,
                id = stock.id,
                unit = html_escape(&stock.unit),
                held = html_escape(&held(stock)),
                quantity = html_escape(&quantity),
                checked = if remove { " checked" } else { "" },
            ));
            i += 1;
        }
        fieldsets.push_str(&format!(
            "<fieldset class=\"card\">\n<legend>{legend}</legend>\n{warning}{lines}</fieldset>\n"
        ));
    }
    if preview.ingredients.is_empty() {
        fieldsets =
            "<p>Cette recette n'a aucun ingrédient : rien ne sera retiré du stock.</p>\n".into();
    }
    let (date_error, date_aria) = field_error(
        "eaten_on",
        invalid
            .date
            .then(|| "La date du repas n'est pas une date valide.".to_string()),
    );
    let others_html = others_fieldset(others);
    let used: Vec<&StockItemResponse> = others
        .lines
        .iter()
        .filter(|l| l.used)
        .map(|l| &l.stock)
        .collect();
    let offered = offered_names(preview, &used);
    let question = if preview.can_edit_recipe && !offered.is_empty() {
        add_to_recipe_fieldset(
            &offered,
            typed.and_then(|t| t.add_to_recipe),
            invalid.add_to_recipe,
        )
    } else {
        String::new()
    };
    format!(
        r#"<h1>Recette faite : {name}</h1>
{banner}<p class="muted">À la validation, les quantités cochées sont retirées du stock et le repas est enregistré. Décochez un ingrédient que vous n'avez pas utilisé ; une quantité à 0 ne retire rien non plus.</p>
<form method="post" action="/recipes/{recipe_id}/done">
<input type="hidden" name="back" value="{back}"/>
{date_error}<label>Date du repas
<input type="date" name="eaten_on" value="{eaten_on}"{date_aria}/></label>
{fieldsets}{others_html}{question}<div class="actions">
<button type="submit">Marquer comme faite</button>
<a class="btn secondary" href="{cancel}">Annuler</a>
</div>
</form>"#,
        name = html_escape(&preview.selection.recipe_name),
        eaten_on = html_escape(&eaten_on),
    )
}

/// « Autre chose utilisé ? » (#540): the other products listed, each with
/// its quantity and a « Retirer … du stock » box, then the stock search.
/// Without JavaScript, « Chercher » sends the form back to be shown again
/// with what it finds; the search comes before « Marquer comme faite », so
/// Enter in a field searches rather than marks the recipe done.
fn others_fieldset(others: &Others) -> String {
    let mut lines = String::new();
    for (j, line) in others.lines.iter().enumerate() {
        let stock = &line.stock;
        let name = html_escape(stock.name.trim());
        let (error, aria) = field_error(
            &format!("other_quantity.{j}"),
            line.error.map(|e| match e {
                LineError::InvalidQuantity => format!(
                    "Saisissez une quantité supérieure à 0, ou décochez « Retirer {} du stock ».",
                    stock.name.trim()
                ),
                e => line_error_text(e, &stock.unit, &[]),
            }),
        );
        lines.push_str(&format!(
            r#"<input type="hidden" name="other.{j}" value="{id}"/>
{error}<label>Quantité de {name} à retirer, en {unit} (stock : {held})
<input type="number" name="other_quantity.{j}" step="any" min="0" value="{quantity}"{aria}/></label>
<label class="field inline"><input type="checkbox" name="other_used.{j}"{checked}/> Retirer {name} du stock</label>
"#,
            id = stock.id,
            unit = html_escape(&stock.unit),
            held = html_escape(&held(stock)),
            quantity = html_escape(&line.quantity),
            checked = if line.used { " checked" } else { "" },
        ));
    }
    // After a search only the products chosen are kept: an unchecked line
    // is one it found.
    let none_found = if others.searched && others.lines.iter().all(|l| l.used) {
        format!(
            "<p class=\"muted\">Aucun autre produit du stock ne correspond à « {} ».</p>\n",
            html_escape(others.search.trim())
        )
    } else {
        String::new()
    };
    format!(
        r#"<fieldset class="card">
<legend>Autre chose utilisé ?</legend>
<p class="muted">Cherchez dans le stock un produit que la recette ne prévoit pas, puis cochez-le avec la quantité utilisée.</p>
{lines}{none_found}<label>Chercher dans le stock
<input type="search" name="q" value="{search}"/></label>
<div class="actions"><button type="submit" name="intent" value="search" class="secondary" formnovalidate>Chercher</button></div>
</fieldset>
"#,
        search = html_escape(&others.search),
    )
}

/// « Ajouter X, Y à la recette pour la prochaine fois ? », Oui or Non,
/// with the answer given (`answer`), and its error when left unanswered.
fn add_to_recipe_fieldset(names: &[String], answer: Option<bool>, unanswered: bool) -> String {
    let (error, _) = field_error(
        "add_to_recipe",
        unanswered.then(|| {
            "Répondez à la question : ajouter ces produits à la recette ou non.".to_string()
        }),
    );
    // The group names the error; `aria-invalid` is for its fields.
    let aria = if unanswered {
        r#" aria-describedby="add_to_recipe-error""#
    } else {
        ""
    };
    let radio = |value: &str, label: &str, checked: bool| {
        format!(
            r#"<label class="field inline"><input type="radio" name="add_to_recipe" value="{value}"{checked}/> {label}</label>
"#,
            checked = if checked { " checked" } else { "" },
        )
    };
    format!(
        "<fieldset class=\"card\"{aria}>\n<legend>Ajouter {names} à la recette pour la prochaine fois ?</legend>\n{error}{yes}{no}</fieldset>\n",
        names = html_escape(&names.join(", ")),
        yes = radio("yes", "Oui", answer == Some(true)),
        no = radio("no", "Non", answer == Some(false)),
    )
}

/// The message of a field in error and the attributes tying the field to
/// it (`aria-invalid`, `aria-describedby`), or nothing.
fn field_error(name: &str, message: Option<String>) -> (String, String) {
    match message {
        Some(m) => (
            format!(
                "<p class=\"field-error\" id=\"{name}-error\">{}</p>\n",
                html_escape(&m)
            ),
            format!(r#" aria-invalid="true" aria-describedby="{name}-error""#),
        ),
        None => (String::new(), String::new()),
    }
}

/// The `abandon` page: the grocery items generated for the recipe, each
/// with a « Retirer » box checked by default, or as `checked` says when the
/// form comes back.
pub(crate) fn abandon_body(
    recipe_id: Uuid,
    preview: &AbandonPreview,
    checked: Option<&[Uuid]>,
    banner: &str,
    back: &str,
) -> String {
    let back = back_value(back);
    let cancel = back_path(back, recipe_id);
    let items = if preview.items.is_empty() {
        "<p>Aucun article de la liste de courses n'a été généré pour cette recette.</p>\n"
            .to_string()
    } else {
        let boxes: String = preview
            .items
            .iter()
            .map(|item| item_checkbox(item, checked.is_none_or(|c| c.contains(&item.id))))
            .collect();
        format!(
            r#"<fieldset class="card">
<legend>Articles de la liste de courses</legend>
<p class="muted">Les articles cochés sont retirés de la liste de courses ; décochez ceux que vous gardez.</p>
{boxes}</fieldset>
"#
        )
    };
    format!(
        r#"<h1>Abandonner : {name}</h1>
{banner}<p class="muted">La recette ne sera plus à réaliser ; rien n'est retiré du stock.</p>
<form method="post" action="/recipes/{recipe_id}/abandon">
<input type="hidden" name="back" value="{back}"/>
{items}<div class="actions">
<button type="submit">Abandonner la recette</button>
<a class="btn secondary" href="{cancel}">Annuler</a>
</div>
</form>"#,
        name = html_escape(&preview.selection.recipe_name),
    )
}

/// One grocery item of the `abandon` form, `checked` for removal or not.
fn item_checkbox(item: &AbandonItem, checked: bool) -> String {
    let what = match amount(item.quantity, item.unit.as_deref()) {
        Some(a) => format!("{} ({a})", item.name),
        None => item.name.clone(),
    };
    format!(
        "<label class=\"field inline\"><input type=\"checkbox\" name=\"remove\" value=\"{id}\"{checked}/> Retirer « {what} » de la liste</label>\n",
        id = item.id,
        what = html_escape(&what),
        checked = if checked { " checked" } else { "" },
    )
}

/// The banner of a form shown again.
fn banner(code: &str) -> String {
    let text = match code {
        "stock_changed" => "Le stock a changé depuis l'affichage de cette page : voici l'aperçu à jour. Vérifiez les quantités, puis validez à nouveau.",
        "list_changed" => "La liste de courses a changé depuis l'affichage de cette page : voici les articles à jour. Vérifiez-les, puis validez à nouveau.",
        "invalid" => "Le formulaire ne peut pas être enregistré tel quel : corrigez les champs signalés.",
        "rejected" => "Ce formulaire n'a pas pu être enregistré, à cause d'une erreur de l'application. Rechargez la page, puis réessayez ; si l'erreur revient, signalez-la.",
        _ => "Service momentanément indisponible, merci de réessayer.",
    };
    format!("<p class=\"notice error\">{}</p>\n", html_escape(text))
}

// -- Handlers -----------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct OutcomeQuery {
    #[serde(default)]
    back: String,
}

/// What reading a preview gave.
enum Preview<T> {
    Ok(T),
    /// No `planned` selection for this recipe (any more), or no such recipe.
    Gone,
    Login,
    Forbidden,
    Unavailable,
}

/// `GET …/selection/{action}`: the preview of `done` or `abandoned`.
async fn preview<T: serde::de::DeserializeOwned>(
    state: &AppState,
    gid: Uuid,
    recipe_id: Uuid,
    action: &str,
    cookie: Option<&str>,
) -> Preview<T> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/{recipe_id}/selection/{action}"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<T>(resp.body) {
                Ok(p) => Preview::Ok(p),
                Err(_) => Preview::Unavailable,
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => Preview::Gone,
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => Preview::Login,
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => Preview::Forbidden,
        Ok(_) | Err(_) => Preview::Unavailable,
    }
}

/// The answer to a preview that could not be shown (`gone` when there is
/// no selection).
async fn not_shown<T>(
    p: Preview<T>,
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
    back: &str,
    recipe_id: Uuid,
) -> Response {
    match p {
        Preview::Gone => gone(state, gid, recipe_id, cookie, back).await,
        Preview::Login => Redirect::to("/login").into_response(),
        Preview::Forbidden => forbidden_page().into_response(),
        Preview::Ok(_) | Preview::Unavailable => service_unavailable_page().into_response(),
    }
}

/// The answer when apps/api finds no « À réaliser » selection for the
/// recipe: back to where the member came from, which says so; or, when the
/// recipe itself does not exist (any more), the « Recette introuvable »
/// page, which leads to the list rather than to a recipe that would answer
/// 404 too.
async fn gone(
    state: &AppState,
    gid: Uuid,
    recipe_id: Uuid,
    cookie: Option<&str>,
    back: &str,
) -> Response {
    let recipe = api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/recipes/{recipe_id}"),
        cookie,
        None,
    )
    .await;
    // Only a definite 404 is « no such recipe »; a failed read keeps the
    // way back.
    if matches!(&recipe, Ok(r) if r.status == reqwest::StatusCode::NOT_FOUND) {
        return (StatusCode::NOT_FOUND, recipe_not_found_page()).into_response();
    }
    Redirect::to(&back_with(back, recipe_id, "error=selection_gone")).into_response()
}

fn render(header: &str, title: &str, body: &str) -> Html<String> {
    Html(shell_with_header(Width::Form, title, header, body))
}

/// Today in Paris, the day the pages judge dates in: the UTC day is still
/// the day before between midnight and 1 or 2 a.m. A meal date left empty
/// is dated by apps/api, on the same Paris day (#608).
fn today() -> NaiveDate {
    crate::routes::agenda::today_paris()
}

/// The form fields of a `POST`, or the status refusing them.
fn form_pairs(headers: &HeaderMap, body: &[u8]) -> Result<Vec<(String, String)>, StatusCode> {
    if !is_form_urlencoded(headers) {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    serde_urlencoded::from_bytes(body).map_err(|_| StatusCode::BAD_REQUEST)
}

/// `GET /recipes/:id/done`: the ingredients and the stock lines they are
/// taken from, as apps/api proposes them.
pub async fn done_get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Query(query): Query<OutcomeQuery>,
) -> Response {
    let path = format!("/recipes/{recipe_id}/done");
    let Some(fam) = family_context(&state, &headers, &me, &path).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    match preview::<DonePreview>(&state, fam.gid, recipe_id, "done", cookie.as_deref()).await {
        Preview::Ok(p) => render(
            &fam.header,
            "Recette faite",
            &done_body(
                recipe_id,
                &p,
                None,
                &Invalid::default(),
                "",
                &query.back,
                today(),
                &Others::default(),
            ),
        )
        .into_response(),
        other => {
            not_shown(
                other,
                &state,
                fam.gid,
                cookie.as_deref(),
                &query.back,
                recipe_id,
            )
            .await
        }
    }
}

/// `POST /recipes/:id/done`: checks the form against a fresh preview,
/// applies it, and goes back with a notice.
pub async fn done_post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    raw_body: axum::body::Bytes,
) -> Response {
    let pairs = match form_pairs(&headers, &raw_body) {
        Ok(p) => p,
        Err(status) => return status.into_response(),
    };
    let form = parse_done_form(&pairs);
    let back = back_value(&form.back);
    let path = format!("/recipes/{recipe_id}/done");
    let Some(fam) = family_context(&state, &headers, &me, &path).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let show = |status: StatusCode,
                p: &DonePreview,
                typed: Option<&DoneForm>,
                invalid: &Invalid,
                code: &str,
                others: &Others| {
        let banner = if code.is_empty() {
            String::new()
        } else {
            banner(code)
        };
        (
            status,
            render(
                &fam.header,
                "Recette faite",
                &done_body(recipe_id, p, typed, invalid, &banner, back, today(), others),
            ),
        )
            .into_response()
    };
    let current =
        match preview::<DonePreview>(&state, fam.gid, recipe_id, "done", cookie.as_deref()).await {
            Preview::Ok(p) => p,
            other => {
                return not_shown(other, &state, fam.gid, cookie.as_deref(), back, recipe_id).await
            }
        };
    // The group's stock, for the other products and the search only.
    let stock = if form.searching || !form.others.is_empty() {
        match group_stock(&state, fam.gid, cookie.as_deref()).await {
            Some(s) => s,
            None => {
                return show(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &current,
                    Some(&form),
                    &Invalid::default(),
                    "unavailable",
                    &Others::default(),
                )
            }
        }
    } else {
        Vec::new()
    };
    // « Chercher »: the same form, with what the search finds.
    if form.searching {
        return show(
            StatusCode::OK,
            &current,
            Some(&form),
            &Invalid::default(),
            "",
            &others_section(&form, &current, &stock, &[], true),
        );
    }
    let as_typed =
        |errors: &[Option<LineError>]| others_section(&form, &current, &stock, errors, false);
    let request = match validate_done(&form, &current, &stock) {
        Ok(r) => r,
        Err(DoneError::Stale) => {
            return show(
                StatusCode::CONFLICT,
                &current,
                None,
                &Invalid::default(),
                "stock_changed",
                &Others::default(),
            )
        }
        Err(DoneError::Invalid(invalid)) => {
            return show(
                StatusCode::UNPROCESSABLE_ENTITY,
                &current,
                Some(&form),
                &invalid,
                "invalid",
                &as_typed(&invalid.others),
            )
        }
    };
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/recipes/{}/selection/done", fam.gid, recipe_id),
        cookie.as_deref(),
        Some(serde_json::to_value(&request).unwrap()),
    )
    .await;
    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            Redirect::to(&back_with(back, recipe_id, "notice=recipe_done")).into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            Redirect::to("/login").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(resp)
            if resp.status == reqwest::StatusCode::CONFLICT
                || resp.status == reqwest::StatusCode::NOT_FOUND
                || resp.status == reqwest::StatusCode::BAD_REQUEST =>
        {
            match done_refusal(resp.status) {
                // A stock line lowered or gone since the fresh preview: the
                // updated preview, not the typed corrections of another
                // one. A 404 is that, or the selection closed since, which
                // the preview then answers.
                Refusal::Changed => match preview::<DonePreview>(
                    &state,
                    fam.gid,
                    recipe_id,
                    "done",
                    cookie.as_deref(),
                )
                .await
                {
                    Preview::Ok(p) => show(
                        StatusCode::CONFLICT,
                        &p,
                        None,
                        &Invalid::default(),
                        "stock_changed",
                        &Others::default(),
                    ),
                    other => {
                        not_shown(other, &state, fam.gid, cookie.as_deref(), back, recipe_id).await
                    }
                },
                Refusal::Rejected => {
                    tracing::error!(%recipe_id, body = %resp.body, "apps/api rejected the done request this page built");
                    show(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &current,
                        Some(&form),
                        &Invalid::default(),
                        "rejected",
                        &as_typed(&[]),
                    )
                }
            }
        }
        Ok(_) | Err(_) => show(
            StatusCode::SERVICE_UNAVAILABLE,
            &current,
            Some(&form),
            &Invalid::default(),
            "unavailable",
            &as_typed(&[]),
        ),
    }
}

/// The group's stock lines, or `None` when apps/api does not give them.
async fn group_stock(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> Option<Vec<StockItemResponse>> {
    match api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/stock-items"),
        cookie,
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            serde_json::from_value::<StockItemList>(resp.body)
                .ok()
                .map(|l| l.items)
        }
        _ => None,
    }
}

/// `GET /recipes/:id/abandon`: the grocery items generated for the recipe.
pub async fn abandon_get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    Query(query): Query<OutcomeQuery>,
) -> Response {
    let path = format!("/recipes/{recipe_id}/abandon");
    let Some(fam) = family_context(&state, &headers, &me, &path).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    match preview::<AbandonPreview>(&state, fam.gid, recipe_id, "abandoned", cookie.as_deref())
        .await
    {
        Preview::Ok(p) => render(
            &fam.header,
            "Abandonner une recette",
            &abandon_body(recipe_id, &p, None, "", &query.back),
        )
        .into_response(),
        other => {
            not_shown(
                other,
                &state,
                fam.gid,
                cookie.as_deref(),
                &query.back,
                recipe_id,
            )
            .await
        }
    }
}

/// `POST /recipes/:id/abandon`: removes the checked items, and goes back
/// with a notice.
pub async fn abandon_post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(recipe_id): Path<Uuid>,
    raw_body: axum::body::Bytes,
) -> Response {
    let pairs = match form_pairs(&headers, &raw_body) {
        Ok(p) => p,
        Err(status) => return status.into_response(),
    };
    let Some((remove, back)) = parse_abandon_form(&pairs) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let back = back_value(&back);
    let path = format!("/recipes/{recipe_id}/abandon");
    let Some(fam) = family_context(&state, &headers, &me, &path).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = recipes_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!(
            "/groups/{}/recipes/{}/selection/abandoned",
            fam.gid, recipe_id
        ),
        cookie.as_deref(),
        Some(
            serde_json::to_value(AbandonRequest {
                remove: remove.clone(),
            })
            .unwrap(),
        ),
    )
    .await;
    let (status, code) = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            return Redirect::to(&back_with(back, recipe_id, "notice=recipe_abandoned"))
                .into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNAUTHORIZED => {
            return Redirect::to("/login").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            return forbidden_page().into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            return not_shown(
                Preview::<()>::Gone,
                &state,
                fam.gid,
                cookie.as_deref(),
                back,
                recipe_id,
            )
            .await
        }
        // An item checked or deleted since the page was shown.
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            (StatusCode::CONFLICT, "list_changed")
        }
        // A request this page should never have built (an item twice).
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            tracing::error!(%recipe_id, body = %resp.body, "apps/api rejected the abandon request this page built");
            (StatusCode::INTERNAL_SERVER_ERROR, "rejected")
        }
        Ok(_) | Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    };
    match preview::<AbandonPreview>(&state, fam.gid, recipe_id, "abandoned", cookie.as_deref())
        .await
    {
        Preview::Ok(p) => (
            status,
            render(
                &fam.header,
                "Abandonner une recette",
                // The items as they are now after a change, otherwise the
                // choices just made.
                &abandon_body(
                    recipe_id,
                    &p,
                    (code != "list_changed").then_some(remove.as_slice()),
                    &banner(code),
                    back,
                ),
            ),
        )
            .into_response(),
        other => not_shown(other, &state, fam.gid, cookie.as_deref(), back, recipe_id).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::dto::recipes::{DoneLot, RecipeSelectionResponse};
    use manage_our_home_shared::validation::stocks::ExpiryStatus;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn selection(name: &str) -> RecipeSelectionResponse {
        RecipeSelectionResponse {
            id: id(900),
            recipe_id: id(901),
            recipe_name: name.into(),
            status: "planned".into(),
            servings: Some(4),
            created_by: id(902),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn stock(n: u128, name: &str, quantity: f64, unit: &str) -> StockItemResponse {
        StockItemResponse {
            id: id(n),
            group_id: id(800),
            created_by: id(801),
            name: name.into(),
            category: None,
            quantity,
            unit: unit.into(),
            reorder_threshold: None,
            low_stock: false,
            expires_on: None,
            expiry_status: ExpiryStatus::Unknown,
            barcode: None,
        }
    }

    #[test]
    fn a_stock_line_writes_its_quantity_as_the_stock_list_does() {
        assert_eq!(held(&stock(10, "Pâtes", 2.0, "paquet")), "2 paquets");
        assert_eq!(held(&stock(11, "Œufs", 6.0, "unité")), "6");
        assert_eq!(held(&stock(12, "Farine", 1.5, "kg")), "1,5 kg");
    }

    fn ingredient(
        n: u128,
        name: &str,
        quantity: Option<f64>,
        unit: Option<&str>,
        lots: Vec<(StockItemResponse, f64)>,
        warning: Option<RemovalWarning>,
    ) -> DoneIngredient {
        let proposed_quantity = lots.iter().map(|(_, q)| q).sum();
        DoneIngredient {
            ingredient_id: id(n),
            name: name.into(),
            quantity,
            unit: unit.map(str::to_string),
            is_optional: false,
            lots: lots
                .into_iter()
                .map(|(stock_item, quantity)| DoneLot {
                    stock_item,
                    quantity,
                })
                .collect(),
            proposed_quantity,
            warning,
        }
    }

    /// Farine 400 g over two lots (100 g + 500 g), Lait 1 L of 2 L.
    fn preview() -> DonePreview {
        DonePreview {
            can_edit_recipe: false,
            selection: selection("Crêpes"),
            ingredients: vec![
                ingredient(
                    1,
                    "Farine",
                    Some(400.0),
                    Some("g"),
                    vec![
                        (stock(10, "Farine", 100.0, "g"), 100.0),
                        (stock(11, "Farine", 500.0, "g"), 300.0),
                    ],
                    None,
                ),
                ingredient(
                    2,
                    "Lait",
                    Some(1.0),
                    Some("L"),
                    vec![(stock(20, "Lait", 2.0, "L"), 1.0)],
                    None,
                ),
            ],
        }
    }

    fn line(n: u128, quantity: &str, remove: bool) -> DoneLine {
        DoneLine {
            stock_item_id: id(n),
            quantity: quantity.into(),
            remove,
        }
    }

    fn form(lines: Vec<DoneLine>) -> DoneForm {
        DoneForm {
            lines,
            back: "detail".into(),
            ..DoneForm::default()
        }
    }

    fn removal(n: u128, quantity: f64, exclude: bool) -> DoneRemoval {
        DoneRemoval {
            stock_item_id: id(n),
            quantity,
            exclude,
        }
    }

    fn today() -> NaiveDate {
        "2026-10-26".parse().unwrap()
    }

    fn invalid_lines(lines: Vec<Option<LineError>>) -> DoneError {
        DoneError::Invalid(Invalid {
            lines,
            ..Invalid::default()
        })
    }

    // -- parse_done_form -----------------------------------------------------

    #[test]
    fn the_done_form_reads_its_numbered_lines_in_numeric_order() {
        let got = parse_done_form(&pairs(&[
            ("back", "list"),
            ("eaten_on", "2026-10-25"),
            ("stock.10", &id(30).to_string()),
            ("quantity.10", "3"),
            ("stock.2", &id(20).to_string()),
            ("quantity.2", "2"),
            ("remove.2", "on"),
        ]));
        assert_eq!(got.back, "list");
        assert_eq!(got.eaten_on, "2026-10-25");
        // 2 before 10: numeric order, not the text's.
        assert_eq!(got.lines, vec![line(20, "2", true), line(30, "3", false)]);
    }

    #[test]
    fn the_done_form_drops_a_line_without_a_valid_id_and_keeps_a_first_field() {
        let got = parse_done_form(&pairs(&[
            ("stock.0", "not-a-uuid"),
            ("quantity.0", "1"),
            ("stock.1", &id(20).to_string()),
            ("quantity.1", "2"),
            ("quantity.1", "9"),
            ("quantity.x", "4"),
            ("remove.1", "on"),
        ]));
        assert_eq!(got.lines, vec![line(20, "2", true)]);
    }

    // -- validate_done -------------------------------------------------------

    #[test]
    fn the_preview_as_proposed_is_sent_line_by_line() {
        let got = validate_done(
            &form(vec![
                line(10, "100", true),
                line(11, "300", true),
                line(20, "1", true),
            ]),
            &preview(),
            &[],
        );
        assert_eq!(
            got,
            Ok(DoneRequest {
                removals: vec![
                    removal(10, 100.0, false),
                    removal(11, 300.0, false),
                    removal(20, 1.0, false),
                ],
                eaten_on: None,
                ..DoneRequest::default()
            })
        );
    }

    #[test]
    fn an_unchecked_line_is_excluded_whatever_its_quantity() {
        let got = validate_done(
            &form(vec![
                line(10, "100", true),
                line(11, "not a number", false),
                line(20, "", false),
            ]),
            &preview(),
            &[],
        )
        .unwrap();
        assert_eq!(
            got.removals,
            vec![
                removal(10, 100.0, false),
                removal(11, 0.0, true),
                removal(20, 0.0, true),
            ]
        );
    }

    #[test]
    fn zero_is_accepted_and_a_comma_reads_as_a_decimal_point() {
        let got = validate_done(
            &form(vec![
                line(10, "0", true),
                line(11, " 12,5 ", true),
                line(20, "0.5", true),
            ]),
            &preview(),
            &[],
        )
        .unwrap();
        assert_eq!(
            got.removals,
            vec![
                removal(10, 0.0, false),
                removal(11, 12.5, false),
                removal(20, 0.5, false),
            ]
        );
    }

    #[test]
    fn a_quantity_that_does_not_read_is_an_error_on_its_line() {
        for bad in ["", "abc", "-1", "NaN", "inf"] {
            let got = validate_done(
                &form(vec![
                    line(10, "100", true),
                    line(11, bad, true),
                    line(20, "1", true),
                ]),
                &preview(),
                &[],
            );
            assert_eq!(
                got,
                Err(invalid_lines(vec![
                    None,
                    Some(LineError::InvalidQuantity),
                    None
                ])),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn more_than_the_line_holds_is_an_error_naming_what_it_holds() {
        // 500 g is all of the second lot: accepted; 501 is not.
        assert!(validate_done(
            &form(vec![
                line(10, "100", true),
                line(11, "500", true),
                line(20, "2", true)
            ]),
            &preview(),
            &[],
        )
        .is_ok());
        let got = validate_done(
            &form(vec![
                line(10, "100", true),
                line(11, "501", true),
                line(20, "3", true),
            ]),
            &preview(),
            &[],
        );
        assert_eq!(
            got,
            Err(invalid_lines(vec![
                None,
                Some(LineError::TooMuch { available: 500.0 }),
                Some(LineError::TooMuch { available: 2.0 }),
            ]))
        );
    }

    #[test]
    fn lines_that_are_not_the_previews_any_more_are_stale() {
        // A line gone, a line added, another line, the same lines reordered.
        let cases = [
            vec![line(10, "100", true), line(20, "1", true)],
            vec![
                line(10, "100", true),
                line(11, "300", true),
                line(20, "1", true),
                line(30, "1", true),
            ],
            vec![
                line(10, "100", true),
                line(12, "300", true),
                line(20, "1", true),
            ],
            vec![
                line(11, "300", true),
                line(10, "100", true),
                line(20, "1", true),
            ],
        ];
        for lines in cases {
            assert_eq!(
                validate_done(&form(lines.clone()), &preview(), &[]),
                Err(DoneError::Stale),
                "{lines:?}"
            );
        }
    }

    #[test]
    fn the_meal_date_is_sent_when_given_and_refused_when_it_does_not_read() {
        let mut f = form(vec![
            line(10, "100", true),
            line(11, "300", true),
            line(20, "1", true),
        ]);
        f.eaten_on = "2026-10-24".into();
        assert_eq!(
            validate_done(&f, &preview(), &[]).unwrap().eaten_on,
            Some("2026-10-24".parse().unwrap())
        );
        f.eaten_on = "24/10/2026".into();
        assert_eq!(
            validate_done(&f, &preview(), &[]),
            Err(DoneError::Invalid(Invalid {
                lines: vec![],
                date: true,
                ..Invalid::default()
            }))
        );
    }

    #[test]
    fn a_wrong_date_is_reported_with_the_lines_in_error() {
        let mut f = form(vec![
            line(10, "abc", true),
            line(11, "300", true),
            line(20, "1", true),
        ]);
        f.eaten_on = "24/10/2026".into();
        assert_eq!(
            validate_done(&f, &preview(), &[]),
            Err(DoneError::Invalid(Invalid {
                lines: vec![Some(LineError::InvalidQuantity), None, None],
                date: true,
                ..Invalid::default()
            }))
        );
    }

    #[test]
    fn the_done_page_says_on_the_field_that_the_date_does_not_read() {
        let html = done_body(
            id(901),
            &preview(),
            None,
            &Invalid {
                lines: vec![],
                date: true,
                ..Invalid::default()
            },
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains("La date du repas n'est pas une date valide."),
            "{html}"
        );
    }

    #[test]
    fn a_field_in_error_is_tied_to_its_message() {
        let typed = form(vec![
            line(10, "abc", true),
            line(11, "300", true),
            line(20, "1", true),
        ]);
        let html = done_body(
            id(901),
            &preview(),
            Some(&typed),
            &Invalid {
                lines: vec![Some(LineError::InvalidQuantity), None, None],
                date: true,
                ..Invalid::default()
            },
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains(r#"<p class="field-error" id="quantity.0-error">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="quantity.0" step="any" min="0" value="abc" aria-invalid="true" aria-describedby="quantity.0-error"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<p class="field-error" id="eaten_on-error">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="eaten_on" value="" aria-invalid="true" aria-describedby="eaten_on-error"/>"#),
            "{html}"
        );
        // A line without error is not marked.
        assert!(
            html.contains(r#"name="quantity.1" step="any" min="0" value="300"/>"#),
            "{html}"
        );
        assert_eq!(html.matches("aria-invalid").count(), 2, "{html}");
    }

    #[test]
    fn two_ingredients_on_one_stock_line_are_sent_as_one_line() {
        // « Sel » twice in the recipe, both proposed from the same line.
        let p = DonePreview {
            can_edit_recipe: false,
            selection: selection("Soupe"),
            ingredients: vec![
                ingredient(
                    1,
                    "Sel",
                    Some(0.1),
                    Some("kg"),
                    vec![(stock(10, "Sel", 0.3, "kg"), 0.1)],
                    None,
                ),
                ingredient(
                    2,
                    "sel",
                    Some(0.2),
                    Some("kg"),
                    vec![(stock(10, "Sel", 0.3, "kg"), 0.2)],
                    None,
                ),
            ],
        };
        // 0.1 + 0.2 is all of the 0.3 held, without its binary noise.
        let got = validate_done(
            &form(vec![line(10, "0.1", true), line(10, "0.2", true)]),
            &p,
            &[],
        );
        assert_eq!(got.unwrap().removals, vec![removal(10, 0.3, false)]);
        // One of the two unchecked: only the other is taken.
        let got = validate_done(
            &form(vec![line(10, "0.1", false), line(10, "0.2", true)]),
            &p,
            &[],
        );
        assert_eq!(got.unwrap().removals, vec![removal(10, 0.2, false)]);
        // Both unchecked: excluded.
        let got = validate_done(
            &form(vec![line(10, "0.1", false), line(10, "0.2", false)]),
            &p,
            &[],
        );
        assert_eq!(got.unwrap().removals, vec![removal(10, 0.0, true)]);
        // Together above the line: the line that goes over is in error.
        let got = validate_done(
            &form(vec![line(10, "0.2", true), line(10, "0.2", true)]),
            &p,
            &[],
        );
        assert_eq!(
            got,
            Err(invalid_lines(vec![
                None,
                Some(LineError::TooMuch { available: 0.3 })
            ]))
        );
    }

    #[test]
    fn the_stock_is_compared_as_apps_api_compares_it_twelve_significant_digits() {
        // One line holding 0.1234567891 L: apps/api refuses 0.1234567891234
        // (more than it holds), even though both read 0.123456789 at the
        // billionth.
        let p = DonePreview {
            can_edit_recipe: false,
            selection: selection("Sauce"),
            ingredients: vec![ingredient(
                1,
                "Vinaigre",
                Some(0.1),
                Some("L"),
                vec![(stock(10, "Vinaigre", 0.1234567891, "L"), 0.1)],
                None,
            )],
        };
        assert_eq!(
            validate_done(&form(vec![line(10, "0.1234567891234", true)]), &p, &[]),
            Err(invalid_lines(vec![Some(LineError::TooMuch {
                available: 0.1234567891
            })]))
        );
        // Exactly what it holds is accepted, and sent as typed.
        assert_eq!(
            validate_done(&form(vec![line(10, "0.1234567891", true)]), &p, &[])
                .unwrap()
                .removals,
            vec![removal(10, 0.1234567891, false)]
        );
    }

    #[test]
    fn an_ingredient_without_stock_has_no_line_to_send() {
        let mut p = preview();
        p.ingredients.push(ingredient(
            3,
            "Sucre",
            Some(50.0),
            Some("g"),
            vec![],
            Some(RemovalWarning::NoStock),
        ));
        let got = validate_done(
            &form(vec![
                line(10, "100", true),
                line(11, "300", true),
                line(20, "1", true),
            ]),
            &p,
            &[],
        );
        assert_eq!(got.unwrap().removals.len(), 3);
    }

    // -- done_refusal --------------------------------------------------------

    #[test]
    fn a_refusal_for_a_change_made_since_is_told_from_a_rejected_form() {
        use reqwest::StatusCode as S;
        // A stock line lowered or gone (409), an other product gone or the
        // selection closed (404, told apart by the preview read again).
        for status in [S::CONFLICT, S::NOT_FOUND] {
            assert_eq!(done_refusal(status), Refusal::Changed, "{status}");
        }
        // A request the page should never have built: not a change, which
        // another try would not fix. A line naming no ingredient is no
        // longer refused (#540).
        assert_eq!(done_refusal(S::BAD_REQUEST), Refusal::Rejected);
    }

    // -- gone ----------------------------------------------------------------

    /// A stand-in for apps/api answering the recipe's read with `status`.
    async fn fake_recipe_api(status: u16) -> AppState {
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/groups/:gid/recipes/:id",
            get(move || async move {
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    r#"{"error":"x"}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: format!("http://{addr}"),
            api_public_base_url: "/api".into(),
            body_read_limits: manage_our_home_http_guard::BodyReadLimits::PRODUCTION,
            upload_gate: manage_our_home_http_guard::UploadGate::new(8, 2),
            shutdown: manage_our_home_http_guard::Shutdown::new(),
        }
    }

    #[tokio::test]
    async fn a_recipe_that_does_not_exist_is_not_sent_back_to() {
        let state = fake_recipe_api(404).await;
        let resp = gone(&state, id(1), id(901), Some("session=s"), "detail").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Recette introuvable"), "{html}");
        assert!(html.contains(r#"href="/recipes""#), "{html}");
        assert!(!html.contains("n'est plus à réaliser"), "{html}");
    }

    #[tokio::test]
    async fn a_recipe_no_longer_to_cook_goes_back_where_the_member_came_from() {
        // The recipe reads (200), or its read fails: back with the notice.
        for status in [200, 503] {
            let state = fake_recipe_api(status).await;
            let resp = gone(&state, id(1), id(901), Some("session=s"), "detail").await;
            assert_eq!(
                resp.headers()[axum::http::header::LOCATION],
                format!("/recipes/{}?error=selection_gone", id(901)),
                "{status}"
            );
        }
        let state = fake_recipe_api(200).await;
        let resp = gone(&state, id(1), id(901), None, "list").await;
        assert_eq!(
            resp.headers()[axum::http::header::LOCATION],
            "/recipes?error=selection_gone"
        );
        let resp = gone(&state, id(1), id(901), None, "planned").await;
        assert_eq!(
            resp.headers()[axum::http::header::LOCATION],
            "/recipes?vue=prevues&error=selection_gone"
        );
    }

    // -- parse_abandon_form --------------------------------------------------

    #[test]
    fn the_abandon_form_reads_the_items_to_remove_once_each() {
        let got = parse_abandon_form(&pairs(&[
            ("back", "list"),
            ("remove", &id(2).to_string()),
            ("remove", &id(1).to_string()),
            ("remove", &id(2).to_string()),
        ]));
        assert_eq!(got, Some((vec![id(2), id(1)], "list".to_string())));
        // Nothing checked: everything is kept.
        assert_eq!(parse_abandon_form(&[]), Some((vec![], String::new())));
    }

    #[test]
    fn the_abandon_form_refuses_an_id_that_does_not_read() {
        assert_eq!(parse_abandon_form(&pairs(&[("remove", "nope")])), None);
    }

    // -- rendering -----------------------------------------------------------

    #[test]
    fn from_the_planned_tab_the_pages_go_back_to_it() {
        let html = done_body(
            id(901),
            &preview(),
            None,
            &Invalid::default(),
            "",
            "planned",
            today(),
            &Others::default(),
        );
        assert!(html.contains(r#"name="back" value="planned""#), "{html}");
        assert!(
            html.contains(r#"<a class="btn secondary" href="/recipes?vue=prevues">Annuler</a>"#),
            "{html}"
        );
        // Anything else is still the recipe.
        let html = done_body(
            id(901),
            &preview(),
            None,
            &Invalid::default(),
            "",
            "/elsewhere",
            today(),
            &Others::default(),
        );
        assert!(html.contains(r#"name="back" value="detail""#), "{html}");
    }

    #[test]
    fn the_done_page_proposes_every_lot_checked_for_removal() {
        let html = done_body(
            id(901),
            &preview(),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(html.contains("<h1>Recette faite : Crêpes</h1>"), "{html}");
        assert!(
            html.contains(&format!(r#"action="/recipes/{}/done""#, id(901))),
            "{html}"
        );
        assert!(html.contains(r#"name="back" value="list""#), "{html}");
        assert!(
            html.contains(r#"name="eaten_on" value="2026-10-26""#),
            "{html}"
        );
        // One fieldset per ingredient, named with the recipe's quantity.
        assert!(
            html.contains("<legend>Farine · 400 g pour la recette</legend>"),
            "{html}"
        );
        // One numbered line per lot, its quantity editable, checked.
        assert!(
            html.contains(&format!(r#"name="stock.1" value="{}""#, id(11))),
            "{html}"
        );
        assert!(
            html.contains(r#"name="quantity.1" step="any" min="0" value="300""#),
            "{html}"
        );
        assert!(html.contains(r#"name="remove.1" checked"#), "{html}");
        // Explicit labels, which name the stock line taken from.
        assert!(
            html.contains("Quantité de Farine à retirer, en g (stock : 500 g)"),
            "{html}"
        );
        assert!(html.contains("Retirer Farine du stock"), "{html}");
        assert!(
            html.contains(r#"<button type="submit">Marquer comme faite</button>"#),
            "{html}"
        );
        assert!(html.contains(r#"href="/recipes""#), "{html}");
        // Not a `required` field: an unchecked line may be left empty.
        assert!(!html.contains("required"), "{html}");
    }

    #[test]
    fn the_done_page_says_why_a_line_is_proposed_at_zero_or_capped() {
        let p = DonePreview {
            can_edit_recipe: false,
            selection: selection("Gâteau"),
            ingredients: vec![
                ingredient(
                    1,
                    "Sucre",
                    Some(50.0),
                    Some("g"),
                    vec![],
                    Some(RemovalWarning::NoStock),
                ),
                ingredient(
                    2,
                    "Lait",
                    Some(500.0),
                    Some("mL"),
                    vec![(stock(20, "Lait", 2.0, "L"), 0.0)],
                    Some(RemovalWarning::UnitMismatch),
                ),
                ingredient(
                    3,
                    "Sel",
                    None,
                    None,
                    vec![(stock(30, "Sel", 1.0, "unité"), 0.0)],
                    Some(RemovalWarning::NoQuantity),
                ),
                ingredient(
                    4,
                    "Oeufs",
                    Some(6.0),
                    None,
                    vec![(stock(40, "Oeufs", 4.0, "unité"), 4.0)],
                    Some(RemovalWarning::Capped),
                ),
            ],
        };
        let html = done_body(
            id(901),
            &p,
            None,
            &Invalid::default(),
            "",
            "detail",
            today(),
            &Others::default(),
        );
        assert!(html.contains("<legend>Sucre · 50 g pour la recette</legend>"));
        assert!(html.contains("Aucun article de ce nom en stock : rien à retirer."));
        assert!(
            html.contains("Le stock est en L et la recette en mL : saisissez la quantité en L.")
        );
        assert!(html.contains("<legend>Sel</legend>"), "{html}");
        assert!(html.contains("La recette ne donne pas de quantité : saisissez-la."));
        assert!(html.contains("Le stock n'en contient que 4 : tout est proposé."));
        // No line for the ingredient without stock: Lait is line 0.
        assert!(html.contains(&format!(r#"name="stock.0" value="{}""#, id(20))));
        assert!(html.contains(&format!(r#"name="stock.2" value="{}""#, id(40))));
        assert!(!html.contains("stock.3"));
    }

    /// « Farine » twice in the recipe (400 g, then 200 g), from one stock
    /// line holding `held` g, as apps/api proposes it: the first takes what
    /// it needs, the second what is left.
    fn shared_preview(held: f64) -> DonePreview {
        let first = held.min(400.0);
        let second = (held - first).min(200.0);
        DonePreview {
            can_edit_recipe: false,
            selection: selection("Tarte"),
            ingredients: vec![
                ingredient(
                    1,
                    "Farine",
                    Some(400.0),
                    Some("g"),
                    vec![(stock(10, "Farine", held, "g"), first)],
                    (first < 400.0).then_some(RemovalWarning::Capped),
                ),
                ingredient(
                    2,
                    "farine",
                    Some(200.0),
                    Some("g"),
                    vec![(stock(10, "Farine", held, "g"), second)],
                    (second < 200.0).then_some(RemovalWarning::Capped),
                ),
            ],
        }
    }

    #[test]
    fn two_ingredients_of_one_name_have_distinct_labels() {
        let html = done_body(
            id(901),
            &shared_preview(1000.0),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        for n in [1, 2] {
            let name = if n == 1 { "Farine" } else { "farine" };
            assert!(
                html.contains(&format!(
                    "Quantité de {name} (n° {n}) à retirer, en g (stock : 1000 g)"
                )),
                "{html}"
            );
            assert!(
                html.contains(&format!("Retirer {name} (n° {n}) du stock")),
                "{html}"
            );
        }
        assert!(
            html.contains("<legend>Farine (n° 1) · 400 g pour la recette</legend>"),
            "{html}"
        );
        assert!(
            html.contains("<legend>farine (n° 2) · 200 g pour la recette</legend>"),
            "{html}"
        );
        // A name used once keeps its label as it is.
        let html = done_body(
            id(901),
            &preview(),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(!html.contains("n° "), "{html}");
    }

    #[test]
    fn a_stock_line_shared_by_two_ingredients_says_what_is_left_for_the_second() {
        // 250 g: all for the first, nothing left for the second.
        let html = done_body(
            id(901),
            &shared_preview(250.0),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains("Le stock n'en contient que 250 g : tout est proposé."),
            "{html}"
        );
        assert!(
            html.contains(
                "Ce stock sert d'abord à Farine (n° 1) : il n'en reste rien pour cet ingrédient."
            ),
            "{html}"
        );
        assert!(!html.contains("que 0 g"), "{html}");
        // 500 g: 100 g left for the second.
        let html = done_body(
            id(901),
            &shared_preview(500.0),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains(
                "Ce stock sert d'abord à Farine (n° 1) : il en reste 100 g pour cet ingrédient."
            ),
            "{html}"
        );
    }

    #[test]
    fn too_much_on_a_shared_stock_line_names_the_ingredients_sharing_it() {
        let typed = form(vec![line(10, "200", true), line(10, "100", true)]);
        let errors = [None, Some(LineError::TooMuch { available: 250.0 })];
        let html = done_body(
            id(901),
            &shared_preview(250.0),
            Some(&typed),
            &Invalid {
                lines: errors.to_vec(),
                date: false,
                ..Invalid::default()
            },
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains(
                "Le stock n'en contient que 250 g pour Farine (n° 1) et farine (n° 2) ensemble."
            ),
            "{html}"
        );
    }

    #[test]
    fn too_much_writes_what_the_stock_holds_as_the_stock_list_does() {
        let too_much = |available| LineError::TooMuch { available };
        assert_eq!(
            line_error_text(too_much(4.0), "unité", &[]),
            "Le stock n'en contient que 4."
        );
        assert_eq!(
            line_error_text(too_much(1.5), "L", &[]),
            "Le stock n'en contient que 1,5 L."
        );
        assert_eq!(
            line_error_text(too_much(3.0), "paquet", &["Riz", "riz"]),
            "Le stock n'en contient que 3 paquets pour Riz et riz ensemble."
        );
        assert_eq!(
            line_error_text(too_much(2.0), "unité", &["Œuf", "œuf"]),
            "Le stock n'en contient que 2 pour Œuf et œuf ensemble."
        );
    }

    #[test]
    fn the_done_page_marks_an_optional_ingredient() {
        let mut p = preview();
        p.ingredients[1].is_optional = true;
        let html = done_body(
            id(901),
            &p,
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains("<legend>Lait · 1 L pour la recette · optionnel</legend>"),
            "{html}"
        );
    }

    #[test]
    fn a_done_form_in_error_keeps_what_was_typed_and_says_why() {
        let mut typed = form(vec![
            line(10, "abc", true),
            line(11, "300", false),
            line(20, "3", true),
        ]);
        typed.eaten_on = "2026-10-20".into();
        let errors = [
            Some(LineError::InvalidQuantity),
            None,
            Some(LineError::TooMuch { available: 2.0 }),
        ];
        let html = done_body(
            id(901),
            &preview(),
            Some(&typed),
            &Invalid {
                lines: errors.to_vec(),
                date: false,
                ..Invalid::default()
            },
            "<p class=\"notice error\">x</p>",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains(r#"name="eaten_on" value="2026-10-20""#),
            "{html}"
        );
        assert!(html.contains(r#"name="quantity.0" step="any" min="0" value="abc""#));
        assert!(html.contains(r#"name="remove.0" checked"#));
        assert!(html.contains(r#"name="remove.1"/>"#), "{html}");
        assert!(
            html.contains("Saisissez une quantité de 0 ou plus, ou décochez « Retirer du stock ».")
        );
        assert!(html.contains("Le stock n'en contient que 2 L."), "{html}");
        assert!(html.contains("<p class=\"notice error\">x</p>"));
    }

    #[test]
    fn the_done_page_escapes_what_members_typed() {
        let mut p = preview();
        p.selection.recipe_name = "<b>Crêpes</b>".into();
        p.ingredients[0].name = "Farine & <i>".into();
        let html = done_body(
            id(901),
            &p,
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(!html.contains("<b>") && !html.contains("<i>"), "{html}");
        assert!(html.contains("&lt;b&gt;Crêpes&lt;/b&gt;"));
    }

    #[test]
    fn the_abandon_page_proposes_every_item_checked_for_removal() {
        let p = AbandonPreview {
            selection: selection("Crêpes"),
            items: vec![
                AbandonItem {
                    id: id(1),
                    name: "Farine".into(),
                    quantity: Some(400.0),
                    unit: Some("g".into()),
                },
                AbandonItem {
                    id: id(2),
                    name: "Oeufs".into(),
                    quantity: None,
                    unit: None,
                },
            ],
        };
        let html = abandon_body(id(901), &p, None, "", "detail");
        assert!(html.contains("<h1>Abandonner : Crêpes</h1>"), "{html}");
        assert!(html.contains(&format!(r#"action="/recipes/{}/abandon""#, id(901))));
        assert!(html.contains(r#"name="back" value="detail""#));
        assert!(html.contains(&format!(
            r#"<input type="checkbox" name="remove" value="{}" checked/> Retirer « Farine (400 g) » de la liste"#,
            id(1)
        )), "{html}");
        assert!(html.contains("Retirer « Oeufs » de la liste"), "{html}");
        assert!(html.contains(r#"<button type="submit">Abandonner la recette</button>"#));
        assert!(html.contains(&format!(r#"href="/recipes/{}""#, id(901))));
    }

    #[test]
    fn the_abandon_page_shown_again_keeps_the_choices_made() {
        let item = |n: u128, name: &str| AbandonItem {
            id: id(n),
            name: name.into(),
            quantity: None,
            unit: None,
        };
        let p = AbandonPreview {
            selection: selection("Crêpes"),
            items: vec![item(1, "Farine"), item(2, "Lait")],
        };
        let html = abandon_body(id(901), &p, Some(&[id(1)]), "", "list");
        assert!(
            html.contains(&format!(
                r#"name="remove" value="{}" checked/> Retirer « Farine »"#,
                id(1)
            )),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"name="remove" value="{}"/> Retirer « Lait »"#,
                id(2)
            )),
            "{html}"
        );
    }

    #[test]
    fn the_abandon_page_without_items_says_so() {
        let p = AbandonPreview {
            selection: selection("Crêpes"),
            items: vec![],
        };
        let html = abandon_body(id(901), &p, None, "", "list");
        assert!(html
            .contains("Aucun article de la liste de courses n'a été généré pour cette recette."));
        assert!(!html.contains(r#"name="remove""#));
        assert!(html.contains(r#"<button type="submit">Abandonner la recette</button>"#));
    }

    // -- other products (#540) -------------------------------------------------

    /// The group's stock beside `preview()`: its three lots, and Crème,
    /// Crème fraîche and Sel that the recipe does not call for.
    fn group_stock() -> Vec<StockItemResponse> {
        vec![
            stock(10, "Farine", 100.0, "g"),
            stock(11, "Farine", 500.0, "g"),
            stock(20, "Lait", 2.0, "L"),
            stock(30, "Crème", 50.0, "cL"),
            stock(31, "Crème fraîche", 20.0, "cL"),
            stock(40, "Sel", 1.0, "unité"),
        ]
    }

    fn recipe_lines() -> Vec<DoneLine> {
        vec![
            line(10, "100", true),
            line(11, "300", true),
            line(20, "1", true),
        ]
    }

    fn other_line(n: u128, quantity: &str, used: bool) -> OtherLine {
        OtherLine {
            stock_item_id: id(n),
            quantity: quantity.into(),
            used,
        }
    }

    fn editable() -> DonePreview {
        DonePreview {
            can_edit_recipe: true,
            ..preview()
        }
    }

    #[test]
    fn the_done_form_reads_the_other_products_the_search_and_the_answer() {
        let got = parse_done_form(&pairs(&[
            ("stock.0", &id(10).to_string()),
            ("quantity.0", "100"),
            ("other.1", &id(40).to_string()),
            ("other_quantity.1", "1"),
            ("other.0", &id(30).to_string()),
            ("other_quantity.0", "20"),
            ("other_used.0", "on"),
            ("q", " crè "),
            ("intent", "search"),
            ("add_to_recipe", "yes"),
        ]));
        // The other products do not mix with the recipe's lines of the
        // same number.
        assert_eq!(got.lines, vec![line(10, "100", false)]);
        assert_eq!(
            got.others,
            vec![other_line(30, "20", true), other_line(40, "1", false)]
        );
        assert_eq!(got.search, " crè ");
        assert!(got.searching);
        assert_eq!(got.add_to_recipe, Some(true));
        let got = parse_done_form(&pairs(&[("add_to_recipe", "no")]));
        assert_eq!(got.add_to_recipe, Some(false));
        assert!(!got.searching);
        assert_eq!(parse_done_form(&[]).add_to_recipe, None);
    }

    fn stock_line(n: u128, name: &str) -> StockItemResponse {
        stock(n, name, 1.0, "unité")
    }

    #[test]
    fn the_search_finds_the_stock_lines_not_already_on_the_page() {
        let stock = group_stock();
        let ids =
            |found: Vec<&StockItemResponse>| -> Vec<Uuid> { found.iter().map(|s| s.id).collect() };
        // Words inside the name, whatever the case.
        assert_eq!(ids(search_stock(&stock, " CRÈ", &[])), vec![id(30), id(31)]);
        // A line already on the page is not offered twice.
        assert_eq!(ids(search_stock(&stock, "crème", &[id(30)])), vec![id(31)]);
        assert!(search_stock(&stock, "  ", &[]).is_empty());
        assert!(search_stock(&stock, "chocolat", &[]).is_empty());
        // At most SEARCH_MAX lines.
        let many: Vec<StockItemResponse> = (0..30)
            .map(|n| stock_line(100 + n, &format!("Épice {n}")))
            .collect();
        assert_eq!(search_stock(&many, "épice", &[]).len(), SEARCH_MAX);
    }

    #[test]
    fn a_search_keeps_the_products_chosen_and_offers_the_others_unchecked() {
        let mut f = form(recipe_lines());
        f.others = vec![other_line(30, "20", true), other_line(40, "", false)];
        f.search = "crème".into();
        f.searching = true;
        let got = others_section(&f, &preview(), &group_stock(), &[], true);
        // Crème kept as typed; Sel, left unchecked, dropped; Crème fraîche
        // offered. Crème is not offered twice.
        let shown: Vec<(Uuid, &str, bool)> = got
            .lines
            .iter()
            .map(|l| (l.stock.id, l.quantity.as_str(), l.used))
            .collect();
        assert_eq!(shown, vec![(id(30), "20", true), (id(31), "", false)]);
        assert_eq!(got.search, "crème");
        assert!(got.searched);
        // The recipe's own lots are never offered.
        f.search = "farine".into();
        let got = others_section(&f, &preview(), &group_stock(), &[], true);
        assert_eq!(got.lines.len(), 1);
    }

    #[test]
    fn a_form_shown_again_keeps_every_other_product_as_typed() {
        let mut f = form(recipe_lines());
        f.others = vec![other_line(30, "abc", true), other_line(40, "", false)];
        let errors = [Some(LineError::InvalidQuantity), None];
        let got = others_section(&f, &preview(), &group_stock(), &errors, false);
        let shown: Vec<(Uuid, &str, bool, Option<LineError>)> = got
            .lines
            .iter()
            .map(|l| (l.stock.id, l.quantity.as_str(), l.used, l.error))
            .collect();
        assert_eq!(
            shown,
            vec![
                (id(30), "abc", true, Some(LineError::InvalidQuantity)),
                (id(40), "", false, None)
            ]
        );
        assert!(!got.searched);
    }

    #[test]
    fn the_products_chosen_are_sent_with_the_recipes_lines() {
        let mut f = form(recipe_lines());
        f.others = vec![
            other_line(30, "20", true),
            other_line(31, "5", false),
            other_line(40, "1", true),
        ];
        f.add_to_recipe = Some(false);
        let got = validate_done(&f, &editable(), &group_stock()).unwrap();
        assert_eq!(got.removals.len(), 3);
        assert_eq!(
            got.others,
            vec![
                DoneOther {
                    stock_item_id: id(30),
                    quantity: 20.0
                },
                DoneOther {
                    stock_item_id: id(40),
                    quantity: 1.0
                },
            ]
        );
        assert!(!got.add_to_recipe);
        f.add_to_recipe = Some(true);
        assert!(
            validate_done(&f, &editable(), &group_stock())
                .unwrap()
                .add_to_recipe
        );
    }

    #[test]
    fn a_product_chosen_needs_a_quantity_above_zero_within_its_stock() {
        let mut f = form(recipe_lines());
        f.add_to_recipe = Some(false);
        for (typed, error) in [
            ("0", LineError::InvalidQuantity),
            ("", LineError::InvalidQuantity),
            ("abc", LineError::InvalidQuantity),
            ("51", LineError::TooMuch { available: 50.0 }),
        ] {
            f.others = vec![other_line(30, typed, true), other_line(40, "x", false)];
            assert_eq!(
                validate_done(&f, &editable(), &group_stock()),
                Err(DoneError::Invalid(Invalid {
                    others: vec![Some(error), None],
                    ..Invalid::default()
                })),
                "{typed:?}"
            );
        }
        f.others = vec![other_line(30, "50", true)];
        assert!(validate_done(&f, &editable(), &group_stock()).is_ok());
    }

    #[test]
    fn a_product_chosen_that_left_the_stock_is_stale() {
        let mut f = form(recipe_lines());
        f.others = vec![other_line(99, "1", true)];
        f.add_to_recipe = Some(false);
        assert_eq!(
            validate_done(&f, &editable(), &group_stock()),
            Err(DoneError::Stale)
        );
        // Unchecked, it is not sent: no matter.
        f.others = vec![other_line(99, "1", false)];
        assert!(validate_done(&f, &editable(), &group_stock()).is_ok());
    }

    #[test]
    fn the_question_names_the_products_the_recipe_does_not_have_yet() {
        let stock = group_stock();
        // Farine is already an ingredient.
        let used = [&stock[3], &stock[5], &stock[0]];
        assert_eq!(offered_names(&preview(), &used), vec!["Crème", "Sel"]);
        let twice = [&stock[3], &stock[3]];
        assert_eq!(offered_names(&preview(), &twice), vec!["Crème"]);
        assert!(offered_names(&preview(), &[]).is_empty());
    }

    #[test]
    fn the_question_must_be_answered_before_sending() {
        let mut f = form(recipe_lines());
        f.others = vec![other_line(30, "20", true)];
        assert_eq!(
            validate_done(&f, &editable(), &group_stock()),
            Err(DoneError::Invalid(Invalid {
                add_to_recipe: true,
                ..Invalid::default()
            }))
        );
        // Not asked of a member who may not change the recipe: nothing is
        // added.
        let got = validate_done(&f, &preview(), &group_stock()).unwrap();
        assert!(!got.add_to_recipe);
        // Nor when every product chosen is already an ingredient.
        let mut p = editable();
        p.ingredients[1].name = "crème".into();
        assert!(!validate_done(&f, &p, &group_stock()).unwrap().add_to_recipe);
    }

    #[test]
    fn a_yes_without_a_product_chosen_adds_nothing() {
        let mut f = form(recipe_lines());
        f.add_to_recipe = Some(true);
        let got = validate_done(&f, &editable(), &group_stock()).unwrap();
        assert!(got.others.is_empty());
        assert!(!got.add_to_recipe);
    }

    fn section(lines: Vec<(StockItemResponse, &str, bool)>) -> Others {
        Others {
            lines: lines
                .into_iter()
                .map(|(stock, quantity, used)| OtherShown {
                    stock,
                    quantity: quantity.into(),
                    used,
                    error: None,
                })
                .collect(),
            search: String::new(),
            searched: false,
        }
    }

    #[test]
    fn the_done_page_offers_to_search_the_stock_for_other_products() {
        let html = done_body(
            id(901),
            &editable(),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &Others::default(),
        );
        assert!(
            html.contains("<legend>Autre chose utilisé ?</legend>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="search" name="q" value=""/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit" name="intent" value="search" class="secondary" formnovalidate>Chercher</button>"#),
            "{html}"
        );
        // Nothing chosen: no question yet.
        assert!(!html.contains("pour la prochaine fois"), "{html}");
    }

    #[test]
    fn the_done_page_lists_the_products_found_and_asks_about_those_chosen() {
        let mut others = section(vec![
            (stock(30, "Crème", 50.0, "cL"), "20", true),
            (stock(40, "Sel", 1.0, "unité"), "", false),
            (stock(41, "Poivre", 1.0, "unité"), "1", true),
        ]);
        others.search = "<s>".into();
        others.searched = true;
        let html = done_body(
            id(901),
            &editable(),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &others,
        );
        assert!(
            html.contains(&format!(r#"name="other.0" value="{}""#, id(30))),
            "{html}"
        );
        assert!(
            html.contains(r#"name="other_quantity.0" step="any" min="0" value="20""#),
            "{html}"
        );
        assert!(html.contains(r#"name="other_used.0" checked"#), "{html}");
        assert!(html.contains(r#"name="other_used.1"/>"#), "{html}");
        assert!(
            html.contains("Quantité de Crème à retirer, en cL (stock : 50 cL)"),
            "{html}"
        );
        assert!(html.contains("Retirer Sel du stock"), "{html}");
        assert!(html.contains(r#"value="&lt;s&gt;""#), "{html}");
        assert!(
            html.contains(
                "<legend>Ajouter Crème, Poivre à la recette pour la prochaine fois ?</legend>"
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="add_to_recipe" value="yes"/> Oui"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="radio" name="add_to_recipe" value="no"/> Non"#),
            "{html}"
        );
        // A member who may not change the recipe is not asked.
        let html = done_body(
            id(901),
            &preview(),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &others,
        );
        assert!(!html.contains("pour la prochaine fois"), "{html}");
    }

    #[test]
    fn a_search_without_result_says_so() {
        let others = Others {
            search: "chocolat".into(),
            searched: true,
            ..Others::default()
        };
        let html = done_body(
            id(901),
            &preview(),
            None,
            &Invalid::default(),
            "",
            "list",
            today(),
            &others,
        );
        assert!(
            html.contains("Aucun autre produit du stock ne correspond à « chocolat »."),
            "{html}"
        );
    }

    #[test]
    fn the_unanswered_question_and_an_other_product_in_error_are_marked() {
        let mut others = section(vec![(stock(30, "Crème", 50.0, "cL"), "60", true)]);
        others.lines[0].error = Some(LineError::TooMuch { available: 50.0 });
        let typed = DoneForm {
            add_to_recipe: Some(true),
            ..form(recipe_lines())
        };
        let html = done_body(
            id(901),
            &editable(),
            Some(&typed),
            &Invalid {
                add_to_recipe: true,
                ..Invalid::default()
            },
            "",
            "list",
            today(),
            &others,
        );
        assert!(html.contains("Le stock n'en contient que 50 cL."), "{html}");
        assert!(
            html.contains(r#"name="other_quantity.0" step="any" min="0" value="60" aria-invalid="true" aria-describedby="other_quantity.0-error"/>"#),
            "{html}"
        );
        assert!(
            html.contains("Répondez à la question : ajouter ces produits à la recette ou non."),
            "{html}"
        );
        // The answer typed comes back checked.
        assert!(
            html.contains(r#"name="add_to_recipe" value="yes" checked/>"#),
            "{html}"
        );
    }
}
