//! `/budget/settings` — « Réglages du budget » (#533): the family's monthly
//! cap, the categories (name, colour, optional cap, deletion), the two
//! switches of the cap alerts (#536), and who sees the budget, with a link
//! to the members' roles. Same bar as every budget page (#531): an owner or
//! an admin; a standard member gets the 404 of an unknown path.
//!
//! Each card is its own plain `<form method=post>`, and every mutation ends
//! in a PRG to `/budget/settings?notice=…` or `?error=…`. A category's name,
//! colour and cap are pre-checked by the API's own guards
//! (`validate_category_name`, `is_category_color`, `parse_cap_input` over
//! `cap_to_cents`); the backend's 400s and 409s are mapped all the same.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::budget::{
    BudgetAlertSettings, BudgetCategory, BudgetCategoryList, BudgetSettings,
    CreateBudgetCategoryRequest, UpdateBudgetAlertSettings, UpdateBudgetCategoryRequest,
};
use manage_our_home_shared::validation::budget::{
    amount_input_value, category_color_name, format_euros, is_category_color, parse_cap_input,
    validate_category_name, CategoryError, CATEGORY_COLORS, CATEGORY_NAME_MAX,
};
use uuid::Uuid;

use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

use super::{budget_cookie, category_class, family_context, service_unavailable_page};

#[derive(serde::Deserialize)]
pub struct SettingsQuery {
    notice: Option<String>,
    error: Option<String>,
}

fn notice_text(notice: &str) -> Option<&'static str> {
    Some(match notice {
        "cap_saved" => "Plafond mensuel enregistré.",
        "alerts_saved" => "Alertes enregistrées.",
        "category_created" => "Catégorie ajoutée.",
        "category_updated" => "Catégorie mise à jour.",
        "category_deleted" => "Catégorie supprimée ; ses dépenses passent dans « Sans catégorie ».",
        _ => return None,
    })
}

fn error_text(error: &str) -> Option<&'static str> {
    Some(match error {
        "name_required" => "Le nom de la catégorie est obligatoire.",
        "name_too_long" => "Le nom de la catégorie ne doit pas dépasser 40 caractères.",
        "invalid_color" => "Choisissez une des couleurs proposées.",
        "cap_must_be_positive" => "Un plafond est un montant d'au moins 0,01 € ; laissez le champ vide pour n'en fixer aucun.",
        "category_name_taken" => "Une catégorie porte déjà ce nom.",
        "category_limit_reached" => "Une famille ne peut avoir plus de 30 catégories.",
        "category_not_found" => "Cette catégorie n'existe plus.",
        "unavailable" => "Service momentanément indisponible, merci de réessayer.",
        _ => return None,
    })
}

