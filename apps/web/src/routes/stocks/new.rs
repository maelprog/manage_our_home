//! `/stocks/new` — manually add a stock item (name, category, quantity, unit,
//! reorder threshold, expiry date). The empty-name / empty-unit / negative-quantity /
//! negative-threshold rules are pre-validated by the shared
//! `validate_item_form` (inline error, no round trip); the backend's matching
//! 400s are mapped defensively. Any member may create, so 403 isn't reachable
//! via the UI. Success (201) → PRG `/stocks?notice=item_created`.
//!
//! Barcode (#402): `GET /stocks/new?scan=<raw>` asks apps/api what the code
//! stands for (`POST /groups/:id/stock-items/scan`) and offers, in order:
//! the "Déjà en stock" screen when an article of the family carries the code
//! (its "+1" is the quantity adjustment any member may make, and a link
//! creates another article anyway, without the code); otherwise the form,
//! pre-filled from Open Food Facts when the product is known, with the code
//! in a hidden field; a weighing label, an unknown product or a nameless
//! record get the same form, empty but for the code; a string that is no
//! code gets the form and a message. The `scan` value comes from the
//! "Code-barres" field — a GET form that works without JavaScript — or from
//! the "Scanner" button (`SCAN_SCRIPT`), which only decodes the image.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::NaiveDate;
use manage_our_home_shared::dto::stocks::{
    CreateStockItemRequest, ScanRequest, ScanResult, ScannedProduct, StockItemResponse,
};
use manage_our_home_shared::validation::stocks::{validate_item_form, ItemFormError};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::assets::{Script, Vendored};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{family_context, fmt_num, forbidden_page, stocks_cookie, FamilyContext};

/// The "Scanner" button's behaviour (`assets::Script::StockScan`).
pub(crate) const SCAN_SCRIPT: &str = include_str!("../../stock_scan.js");

/// The shared item form fields, used by both create and edit. `quantity` /
/// `reorder_threshold` arrive as strings and are parsed in the handler.
#[derive(serde::Deserialize, Default)]
pub struct ItemForm {
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub quantity: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub reorder_threshold: String,
    /// `YYYY-MM-DD` from the `<input type="date">`, empty for no date.
    #[serde(default)]
    pub expires_on: String,
    /// The scanned code (#402), carried by a hidden field of the creation
    /// form; empty for an article entered without one.
    #[serde(default)]
    pub barcode: String,
}

/// French copy for a stock-item form error code.
pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "invalid_barcode" => {
            "Ce code-barres n'est pas reconnu : un EAN-13, un EAN-8 ou un UPC-A est attendu."
        }
        "barcode_already_in_stock" => {
            "Un article de la famille porte déjà ce code-barres : celui-ci sera ajouté sans code si vous validez à nouveau."
        }
        "name_required" => "Le nom est obligatoire.",
        "unit_required" => "L'unité est obligatoire.",
        "quantity_must_be_non_negative" => "La quantité ne peut pas être négative.",
        "reorder_threshold_must_be_non_negative" => "Le seuil de réappro ne peut pas être négatif.",
        "invalid_expires_on" => "La date de péremption n'est pas une date valide.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

/// Maps a shared `ItemFormError` to the backend's matching error code, so the
/// inline message and a defensively-mapped 400 read identically.
pub(crate) fn form_error_code(err: ItemFormError) -> &'static str {
    match err {
        ItemFormError::NameRequired => "name_required",
        ItemFormError::UnitRequired => "unit_required",
        ItemFormError::QuantityNegative => "quantity_must_be_non_negative",
        ItemFormError::ThresholdNegative => "reorder_threshold_must_be_non_negative",
    }
}

/// Parses a form quantity string to a non-negative `f64`, defaulting an empty
/// input to `0.0`. Returns `None` only on an unparseable value.
pub(crate) fn parse_quantity(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        Some(0.0)
    } else {
        t.parse::<f64>().ok()
    }
}

