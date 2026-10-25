//! `/grocery-list/reception` (#472) — receiving the checked items of the
//! list into the stock, through apps/api's `stocks::reception` (#471).
//!
//! `GET` lists one line per checked item, pre-filled with apps/api's
//! proposal: quantity, unit, an empty expiry date with the expiry shortcuts
//! (`data-expiry-shortcuts`), the choice between adding to the existing
//! stock line and making a new one, and a box that leaves the item out.
//! `POST` checks the lines, sends the lot, and goes back to the list with a
//! notice that counts the articles that entered the stock; the items left
//! out stay on the list.
//!
//! No JavaScript is needed: one plain form, its fields numbered by line
//! (`quantity.0`, `mode.0`…). The form round-trips everything it shows (the
//! name and the label of the merge choice ride in hidden fields), so a line
//! in error is shown again with what was typed, without a second read of
//! the proposal.
//!
//! The lot is all or nothing on apps/api's side: before sending it, the
//! lines are checked here with the same rules (`validate_lines`), so the
//! error is shown on its line rather than as one refusal of the lot.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::NaiveDate;
use manage_our_home_shared::dto::stocks::{
    ReceiveRequest, ReceivedItemRequest, ReceptionLine, ReceptionList, StockItemList,
    StockItemResponse,
};
use manage_our_home_shared::validation::stocks::{
    merge_target, normalize_unit, ReceptionMode, StockCandidate,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::agenda::new::is_form_urlencoded;
use crate::routes::stocks::{fmt_date, fmt_num};
use crate::state::{api_request_auth, AppState};

use super::{family_context, grocery_cookie, service_unavailable_page};

/// The largest quantity a received article may carry: apps/api's
/// `stocks::reception::MAX_QUANTITY`, which refuses the lot above it.
const MAX_QUANTITY: f64 = 1_000_000_000.0;

/// One line of the form, as shown and as submitted: every field is the
/// text of its input, parsed only by `validate_lines`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct FormLine {
    pub grocery_item_id: Uuid,
    /// The article's name, shown, not editable (a hidden field).
    pub name: String,
    /// The wording of the « add to the existing stock » choice; empty when
    /// the proposal is a new line, which is then the only choice.
    pub merge_label: String,
    pub quantity: String,
    pub unit: String,
    pub expires_on: String,
    /// `None` for a value that is neither `merge` nor `new`: apps/api then
    /// applies its proposal.
    pub mode: Option<ReceptionMode>,
    pub excluded: bool,
}

/// What stops a line from being received as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineError {
    QuantityMustBePositive,
    QuantityTooLarge,
    InvalidDate,
    NoStockToMerge,
}

impl LineError {
    fn text(self) -> &'static str {
        match self {
            LineError::QuantityMustBePositive => "La quantité doit être supérieure à zéro.",
            LineError::QuantityTooLarge => "La quantité ne peut pas dépasser 1 000 000 000.",
            LineError::InvalidDate => "La date de péremption n'est pas une date valide.",
            LineError::NoStockToMerge => {
                "Aucun stock existant de ce nom et de cette unité n'a cette date de péremption : choisissez « Nouvelle ligne de stock », ou laissez la date vide."
            }
        }
    }
}

/// Why the lot is not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LotError {
    /// One entry per line, `Some` on the lines in error.
    Lines(Vec<Option<LineError>>),
    /// Every line is left out.
    NothingToReceive,
}

/// The line the form shows for one proposal of apps/api.
pub(crate) fn form_line(line: &ReceptionLine) -> FormLine {
    let merge_label = match (line.mode, &line.stock_item) {
        (ReceptionMode::New, _) => String::new(),
        (ReceptionMode::Merge, Some(stock)) => {
            format!("Ajouter au stock existant ({})", stock_summary(stock))
        }
        (ReceptionMode::Merge, None) => {
            format!("Ajouter à la ligne « {} » créée plus haut", line.name)
        }
    };
    FormLine {
        grocery_item_id: line.grocery_item_id,
        name: line.name.clone(),
        merge_label,
        quantity: fmt_num(line.quantity),
        unit: line.unit.clone(),
        expires_on: line.expires_on.map(|d| d.to_string()).unwrap_or_default(),
        mode: Some(line.mode),
        excluded: false,
    }
}

/// A stock line as the merge choice names it: what it holds, and its date.
fn stock_summary(stock: &StockItemResponse) -> String {
    let held = format!("{} {}", fmt_num(stock.quantity), stock.unit);
    match stock.expires_on {
        Some(d) => format!("{held}, péremption le {}", fmt_date(d)),
        None => held,
    }
}

