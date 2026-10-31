//! `/budget/new` — add an expense, as the mock-up « Nouvelle dépense » draws
//! it (#533): amount, name, a category in pills (« Sans catégorie »
//! included), who paid it in pills (#638: the signed-in member ticked,
//! « Personne » offered), date, and the grocery items it paid for, if any
//! (#633). The
//! empty-name / negative-amount rules are pre-validated by the shared
//! `validate_entry_form` (inline error, no round trip); the backend's
//! matching 400s, and the 409 of an item another expense already links, are
//! mapped defensively. Whoever reaches the page may create (an owner or an
//! admin, #531), so 403 isn't reachable via the UI.
//! Success (201) → PRG `/budget?notice=entry_created`.
//!
//! The body is read raw, as the agenda's forms are: the checked grocery
//! items arrive as one `grocery_item_ids` key given several times, which
//! `axum::Form` cannot read (`uuids_from_raw_form`).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::NaiveDate;
use manage_our_home_shared::dto::budget::{
    BudgetCategory, BudgetCategoryList, BudgetEntryList, BudgetEntryResponse,
    CreateBudgetEntryRequest, LinkedGroceryItem,
};
use manage_our_home_shared::dto::grocery_list::GroceryItemList;
use manage_our_home_shared::dto::groups::GroupMember;
use manage_our_home_shared::validation::budget::validate_entry_form;
use uuid::Uuid;

use crate::app::{html_escape, member_avatar, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::agenda::new::is_form_urlencoded;
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

use super::{
    budget_cookie, category_class, family_context, forbidden_page, linkable_items,
    uuids_from_raw_form,
};

/// Hidden field the category pills carry when the family's categories were
/// loaded. Without it the pills were rendered blind, and an edit must leave
/// the entry's category alone rather than read the « Sans catégorie » it
/// could not offer otherwise as a choice (the agenda's assignees, #98).
pub(crate) const CATEGORIES_PRESENT_FIELD: &str = "categories_present";

/// Same for the grocery items: without it, an edit leaves the entry's links
/// as they are instead of unlinking every item it could not list.
pub(crate) const ITEMS_PRESENT_FIELD: &str = "grocery_items_present";

/// Same for the payer (#638): without it, an edit leaves the entry's payer
/// as it is, and a new expense goes to the signed-in member, the payer the
/// pills would have ticked.
pub(crate) const PAYERS_PRESENT_FIELD: &str = "payers_present";

/// The shared entry form fields, used by both create and edit. `amount` and
/// `spent_at` arrive as strings and are parsed in the handler; the grocery
/// items, a repeated key, are read from the raw body.
#[derive(serde::Deserialize, Default)]
pub struct EntryForm {
    pub name: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub spent_at: String,
    /// A category's id, or empty for « Sans catégorie ».
    #[serde(default)]
    pub category_id: String,
    #[serde(default)]
    pub categories_present: Option<String>,
    #[serde(default)]
    pub grocery_items_present: Option<String>,
    /// A member's id, or empty for « Personne » (#638).
    #[serde(default)]
    pub paid_by: String,
    #[serde(default)]
    pub payers_present: Option<String>,
}

impl EntryForm {
    /// The category picked: `Some(None)` is « Sans catégorie »; `None`, no
    /// opinion — the pills were not drawn.
    pub(crate) fn category(&self) -> Option<Option<Uuid>> {
        self.categories_present
            .as_ref()
            .map(|_| Uuid::parse_str(self.category_id.trim()).ok())
    }

    /// The payer picked (#638): `Some(None)` is « Personne »; `None`, no
    /// opinion — the pills were not drawn.
    pub(crate) fn paid_by(&self) -> Option<Option<Uuid>> {
        self.payers_present
            .as_ref()
            .map(|_| Uuid::parse_str(self.paid_by.trim()).ok())
    }
}

/// The payer of a new expense: the one picked, else — the pills were not
/// drawn — `me`, whom they would have ticked (user's decision of
/// 2026-10-30).
pub(crate) fn new_entry_payer(form: &EntryForm, me: Uuid) -> Option<Uuid> {
    form.paid_by().unwrap_or(Some(me))
}

/// What the pills of the form offer: the family's categories, the grocery
/// items the expense may link to and its members, each `None` when it could
/// not be loaded.
pub(crate) struct Choices {
    pub categories: Option<Vec<BudgetCategory>>,
    pub items: Option<Vec<LinkedGroceryItem>>,
    pub members: Option<Vec<GroupMember>>,
}

/// French copy for a budget-entry form error code.
pub(crate) fn error_message(code: &str) -> &'static str {
    match code {
        "name_required" => "Le nom est obligatoire.",
        "amount_must_be_non_negative" => "Le montant ne peut pas être négatif.",
        "amount_too_large" => "Le montant ne peut pas dépasser 10 milliards d'euros.",
        "grocery_item_already_priced" => {
            "Un des articles choisis est déjà compté dans une autre dépense."
        }
        "too_many_grocery_items" => "Une dépense ne peut regrouper plus de 100 articles.",
        "paid_by_not_a_member" => {
            "Le membre choisi comme payeur ne fait plus partie de la famille."
        }
        "reference_not_found" => {
            "La catégorie ou un des articles choisis n'existe plus. Vérifiez le formulaire."
        }
        "invalid_form" => "Le formulaire envoyé est illisible, merci de réessayer.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => "Une erreur est survenue, merci de réessayer.",
    }
}