/// Parses an optional threshold: empty string → `None` (no threshold), else a
/// parsed `f64` wrapped in `Some`. `Err(())` on an unparseable value.
pub(crate) fn parse_threshold(s: &str) -> Result<Option<f64>, ()> {
    let t = s.trim();
    if t.is_empty() {
        Ok(None)
    } else {
        t.parse::<f64>().map(Some).map_err(|_| ())
    }
}

/// Parses the optional expiry date: empty → `Ok(None)` (no date), a
/// `YYYY-MM-DD` date → `Ok(Some)`, anything else → `Err(())`. Unlike the
/// budget date, a bad value is refused rather than dropped: silently losing
/// an expiry date would hide the very warning the field exists for.
pub(crate) fn parse_expires_on(s: &str) -> Result<Option<NaiveDate>, ()> {
    let t = s.trim();
    if t.is_empty() {
        Ok(None)
    } else {
        NaiveDate::parse_from_str(t, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| ())
    }
}

/// Renders the item form fields (shared markup, pre-filled). Used by create
/// (empty defaults) and edit (existing values).
pub(crate) fn form_fields(
    name: &str,
    category: &str,
    quantity: &str,
    unit: &str,
    reorder_threshold: &str,
    expires_on: &str,
) -> String {
    format!(
        r#"<label>Nom <input type="text" name="name" required value="{name}"/></label>
<label>Catégorie <input type="text" name="category" value="{category}" placeholder="Optionnel (ex. Cellier, Frigo)"/></label>
<label>Quantité <input type="number" name="quantity" step="any" min="0" value="{quantity}"/></label>
<label>Unité <input type="text" name="unit" required value="{unit}" placeholder="ex. kg, L, unité"/></label>
<label>Seuil de réappro
<input type="number" name="reorder_threshold" step="any" min="0" value="{reorder_threshold}" placeholder="Optionnel — laisser vide pour aucun seuil"/>
<span class="muted">En dessous ou à ce niveau, l'article est signalé « stock bas ». Partagé au niveau de la famille.</span>
</label>
<label>Date de péremption
<input type="date" name="expires_on" value="{expires_on}"/>
<span class="muted">Optionnel. Si l'article en a plusieurs, la plus proche.</span>
</label>"#,
        name = html_escape(name),
        category = html_escape(category),
        quantity = html_escape(quantity),
        unit = html_escape(unit),
        reorder_threshold = html_escape(reorder_threshold),
        expires_on = html_escape(expires_on),
    )
}

/// The article name the form is pre-filled with: the product's name, with
/// its pack size in parentheses when Open Food Facts gives one and the name
/// does not already carry it — two sizes of a product are two codes, so two
/// articles, which the name alone would not tell apart.
pub(crate) fn product_label(product: &ScannedProduct) -> String {
    match product.quantity.as_deref() {
        Some(size) if !product.name.to_lowercase().contains(&size.to_lowercase()) => {
            format!("{} ({size})", product.name)
        }
        _ => product.name.clone(),
    }
}

/// What a scan pre-fills: the product's label (`product_label`), one unit
/// in hand, and the code in the hidden field unless `with_code` is false
/// ("créer quand même un autre article": one article per code).
pub(crate) fn prefilled_form(
    code: &str,
    product: Option<&ScannedProduct>,
    with_code: bool,
) -> ItemForm {
    ItemForm {
        name: product.map(product_label).unwrap_or_default(),
        quantity: "1".to_string(),
        unit: "unité".to_string(),
        barcode: if with_code {
            code.to_string()
        } else {
            String::new()
        },
        ..Default::default()
    }
}

/// The ODbL attribution Open Food Facts' data requires wherever it is shown.
const OFF_ATTRIBUTION: &str = r#"<p class="muted">Données produits : <a href="https://world.openfoodfacts.org">Open Food Facts</a>, <a href="https://opendatacommons.org/licenses/odbl/1-0/">ODbL</a>.</p>"#;