/// Reads the numbered fields of the form back into lines, in their order.
/// A line without a valid item id is dropped; a repeated field keeps its
/// first occurrence.
pub(crate) fn parse_form(pairs: &[(String, String)]) -> Vec<FormLine> {
    use std::collections::btree_map::{BTreeMap, Entry};
    // line index → field name → value.
    let mut fields: BTreeMap<usize, BTreeMap<&str, &str>> = BTreeMap::new();
    for (key, value) in pairs {
        let Some((field, index)) = key.split_once('.') else {
            continue;
        };
        let Ok(index) = index.parse::<usize>() else {
            continue;
        };
        if let Entry::Vacant(slot) = fields.entry(index).or_default().entry(field) {
            slot.insert(value);
        }
    }
    fields
        .into_values()
        .filter_map(|f| {
            let text = |name: &str| f.get(name).map(|v| v.to_string()).unwrap_or_default();
            let grocery_item_id = f.get("id")?.trim().parse::<Uuid>().ok()?;
            Some(FormLine {
                grocery_item_id,
                name: text("name"),
                merge_label: text("merge_label"),
                quantity: text("quantity"),
                unit: text("unit"),
                expires_on: text("expires_on"),
                mode: match f.get("mode").copied() {
                    Some("merge") => Some(ReceptionMode::Merge),
                    Some("new") => Some(ReceptionMode::New),
                    _ => None,
                },
                excluded: f.contains_key("exclude"),
            })
        })
        .collect()
}