/// Parses a required euro amount string to an `f64`. An empty or unparseable
/// value maps to `Err(())`, which the handler surfaces as
/// `amount_must_be_non_negative` (the same code the backend's `to_cents`
/// rejects a bad amount with). The `>= 0`/finite check and the upper bound
/// (`amount_too_large`) live in the shared `validate_entry_form`.
pub(crate) fn parse_amount(s: &str) -> Result<f64, ()> {
    s.trim().parse::<f64>().map_err(|_| ())
}

/// Parses an optional date string (`YYYY-MM-DD` from an `<input type=date>`):
/// empty → `None` (backend defaults to today), a valid date → `Some`, anything
/// unparseable → `None` (let the backend default rather than 400 on a value the
/// date picker shouldn't have produced).
pub(crate) fn parse_spent_at(s: &str) -> Option<NaiveDate> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        NaiveDate::parse_from_str(t, "%Y-%m-%d").ok()
    }
}

/// Loads what the form's pills offer. `entry` is the expense being edited,
/// whose own items stay offered (`linkable_items`).
pub(crate) async fn fetch_choices(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
    entry: Option<&BudgetEntryResponse>,
) -> Choices {
    let get = |path: String| async move {
        match api_request_auth(state, reqwest::Method::GET, &path, cookie, None).await {
            Ok(resp) if resp.status == reqwest::StatusCode::OK => Some(resp.body),
            _ => None,
        }
    };
    let categories = get(format!("/groups/{gid}/budget-categories"))
        .await
        .and_then(|b| serde_json::from_value::<BudgetCategoryList>(b).ok())
        .map(|l| l.categories);
    let items = get(format!("/groups/{gid}/grocery-items"))
        .await
        .and_then(|b| serde_json::from_value::<GroceryItemList>(b).ok());
    let entries = get(format!("/groups/{gid}/budget-entries"))
        .await
        .and_then(|b| serde_json::from_value::<BudgetEntryList>(b).ok());
    let items = match (items, entries) {
        (Some(items), Some(entries)) => Some(linkable_items(&items.items, &entries.entries, entry)),
        _ => None,
    };
    // A family always has at least one member: an empty roster is one
    // that could not be read, and draws no pills.
    let members = fetch_group_detail(state, cookie, gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members)
        .filter(|m| !m.is_empty());
    Choices {
        categories,
        items,
        members,
    }
}