/// The colour of a category as a `<select>`: each key under its French
/// name, `selected` ticked.
fn color_select(selected: &str) -> String {
    let options: String = CATEGORY_COLORS
        .iter()
        .map(|c| {
            format!(
                r#"<option value="{c}"{on}>{name}</option>"#,
                on = if *c == selected { " selected" } else { "" },
                name = category_color_name(c),
            )
        })
        .collect();
    format!(r#"<label>Couleur <select name="color">{options}</select></label>"#)
}

/// A cap as an `<input type=number>` pre-fill: empty without one.
fn cap_value(cap: Option<f64>) -> String {
    cap.map(amount_input_value).unwrap_or_default()
}

/// One category: its swatch, name and cap as a disclosure, which opens on
/// its edit form and its deletion.
fn category_row(c: &BudgetCategory) -> String {
    let cap = c
        .monthly_cap
        .map(format_euros)
        .map(|cap| format!("plafond {cap}"))
        .unwrap_or_else(|| "sans plafond".to_string());
    format!(
        r#"<li class="list-row stacked">
<details>
<summary><span class="swatch {class}" aria-hidden="true"></span> {name} — {cap}</summary>
<form method="post" action="/budget/settings/categories/{id}">
<label>Nom <input type="text" name="name" required maxlength="{max}" value="{name}"/></label>
{color}
<label>Plafond mensuel (€, facultatif) <input type="number" name="monthly_cap" step="0.01" min="0.01" value="{cap_value}"/></label>
<button type="submit">Enregistrer</button>
</form>
<form method="post" action="/budget/settings/categories/{id}/delete" class="actions">
<button type="submit" class="secondary danger">Supprimer la catégorie</button>
</form>
<p class="muted">Ses dépenses passeront dans « Sans catégorie ».</p>
</details>
</li>"#,
        class = category_class(&c.color),
        name = html_escape(&c.name),
        cap = html_escape(&cap),
        id = c.id,
        max = CATEGORY_NAME_MAX,
        color = color_select(&c.color),
        cap_value = cap_value(c.monthly_cap),
    )
}

fn categories_card(categories: Option<&[BudgetCategory]>) -> String {
    let list = match categories {
        None => r#"<p class="muted">Les catégories n'ont pas pu être chargées.</p>"#.to_string(),
        Some([]) => r#"<p class="muted">Aucune catégorie pour le moment.</p>"#.to_string(),
        Some(categories) => format!(
            r#"<ul class="list">{}</ul>"#,
            categories.iter().map(category_row).collect::<String>()
        ),
    };
    format!(
        r#"<section class="card">
<h2>Catégories</h2>
{list}
<h3>Nouvelle catégorie</h3>
<form method="post" action="/budget/settings/categories">
<label>Nom de la catégorie <input type="text" name="name" required maxlength="{max}"/></label>
{color}
<label>Plafond de la catégorie (€, facultatif) <input type="number" name="monthly_cap" step="0.01" min="0.01"/></label>
<button type="submit">Ajouter la catégorie</button>
</form>
</section>"#,
        max = CATEGORY_NAME_MAX,
        color = color_select(CATEGORY_COLORS[0]),
    )
}

fn cap_card(settings: Option<&BudgetSettings>) -> String {
    let Some(settings) = settings else {
        return r#"<section class="card">
<h2>Plafond mensuel</h2>
<p class="muted">Le plafond n'a pas pu être chargé.</p>
</section>"#
            .to_string();
    };
    format!(
        r#"<section class="card">
<h2>Plafond mensuel</h2>
<form method="post" action="/budget/settings/cap">
<label>Plafond de la famille (€) <input type="number" name="monthly_cap" step="0.01" min="0.01" value="{cap}"/></label>
<p class="muted">Laissez vide pour ne fixer aucun plafond.</p>
<button type="submit">Enregistrer le plafond</button>
</form>
</section>"#,
        cap = cap_value(settings.monthly_cap),
    )
}

fn alerts_card(alerts: Option<&BudgetAlertSettings>) -> String {
    let Some(alerts) = alerts else {
        return r#"<section class="card">
<h2>Alertes</h2>
<p class="muted">Les alertes n'ont pas pu être chargées.</p>
</section>"#
            .to_string();
    };
    let on = |b: bool| if b { " checked" } else { "" };
    format!(
        r#"<section class="card">
<h2>Alertes</h2>
<form method="post" action="/budget/settings/alerts">
<label class="field inline"><input type="checkbox" name="global_cap_alerts" value="1"{global}/>À 80 % puis à 100 % du plafond de la famille</label>
<label class="field inline"><input type="checkbox" name="category_cap_alerts" value="1"{category}/>Au dépassement du plafond d'une catégorie</label>
<button type="submit" class="secondary">Enregistrer les alertes</button>
</form>
</section>"#,
        global = on(alerts.global_cap_alerts),
        category = on(alerts.category_cap_alerts),
    )
}

fn who_card(gid: Uuid) -> String {
    format!(
        r#"<section class="card">
<h2>Qui voit le budget</h2>
<p>Seuls les propriétaires et les admins de la famille voient le budget, ses alertes et ses réglages. Les autres membres n'en voient rien, pas même le lien.</p>
<p><a href="/groups/{gid}/members">Gérer les rôles des membres</a></p>
</section>"#
    )
}

fn page(
    header: &str,
    gid: Uuid,
    settings: Option<&BudgetSettings>,
    alerts: Option<&BudgetAlertSettings>,
    categories: Option<&[BudgetCategory]>,
    notice: Option<&str>,
    error: Option<&str>,
) -> String {
    let notice = notice
        .and_then(notice_text)
        .map(|n| format!(r#"<p class="notice success">{}</p>"#, html_escape(n)))
        .unwrap_or_default();
    let error = error
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let body = format!(
        r#"<hgroup>
<p class="eyebrow">Budget</p>
<h1>Réglages du budget</h1>
</hgroup>
{notice}{error}
<div class="settings">
<div>
{cap}
{categories}
</div>
<div>
{alerts}
{who}
</div>
</div>
<div class="links"><a href="/budget">Retour au budget</a></div>"#,
        cap = cap_card(settings),
        categories = categories_card(categories),
        alerts = alerts_card(alerts),
        who = who_card(gid),
    );
    shell_with_header(Width::Full, "Réglages du budget", header, &body)
}

pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SettingsQuery>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget/settings").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);
    let get = |path: String| {
        let (state, cookie) = (&state, cookie.as_deref());
        async move {
            match api_request_auth(state, reqwest::Method::GET, &path, cookie, None).await {
                Ok(resp) if resp.status == reqwest::StatusCode::OK => Ok(Some(resp.body)),
                Ok(_) => Ok(None),
                Err(e) => Err(e),
            }
        }
    };
    let gid = fam.gid;
    let (Ok(settings), Ok(alerts), Ok(categories)) = (
        get(format!("/groups/{gid}/budget-settings")).await,
        get(format!("/groups/{gid}/budget-alert-settings")).await,
        get(format!("/groups/{gid}/budget-categories")).await,
    ) else {
        return service_unavailable_page().into_response();
    };
    let settings = settings.and_then(|b| serde_json::from_value::<BudgetSettings>(b).ok());
    let alerts = alerts.and_then(|b| serde_json::from_value::<BudgetAlertSettings>(b).ok());
    let categories = categories
        .and_then(|b| serde_json::from_value::<BudgetCategoryList>(b).ok())
        .map(|l| l.categories);

    Html(page(
        &fam.header,
        gid,
        settings.as_ref(),
        alerts.as_ref(),
        categories.as_deref(),
        query.notice.as_deref(),
        query.error.as_deref(),
    ))
    .into_response()
}

fn back(query: &str) -> Response {
    Redirect::to(&format!("/budget/settings?{query}")).into_response()
}

/// Where a mutation's answer sends the browser: its notice on success, the
/// API's own code on a 400 or a 409 (`error_text` words those it knows),
/// `category_not_found` on a 404, `unavailable` otherwise.
fn answer(
    result: Result<crate::state::ApiResponse, String>,
    ok: reqwest::StatusCode,
    notice: &str,
) -> Response {
    match result {
        Ok(resp) if resp.status == ok => back(&format!("notice={notice}")),
        Ok(resp)
            if resp.status == reqwest::StatusCode::BAD_REQUEST
                || resp.status == reqwest::StatusCode::CONFLICT =>
        {
            let code = resp
                .body
                .get("error")
                .and_then(|e| e.as_str())
                .filter(|code| error_text(code).is_some())
                .unwrap_or("unavailable");
            back(&format!("error={code}"))
        }
        Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
            back("error=category_not_found")
        }
        Ok(_) | Err(_) => back("error=unavailable"),
    }
}

#[derive(serde::Deserialize)]
pub struct CapForm {
    #[serde(default)]
    monthly_cap: String,
}

pub async fn cap(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CapForm>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget/settings").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let monthly_cap = match parse_cap_input(&form.monthly_cap) {
        Ok(cap) => cap,
        Err(e) => return back(&format!("error={}", e.code())),
    };
    let cookie = budget_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PUT,
        &format!("/groups/{}/budget-settings", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(BudgetSettings { monthly_cap }).unwrap()),
    )
    .await;
    answer(result, reqwest::StatusCode::OK, "cap_saved")
}

