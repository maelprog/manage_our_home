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
//! "Code-barres" field — a GET form — or from the photo of the barcode
//! (`POST /stocks/new/photo`, decoded in memory by `photo::decode_photo`),
//! both without JavaScript. A photo that yields no code lands on
//! `?photo=<error>`: take the photo again, or type the code, and the scan
//! runs again — never straight to the manual form.
//!
//! GS1 2D codes (#403): a DataMatrix or QR code may carry an expiry date,
//! which apps/api reads and returns with the product. It pre-fills the
//! form's date, marked "lue sur le code", for the member to check; on
//! "Déjà en stock", a sooner date than the article's is proposed through
//! the edit form, never written by the scan.
//!
//! Without such a date, a known product's Open Food Facts category may
//! propose one (#404, apps/api's `stocks::shelf_life`): the form shows it
//! "proposée d'après la catégorie …", for the member to check. The date
//! field also gets "+3 j", "+1 sem." and "+1 mois" shortcuts, put there by
//! `app::ENHANCE_SCRIPT` (`data-expiry-shortcuts`) and counted from the
//! browser's day: without JavaScript the field is typed by hand.

use axum::extract::{Multipart, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::NaiveDate;
use manage_our_home_shared::dto::stocks::{
    CreateStockItemRequest, ExpiresOnSource, ScanRequest, ScanResult, ScannedProduct,
    StockItemResponse,
};
use manage_our_home_shared::validation::stocks::{validate_item_form, ItemFormError};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::photo::{decode_photo, PhotoError, DECODES, MAX_PHOTO_BYTES};
use super::{
    can_modify, family_context, fmt_date, fmt_num, forbidden_page, service_unavailable_page,
    stocks_cookie, FamilyContext,
};

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
    /// Where `expires_on` comes from when the member did not type it: the
    /// field says so. Never sent by the browser.
    #[serde(skip)]
    pub expires_on_origin: DateOrigin,
}

/// Where the date in the form comes from, said next to the field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DateOrigin {
    /// The article's own date, or none: nothing to say.
    #[default]
    Member,
    /// Read on a GS1 code (#403).
    Code,
    /// The scan's day plus the shelf life of this Open Food Facts category,
    /// by its French name (#404).
    Category(String),
}

/// The date a scan proposes, and where it comes from (`DateOrigin`).
/// A date without a source is left out rather than shown as the member's.
pub(crate) fn proposed_date(scan: &ScanResult) -> Option<(NaiveDate, DateOrigin)> {
    let origin = match scan.expires_on_source? {
        ExpiresOnSource::Gs1 => DateOrigin::Code,
        ExpiresOnSource::Category => {
            DateOrigin::Category(scan.expires_on_category.clone().unwrap_or_default())
        }
    };
    Some((scan.expires_on?, origin))
}