/// « Catégorie » in pills: « Sans catégorie », then each category with its
/// swatch. `selected` is `None` for « Sans catégorie ».
fn category_picks(categories: Option<&[BudgetCategory]>, selected: Option<Uuid>) -> String {
    let Some(categories) = categories else {
        return r#"<fieldset class="card">
<legend>Catégorie</legend>
<p class="muted">Les catégories n'ont pas pu être chargées.</p>
</fieldset>"#
            .to_string();
    };
    let checked = |on: bool| if on { " checked" } else { "" };
    let mut picks = format!(
        r#"<label class="field inline pick"><input type="radio" name="category_id" value=""{}/>Sans catégorie</label>"#,
        checked(selected.is_none()),
    );
    for c in categories {
        picks.push_str(&format!(
            r#"<label class="field inline pick"><input type="radio" name="category_id" value="{id}"{on}/><span class="swatch {class}" aria-hidden="true"></span>{name}</label>"#,
            id = c.id,
            on = checked(selected == Some(c.id)),
            class = category_class(&c.color),
            name = html_escape(&c.name),
        ));
    }
    let hint = if categories.is_empty() {
        r#"<p class="muted">Aucune catégorie. <a href="/budget/settings">Créer une catégorie</a></p>"#
    } else {
        ""
    };
    format!(
        r#"<fieldset class="card picks">
<legend>Catégorie</legend>
<input type="hidden" name="{CATEGORIES_PRESENT_FIELD}" value="1"/>
{picks}{hint}
</fieldset>"#
    )
}

/// « Payé par » in pills (#638): « Personne », then each member with their
/// initial, as the agenda's « Assigné à » draws them. `selected` is `None`
/// for « Personne ».
fn payer_picks(members: Option<&[GroupMember]>, selected: Option<Uuid>) -> String {
    let Some(members) = members else {
        return r#"<fieldset class="card">
<legend>Payé par</legend>
<p class="muted">La liste des membres n'a pas pu être chargée.</p>
</fieldset>"#
            .to_string();
    };
    let checked = |on: bool| if on { " checked" } else { "" };
    let mut picks = format!(
        r#"<label class="field inline pick"><input type="radio" name="paid_by" value=""{}/>Personne</label>"#,
        checked(selected.is_none()),
    );
    for m in members {
        picks.push_str(&format!(
            r#"<label class="field inline pick"><input type="radio" name="paid_by" value="{id}"{on}/>{avatar}{name}</label>"#,
            id = m.user_id,
            on = checked(selected == Some(m.user_id)),
            avatar = member_avatar(m.user_id, &m.display_name, None),
            name = html_escape(&m.display_name),
        ));
    }
    format!(
        r#"<fieldset class="card picks">
<legend>Payé par</legend>
<input type="hidden" name="{PAYERS_PRESENT_FIELD}" value="1"/>
{picks}
</fieldset>"#
    )
}

/// « Articles de courses » in pills: the items the expense may link to,
/// ticked when it does.
fn item_picks(items: Option<&[LinkedGroceryItem]>, selected: &[Uuid]) -> String {
    let Some(items) = items else {
        return r#"<fieldset class="card">
<legend>Articles de courses (facultatif)</legend>
<p class="muted">Les articles n'ont pas pu être chargés.</p>
</fieldset>"#
            .to_string();
    };
    let picks: String = if items.is_empty() {
        r#"<p class="muted">Aucun article coché à rattacher.</p>"#.to_string()
    } else {
        items
            .iter()
            .map(|i| {
                format!(
                    r#"<label class="field inline pick"><input type="checkbox" name="grocery_item_ids" value="{id}"{on}/>{name}</label>"#,
                    id = i.id,
                    on = if selected.contains(&i.id) { " checked" } else { "" },
                    name = html_escape(&i.name),
                )
            })
            .collect()
    };
    format!(
        r#"<fieldset class="card picks">
<legend>Articles de courses (facultatif)</legend>
<input type="hidden" name="{ITEMS_PRESENT_FIELD}" value="1"/>
{picks}
</fieldset>"#
    )
}