/// An unticked checkbox sends nothing: each switch is its key's presence.
#[derive(serde::Deserialize)]
pub struct AlertsForm {
    global_cap_alerts: Option<String>,
    category_cap_alerts: Option<String>,
}

pub async fn alerts(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AlertsForm>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget/settings").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let body = UpdateBudgetAlertSettings {
        global_cap_alerts: Some(form.global_cap_alerts.is_some()),
        category_cap_alerts: Some(form.category_cap_alerts.is_some()),
    };
    let cookie = budget_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/budget-alert-settings", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(body).unwrap()),
    )
    .await;
    answer(result, reqwest::StatusCode::OK, "alerts_saved")
}

#[derive(serde::Deserialize)]
pub struct CategoryForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    monthly_cap: String,
}

/// A category form checked as the API checks it, in its order: name,
/// colour, cap. The name comes back trimmed.
fn checked_category(form: &CategoryForm) -> Result<(String, Option<f64>), CategoryError> {
    let name = validate_category_name(&form.name)?.to_string();
    if !is_category_color(&form.color) {
        return Err(CategoryError::InvalidColor);
    }
    let cap = parse_cap_input(&form.monthly_cap)?;
    Ok((name, cap))
}

pub async fn create_category(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CategoryForm>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget/settings").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let (name, monthly_cap) = match checked_category(&form) {
        Ok(checked) => checked,
        Err(e) => return back(&format!("error={}", e.code())),
    };
    let body = CreateBudgetCategoryRequest {
        name,
        color: form.color.clone(),
        monthly_cap,
    };
    let cookie = budget_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        &format!("/groups/{}/budget-categories", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(body).unwrap()),
    )
    .await;
    answer(result, reqwest::StatusCode::CREATED, "category_created")
}