/// Checks the lines the way apps/api will (`stocks::reception`), and builds
/// the lot from those not left out. A `merge` must find a line to add to,
/// with the rule apps/api applies (`merge_target`): among the family's
/// stock (`stock`) and the new lines the earlier articles of the lot make.
pub(crate) fn validate_lines(
    lines: &[FormLine],
    stock: &[StockCandidate<'_>],
) -> Result<Vec<ReceivedItemRequest>, LotError> {
    // The units as apps/api stores them, so that the new lines of the lot
    // can be borrowed as candidates below.
    let units: Vec<String> = lines.iter().map(|l| normalize_unit(&l.unit)).collect();
    let mut candidates = stock.to_vec();
    let mut errors = Vec::with_capacity(lines.len());
    let mut lot = Vec::new();
    for (line, unit) in lines.iter().zip(&units) {
        if line.excluded {
            errors.push(None);
            continue;
        }
        let quantity = match line.quantity.trim().parse::<f64>() {
            Ok(q) if !q.is_finite() || q > MAX_QUANTITY => Err(LineError::QuantityTooLarge),
            Ok(q) if q > 0.0 => Ok(q),
            _ => Err(LineError::QuantityMustBePositive),
        };
        let expires_on = match line.expires_on.trim() {
            "" => Ok(None),
            d => d
                .parse::<NaiveDate>()
                .map(Some)
                .map_err(|_| LineError::InvalidDate),
        };
        let (quantity, expires_on) = match (quantity, expires_on) {
            (Ok(q), Ok(d)) => (q, d),
            (Err(e), _) | (_, Err(e)) => {
                errors.push(Some(e));
                continue;
            }
        };
        // apps/api's rule (`plan`): a merge needs an eligible line, and an
        // article that makes a new line is one for the articles after it.
        let target = merge_target(&line.name, unit, expires_on, &candidates);
        let new_line = match line.mode {
            Some(ReceptionMode::Merge) if target.is_none() => {
                errors.push(Some(LineError::NoStockToMerge));
                continue;
            }
            Some(ReceptionMode::Merge) => false,
            Some(ReceptionMode::New) => true,
            None => target.is_none(),
        };
        if new_line {
            candidates.push(StockCandidate {
                id: line.grocery_item_id,
                name: &line.name,
                unit,
                expires_on,
            });
        }
        errors.push(None);
        lot.push(ReceivedItemRequest {
            grocery_item_id: line.grocery_item_id,
            name: None,
            quantity: Some(quantity),
            // Sent even empty: apps/api normalizes an empty unit to
            // « unité », where an absent one would take the list's.
            unit: Some(line.unit.trim().to_string()),
            expires_on,
            mode: line.mode,
        });
    }
    if errors.iter().any(Option::is_some) {
        Err(LotError::Lines(errors))
    } else if lot.is_empty() {
        Err(LotError::NothingToReceive)
    } else {
        Ok(lot)
    }
}

/// The body of the page: the form, or a sentence when nothing is checked.
pub(crate) fn page_body(lines: &[FormLine], errors: &[Option<LineError>], banner: &str) -> String {
    let head = format!(
        r#"<p><a href="/grocery-list">← Retour à la liste de courses</a></p>
<h1>Réception des courses</h1>
{banner}"#
    );
    if lines.is_empty() {
        return format!(
            r#"{head}<p>Aucun article coché à réceptionner : cochez sur la liste les articles achetés.</p>
<a class="btn secondary" href="/grocery-list">Retour à la liste de courses</a>"#
        );
    }
    let fieldsets: String = lines
        .iter()
        .enumerate()
        .map(|(i, line)| line_fieldset(i, line, errors.get(i).copied().flatten()))
        .collect();
    format!(
        r#"{head}<p class="muted">Vérifiez les quantités et ajoutez les dates de péremption : à la validation, les articles entrent en stock et quittent la liste. Un article « Ne pas réceptionner » reste sur la liste.</p>
<form method="post" action="/grocery-list/reception">
{fieldsets}<button type="submit">Réceptionner</button>
</form>"#
    )
}

/// One line of the form, its fields numbered `i`.
fn line_fieldset(i: usize, line: &FormLine, error: Option<LineError>) -> String {
    let checked = |on: bool| if on { " checked" } else { "" };
    let error = error
        .map(|e| format!("<p class=\"field-error\">{}</p>\n", html_escape(e.text())))
        .unwrap_or_default();
    let mode = if line.merge_label.is_empty() {
        format!(
            r#"<input type="hidden" name="mode.{i}" value="new"/>
<p class="muted">Nouvelle ligne de stock</p>"#
        )
    } else {
        // Neither is checked when the mode came back unreadable: apps/api
        // then applies its proposal.
        format!(
            r#"<div class="field" role="radiogroup" aria-label="Entrée en stock">
<label class="field inline"><input type="radio" name="mode.{i}" value="merge"{merge}/> {merge_label}</label>
<label class="field inline"><input type="radio" name="mode.{i}" value="new"{new}/> Nouvelle ligne de stock</label>
</div>"#,
            merge = checked(line.mode == Some(ReceptionMode::Merge)),
            new = checked(line.mode == Some(ReceptionMode::New)),
            merge_label = html_escape(&line.merge_label),
        )
    };
    format!(
        r#"<fieldset class="card">
<legend>{name}</legend>
{error}<input type="hidden" name="id.{i}" value="{id}"/>
<input type="hidden" name="name.{i}" value="{name}"/>
<input type="hidden" name="merge_label.{i}" value="{merge_label}"/>
<label>Quantité <input type="number" name="quantity.{i}" step="any" min="0" required value="{quantity}"/></label>
<label>Unité <input type="text" name="unit.{i}" value="{unit}" placeholder="unité si vide (ex. kg, L)"/></label>
<label>Date de péremption
<input type="date" name="expires_on.{i}" value="{expires_on}" data-expiry-shortcuts/>
</label>
{mode}
<label class="field inline"><input type="checkbox" name="exclude.{i}"{excluded}/> Ne pas réceptionner</label>
</fieldset>
"#,
        name = html_escape(&line.name),
        id = line.grocery_item_id,
        merge_label = html_escape(&line.merge_label),
        quantity = html_escape(&line.quantity),
        unit = html_escape(&line.unit),
        expires_on = html_escape(&line.expires_on),
        excluded = checked(line.excluded),
    )
}

/// The banner for an error of the whole lot: one apps/api refused it with,
/// or one found here.
fn banner(code: &str) -> String {
    let text = match code {
        "lines" => "Certains articles ne peuvent pas être réceptionnés tels quels : corrigez les lignes signalées.",
        "items_required" => "Tous les articles sont exclus : il n'y a rien à réceptionner.",
        "not_found" => "Un des articles n'est plus sur la liste : rechargez la page de réception.",
        "no_stock_to_merge" => "Un article ne peut plus être ajouté au stock existant : choisissez « Nouvelle ligne de stock » pour lui.",
        "quantity_too_large" => "Une quantité, ou le stock qu'elle complète, dépasserait 1 000 000 000.",
        "forbidden" => "Vous n'avez pas les droits nécessaires pour cette action.",
        _ => "Service momentanément indisponible, merci de réessayer.",
    };
    format!(r#"<p class="notice error">{}</p>"#, html_escape(text))
}

fn render(header: &str, body: &str) -> Html<String> {
    Html(shell_with_header(
        Width::Form,
        "Réception des courses",
        header,
        body,
    ))
}

/// `GET /grocery-list/reception`: one line per checked item, as apps/api
/// proposes it.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list/reception").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let cookie = grocery_cookie(&headers);
    let proposals = match api_request_auth(
        &state,
        reqwest::Method::GET,
        &format!("/groups/{}/stock-items/reception", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await
    {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<ReceptionList>(resp.body) {
                Ok(list) => list.items,
                Err(_) => return service_unavailable_page().into_response(),
            }
        }
        Ok(_) | Err(_) => return service_unavailable_page().into_response(),
    };
    let lines: Vec<FormLine> = proposals.iter().map(form_line).collect();
    render(&fam.header, &page_body(&lines, &[], "")).into_response()
}

/// The family's stock lines, which a `merge` is checked against; `None`
/// when they cannot be read.
async fn stock_lines(
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

/// `POST /grocery-list/reception`: checks the lines, sends the lot, and
/// goes back to the list counting the articles received. A line in error,
/// or a lot apps/api refuses, shows the form again as it was typed.
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: axum::body::Bytes,
) -> Response {
    if !is_form_urlencoded(&headers) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Some(fam) = family_context(&state, &headers, &me, "/grocery-list/reception").await else {
        return Redirect::to("/groups/new").into_response();
    };
    let Ok(pairs) = serde_urlencoded::from_bytes::<Vec<(String, String)>>(&raw_body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let lines = parse_form(&pairs);
    let again = |status: StatusCode, errors: &[Option<LineError>], code: &str| {
        (
            status,
            render(&fam.header, &page_body(&lines, errors, &banner(code))),
        )
            .into_response()
    };
    if lines.is_empty() {
        return again(StatusCode::UNPROCESSABLE_ENTITY, &[], "items_required");
    }

    let cookie = grocery_cookie(&headers);
    let Some(stock) = stock_lines(&state, fam.gid, cookie.as_deref()).await else {
        return again(StatusCode::SERVICE_UNAVAILABLE, &[], "unavailable");
    };
    let candidates: Vec<StockCandidate<'_>> = stock
        .iter()
        .map(|s| StockCandidate {
            id: s.id,
            name: &s.name,
            unit: &s.unit,
            expires_on: s.expires_on,
        })
        .collect();
    let items = match validate_lines(&lines, &candidates) {
        Ok(items) => items,
        Err(LotError::Lines(errors)) => {
            return again(StatusCode::UNPROCESSABLE_ENTITY, &errors, "lines")
        }
        Err(LotError::NothingToReceive) => {
            return again(StatusCode::UNPROCESSABLE_ENTITY, &[], "items_required")
        }
    };

    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/stock-items/reception", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(ReceiveRequest { items }).unwrap()),
    )
    .await;
    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            // An answer that does not read leaves the count out, and the
            // notice says the articles entered without a number.
            let target = match serde_json::from_value::<ReceptionList>(resp.body) {
                Ok(l) => format!("/grocery-list?notice=received&received={}", l.items.len()),
                Err(_) => "/grocery-list?notice=received".to_string(),
            };
            Redirect::to(&target).into_response()
        }
        Ok(resp) => {
            let status = if resp.status.is_server_error() {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            let code = match resp.status {
                reqwest::StatusCode::NOT_FOUND => "not_found",
                reqwest::StatusCode::FORBIDDEN => "forbidden",
                _ => resp
                    .body
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unavailable"),
            };
            again(status, &[], code)
        }
        Err(_) => again(StatusCode::SERVICE_UNAVAILABLE, &[], "unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::validation::stocks::ExpiryStatus;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn line(n: u128, name: &str, mode: ReceptionMode) -> FormLine {
        FormLine {
            grocery_item_id: id(n),
            name: name.into(),
            merge_label: if mode == ReceptionMode::Merge {
                "Ajouter au stock existant (1 L)".into()
            } else {
                String::new()
            },
            quantity: "2".into(),
            unit: "L".into(),
            expires_on: String::new(),
            mode: Some(mode),
            excluded: false,
        }
    }

    fn stock_item(
        n: u128,
        name: &str,
        unit: &str,
        expires_on: Option<NaiveDate>,
    ) -> StockItemResponse {
        StockItemResponse {
            id: id(n),
            group_id: id(900),
            created_by: id(901),
            name: name.into(),
            category: None,
            quantity: 1.0,
            unit: unit.into(),
            reorder_threshold: None,
            low_stock: false,
            expires_on,
            expiry_status: ExpiryStatus::Unknown,
            barcode: None,
        }
    }

    fn candidate<'a>(
        n: u128,
        name: &'a str,
        unit: &'a str,
        expires_on: Option<NaiveDate>,
    ) -> StockCandidate<'a> {
        StockCandidate {
            id: id(n),
            name,
            unit,
            expires_on,
        }
    }

    fn pairs(raw: &[(&str, &str)]) -> Vec<(String, String)> {
        raw.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // -- form_line ------------------------------------------------------------

    #[test]
    fn a_merge_proposal_names_the_stock_line_it_adds_to() {
        let proposal = ReceptionLine {
            grocery_item_id: id(1),
            name: "Lait".into(),
            quantity: 2.0,
            unit: "L".into(),
            expires_on: None,
            mode: ReceptionMode::Merge,
            stock_item: Some(stock_item(10, "Lait", "L", Some(date("2026-11-12")))),
        };
        let l = form_line(&proposal);
        assert_eq!(l.grocery_item_id, id(1));
        assert_eq!(l.name, "Lait");
        assert_eq!(l.quantity, "2");
        assert_eq!(l.unit, "L");
        assert_eq!(l.expires_on, "");
        assert_eq!(l.mode, Some(ReceptionMode::Merge));
        assert!(!l.excluded);
        assert_eq!(
            l.merge_label,
            "Ajouter au stock existant (1 L, péremption le 12/11/2026)"
        );
    }

    #[test]
    fn an_undated_stock_line_is_named_by_its_quantity() {
        let proposal = ReceptionLine {
            grocery_item_id: id(1),
            name: "Riz".into(),
            quantity: 0.5,
            unit: "kg".into(),
            expires_on: None,
            mode: ReceptionMode::Merge,
            stock_item: Some(stock_item(10, "Riz", "kg", None)),
        };
        let l = form_line(&proposal);
        assert_eq!(l.quantity, "0.5");
        assert_eq!(l.merge_label, "Ajouter au stock existant (1 kg)");
    }

    #[test]
    fn a_merge_into_the_lot_names_the_line_made_above() {
        let proposal = ReceptionLine {
            grocery_item_id: id(2),
            name: "Lait".into(),
            quantity: 1.0,
            unit: "L".into(),
            expires_on: None,
            mode: ReceptionMode::Merge,
            stock_item: None,
        };
        assert_eq!(
            form_line(&proposal).merge_label,
            "Ajouter à la ligne « Lait » créée plus haut"
        );
    }

    #[test]
    fn a_new_proposal_offers_no_merge() {
        let proposal = ReceptionLine {
            grocery_item_id: id(1),
            name: "Pain".into(),
            quantity: 1.0,
            unit: "unité".into(),
            expires_on: None,
            mode: ReceptionMode::New,
            stock_item: None,
        };
        let l = form_line(&proposal);
        assert_eq!(l.merge_label, "");
        assert_eq!(l.mode, Some(ReceptionMode::New));
    }

    // -- parse_form -----------------------------------------------------------

    #[test]
    fn the_numbered_fields_come_back_as_lines_in_their_order() {
        let a = id(1).to_string();
        let b = id(2).to_string();
        let lines = parse_form(&pairs(&[
            ("id.1", &b),
            ("name.1", "Pain"),
            ("merge_label.1", ""),
            ("quantity.1", "1"),
            ("unit.1", "unité"),
            ("expires_on.1", ""),
            ("mode.1", "new"),
            ("exclude.1", "on"),
            ("id.0", &a),
            ("name.0", "Lait"),
            ("merge_label.0", "Ajouter au stock existant (1 L)"),
            ("quantity.0", "2"),
            ("unit.0", "L"),
            ("expires_on.0", "2026-11-12"),
            ("mode.0", "merge"),
        ]));
        assert_eq!(
            lines,
            vec![
                FormLine {
                    grocery_item_id: id(1),
                    name: "Lait".into(),
                    merge_label: "Ajouter au stock existant (1 L)".into(),
                    quantity: "2".into(),
                    unit: "L".into(),
                    expires_on: "2026-11-12".into(),
                    mode: Some(ReceptionMode::Merge),
                    excluded: false,
                },
                FormLine {
                    grocery_item_id: id(2),
                    name: "Pain".into(),
                    merge_label: String::new(),
                    quantity: "1".into(),
                    unit: "unité".into(),
                    expires_on: String::new(),
                    mode: Some(ReceptionMode::New),
                    excluded: true,
                },
            ]
        );
    }

    #[test]
    fn indices_are_ordered_as_numbers() {
        let ids: Vec<String> = (0..12).map(|n| id(n).to_string()).collect();
        let raw: Vec<(String, String)> = ids
            .iter()
            .enumerate()
            .rev()
            .map(|(i, v)| (format!("id.{i}"), v.clone()))
            .collect();
        let lines = parse_form(&raw);
        let got: Vec<Uuid> = lines.iter().map(|l| l.grocery_item_id).collect();
        assert_eq!(got, (0..12).map(id).collect::<Vec<_>>());
    }

    #[test]
    fn a_line_without_a_valid_id_is_dropped() {
        let lines = parse_form(&pairs(&[
            ("id.0", "not-a-uuid"),
            ("quantity.0", "2"),
            ("quantity.1", "3"),
            ("id.x", &id(3).to_string()),
        ]));
        assert!(lines.is_empty(), "{lines:?}");
    }

    #[test]
    fn an_unknown_mode_reads_as_the_proposal() {
        let lines = parse_form(&pairs(&[("id.0", &id(1).to_string()), ("mode.0", "both")]));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].mode, None);
        assert_eq!(lines[0].quantity, "");
        assert!(!lines[0].excluded);
    }

    #[test]
    fn a_repeated_field_keeps_its_first_occurrence() {
        let lines = parse_form(&pairs(&[
            ("id.0", &id(1).to_string()),
            ("id.0", &id(2).to_string()),
            ("quantity.0", "2"),
            ("quantity.0", "5"),
        ]));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].grocery_item_id, id(1));
        assert_eq!(lines[0].quantity, "2");
    }

    // -- validate_lines -------------------------------------------------------

    #[test]
    fn valid_lines_become_the_lot_and_excluded_ones_stay_out() {
        let mut lait = line(1, "Lait", ReceptionMode::Merge);
        lait.quantity = " 1.5 ".into();
        lait.unit = " L ".into();
        let mut pain = line(2, "Pain", ReceptionMode::New);
        pain.excluded = true;
        let mut oeufs = line(3, "Œufs", ReceptionMode::New);
        oeufs.unit = "".into();
        oeufs.expires_on = "2026-11-20".into();
        let stock = [candidate(10, "Lait", "L", None)];
        assert_eq!(
            validate_lines(&[lait, pain, oeufs], &stock),
            Ok(vec![
                ReceivedItemRequest {
                    grocery_item_id: id(1),
                    name: None,
                    quantity: Some(1.5),
                    unit: Some("L".into()),
                    expires_on: None,
                    mode: Some(ReceptionMode::Merge),
                },
                ReceivedItemRequest {
                    grocery_item_id: id(3),
                    name: None,
                    quantity: Some(2.0),
                    // An emptied unit is sent empty: apps/api normalizes it
                    // to « unité », as a stock creation does, instead of
                    // falling back on the list's.
                    unit: Some(String::new()),
                    expires_on: Some(date("2026-11-20")),
                    mode: Some(ReceptionMode::New),
                },
            ])
        );
    }

    #[test]
    fn a_quantity_must_be_positive() {
        for q in ["", "  ", "abc", "0", "-1", "0.0"] {
            let mut l = line(1, "Pain", ReceptionMode::New);
            l.quantity = q.into();
            assert_eq!(
                validate_lines(&[l], &[]),
                Err(LotError::Lines(vec![Some(
                    LineError::QuantityMustBePositive
                )])),
                "{q:?}"
            );
        }
    }

    #[test]
    fn a_quantity_is_bounded() {
        for q in ["1000000001", "1e10", "inf", "NaN"] {
            let mut l = line(1, "Pain", ReceptionMode::New);
            l.quantity = q.into();
            assert_eq!(
                validate_lines(&[l], &[]),
                Err(LotError::Lines(vec![Some(LineError::QuantityTooLarge)])),
                "{q:?}"
            );
        }
        let mut l = line(1, "Pain", ReceptionMode::New);
        l.quantity = "1000000000".into();
        assert!(validate_lines(&[l], &[]).is_ok());
    }

    #[test]
    fn a_date_must_be_a_calendar_day() {
        for d in ["12/11/2026", "2026-02-30", "demain"] {
            let mut l = line(1, "Pain", ReceptionMode::New);
            l.expires_on = d.into();
            assert_eq!(
                validate_lines(&[l], &[]),
                Err(LotError::Lines(vec![Some(LineError::InvalidDate)])),
                "{d:?}"
            );
        }
    }

    #[test]
    fn a_merge_with_another_date_than_the_stock_line_has_nothing_to_merge_into() {
        let mut l = line(1, "Lait", ReceptionMode::Merge);
        l.expires_on = "2026-11-20".into();
        let stock = [candidate(10, "Lait", "L", Some(date("2026-11-12")))];
        assert_eq!(
            validate_lines(&[l.clone()], &stock),
            Err(LotError::Lines(vec![Some(LineError::NoStockToMerge)]))
        );
        // The same date as the line, or no date, merges.
        l.expires_on = "2026-11-12".into();
        assert!(validate_lines(&[l.clone()], &stock).is_ok());
        l.expires_on = String::new();
        assert!(validate_lines(&[l], &stock).is_ok());
    }

    #[test]
    fn a_merge_finds_any_eligible_stock_line() {
        // Two lines of milk: the proposal named the earliest, the date typed
        // is the other's — apps/api merges into that one.
        let mut l = line(1, "Lait", ReceptionMode::Merge);
        l.expires_on = "2026-11-20".into();
        let stock = [
            candidate(10, "Lait", "L", Some(date("2026-11-12"))),
            candidate(11, "lait ", "L", Some(date("2026-11-20"))),
        ];
        assert!(validate_lines(&[l], &stock).is_ok());
    }

    #[test]
    fn a_merge_reads_the_unit_as_apps_api_normalizes_it() {
        let mut l = line(1, "Pain", ReceptionMode::Merge);
        l.unit = "".into();
        let stock = [candidate(10, "Pain", "unité", None)];
        assert!(validate_lines(&[l.clone()], &stock).is_ok());
        l.unit = "kg".into();
        assert_eq!(
            validate_lines(&[l], &stock),
            Err(LotError::Lines(vec![Some(LineError::NoStockToMerge)]))
        );
    }

    #[test]
    fn a_merge_may_go_into_a_line_made_earlier_in_the_lot() {
        let first = line(1, "Lait", ReceptionMode::New);
        let second = line(2, "Lait", ReceptionMode::Merge);
        assert!(validate_lines(&[first.clone(), second.clone()], &[]).is_ok());
        // Not into one made later, nor into one left out.
        assert_eq!(
            validate_lines(&[second.clone(), first.clone()], &[]),
            Err(LotError::Lines(vec![Some(LineError::NoStockToMerge), None]))
        );
        let mut out = first;
        out.excluded = true;
        assert_eq!(
            validate_lines(&[out, second], &[]),
            Err(LotError::Lines(vec![None, Some(LineError::NoStockToMerge)]))
        );
    }

    #[test]
    fn an_excluded_line_is_not_checked() {
        let mut l = line(1, "Pain", ReceptionMode::Merge);
        l.quantity = "abc".into();
        l.expires_on = "demain".into();
        l.excluded = true;
        let ok = line(2, "Riz", ReceptionMode::New);
        assert_eq!(validate_lines(&[l, ok], &[]).map(|lot| lot.len()), Ok(1));
    }

    #[test]
    fn every_line_in_error_is_reported() {
        let mut a = line(1, "Pain", ReceptionMode::New);
        a.quantity = "0".into();
        let b = line(2, "Riz", ReceptionMode::New);
        let mut c = line(3, "Sel", ReceptionMode::New);
        c.expires_on = "x".into();
        assert_eq!(
            validate_lines(&[a, b, c], &[]),
            Err(LotError::Lines(vec![
                Some(LineError::QuantityMustBePositive),
                None,
                Some(LineError::InvalidDate),
            ]))
        );
    }

    #[test]
    fn nothing_left_to_receive_is_its_own_error() {
        let mut l = line(1, "Pain", ReceptionMode::New);
        l.excluded = true;
        assert_eq!(validate_lines(&[l], &[]), Err(LotError::NothingToReceive));
        assert_eq!(validate_lines(&[], &[]), Err(LotError::NothingToReceive));
    }

    #[test]
    fn a_line_without_a_mode_is_left_to_the_proposal() {
        let mut l = line(1, "Lait", ReceptionMode::Merge);
        l.mode = None;
        l.expires_on = "2026-11-20".into();
        let stock = [candidate(10, "Lait", "L", Some(date("2026-11-12")))];
        let lot = validate_lines(&[l], &stock).unwrap();
        assert_eq!(lot[0].mode, None);
    }

    // -- page_body ------------------------------------------------------------

    #[test]
    fn the_page_renders_one_fieldset_per_line_with_its_fields() {
        let mut lait = line(1, "Lait <bio>", ReceptionMode::Merge);
        lait.expires_on = "2026-11-12".into();
        let pain = line(2, "Pain", ReceptionMode::New);
        let html = page_body(&[lait, pain], &[None, None], "");
        assert!(
            html.contains(r#"<form method="post" action="/grocery-list/reception">"#),
            "{html}"
        );
        assert_eq!(
            html.matches("<fieldset class=\"card\">").count(),
            2,
            "{html}"
        );
        assert!(html.contains("<legend>Lait &lt;bio&gt;</legend>"), "{html}");
        let a = id(1);
        for field in [
            format!(r#"<input type="hidden" name="id.0" value="{a}"/>"#),
            r#"<input type="hidden" name="name.0" value="Lait &lt;bio&gt;"/>"#.to_string(),
            r#"<input type="number" name="quantity.0" step="any" min="0" required value="2"/>"#
                .to_string(),
            r#"<input type="text" name="unit.0" value="L" placeholder="unité si vide (ex. kg, L)"/>"#
                .to_string(),
            r#"<input type="date" name="expires_on.0" value="2026-11-12" data-expiry-shortcuts/>"#
                .to_string(),
            r#"<input type="radio" name="mode.0" value="merge" checked/> Ajouter au stock existant (1 L)</label>"#
                .to_string(),
            r#"<input type="radio" name="mode.0" value="new"/> Nouvelle ligne de stock</label>"#
                .to_string(),
            r#"<input type="checkbox" name="exclude.0"/> Ne pas réceptionner</label>"#.to_string(),
        ] {
            assert!(html.contains(&field), "{field}\n{html}");
        }
        // A new proposal: no merge choice, the mode rides a hidden field.
        assert!(
            html.contains(r#"<input type="hidden" name="mode.1" value="new"/>"#),
            "{html}"
        );
        assert!(!html.contains(r#"name="mode.1" value="merge""#), "{html}");
        assert!(
            html.contains("<button type=\"submit\">Réceptionner</button>"),
            "{html}"
        );
        assert!(!html.contains("field-error"), "{html}");
    }

    #[test]
    fn a_line_in_error_shows_its_message_and_keeps_what_was_typed() {
        let mut lait = line(1, "Lait", ReceptionMode::New);
        lait.quantity = "abc".into();
        lait.unit = "bouteille".into();
        lait.expires_on = "2026-11-12".into();
        lait.merge_label = "Ajouter au stock existant (1 L)".into();
        let mut pain = line(2, "Pain", ReceptionMode::New);
        pain.excluded = true;
        let html = page_body(
            &[lait, pain],
            &[Some(LineError::QuantityMustBePositive), None],
            r#"<p class="notice error">x</p>"#,
        );
        assert!(html.contains(r#"<p class="notice error">x</p>"#), "{html}");
        assert_eq!(html.matches("field-error").count(), 1, "{html}");
        assert!(
            html.contains(&format!(
                r#"<p class="field-error">{}</p>"#,
                LineError::QuantityMustBePositive.text()
            )),
            "{html}"
        );
        assert!(
            html.contains(r#"name="quantity.0" step="any" min="0" required value="abc""#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="unit.0" value="bouteille""#),
            "{html}"
        );
        // The choice made is the one checked again.
        assert!(
            html.contains(r#"name="mode.0" value="new" checked/>"#),
            "{html}"
        );
        assert!(html.contains(r#"name="mode.0" value="merge"/>"#), "{html}");
        assert!(
            html.contains(r#"<input type="checkbox" name="exclude.1" checked/>"#),
            "{html}"
        );
    }

    #[test]
    fn every_error_has_its_words() {
        for e in [
            LineError::QuantityMustBePositive,
            LineError::QuantityTooLarge,
            LineError::InvalidDate,
            LineError::NoStockToMerge,
        ] {
            assert!(!e.text().is_empty(), "{e:?}");
        }
    }

    #[test]
    fn without_checked_items_the_page_says_so() {
        let html = page_body(&[], &[], "");
        assert!(html.contains("Aucun article coché"), "{html}");
        assert!(!html.contains("<form"), "{html}");
        assert!(html.contains(r#"href="/grocery-list""#), "{html}");
    }
}