/// Renders the entry form fields (shared markup, pre-filled). Used by create
/// (today's date, empty name/amount) and edit (existing values).
pub(crate) fn form_fields(
    form: &EntryForm,
    choices: &Choices,
    category: Option<Uuid>,
    payer: Option<Uuid>,
    items: &[Uuid],
) -> String {
    format!(
        r#"<label>Montant (€) <input type="number" name="amount" step="0.01" min="0" required value="{amount}"/></label>
<label>Nom <input type="text" name="name" required value="{name}" placeholder="ex. Courses Super U"/></label>
{categories}
{payers}
<label>Date <input type="date" name="spent_at" value="{spent_at}"/></label>
{items}"#,
        amount = html_escape(&form.amount),
        name = html_escape(&form.name),
        spent_at = html_escape(&form.spent_at),
        categories = category_picks(choices.categories.as_deref(), category),
        payers = payer_picks(choices.members.as_deref(), payer),
        items = item_picks(choices.items.as_deref(), items),
    )
}

fn page(
    header: &str,
    me: Uuid,
    form: &EntryForm,
    choices: &Choices,
    items: &[Uuid],
    error: Option<&str>,
) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let fields = form_fields(
        form,
        choices,
        form.category().flatten(),
        new_entry_payer(form, me),
        items,
    );
    let body = format!(
        r#"<h1>Nouvelle dépense</h1>
{error_html}
<form method="post" action="/budget/new">
{fields}
<button type="submit">Ajouter la dépense</button>
</form>
<div class="links"><a href="/budget">Retour au budget</a></div>"#,
    );
    shell_with_header(Width::Form, "Nouvelle dépense", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget/new").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);
    let choices = fetch_choices(&state, fam.gid, cookie.as_deref(), None).await;
    // Pre-fill the date with today for convenience (still optional server-side).
    let form = EntryForm {
        spent_at: chrono::Utc::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string(),
        ..Default::default()
    };
    Html(page(&fam.header, me.user_id, &form, &choices, &[], None)).into_response()
}

pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: axum::body::Bytes,
) -> Response {
    if !is_form_urlencoded(&headers) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let fam = match family_context(&state, &headers, &me, "/budget/new").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);
    let raw = String::from_utf8_lossy(&raw_body);
    let items = uuids_from_raw_form(&raw, "grocery_item_ids");
    let Ok(form) = serde_urlencoded::from_bytes::<EntryForm>(&raw_body) else {
        let choices = fetch_choices(&state, fam.gid, cookie.as_deref(), None).await;
        let html = page(
            &fam.header,
            me.user_id,
            &EntryForm::default(),
            &choices,
            &[],
            Some(error_message("invalid_form")),
        );
        return (StatusCode::UNPROCESSABLE_ENTITY, Html(html)).into_response();
    };

    let gid = fam.gid;
    let me_id = me.user_id;
    let render_error = |code: &'static str| {
        let (state, form, items, header) = (&state, &form, &items, &fam.header);
        let cookie = cookie.as_deref();
        async move {
            let choices = fetch_choices(state, gid, cookie, None).await;
            Html(page(
                header,
                me_id,
                form,
                &choices,
                items,
                Some(error_message(code)),
            ))
            .into_response()
        }
    };

    let Ok(amount) = parse_amount(&form.amount) else {
        return render_error("amount_must_be_non_negative").await;
    };
    if let Err(e) = validate_entry_form(&form.name, amount) {
        return render_error(e.code()).await;
    }

    let req = CreateBudgetEntryRequest {
        name: form.name.trim().to_string(),
        amount,
        spent_at: parse_spent_at(&form.spent_at),
        grocery_item_id: None,
        grocery_item_ids: items.clone(),
        category_id: form.category().flatten(),
        paid_by: new_entry_payer(&form, me.user_id),
    };

    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/budget-entries", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(&req).unwrap()),
    )
    .await;

    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => {
            Redirect::to("/budget?notice=entry_created").into_response()
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            // The shared pre-validation should have caught these; map the
            // backend's exact code defensively if one slips through.
            render_error(known_code(&resp.body)).await
        }
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            render_error("grocery_item_already_priced").await
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            render_error("reference_not_found").await
        }
        Ok(resp) if resp.status == reqwest::StatusCode::FORBIDDEN => {
            forbidden_page().into_response()
        }
        Ok(_) | Err(_) => render_error("unavailable").await,
    }
}