pub async fn update_category(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(category_id): Path<Uuid>,
    Form(form): Form<CategoryForm>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget/settings").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let (name, monthly_cap) = match checked_category(&form) {
        Ok(checked) => checked,
        Err(e) => return back(&format!("error={}", e.code())),
    };
    // Every field is sent: an emptied cap goes out as `null`, which removes it.
    let body = UpdateBudgetCategoryRequest {
        name: Some(name),
        color: Some(form.color.clone()),
        monthly_cap: Some(monthly_cap),
    };
    let cookie = budget_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::PATCH,
        &format!("/groups/{}/budget-categories/{category_id}", fam.gid),
        cookie.as_deref(),
        Some(serde_json::to_value(body).unwrap()),
    )
    .await;
    answer(result, reqwest::StatusCode::OK, "category_updated")
}

pub async fn delete_category(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(category_id): Path<Uuid>,
) -> Response {
    let fam = match family_context(&state, &headers, &me, "/budget/settings").await {
        Ok(fam) => fam,
        Err(answer) => return *answer,
    };
    let cookie = budget_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        &format!("/groups/{}/budget-categories/{category_id}", fam.gid),
        cookie.as_deref(),
        None,
    )
    .await;
    answer(result, reqwest::StatusCode::NO_CONTENT, "category_deleted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn category(cap: Option<f64>) -> BudgetCategory {
        BudgetCategory {
            id: Uuid::from_u128(1),
            group_id: Uuid::from_u128(9),
            name: "Maison".to_string(),
            color: "orange".to_string(),
            monthly_cap: cap,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn form(name: &str, color: &str, cap: &str) -> CategoryForm {
        CategoryForm {
            name: name.to_string(),
            color: color.to_string(),
            monthly_cap: cap.to_string(),
        }
    }

    #[test]
    fn a_category_form_is_checked_in_the_api_s_order() {
        assert_eq!(
            checked_category(&form("  Maison ", "orange", "")),
            Ok(("Maison".to_string(), None))
        );
        assert_eq!(
            checked_category(&form("Maison", "orange", "150,5")),
            Ok(("Maison".to_string(), Some(150.5)))
        );
        assert_eq!(
            checked_category(&form(" ", "teal", "0")),
            Err(CategoryError::NameRequired)
        );
        assert_eq!(
            checked_category(&form("Maison", "teal", "0")),
            Err(CategoryError::InvalidColor)
        );
        assert_eq!(
            checked_category(&form("Maison", "green", "0")),
            Err(CategoryError::CapNotPositive)
        );
    }

    #[test]
    fn every_category_error_and_conflict_is_worded() {
        for code in [
            "name_required",
            "name_too_long",
            "invalid_color",
            "cap_must_be_positive",
            "category_name_taken",
            "category_limit_reached",
            "category_not_found",
            "unavailable",
        ] {
            assert!(error_text(code).is_some(), "{code}");
        }
    }

    #[test]
    fn the_colour_select_names_each_colour_and_keeps_the_current_one() {
        let html = color_select("green");
        assert_eq!(html.matches("<option").count(), 5, "{html}");
        assert!(
            html.contains(r#"<option value="green" selected>Vert</option>"#),
            "{html}"
        );
        assert_eq!(html.matches(" selected").count(), 1, "{html}");
    }

    #[test]
    fn a_category_row_opens_on_its_form_prefilled() {
        let html = category_row(&category(Some(150.0)));
        assert!(html.contains("Maison — plafond 150,00 €"), "{html}");
        assert!(html.contains(r#"value="150.00""#), "{html}");
        assert!(
            html.contains(r#"<option value="orange" selected>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="swatch cat-orange" aria-hidden="true">"#),
            "{html}"
        );
        assert!(
            html.contains(
                "/budget/settings/categories/00000000-0000-0000-0000-000000000001/delete"
            ),
            "{html}"
        );
    }

    #[test]
    fn a_category_without_a_cap_says_so_and_leaves_the_field_empty() {
        let html = category_row(&category(None));
        assert!(html.contains("Maison — sans plafond"), "{html}");
        assert!(html.contains(r#"min="0.01" value=""/>"#), "{html}");
    }

    #[test]
    fn the_switches_are_ticked_as_the_family_set_them() {
        let html = alerts_card(Some(&BudgetAlertSettings {
            global_cap_alerts: true,
            category_cap_alerts: false,
        }));
        assert!(
            html.contains(r#"name="global_cap_alerts" value="1" checked/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="category_cap_alerts" value="1"/>"#),
            "{html}"
        );
    }

    #[test]
    fn who_sees_the_budget_links_to_the_members_roles() {
        let gid = Uuid::from_u128(42);
        let html = who_card(gid);
        assert!(
            html.contains(&format!(r#"href="/groups/{gid}/members""#)),
            "{html}"
        );
        assert!(html.contains("propriétaires et les admins"), "{html}");
    }
}