/// The barcode block above the form: the "Code-barres" field, a GET form that
/// needs no JavaScript, then the "Scanner" button, `hidden` until
/// `SCAN_SCRIPT` finds a camera API to reveal it.
fn scan_block(scan_value: &str) -> String {
    format!(
        r#"<form method="get" action="/stocks/new" class="card inline">
<label>Code-barres <input type="text" name="scan" inputmode="numeric" autocomplete="off" value="{scan}"/></label>
<button type="submit" class="secondary">Chercher le produit</button>
</form>
<div data-scan data-scan-polyfill="{polyfill}" data-scan-wasm="{wasm}" hidden>
<div data-scan-idle><button type="button" class="secondary">Scanner</button></div>
<div data-scan-view hidden><video muted playsinline></video>
<button type="button" class="secondary" data-scan-stop>Fermer la caméra</button></div>
<p class="muted" data-scan-status aria-live="polite"></p>
</div>"#,
        scan = html_escape(scan_value),
        polyfill = Vendored::BarcodePolyfill.href(),
        wasm = Vendored::ZxingReaderWasm.href(),
    )
}

/// What sits between the barcode block and the form: a notice saying what
/// the scan found, and the attribution when Open Food Facts supplied data.
#[derive(Default)]
struct Intro {
    notice: Option<&'static str>,
    attribution: bool,
}

fn page(
    header: &str,
    form: &ItemForm,
    error: Option<&str>,
    scan_value: &str,
    intro: &Intro,
) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let notice_html = intro
        .notice
        .map(|n| format!(r#"<p class="notice">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let attribution = if intro.attribution {
        OFF_ATTRIBUTION
    } else {
        ""
    };
    let fields = form_fields(
        &form.name,
        &form.category,
        &form.quantity,
        &form.unit,
        &form.reorder_threshold,
        &form.expires_on,
    );
    let barcode_html = if form.barcode.is_empty() {
        String::new()
    } else {
        format!(
            r#"<input type="hidden" name="barcode" value="{code}"/>
<p class="muted">Code-barres associé : {code}</p>"#,
            code = html_escape(&form.barcode),
        )
    };
    let body = format!(
        r#"<h1>Nouvel article</h1>
{error_html}
{scan}
{notice_html}
<form method="post" action="/stocks/new">
{fields}
{barcode_html}
<button type="submit">Ajouter l'article</button>
</form>
{attribution}
<div class="links"><a href="/stocks">Retour aux stocks</a></div>
{script}"#,
        scan = scan_block(scan_value),
        script = Script::StockScan.tag(),
    );
    shell_with_header(Width::Form, "Nouvel article", header, &body)
}

/// "Déjà en stock" (#402): the family's article carrying the scanned code,
/// with "+1" — the quantity adjustment any member may make, through the
/// detail page's own route — and a way to create another article anyway.
fn already_in_stock_page(header: &str, item: &StockItemResponse, code: &str) -> String {
    let id = item.id;
    let body = format!(
        r#"<h1>Déjà en stock</h1>
<p>Déjà en stock : <strong>{name}</strong> ({qty} {unit})</p>
<form method="post" action="/stocks/{id}/adjust" class="actions">
<input type="hidden" name="quantity" value="{next}"/>
<button type="submit">+1</button>
</form>
<div class="links">
<a href="/stocks/{id}">Voir l'article</a>
<a href="/stocks/new?scan={code}&amp;other=1">Créer quand même un autre article</a>
<a href="/stocks">Retour aux stocks</a>
</div>"#,
        name = html_escape(&item.name),
        qty = html_escape(&fmt_num(item.quantity)),
        unit = html_escape(&item.unit),
        next = html_escape(&fmt_num(item.quantity + 1.0)),
        code = html_escape(code),
    );
    shell_with_header(Width::Form, "Déjà en stock", header, &body)
}

#[derive(serde::Deserialize, Default)]
pub struct NewQuery {
    /// The scanned or typed code, as is.
    #[serde(default)]
    scan: String,
    /// Set by "Créer quand même un autre article": the form without the code.
    #[serde(default)]
    other: Option<String>,
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<NewQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/stocks/new").await else {
        return Redirect::to("/groups/new").into_response();
    };
    if query.scan.trim().is_empty() {
        // Default a fresh form to quantity 0.
        let form = ItemForm {
            quantity: "0".to_string(),
            ..Default::default()
        };
        return Html(page(&fam.header, &form, None, "", &Intro::default())).into_response();
    }
    scanned(&state, &headers, &fam, &query.scan, query.other.is_some()).await
}