/// A 400's error code, among those `error_message` words; `unavailable`
/// for any other.
pub(crate) fn known_code(body: &serde_json::Value) -> &'static str {
    match body.get("error").and_then(|e| e.as_str()) {
        Some("name_required") => "name_required",
        Some("amount_must_be_non_negative") => "amount_must_be_non_negative",
        Some("amount_too_large") => "amount_too_large",
        Some("too_many_grocery_items") => "too_many_grocery_items",
        Some("paid_by_not_a_member") => "paid_by_not_a_member",
        _ => "unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn category(n: u128, name: &str, color: &str) -> BudgetCategory {
        BudgetCategory {
            id: Uuid::from_u128(n),
            group_id: Uuid::from_u128(999),
            name: name.to_string(),
            color: color.to_string(),
            monthly_cap: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn linked(n: u128, name: &str) -> LinkedGroceryItem {
        LinkedGroceryItem {
            id: Uuid::from_u128(n),
            name: name.to_string(),
        }
    }

    fn form(raw: &str) -> EntryForm {
        serde_urlencoded::from_str(raw).expect(raw)
    }

    fn member(n: u128, name: &str) -> GroupMember {
        GroupMember {
            user_id: Uuid::from_u128(n),
            role: "admin".to_string(),
            display_name: name.to_string(),
            email: format!("{n}@example.test"),
            has_avatar: false,
        }
    }

    #[test]
    fn the_payer_pills_start_with_nobody_and_tick_the_one_chosen() {
        let members = [member(1, "Alice"), member(2, "Bruno")];
        let html = payer_picks(Some(&members), Some(Uuid::from_u128(2)));
        assert!(html.contains("<legend>Payé par</legend>"), "{html}");
        assert!(
            html.contains(r#"name="paid_by" value=""/>Personne"#),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                r#"name="paid_by" value="{}" checked/>"#,
                Uuid::from_u128(2)
            )),
            "{html}"
        );
        assert!(html.contains(r#"class="avatar""#), "{html}");
        assert!(html.contains("Bruno</label>"), "{html}");
        assert!(html.contains(r#"name="payers_present""#), "{html}");
        assert_eq!(html.matches(" checked").count(), 1, "{html}");
        assert_eq!(html.matches(r#"type="radio""#).count(), 3, "{html}");
    }

    #[test]
    fn nobody_chosen_ticks_personne() {
        let html = payer_picks(Some(&[member(1, "Alice")]), None);
        assert!(
            html.contains(r#"name="paid_by" value="" checked/>Personne"#),
            "{html}"
        );
        assert_eq!(html.matches(" checked").count(), 1, "{html}");
    }

    #[test]
    fn a_member_name_is_escaped_in_the_payer_pills() {
        let html = payer_picks(Some(&[member(1, "<b>Alice</b>")]), None);
        assert!(html.contains("&lt;b&gt;Alice&lt;/b&gt;</label>"), "{html}");
    }

    #[test]
    fn members_that_did_not_load_carry_no_payer_marker() {
        let html = payer_picks(None, Some(Uuid::from_u128(1)));
        assert!(!html.contains("payers_present"), "{html}");
        assert!(!html.contains("radio"), "{html}");
        assert!(html.contains("Payé par"), "{html}");
    }

    #[test]
    fn the_chosen_payer_is_read_only_when_the_pills_were_drawn() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            form(&format!("name=x&payers_present=1&paid_by={id}")).paid_by(),
            Some(Some(id))
        );
        assert_eq!(
            form("name=x&payers_present=1&paid_by=").paid_by(),
            Some(None)
        );
        assert_eq!(
            form("name=x&payers_present=1&paid_by=pas-un-uuid").paid_by(),
            Some(None)
        );
        assert_eq!(form(&format!("name=x&paid_by={id}")).paid_by(), None);
    }

    #[test]
    fn a_new_expense_is_paid_by_the_signed_in_member_unless_another_is_picked() {
        let me = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        // The pills not drawn: the member they would have ticked.
        assert_eq!(new_entry_payer(&form("name=x"), me), Some(me));
        assert_eq!(
            new_entry_payer(
                &form(&format!("name=x&payers_present=1&paid_by={other}")),
                me
            ),
            Some(other)
        );
        // « Personne » stays possible.
        assert_eq!(
            new_entry_payer(&form("name=x&payers_present=1&paid_by="), me),
            None
        );
    }

    #[test]
    fn a_payer_who_has_left_is_named_in_french() {
        assert_eq!(
            known_code(&serde_json::json!({ "error": "paid_by_not_a_member" })),
            "paid_by_not_a_member"
        );
        assert!(error_message("paid_by_not_a_member").contains("payeur"));
    }

    #[test]
    fn the_category_pills_start_with_none_and_tick_the_one_chosen() {
        let cats = [
            category(1, "Alimentation", "blue"),
            category(2, "Maison", "orange"),
        ];
        let html = category_picks(Some(&cats), Some(Uuid::from_u128(2)));
        assert!(html.contains(r#"value=""/>Sans catégorie"#), "{html}");
        assert!(
            html.contains(&format!(
                r#"value="{}" checked/><span class="swatch cat-orange" aria-hidden="true"></span>Maison"#,
                Uuid::from_u128(2)
            )),
            "{html}"
        );
        assert!(html.contains(r#"name="categories_present""#), "{html}");
        assert_eq!(html.matches(" checked").count(), 1, "{html}");
    }

    #[test]
    fn categories_that_did_not_load_carry_no_marker() {
        let html = category_picks(None, None);
        assert!(!html.contains("categories_present"), "{html}");
        assert!(!html.contains("radio"), "{html}");
    }

    #[test]
    fn the_item_pills_tick_the_linked_ones() {
        let items = [linked(1, "Pain"), linked(2, "Lait")];
        let html = item_picks(Some(&items), &[Uuid::from_u128(2)]);
        assert!(html.contains(r#"name="grocery_items_present""#), "{html}");
        assert_eq!(
            html.matches(r#"name="grocery_item_ids""#).count(),
            2,
            "{html}"
        );
        assert_eq!(html.matches(" checked").count(), 1, "{html}");
        assert!(html.contains(" checked/>Lait"), "{html}");
    }

    #[test]
    fn no_item_to_link_says_so_and_still_carries_the_marker() {
        let html = item_picks(Some(&[]), &[]);
        assert!(html.contains("Aucun article coché"), "{html}");
        assert!(html.contains("grocery_items_present"), "{html}");
    }

    #[test]
    fn items_that_did_not_load_carry_no_marker() {
        let html = item_picks(None, &[]);
        assert!(!html.contains("grocery_items_present"), "{html}");
    }

    #[test]
    fn the_chosen_category_is_read_only_when_the_pills_were_drawn() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            form(&format!("name=x&categories_present=1&category_id={id}")).category(),
            Some(Some(id))
        );
        assert_eq!(
            form("name=x&categories_present=1&category_id=").category(),
            Some(None)
        );
        assert_eq!(form(&format!("name=x&category_id={id}")).category(), None);
    }

    #[test]
    fn a_repeated_item_key_does_not_stop_the_form_from_reading() {
        let raw = format!(
            "name=Courses&amount=3&grocery_item_ids={}&grocery_item_ids={}",
            Uuid::from_u128(1),
            Uuid::from_u128(2)
        );
        assert_eq!(form(&raw).name, "Courses");
    }

    #[test]
    fn a_conflict_or_an_unknown_code_is_named_in_french() {
        assert_eq!(
            known_code(&serde_json::json!({ "error": "too_many_grocery_items" })),
            "too_many_grocery_items"
        );
        assert_eq!(
            known_code(&serde_json::json!({ "error": "x" })),
            "unavailable"
        );
        assert!(error_message("grocery_item_already_priced").contains("autre dépense"));
    }

    #[test]
    fn an_amount_above_the_bound_is_worded_from_the_api_and_the_form() {
        assert_eq!(
            known_code(&serde_json::json!({ "error": "amount_too_large" })),
            "amount_too_large"
        );
        assert!(error_message("amount_too_large").contains("10 milliards"));
    }
}