/// French copy for a stock-item form error code.
pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "invalid_barcode" => {
            "Ce code-barres n'est pas reconnu : un EAN-13, un EAN-8, un UPC-A ou un code GS1 (DataMatrix, QR) est attendu."
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
    expires_on_origin: &DateOrigin,
) -> String {
    // Next to the field, so the member checks it before saving.
    let origin_note = match expires_on_origin {
        DateOrigin::Member => String::new(),
        DateOrigin::Code => {
            "\n<span class=\"muted\">Lue sur le code : vérifiez-la, vous pouvez la corriger.</span>"
                .to_string()
        }
        DateOrigin::Category(label) => {
            let category = if label.is_empty() {
                "du produit".to_string()
            } else {
                format!("« {} »", html_escape(label))
            };
            format!("\n<span class=\"muted\">Proposée d'après la catégorie {category} : vérifiez-la, vous pouvez la corriger.</span>")
        }
    };
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
<input type="date" name="expires_on" value="{expires_on}" data-expiry-shortcuts/>{origin_note}
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
/// in hand, the code in the hidden field unless `with_code` is false
/// ("créer quand même un autre article": one article per code), and the
/// date the scan proposes (`proposed_date`), with where it comes from.
pub(crate) fn prefilled_form(
    code: &str,
    product: Option<&ScannedProduct>,
    with_code: bool,
    proposed: Option<(NaiveDate, DateOrigin)>,
) -> ItemForm {
    let (expires_on, expires_on_origin) = match proposed {
        Some((on, origin)) => (on.format("%Y-%m-%d").to_string(), origin),
        None => Default::default(),
    };
    ItemForm {
        name: product.map(product_label).unwrap_or_default(),
        quantity: "1".to_string(),
        unit: "unité".to_string(),
        barcode: if with_code {
            code.to_string()
        } else {
            String::new()
        },
        expires_on,
        expires_on_origin,
        ..Default::default()
    }
}

/// The date a GS1 code carries, proposed for the article already in stock
/// (#403) when it comes sooner than the article's own, or the article has
/// none. Never written without the member's say.
pub(crate) fn date_to_propose(
    read: Option<NaiveDate>,
    current: Option<NaiveDate>,
) -> Option<NaiveDate> {
    let read = read?;
    match current {
        Some(current) if current <= read => None,
        _ => Some(read),
    }
}

/// The ODbL attribution Open Food Facts' data requires wherever it is shown.
const OFF_ATTRIBUTION: &str = r#"<p class="muted">Données produits : <a href="https://world.openfoodfacts.org">Open Food Facts</a>, <a href="https://opendatacommons.org/licenses/odbl/1-0/">ODbL</a>.</p>"#;

/// The barcode block above the form, two plain forms that need no
/// JavaScript: the photo of the barcode (`capture="environment"` opens the
/// rear camera on a phone; `data-submit-on-change` sends it as soon as it is
/// taken where `enhance.js` runs, and the button sends it elsewhere), and
/// the "Code-barres" field, for the digits typed by hand.
fn scan_block(scan_value: &str) -> String {
    format!(
        r#"<form method="post" action="/stocks/new/photo" enctype="multipart/form-data" class="card inline">
<label>Photographier le code <input type="file" name="photo" accept="image/*" capture="environment" required data-submit-on-change/></label>
<button type="submit" class="secondary">Scanner</button>
</form>
<form method="get" action="/stocks/new" class="card inline">
<label>Code-barres <input type="text" name="scan" inputmode="numeric" autocomplete="off" value="{scan}"/></label>
<button type="submit" class="secondary">Chercher le produit</button>
</form>
<p class="muted">La photo est lue sur le serveur puis oubliée : elle n'est ni conservée ni transmise.</p>"#,
        scan = html_escape(scan_value),
    )
}

/// French copy for a failed photo scan (`?photo=`).
pub(crate) fn photo_error_message(code: &str) -> &'static str {
    match code {
        "unreadable" => "Aucun code-barres n'a pu être lu sur la photo : elle est peut-être floue, prise de trop loin, ou le code sort du cadre.",
        "too_large" => "La photo dépasse 12 Mo.",
        "not_an_image" => "Ce fichier n'est pas une photo lisible (JPEG ou PNG).",
        "busy" => "Trop d'envois en cours, merci de réessayer dans un instant.",
        _ => "L'envoi de la photo a échoué, merci de réessayer.",
    }
}

/// After a photo that gave no code: take it again, or type the digits —
/// either way the scan runs again. The manual article form is offered last,
/// not instead.
fn photo_retry_page(header: &str, code: &str) -> String {
    let body = format!(
        r#"<h1>Scanner un article</h1>
<p class="notice error">{message}</p>
<p>Reprenez la photo, ou tapez les chiffres imprimés sous les barres : le scan sera relancé.</p>
{scan}
<div class="links">
<a href="/stocks/new">Saisir l'article sans code-barres</a>
<a href="/stocks">Retour aux stocks</a>
</div>"#,
        message = html_escape(photo_error_message(code)),
        scan = scan_block(""),
    );
    shell_with_header(Width::Form, "Scanner un article", header, &body)
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
        &form.expires_on_origin,
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
<div class="links"><a href="/stocks">Retour aux stocks</a></div>"#,
        scan = scan_block(scan_value),
    );
    shell_with_header(Width::Form, "Nouvel article", header, &body)
}