/// `GET /stocks/new?scan=…`: what the code stands for, and the page for it.
async fn scanned(
    state: &AppState,
    headers: &HeaderMap,
    fam: &FamilyContext,
    raw: &str,
    other: bool,
) -> Response {
    let cookie = stocks_cookie(headers);
    let body = ScanRequest {
        raw: raw.to_string(),
    };
    let result = api_request_auth(
        state,
        reqwest::Method::POST,
        &format!("/groups/{}/stock-items/scan", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&body).unwrap()),
    )
    .await;
    let manual = |error: &str| {
        let form = ItemForm {
            quantity: "0".to_string(),
            ..Default::default()
        };
        Html(page(
            &fam.header,
            &form,
            Some(error_message(error)),
            raw,
            &Intro::default(),
        ))
        .into_response()
    };
    let scan = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<ScanResult>(resp.body) {
                Ok(scan) => scan,
                Err(_) => return manual("unavailable"),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            return manual("invalid_barcode")
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            return forbidden_page().into_response()
        }
        Ok(_) | Err(_) => return manual("unavailable"),
    };
    let Some(code) = scan.code.as_deref() else {
        return manual("invalid_barcode");
    };

    let existing = match scan.existing_item_id {
        Some(item_id) => fetch_item(state, cookie.as_deref(), fam.gid, item_id).await,
        None => None,
    };
    if let (Some(item), false) = (&existing, other) {
        return Html(already_in_stock_page(&fam.header, item, code)).into_response();
    }

    let mut form = prefilled_form(code, scan.product.as_ref(), !other);
    // A code already in stock brings no product (apps/api does not ask Open
    // Food Facts for it): "another article anyway" starts from the name of
    // the article that carries it.
    if let (Some(item), true) = (&existing, form.name.is_empty()) {
        form.name = item.name.clone();
    }
    let notice = if other {
        "Cet article ne portera pas le code-barres : un seul article par code dans la famille."
    } else if scan.weighed {
        "Étiquette de pesée du magasin : elle ne désigne pas un produit référencé. Saisissez l'article à la main."
    } else if scan.product.is_some() {
        "Produit trouvé : vérifiez les champs avant d'ajouter l'article."
    } else {
        "Aucune fiche produit pour ce code : saisissez l'article à la main, le code-barres lui sera associé."
    };
    let intro = Intro {
        notice: Some(notice),
        attribution: scan.product.is_some(),
    };
    Html(page(&fam.header, &form, None, code, &intro)).into_response()
}

async fn fetch_item(
    state: &AppState,
    cookie: Option<&str>,
    gid: Uuid,
    item_id: Uuid,
) -> Option<StockItemResponse> {
    let resp = api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/stock-items/{item_id}"),
        cookie,
        None,
    )
    .await
    .ok()?;
    if resp.status != reqwest::StatusCode::OK {
        return None;
    }
    serde_json::from_value(resp.body).ok()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ItemForm>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, "/stocks/new").await else {
        return Redirect::to("/groups/new").into_response();
    };

    let render_error = |code: &str| {
        Html(page(
            &fam.header,
            &form,
            Some(error_message(code)),
            "",
            &Intro::default(),
        ))
        .into_response()
    };

    let Some(quantity) = parse_quantity(&form.quantity) else {
        return render_error("quantity_must_be_non_negative");
    };
    let Ok(reorder_threshold) = parse_threshold(&form.reorder_threshold) else {
        return render_error("reorder_threshold_must_be_non_negative");
    };
    let Ok(expires_on) = parse_expires_on(&form.expires_on) else {
        return render_error("invalid_expires_on");
    };

    if let Err(e) = validate_item_form(&form.name, &form.unit, quantity, reorder_threshold) {
        return render_error(form_error_code(e));
    }

    let category = {
        let c = form.category.trim();
        (!c.is_empty()).then(|| c.to_string())
    };
    let req = CreateStockItemRequest {
        name: form.name.trim().to_string(),
        category,
        quantity,
        unit: form.unit.trim().to_string(),
        reorder_threshold,
        expires_on,
        barcode: {
            let b = form.barcode.trim();
            (!b.is_empty()).then(|| b.to_string())
        },
    };

    let cookie = stocks_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/stock-items", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/stocks?notice=item_created").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            // The shared pre-validation should have caught these; map the
            // backend's exact code defensively if one slips through.
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unavailable");
            render_error(code)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            // Another article of the family took the code since the scan.
            // The form comes back without it, so a second submission
            // creates this one plain, as the message says.
            let form = ItemForm {
                barcode: String::new(),
                ..form
            };
            Html(page(
                &fam.header,
                &form,
                Some(error_message("barcode_already_in_stock")),
                "",
                &Intro::default(),
            ))
            .into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) | Err(_) => render_error("unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(name: &str, quantity: Option<&str>) -> ScannedProduct {
        ScannedProduct {
            name: name.into(),
            quantity: quantity.map(Into::into),
            categories_tags: vec!["en:spreads".into()],
        }
    }

    #[test]
    fn the_label_carries_the_pack_size() {
        assert_eq!(
            product_label(&product("Nutella", Some("400 g"))),
            "Nutella (400 g)"
        );
    }

    #[test]
    fn a_product_without_a_pack_size_is_labelled_by_its_name() {
        assert_eq!(product_label(&product("Nutella", None)), "Nutella");
    }

    #[test]
    fn a_name_that_already_says_the_size_is_not_repeated() {
        assert_eq!(
            product_label(&product("Coca-Cola 33 cl", Some("33 cl"))),
            "Coca-Cola 33 cl"
        );
        // Compared without regard to case.
        assert_eq!(
            product_label(&product("Lait demi-écrémé 1L", Some("1l"))),
            "Lait demi-écrémé 1L"
        );
    }

    #[test]
    fn a_known_product_prefills_name_one_unit_and_the_code() {
        let form = prefilled_form(
            "3017620422003",
            Some(&product("Nutella", Some("400 g"))),
            true,
        );
        assert_eq!(form.name, "Nutella (400 g)");
        assert_eq!(form.quantity, "1");
        assert_eq!(form.unit, "unité");
        assert_eq!(form.barcode, "3017620422003");
        assert_eq!(form.category, "");
        assert_eq!(form.expires_on, "");
    }

    #[test]
    fn an_unknown_product_prefills_the_code_only() {
        let form = prefilled_form("2123456012347", None, true);
        assert_eq!(form.name, "");
        assert_eq!(form.quantity, "1");
        assert_eq!(form.unit, "unité");
        assert_eq!(form.barcode, "2123456012347");
    }

    #[test]
    fn another_article_anyway_leaves_the_code_out() {
        let form = prefilled_form("3017620422003", Some(&product("Nutella", None)), false);
        assert_eq!(form.name, "Nutella");
        assert_eq!(form.barcode, "");
    }

    #[test]
    fn the_scan_block_works_without_javascript_and_hides_the_camera() {
        let html = scan_block("3017620422003");
        // The no-JS path: a GET form whose field is named `scan`.
        assert!(
            html.contains(r#"<form method="get" action="/stocks/new""#),
            "{html}"
        );
        assert!(html.contains(r#"name="scan""#), "{html}");
        assert!(html.contains(r#"value="3017620422003""#), "{html}");
        // The camera block stays hidden until the script reveals it, and
        // points at the files the binary serves.
        assert!(html.contains("data-scan "), "{html}");
        assert!(html.contains(" hidden>"), "{html}");
        assert!(html.contains(Vendored::BarcodePolyfill.href()), "{html}");
        assert!(html.contains(Vendored::ZxingReaderWasm.href()), "{html}");
    }

    #[test]
    fn a_typed_value_is_escaped_back_into_the_field() {
        let html = scan_block(r#""><script>x</script>"#);
        assert!(!html.contains("<script>x"), "{html}");
        assert!(html.contains("&quot;&gt;&lt;script&gt;"), "{html}");
    }
}