/// "Déjà en stock" (#402): the family's article carrying the scanned code,
/// with "+1" — the quantity adjustment any member may make, through the
/// detail page's own route — and a way to create another article anyway,
/// which scans `raw`, the string as it came, again (a GS1 code's date then
/// reaches that form too). The date a GS1 code carried is proposed when it
/// comes sooner (`date_to_propose`, #403).
fn already_in_stock_page(
    header: &str,
    item: &StockItemResponse,
    raw: &str,
    proposal: Option<NaiveDate>,
    may_edit: bool,
) -> String {
    let proposal_html = match proposal {
        None => String::new(),
        Some(date) => {
            let compared = match item.expires_on {
                Some(current) => format!(
                    ", plus proche que celle de l'article ({})",
                    fmt_date(current)
                ),
                None => " ; l'article n'en a pas".to_string(),
            };
            // Proposed, never written from here: the edit form opens with
            // it, and saving is the member's call.
            let action = if may_edit {
                format!(
                    r#"
<p><a href="/stocks/{id}/edit?expires_on={iso}">Mettre cette date sur l'article</a></p>"#,
                    id = item.id,
                    iso = date.format("%Y-%m-%d"),
                )
            } else {
                r#"
<p class="muted">Seuls le créateur de l'article et les administrateurs de la famille peuvent changer sa date.</p>"#
                    .to_string()
            };
            format!(
                r#"
<p>Date de péremption lue sur le code : <strong>{date}</strong>{compared}.</p>{action}"#,
                date = fmt_date(date),
                compared = html_escape(&compared),
            )
        }
    };
    let id = item.id;
    let body = format!(
        r#"<h1>Déjà en stock</h1>
<p>Déjà en stock : <strong>{name}</strong> ({qty} {unit})</p>
<form method="post" action="/stocks/{id}/adjust" class="actions">
<input type="hidden" name="quantity" value="{next}"/>
<button type="submit">+1</button>
</form>{proposal_html}
<div class="links">
<a href="/stocks/{id}">Voir l'article</a>
<a href="{other}">Créer quand même un autre article</a>
<a href="/stocks">Retour aux stocks</a>
</div>"#,
        name = html_escape(&item.name),
        qty = html_escape(&fmt_num(item.quantity)),
        unit = html_escape(&item.unit),
        next = html_escape(&fmt_num(item.quantity + 1.0)),
        other = html_escape(&format!(
            "/stocks/new?{}&other=1",
            serde_urlencoded::to_string([("scan", raw)]).unwrap_or_default()
        )),
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
    /// Set after a photo that gave no code (`PhotoError::code`, `busy`,
    /// `failed`): the retry page.
    #[serde(default)]
    photo: Option<String>,
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
    if let Some(code) = query.photo.as_deref() {
        return Html(photo_retry_page(&fam.header, code)).into_response();
    }
    if query.scan.trim().is_empty() {
        // Default a fresh form to quantity 0.
        let form = ItemForm {
            quantity: "0".to_string(),
            ..Default::default()
        };
        return Html(page(&fam.header, &form, None, "", &Intro::default())).into_response();
    }
    scanned(
        &state,
        &headers,
        &fam,
        me.user_id,
        &query.scan,
        query.other.is_some(),
    )
    .await
}

/// `GET /stocks/new?scan=…`: what the code stands for, and the page for it.
async fn scanned(
    state: &AppState,
    headers: &HeaderMap,
    fam: &FamilyContext,
    user_id: Uuid,
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
        // Only a date read on the code is worth proposing for an article
        // that has its own (apps/api gives no other for it anyway).
        let read = match proposed_date(&scan) {
            Some((on, DateOrigin::Code)) => Some(on),
            _ => None,
        };
        let proposal = date_to_propose(read, item.expires_on);
        let may_edit = can_modify(&fam.role, item.created_by == user_id);
        return Html(already_in_stock_page(
            &fam.header,
            item,
            raw,
            proposal,
            may_edit,
        ))
        .into_response();
    }

    let mut form = prefilled_form(code, scan.product.as_ref(), !other, proposed_date(&scan));
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

/// Where a photo scan goes next: the code through `?scan=`, the same path as
/// the "Code-barres" field, or the retry page.
pub(crate) fn photo_redirect(outcome: Result<String, &str>) -> String {
    match outcome {
        Ok(code) => format!(
            "/stocks/new?{}",
            serde_urlencoded::to_string([("scan", code.as_str())]).unwrap_or_default()
        ),
        Err(error) => format!("/stocks/new?photo={error}"),
    }
}

/// `POST /stocks/new/photo` — the photo of a barcode, decoded in memory
/// (`photo::decode_photo`) and dropped with the request. Only the decoded
/// code (digits, or a GS1 2D code's text) leaves this handler, in the
/// redirect.
pub async fn photo(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if family_context(&state, &headers, &me, "/stocks/new")
        .await
        .is_none()
    {
        return Redirect::to("/groups/new").into_response();
    }
    // The photo is held in memory whole, like an attachment: same bound on
    // how many are held at once, per account and per process.
    let _permit = match state.upload_gate.try_acquire(me.user_id) {
        Ok(permit) => permit,
        Err(busy) => {
            tracing::info!(?busy, "photo turned away");
            return Redirect::to(&photo_redirect(Err("busy"))).into_response();
        }
    };

    let mut photo: Option<Vec<u8>> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(mut field)) => {
                if field.name() != Some("photo") || photo.is_some() {
                    continue;
                }
                let mut bytes = Vec::new();
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            if bytes.len() + chunk.len() > MAX_PHOTO_BYTES {
                                return Redirect::to(&photo_redirect(Err(
                                    PhotoError::TooLarge.code()
                                )))
                                .into_response();
                            }
                            bytes.extend_from_slice(&chunk);
                        }
                        Ok(None) => break,
                        Err(_) => {
                            return Redirect::to(&photo_redirect(Err("failed"))).into_response()
                        }
                    }
                }
                photo = Some(bytes);
            }
            Ok(None) => break,
            Err(_) => return Redirect::to(&photo_redirect(Err("failed"))).into_response(),
        }
    }
    let Some(bytes) = photo else {
        return Redirect::to(&photo_redirect(Err("failed"))).into_response();
    };

    // At most `DECODE_PERMITS` decodes at once in the process: the upload
    // permit bounds the bodies held, not the working memory of decoding
    // them, many times larger. Waiting here holds the upload permit, so the
    // queue is bounded by the upload gate. The permit moves into the
    // blocking task and is released when the decode ends, even if the
    // client has gone.
    let Ok(decode_permit) = DECODES.acquire().await else {
        return service_unavailable_page().into_response();
    };
    // CPU-bound: off the async workers.
    let outcome = match tokio::task::spawn_blocking(move || {
        let _permit = decode_permit;
        decode_photo(&bytes)
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => return service_unavailable_page().into_response(),
    };
    let target = match &outcome {
        Ok(code) => photo_redirect(Ok(code.clone())),
        Err(error) => photo_redirect(Err(error.code())),
    };
    Redirect::to(&target).into_response()
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
            None,
        );
        assert_eq!(form.name, "Nutella (400 g)");
        assert_eq!(form.quantity, "1");
        assert_eq!(form.unit, "unité");
        assert_eq!(form.barcode, "3017620422003");
        assert_eq!(form.category, "");
        assert_eq!(form.expires_on, "");
        assert_eq!(form.expires_on_origin, DateOrigin::Member);
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_date_read_on_the_code_prefills_the_field_and_says_so() {
        let form = prefilled_form(
            "3017620422003",
            Some(&product("Nutella", None)),
            true,
            Some((day(2027, 1, 31), DateOrigin::Code)),
        );
        assert_eq!(form.expires_on, "2027-01-31");
        assert_eq!(form.expires_on_origin, DateOrigin::Code);
        // "Créer quand même un autre article" keeps it too: same product.
        let other = prefilled_form(
            "3017620422003",
            None,
            false,
            Some((day(2027, 1, 31), DateOrigin::Code)),
        );
        assert_eq!(other.expires_on, "2027-01-31");
    }

    fn scan_result(
        expires_on: Option<NaiveDate>,
        source: Option<ExpiresOnSource>,
        category: Option<&str>,
    ) -> ScanResult {
        ScanResult {
            code: Some("3033490001063".into()),
            weighed: false,
            product: None,
            existing_item_id: None,
            expires_on,
            expires_on_source: source,
            expires_on_category: category.map(Into::into),
        }
    }

    #[test]
    fn the_scan_says_where_its_date_comes_from() {
        let on = day(2026, 10, 29);
        assert_eq!(
            proposed_date(&scan_result(Some(on), Some(ExpiresOnSource::Gs1), None)),
            Some((on, DateOrigin::Code))
        );
        assert_eq!(
            proposed_date(&scan_result(
                Some(on),
                Some(ExpiresOnSource::Category),
                Some("Yaourts")
            )),
            Some((on, DateOrigin::Category("Yaourts".into())))
        );
        assert_eq!(proposed_date(&scan_result(None, None, None)), None);
        // A date without a source would read as the member's own: none.
        assert_eq!(proposed_date(&scan_result(Some(on), None, None)), None);
    }

    #[test]
    fn a_category_date_prefills_the_field_and_names_the_category() {
        let form = prefilled_form(
            "3033490001063",
            Some(&product("Yaourt nature", None)),
            true,
            Some((day(2026, 10, 29), DateOrigin::Category("Yaourts".into()))),
        );
        assert_eq!(form.expires_on, "2026-10-29");
        let html = form_fields(
            "",
            "",
            "",
            "",
            "",
            &form.expires_on,
            &form.expires_on_origin,
        );
        let label = html.split("<label>Date de péremption").nth(1).unwrap();
        let label = label.split("</label>").next().unwrap();
        assert!(label.contains(r#"value="2026-10-29""#), "{label}");
        assert!(
            label.contains("Proposée d'après la catégorie « Yaourts »"),
            "{label}"
        );
        assert!(!label.contains("Lue sur le code"), "{label}");
        // A missing name still says where the date comes from.
        let unnamed = form_fields(
            "",
            "",
            "",
            "",
            "",
            "2026-10-29",
            &DateOrigin::Category(String::new()),
        );
        assert!(
            unnamed.contains("Proposée d'après la catégorie du produit"),
            "{unnamed}"
        );
        // The name is escaped like any text from Open Food Facts.
        let hostile = form_fields(
            "",
            "",
            "",
            "",
            "",
            "2026-10-29",
            &DateOrigin::Category("<b>".into()),
        );
        assert!(hostile.contains("« &lt;b&gt; »"), "{hostile}");
    }

    #[test]
    fn the_date_field_asks_for_its_shortcuts_without_carrying_them() {
        let html = form_fields("", "", "", "", "", "", &DateOrigin::Member);
        assert!(
            html.contains(
                r#"<input type="date" name="expires_on" value="" data-expiry-shortcuts/>"#
            ),
            "{html}"
        );
        // Put there by the script: no button in the markup itself, which
        // works without it.
        assert!(!html.contains("<button"), "{html}");
        assert!(!html.contains("+1 sem."), "{html}");
        let script = crate::app::ENHANCE_SCRIPT;
        assert!(script.contains("input[data-expiry-shortcuts]"));
        for label in ["\"+3 j\"", "\"+1 sem.\"", "\"+1 mois\""] {
            assert!(script.contains(label), "{label}");
        }
    }

    #[test]
    fn the_date_field_carries_the_mention_only_when_read_on_the_code() {
        let read = form_fields(
            "Nutella",
            "",
            "1",
            "unité",
            "",
            "2027-01-31",
            &DateOrigin::Code,
        );
        assert!(read.contains(r#"value="2027-01-31""#), "{read}");
        // The mention sits in the date's own label, next to the field.
        let label = read.split("<label>Date de péremption").nth(1).unwrap();
        let label = label.split("</label>").next().unwrap();
        assert!(label.contains("Lue sur le code"), "{label}");
        let typed = form_fields(
            "Nutella",
            "",
            "1",
            "unité",
            "",
            "2027-01-31",
            &DateOrigin::Member,
        );
        assert!(!typed.contains("Lue sur le code"), "{typed}");
    }

    #[test]
    fn a_sooner_date_or_a_first_date_is_proposed() {
        let read = Some(day(2027, 1, 31));
        assert_eq!(date_to_propose(read, Some(day(2027, 3, 1))), read);
        assert_eq!(date_to_propose(read, None), read);
        // Later, or the same: nothing to propose.
        assert_eq!(date_to_propose(read, Some(day(2027, 1, 31))), None);
        assert_eq!(date_to_propose(read, Some(day(2027, 1, 1))), None);
        // No date read: nothing either.
        assert_eq!(date_to_propose(None, Some(day(2027, 1, 1))), None);
        assert_eq!(date_to_propose(None, None), None);
    }

    fn item(expires_on: Option<NaiveDate>) -> StockItemResponse {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(5),
            "group_id": Uuid::nil(),
            "name": "Nutella",
            "category": null,
            "quantity": 2.0,
            "unit": "unité",
            "reorder_threshold": null,
            "low_stock": false,
            "created_by": Uuid::from_u128(7),
            "created_at": "2026-10-01T10:00:00Z",
            "updated_at": "2026-10-01T10:00:00Z",
            "expires_on": expires_on,
            "expiry_status": "unknown",
        }))
        .unwrap()
    }

    #[test]
    fn already_in_stock_proposes_the_sooner_date_without_writing_it() {
        let html = already_in_stock_page(
            "",
            &item(Some(day(2027, 3, 1))),
            "3017620422003",
            Some(day(2027, 1, 31)),
            true,
        );
        assert!(html.contains("31/01/2027"), "{html}");
        assert!(html.contains("01/03/2027"), "{html}");
        // A link to the edit form, pre-filled: nothing is saved from here.
        let link = format!(
            r#"href="/stocks/{}/edit?expires_on=2027-01-31""#,
            Uuid::from_u128(5)
        );
        assert!(html.contains(&link), "{html}");
        // The "+1" stays.
        assert!(html.contains(">+1</button>"), "{html}");
    }

    #[test]
    fn a_member_who_may_not_edit_sees_the_date_without_the_link() {
        let html = already_in_stock_page(
            "",
            &item(None),
            "3017620422003",
            Some(day(2027, 1, 31)),
            false,
        );
        assert!(html.contains("31/01/2027"), "{html}");
        assert!(!html.contains("/edit?expires_on="), "{html}");
    }

    #[test]
    fn another_article_anyway_scans_the_same_string_again() {
        // The GS1 string as scanned, so that its date reaches the form.
        let html = already_in_stock_page(
            "",
            &item(None),
            "010301762042200310LOT\u{1d}17270131",
            None,
            true,
        );
        assert!(
            html.contains(
                r#"href="/stocks/new?scan=010301762042200310LOT%1D17270131&amp;other=1""#
            ),
            "{html}"
        );
        let html = already_in_stock_page("", &item(None), "3017620422003", None, true);
        assert!(
            html.contains(r#"href="/stocks/new?scan=3017620422003&amp;other=1""#),
            "{html}"
        );
    }

    #[test]
    fn without_a_proposal_the_page_says_nothing_of_a_date() {
        let html = already_in_stock_page("", &item(None), "3017620422003", None, true);
        assert!(!html.contains("lue sur le code"), "{html}");
        assert!(!html.contains("/edit?expires_on="), "{html}");
    }

    #[test]
    fn an_unknown_product_prefills_the_code_only() {
        let form = prefilled_form("2123456012347", None, true, None);
        assert_eq!(form.name, "");
        assert_eq!(form.quantity, "1");
        assert_eq!(form.unit, "unité");
        assert_eq!(form.barcode, "2123456012347");
    }

    #[test]
    fn another_article_anyway_leaves_the_code_out() {
        let form = prefilled_form(
            "3017620422003",
            Some(&product("Nutella", None)),
            false,
            None,
        );
        assert_eq!(form.name, "Nutella");
        assert_eq!(form.barcode, "");
    }

    #[test]
    fn the_scan_block_is_two_plain_forms() {
        let html = scan_block("3017620422003");
        // The photo: a multipart POST of one image, the rear camera asked.
        assert!(
            html.contains(
                r#"<form method="post" action="/stocks/new/photo" enctype="multipart/form-data""#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<input type="file" name="photo" accept="image/*" capture="environment""#
            ),
            "{html}"
        );
        // The digits: a GET form whose field is named `scan`.
        assert!(
            html.contains(r#"<form method="get" action="/stocks/new""#),
            "{html}"
        );
        assert!(html.contains(r#"name="scan""#), "{html}");
        assert!(html.contains(r#"value="3017620422003""#), "{html}");
        // No script of its own.
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn a_failed_photo_offers_both_ways_to_scan_again() {
        let html = photo_retry_page("", "unreadable");
        assert!(html.contains(photo_error_message("unreadable")), "{html}");
        assert!(html.contains(r#"action="/stocks/new/photo""#), "{html}");
        assert!(html.contains(r#"name="scan""#), "{html}");
        // The article form is not where a failed photo lands.
        assert!(!html.contains(r#"name="name""#), "{html}");
    }

    #[test]
    fn a_decoded_photo_goes_through_the_scan_parameter() {
        assert_eq!(
            photo_redirect(Ok("3017620422003".into())),
            "/stocks/new?scan=3017620422003"
        );
        assert_eq!(
            photo_redirect(Ok("a b&c".into())),
            "/stocks/new?scan=a+b%26c"
        );
        assert_eq!(
            photo_redirect(Err("unreadable")),
            "/stocks/new?photo=unreadable"
        );
        // A GS1 code's separator (ASCII 29) survives the redirect (#403).
        assert_eq!(
            photo_redirect(Ok("010301762042200310LOT\u{1d}17270131".into())),
            "/stocks/new?scan=010301762042200310LOT%1D17270131"
        );
    }

    #[test]
    fn every_photo_error_has_its_own_message() {
        let generic = photo_error_message("anything-else");
        for code in [
            PhotoError::TooLarge.code(),
            PhotoError::NotAnImage.code(),
            PhotoError::NoBarcode.code(),
            "busy",
        ] {
            assert_ne!(photo_error_message(code), generic, "{code}");
        }
    }

    #[test]
    fn a_typed_value_is_escaped_back_into_the_field() {
        let html = scan_block(r#""><script>x</script>"#);
        assert!(!html.contains("<script>x"), "{html}");
        assert!(html.contains("&quot;&gt;&lt;script&gt;"), "{html}");
    }
}

/// `POST /stocks/new/photo` through the real router, against a stand-in
/// for apps/api that knows the session and one group.
#[cfg(test)]
mod photo_route_tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{header, HeaderMap, Method, Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use manage_our_home_http_guard::{BodyReadLimits, UploadGate};
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::state::AppState;

    const BOUNDARY: &str = "----manageourhomephotoboundary";

    async fn fake_api() -> String {
        let app = Router::new()
            .route(
                "/auth/me",
                get(|headers: HeaderMap| async move {
                    if headers.get(header::COOKIE).is_none() {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(serde_json::json!({
                        "user_id": Uuid::from_u128(7),
                        "email": "membre@example.test",
                        "display_name": "Membre",
                        "email_verified": true,
                    }))
                    .into_response()
                }),
            )
            .route(
                "/groups",
                get(|| async {
                    Json(serde_json::json!([{
                        "group_id": Uuid::nil(),
                        "name": "Foyer",
                        "role": "owner",
                    }]))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// A multipart body whose one `photo` field is `size` bytes: a PNG
    /// signature, then zeros — no image, so a body read whole ends on
    /// `not_an_image`, and one cut short on `failed`.
    fn photo_of_size(size: usize) -> Body {
        let mut body = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"photo.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .into_bytes();
        let mut file = b"\x89PNG\r\n\x1a\n".to_vec();
        file.resize(size, 0);
        body.extend_from_slice(&file);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        Body::from(body)
    }

    async fn post_photo(size: usize) -> (StatusCode, String) {
        let router = crate::build_router(AppState {
            http: reqwest::Client::new(),
            api_internal_base_url: fake_api().await,
            api_public_base_url: "/api".into(),
            body_read_limits: BodyReadLimits::PRODUCTION,
            upload_gate: UploadGate::new(8, 2),
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/stocks/new/photo")
            .header(header::COOKIE, "session=x")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(photo_of_size(size))
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(20), router.oneshot(request))
            .await
            .expect("an answer within 20 s")
            .unwrap();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        (response.status(), location)
    }

    /// The route reads past axum's 2 MiB default, which would cut a phone's
    /// photo into `failed` (the failure #243 fixed for the attachments).
    #[tokio::test]
    async fn a_photo_over_two_mib_is_read_whole() {
        let (status, location) = post_photo(5 * 1024 * 1024).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, "/stocks/new?photo=not_an_image");
    }

    #[tokio::test]
    async fn a_photo_over_the_cap_is_too_large() {
        let (status, location) = post_photo(super::MAX_PHOTO_BYTES + 1).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, "/stocks/new?photo=too_large");
    }
}
