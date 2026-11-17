//! `/account/allergies` — « Mes allergies et aversions » (#819): the
//! member's own allergies and dislikes (#817's `/me/allergies`,
//! `/me/dislikes`), who learns of each allergy in each of their families,
//! the consent, and « Tout effacer ».
//!
//! An allergy is health data. What this page shows comes from `/me/*` only:
//! the member's own, never another member's. Every form posts, and a tag
//! travels in a body, never in an address — a path or a query string ends
//! up in access logs, a notice code does not name the food.
//!
//! Consent, as `docs/registre-traitements.md` (line 4a) records it: given by
//! the member's own entry, explicit — a box to tick under « Avant de
//! commencer », apart from the terms of service, which « Continuer »
//! carries to the first allergy —, and withdrawn as simply:
//! removing an allergy, setting it back to « Privé » in a family, or
//! « Tout effacer » (`DELETE /me/dietary-constraints`). The service keeps
//! no record of it beyond the allergies themselves: with none left, the box
//! is asked again. Dislikes are not health data and need none.
//!
//! The choice of each level is said in one sentence per family before it is
//! saved: the three sentences are in the page, and the sheet shows the
//! chosen one (`.share`), all three where `:has()` is not supported.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use chrono::{DateTime, Datelike, Utc};
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::validation::budget::month_name_fr;
use manage_our_home_shared::validation::dietary::{
    inco_label, validate_tag, ShareLevel, INCO_ALLERGENS, SEARCH_QUERY_MAX_LEN,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{account_cookie, account_header, service_unavailable_page};
use crate::app::{html_escape, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::state::{api_request_auth, AppState};

pub const PATH: &str = "/account/allergies";

/// The level of one allergy in one family, as `GET /me/allergies` says it.
#[derive(Debug, Clone, Deserialize)]
pub struct Sharing {
    pub group_id: Uuid,
    pub group_name: String,
    pub share: String,
}

/// One of the member's allergies.
#[derive(Debug, Clone, Deserialize)]
pub struct Allergy {
    pub tag: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub sharing: Vec<Sharing>,
}

/// One of the member's dislikes.
#[derive(Debug, Clone, Deserialize)]
pub struct Dislike {
    pub tag: String,
    pub label: String,
}

#[derive(Deserialize)]
struct AllergyList {
    allergies: Vec<Allergy>,
}

#[derive(Deserialize)]
struct DislikeList {
    dislikes: Vec<Dislike>,
}

/// A category `POST /taxonomy/search` found.
#[derive(Debug, Clone, Deserialize)]
pub struct Found {
    pub tag: String,
    pub label: String,
}

#[derive(Deserialize)]
struct SearchResults {
    results: Vec<Found>,
}

/// What the member holds: their allergies and their dislikes.
#[derive(Debug, Clone, Default)]
pub struct Lists {
    pub allergies: Vec<Allergy>,
    pub dislikes: Vec<Dislike>,
}

/// Allergy or dislike: what « Ajouter » records, what « Retirer » removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Allergy,
    Dislike,
}

impl Kind {
    fn parse(value: &str) -> Option<Kind> {
        match value {
            "allergy" => Some(Kind::Allergy),
            "dislike" => Some(Kind::Dislike),
            _ => None,
        }
    }

    /// The `/me/*` route of its kind.
    fn api_path(self) -> &'static str {
        match self {
            Kind::Allergy => "/me/allergies",
            Kind::Dislike => "/me/dislikes",
        }
    }
}

/// What the name of a tag is on this page: an INCO allergen's own name
/// (« Gluten », not the cereals the taxonomy files it under), or the name
/// apps/api gave.
pub(crate) fn name_of<'a>(tag: &str, label: &'a str) -> &'a str {
    inco_label(tag).unwrap_or(label)
}

/// Whether an allergy may be recorded (#819): the first one needs the box
/// of consent ticked — the member has no allergy yet, so no consent in
/// force. A dislike needs none. `Err` is the error code.
pub(crate) fn consent_check(
    kind: Kind,
    has_allergies: bool,
    consent_ticked: bool,
) -> Result<(), &'static str> {
    if kind == Kind::Allergy && !has_allergies && !consent_ticked {
        Err("consent_required")
    } else {
        Ok(())
    }
}

/// The levels a « Enregistrer le partage » form sets: one per family,
/// `share.<family id>` = `public`, `anonymous` or `private`. A key or a
/// value that does not read refuses the whole form (`invalid_share`):
/// nothing is half-saved from a forged form.
pub(crate) fn sharing_choices(
    pairs: &[(String, String)],
) -> Result<Vec<(Uuid, ShareLevel)>, &'static str> {
    let mut choices: Vec<(Uuid, ShareLevel)> = Vec::new();
    for (key, value) in pairs {
        let Some(family) = key.strip_prefix("share.") else {
            continue;
        };
        let family = Uuid::parse_str(family).map_err(|_| "invalid_share")?;
        let share = ShareLevel::parse(value).map_err(|_| "invalid_share")?;
        if choices.iter().any(|(f, _)| *f == family) {
            return Err("invalid_share");
        }
        choices.push((family, share));
    }
    Ok(choices)
}

/// Where the member stands with the consent, which no record keeps beyond
/// the allergies (registre, line 4a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Consent {
    /// No allergy: « Avant de commencer » asks for it.
    Asked,
    /// No allergy yet, the box of « Avant de commencer » just ticked: the
    /// forms carry it to the first allergy, which records it.
    JustGiven,
    /// An allergy is recorded: given, on the day of the oldest.
    InForce(DateTime<Utc>),
}

pub(crate) fn consent_state(lists: &Lists, given_now: bool) -> Consent {
    match lists.allergies.iter().map(|a| a.created_at).min() {
        Some(oldest) => Consent::InForce(oldest),
        None if given_now => Consent::JustGiven,
        None => Consent::Asked,
    }
}

/// A day as French writes it out, the Paris one: « 12 novembre 2026 »,
/// « 1er décembre 2026 ».
pub(crate) fn long_date(at: DateTime<Utc>) -> String {
    let day = at.with_timezone(&chrono_tz::Europe::Paris).date_naive();
    let num = match day.day() {
        1 => "1er".to_string(),
        n => n.to_string(),
    };
    format!("{num} {} {}", month_name_fr(day.month()), day.year())
}

/// « 1 allergie », « 2 allergies ».
pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n > 1 { many } else { one })
}

fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "allergy_added" => Some(
            "Allergie ajoutée. Elle reste privée dans toutes vos familles tant que vous ne la partagez pas.",
        ),
        "dislike_added" => Some("Aversion ajoutée. Elle n'est jamais partagée."),
        "removed" => Some("Retiré de votre liste."),
        "sharing_saved" => Some("Partage enregistré."),
        "cleared" => Some(
            "Toutes vos allergies et aversions ont été effacées, et votre consentement retiré.",
        ),
        _ => None,
    }
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "consent_required" => Some(
            "Votre consentement est nécessaire pour enregistrer une allergie. Cochez la case, puis continuez.",
        ),
        "already_listed" => Some("Cet aliment est déjà dans votre liste."),
        "unknown_tag" => Some("Cet aliment n'est pas reconnu. Choisissez-le dans la liste."),
        "not_found" => Some("Cet élément n'est plus dans votre liste."),
        "invalid_share" => Some("Ce niveau de partage n'est pas proposé."),
        "sharing_failed" => Some(
            "Le partage n'a pas pu être enregistré en entier : vérifiez les niveaux affichés, puis réessayez.",
        ),
        "query_too_long" => Some("Ce nom est trop long."),
        "unavailable" => Some("Service momentanément indisponible, merci de réessayer."),
        _ => None,
    }
}

/// The search made from « Chercher un autre aliment », when one was.
pub struct Search {
    pub query: String,
    pub results: Vec<Found>,
}

/// One family's three levels for one allergy, and what each does.
fn family_levels(me: &str, name: &str, sharing: &Sharing) -> String {
    let family = html_escape(&sharing.group_name);
    let me = html_escape(me);
    let name = html_escape(name);
    let field = format!("share.{}", sharing.group_id);
    let choice = |value: &str, title: &str, hint: &str| {
        let checked = if sharing.share == value {
            " checked"
        } else {
            ""
        };
        format!(
            r#"<label class="list-row pick"><input type="radio" name="{field}" value="{value}"{checked}><span><strong>{title}</strong><br><span class="muted">{hint}</span></span></label>"#
        )
    };
    format!(
        r#"<fieldset class="plain share">
<legend>{family}</legend>
{public}
{anonymous}
{private}
<p class="public">Les membres de {family} verront votre nom et cette allergie, « {me} : {name} », et les recettes qui en contiennent leur seront signalées.</p>
<p class="anonymous">Les membres de {family} sauront seulement qu'un membre de la famille a une allergie, sans votre nom ni laquelle ; les recettes qui en contiennent leur seront signalées, sans dire pourquoi.</p>
<p class="private">Personne dans {family} ne le saura : les recettes proposées aux autres membres n'en tiendront pas compte. Les vôtres, si.</p>
</fieldset>"#,
        public = choice(
            "public",
            "Public",
            "Mon nom et l'allergie sont visibles de tous"
        ),
        anonymous = choice(
            "anonymous",
            "Anonyme",
            "« Un membre de la famille est allergique »"
        ),
        private = choice("private", "Privé", "Personne ne le sait"),
    )
}

/// A family's level as a pill under an allergy's name, the word saying it
/// and the tint repeating it: « public » in the warning's, « anonyme » in
/// the accent's, « privé » neutral.
fn level_badge(sharing: &Sharing) -> String {
    let (class, word) = match sharing.share.as_str() {
        "public" => ("badge soon", "public"),
        "anonymous" => ("badge accent", "anonyme"),
        _ => ("badge", "privé"),
    };
    format!(
        r#" <span class="{class}">{} : {word}</span>"#,
        html_escape(&sharing.group_name)
    )
}

/// One allergy: its name and its levels in pills, then, opened, the form
/// of its levels and « Retirer ».
fn allergy_row(me: &str, allergy: &Allergy) -> String {
    let name = name_of(&allergy.tag, &allergy.label);
    let levels: String = allergy.sharing.iter().map(level_badge).collect();
    let form = if allergy.sharing.is_empty() {
        r#"<p class="muted">Vous n'êtes membre d'aucune famille : personne ne peut la voir.</p>"#
            .to_string()
    } else {
        let families: String = allergy
            .sharing
            .iter()
            .map(|s| family_levels(me, name, s))
            .collect();
        format!(
            r#"<form method="post" action="{PATH}/sharing">
<input type="hidden" name="tag" value="{tag}">
{families}
<button type="submit">Enregistrer le partage</button>
</form>"#,
            tag = html_escape(&allergy.tag),
        )
    };
    format!(
        r#"<li class="list-row stacked">
<details>
<summary><strong>{name}</strong><br><span class="muted">Allergie</span>{levels}</summary>
{form}
{remove}
</details>
</li>"#,
        name = html_escape(name),
        remove = remove_form(Kind::Allergy, &allergy.tag, name),
    )
}

/// « Retirer « … » », in its own form.
fn remove_form(kind: Kind, tag: &str, name: &str) -> String {
    let kind = match kind {
        Kind::Allergy => "allergy",
        Kind::Dislike => "dislike",
    };
    format!(
        r#"<form method="post" action="{PATH}/remove">
<input type="hidden" name="kind" value="{kind}">
<input type="hidden" name="tag" value="{tag}">
<button type="submit" class="secondary danger">Retirer « {name} »</button>
</form>"#,
        tag = html_escape(tag),
        name = html_escape(name),
    )
}

/// One dislike: its name, never shared, and « Retirer ».
fn dislike_row(dislike: &Dislike) -> String {
    let name = name_of(&dislike.tag, &dislike.label);
    format!(
        r#"<li class="list-row stacked">
<span><strong>{name_html}</strong><br><span class="muted">Aversion · jamais partagée</span></span>
{remove}
</li>"#,
        name_html = html_escape(name),
        remove = remove_form(Kind::Dislike, &dislike.tag, name),
    )
}

/// « Mon consentement » once given — the date of the oldest allergy still
/// recorded, or none yet when just given — or, before the first allergy,
/// « Avant de commencer », its box and « Continuer »; with « Tout effacer »
/// as soon as there is anything to erase.
fn consent_card(lists: &Lists, consent: Consent) -> String {
    let clear = if lists.allergies.is_empty() && lists.dislikes.is_empty() {
        r#"<button type="button" class="secondary danger" disabled>Tout effacer</button>
<p class="muted">Rien à effacer pour l'instant.</p>"#
            .to_string()
    } else {
        format!(
            r#"<a class="btn secondary danger" href="{PATH}/clear">Tout effacer</a>
<p class="muted">Supprime toutes vos allergies et aversions, et retire votre consentement.</p>"#
        )
    };
    match consent {
        Consent::InForce(oldest) => format!(
            r#"<section class="card" id="consentement">
<div class="split"><h2>Mon consentement</h2><span class="badge accent">Donné</span></div>
<p>Vous avez consenti, à part des conditions d'utilisation, à l'enregistrement de vos allergies, une donnée de santé. La plus ancienne date du {date}.</p>
<p class="muted">Chaque allergie est gardée tant que vous la gardez. Vous retirez votre consentement aussi simplement : en retirant une allergie, en la repassant à « Privé » dans une famille, ou en effaçant tout.</p>
{clear}
</section>"#,
            date = long_date(oldest),
        ),
        Consent::JustGiven => format!(
            r#"<section class="card" id="consentement">
<div class="split"><h2>Mon consentement</h2><span class="badge accent">Donné</span></div>
<p>Vous avez consenti, à part des conditions d'utilisation, à l'enregistrement de vos allergies, une donnée de santé. Il est enregistré avec votre première allergie : sans elle, il vous sera redemandé.</p>
{clear}
</section>"#
        ),
        Consent::Asked => format!(
            r#"<section class="card" id="consentement">
<h2>Avant de commencer</h2>
<p>Une allergie est une <strong>donnée de santé</strong>. Pour l'enregistrer, nous avons besoin de votre accord, <strong>à part des conditions d'utilisation</strong>.</p>
<ul>
<li>Rien n'est visible des autres membres tant que vous ne le décidez pas : le choix par défaut est « Privé ».</li>
<li>Vous réglez le partage allergie par allergie, famille par famille, et le changez à tout moment.</li>
<li>Une allergie est gardée tant que vous la gardez. « Tout effacer » supprime vos données et retire ce consentement, à tout moment.</li>
<li>Une aversion n'est pas une donnée de santé, et n'est jamais partagée.</li>
</ul>
<form method="post" action="{PATH}/consent">
<label class="list-row pick"><input type="checkbox" name="consent" value="on"><span><strong>Je consens à l'enregistrement de mes allergies</strong><br><span class="muted">Donnée de santé, pour signaler et écarter les recettes qui en contiennent.</span></span></label>
<button type="submit">Continuer</button>
</form>
<p><a href="/privacy-policy">Politique de confidentialité</a></p>
{clear}
</section>"#
        ),
    }
}

/// « Ajouter »: the kind, the food — the fourteen INCO allergens first,
/// then what a search found —; then the search itself. Before the box of
/// « Avant de commencer » is ticked, a dislike only, which needs no
/// consent; just ticked, both forms carry it to the first allergy.
fn add_card(consent: Consent, search: Option<&Search>) -> String {
    let inco: String = INCO_ALLERGENS
        .iter()
        .map(|(tag, label)| format!(r#"<option value="{tag}">{label}</option>"#))
        .collect();
    let (found, said, query) = match search {
        Some(search) if !search.results.is_empty() => {
            let options: String = search
                .results
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    format!(
                        r#"<option value="{tag}"{selected}>{label}</option>"#,
                        tag = html_escape(&f.tag),
                        label = html_escape(&f.label),
                        selected = if i == 0 { " selected" } else { "" },
                    )
                })
                .collect();
            (
                format!(r#"<optgroup label="Autres aliments">{options}</optgroup>"#),
                format!(
                    r#"<p class="muted" role="status">{} : choisissez dans la liste « Aliment » ci-dessus.</p>"#,
                    count(search.results.len(), "aliment trouvé", "aliments trouvés")
                ),
                search.query.as_str(),
            )
        }
        Some(search) => (
            String::new(),
            format!(
                r#"<p class="muted" role="status">Aucun aliment trouvé pour « {} ».</p>"#,
                html_escape(&search.query)
            ),
            search.query.as_str(),
        ),
        None => (String::new(), String::new(), ""),
    };
    let carried = if consent == Consent::JustGiven {
        r#"<input type="hidden" name="consent" value="on">"#
    } else {
        ""
    };
    let (kind, hint) = if consent == Consent::Asked {
        (
            r#"<input type="hidden" name="kind" value="dislike">"#,
            "Une aversion écarte les recettes qu'on vous propose et n'est jamais partagée. Pour une allergie, donnez d'abord votre accord, ci-dessus.",
        )
    } else {
        (
            r#"<fieldset class="plain">
<legend>C'est</legend>
<label class="list-row pick"><input type="radio" name="kind" value="allergy" checked><span><strong>Une allergie</strong><br><span class="muted">Signalée sur les recettes ; partagée seulement si vous le décidez</span></span></label>
<label class="list-row pick"><input type="radio" name="kind" value="dislike"><span><strong>Une aversion</strong><br><span class="muted">Écarte les recettes qu'on vous propose ; jamais partagée</span></span></label>
</fieldset>"#,
            "Par défaut, une allergie est <strong>privée</strong> dans toutes vos familles : personne ne le sait.",
        )
    };
    format!(
        r#"<section class="card">
<h2>Ajouter</h2>
<form method="post" action="{PATH}/add">
{carried}{kind}
<label class="field">Aliment
<select name="tag"><optgroup label="Les 14 allergènes réglementaires (INCO)">{inco}</optgroup>{found}</select>
</label>
<button type="submit">Ajouter à ma liste</button>
<p class="muted">{hint}</p>
</form>
<form method="post" action="{PATH}/search" class="actions">
{carried}<label>Chercher un autre aliment <input type="text" name="query" maxlength="{SEARCH_QUERY_MAX_LEN}" value="{query}" placeholder="kiwi, coriandre…"></label>
<button type="submit" class="secondary">Chercher</button>
</form>
{said}
</section>"#,
        query = html_escape(query),
    )
}

/// The page: the consent, « Ajouter », then the list. `consent_given_now`:
/// the box of « Avant de commencer » was just ticked.
pub(crate) fn page_body(
    me: &str,
    lists: &Lists,
    consent_given_now: bool,
    search: Option<&Search>,
    notice: Option<&str>,
    error: Option<&str>,
) -> String {
    let notice = notice
        .and_then(notice_text)
        .map(|n| format!(r#"<p class="notice success" role="status">{n}</p>"#))
        .unwrap_or_default();
    let error = error
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error" role="alert">{e}</p>"#))
        .unwrap_or_default();
    let consent = consent_state(lists, consent_given_now);
    let total = lists.allergies.len() + lists.dislikes.len();
    let list = if total == 0 {
        r#"<p class="muted">Vous n'avez déclaré aucune allergie ni aversion. Personne ne voit rien à votre sujet.</p>"#
            .to_string()
    } else {
        let rows: String = lists
            .allergies
            .iter()
            .map(|a| allergy_row(me, a))
            .chain(lists.dislikes.iter().map(dislike_row))
            .collect();
        format!(r#"<ul class="list">{rows}</ul>"#)
    };
    format!(
        r#"<hgroup>
<p class="eyebrow">Compte</p>
<h1>Mes allergies et aversions</h1>
</hgroup>
{notice}{error}
{consent}
{add}
<section class="card">
<h2>Ma liste ({total})</h2>
{list}
</section>
<p><a href="/account">Retour à mon compte</a></p>"#,
        consent = consent_card(lists, consent),
        add = add_card(consent, search),
    )
}

/// `GET /me/allergies` and `GET /me/dislikes`, `None` on any failure.
pub(crate) async fn fetch_lists(state: &AppState, cookie: Option<&str>) -> Option<Lists> {
    let allergies = api_request_auth(state, reqwest::Method::GET, "/me/allergies", cookie, None)
        .await
        .ok()
        .filter(|r| r.status == reqwest::StatusCode::OK)?;
    let dislikes = api_request_auth(state, reqwest::Method::GET, "/me/dislikes", cookie, None)
        .await
        .ok()
        .filter(|r| r.status == reqwest::StatusCode::OK)?;
    let allergies: AllergyList = serde_json::from_value(allergies.body).ok()?;
    let dislikes: DislikeList = serde_json::from_value(dislikes.body).ok()?;
    Some(Lists {
        allergies: allergies.allergies,
        dislikes: dislikes.dislikes,
    })
}

async fn render(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    consent_given_now: bool,
    search: Option<&Search>,
    notice: Option<&str>,
    error: Option<&str>,
) -> Response {
    let cookie = account_cookie(headers);
    let Some(lists) = fetch_lists(state, cookie.as_deref()).await else {
        return service_unavailable_page().into_response();
    };
    let header = account_header(state, headers, me, PATH).await;
    let body = page_body(
        &me.display_name,
        &lists,
        consent_given_now,
        search,
        notice,
        error,
    );
    Html(shell_with_header(
        Width::Read,
        "Mes allergies et aversions",
        &header,
        &body,
    ))
    .into_response()
}

#[derive(Deserialize)]
pub struct PageQuery {
    notice: Option<String>,
    error: Option<String>,
    consent: Option<String>,
}

/// `GET /account/allergies`. `?consent=given`: the box of « Avant de
/// commencer » was just ticked (`POST /account/allergies/consent`).
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Response {
    render(
        &state,
        &headers,
        &me,
        query.consent.as_deref() == Some("given"),
        None,
        query.notice.as_deref(),
        query.error.as_deref(),
    )
    .await
}

fn back(param: &str, code: &str) -> Response {
    Redirect::to(&format!("{PATH}?{param}={code}")).into_response()
}

/// [`back`], the consent just given kept for the next form.
fn back_given(param: &str, code: &str, given: bool) -> Response {
    if given {
        Redirect::to(&format!("{PATH}?{param}={code}&consent=given")).into_response()
    } else {
        back(param, code)
    }
}

#[derive(Deserialize)]
pub struct ConsentForm {
    #[serde(default)]
    consent: Option<String>,
}

/// `POST /account/allergies/consent` — « Continuer » under « Avant de
/// commencer »: nothing is recorded, the page comes back with « Ajouter »
/// open to allergies and the consent carried to the first, which records
/// it. Without the box ticked, the error says so.
pub async fn consent(CurrentUser(_me): CurrentUser, Form(form): Form<ConsentForm>) -> Response {
    if form.consent.is_some() {
        Redirect::to(&format!("{PATH}?consent=given")).into_response()
    } else {
        back("error", "consent_required")
    }
}

#[derive(Deserialize)]
pub struct AddForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    consent: Option<String>,
}

/// `POST /account/allergies/add` — records an allergy (`POST /me/allergies`),
/// once consent is in force or given now, or a dislike.
pub async fn add(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AddForm>,
) -> Response {
    let given = form.consent.is_some();
    let Some(kind) = Kind::parse(&form.kind) else {
        return back_given("error", "unknown_tag", given);
    };
    let Ok(tag) = validate_tag(&form.tag) else {
        return back_given("error", "unknown_tag", given);
    };
    let cookie = account_cookie(&headers);
    if kind == Kind::Allergy {
        let Some(lists) = fetch_lists(&state, cookie.as_deref()).await else {
            return back_given("error", "unavailable", given);
        };
        if let Err(code) = consent_check(kind, !lists.allergies.is_empty(), given) {
            return back("error", code);
        }
    }
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        kind.api_path(),
        cookie.as_deref(),
        Some(serde_json::json!({ "tag": tag })),
    )
    .await;
    match result {
        // An allergy recorded holds the consent from now on; anything else
        // keeps it carried.
        Ok(resp) if resp.status == reqwest::StatusCode::CREATED => match kind {
            Kind::Allergy => back("notice", "allergy_added"),
            Kind::Dislike => back_given("notice", "dislike_added", given),
        },
        Ok(resp) if resp.status == reqwest::StatusCode::CONFLICT => {
            back_given("error", "already_listed", given)
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            back_given("error", "unknown_tag", given)
        }
        _ => back_given("error", "unavailable", given),
    }
}

#[derive(Deserialize)]
pub struct RemoveForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    tag: String,
}

/// `POST /account/allergies/remove` — `DELETE /me/allergies` (its levels
/// go with it) or `DELETE /me/dislikes`, the tag in the body.
pub async fn remove(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<RemoveForm>,
) -> Response {
    let Some(kind) = Kind::parse(&form.kind) else {
        return back("error", "not_found");
    };
    let cookie = account_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        kind.api_path(),
        cookie.as_deref(),
        Some(serde_json::json!({ "tag": form.tag })),
    )
    .await;
    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => back("notice", "removed"),
        Ok(resp)
            if resp.status == reqwest::StatusCode::NOT_FOUND
                || resp.status == reqwest::StatusCode::BAD_REQUEST =>
        {
            back("error", "not_found")
        }
        _ => back("error", "unavailable"),
    }
}

/// `POST /account/allergies/sharing` — one `PUT /me/allergies/sharing` per
/// family of the form, its level as shown: a family left as it was is
/// written as it was.
pub async fn sharing(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let tag = pairs
        .iter()
        .find(|(k, _)| k == "tag")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let choices = match sharing_choices(&pairs) {
        Ok(choices) => choices,
        Err(code) => return back("error", code),
    };
    let cookie = account_cookie(&headers);
    for (group_id, share) in choices {
        let result = api_request_auth(
            &state,
            reqwest::Method::PUT,
            "/me/allergies/sharing",
            cookie.as_deref(),
            Some(serde_json::json!({
                "tag": tag,
                "group_id": group_id,
                "share": share.as_str(),
            })),
        )
        .await;
        match result {
            Ok(resp) if resp.status == reqwest::StatusCode::OK => {}
            Ok(resp) if resp.status == reqwest::StatusCode::NOT_FOUND => {
                return back("error", "not_found")
            }
            _ => return back("error", "sharing_failed"),
        }
    }
    back("notice", "sharing_saved")
}

#[derive(Deserialize)]
pub struct SearchForm {
    #[serde(default)]
    query: String,
    #[serde(default)]
    consent: Option<String>,
}

/// `POST /account/allergies/search` — `POST /taxonomy/search`, then the
/// page again with what it found in the list « Aliment ». A `POST`, so that
/// the name looked up stays out of the address.
pub async fn search(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SearchForm>,
) -> Response {
    let cookie = account_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::POST,
        "/taxonomy/search",
        cookie.as_deref(),
        Some(serde_json::json!({ "query": form.query })),
    )
    .await;
    let (found, error) = match result {
        Ok(resp) if resp.status == reqwest::StatusCode::OK => {
            match serde_json::from_value::<SearchResults>(resp.body) {
                Ok(found) => (Some(found.results), None),
                Err(_) => (None, Some("unavailable")),
            }
        }
        Ok(resp) if resp.status == reqwest::StatusCode::BAD_REQUEST => {
            (None, Some("query_too_long"))
        }
        _ => (None, Some("unavailable")),
    };
    let search = found.map(|results| Search {
        query: form.query.trim().to_string(),
        results,
    });
    render(
        &state,
        &headers,
        &me,
        form.consent.is_some(),
        search.as_ref(),
        None,
        error,
    )
    .await
}

/// `GET /account/allergies/clear` — « Tout effacer ? »: what goes, before
/// it goes. A page rather than a dialog: it works without JavaScript.
pub async fn clear_page(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = account_cookie(&headers);
    let Some(lists) = fetch_lists(&state, cookie.as_deref()).await else {
        return service_unavailable_page().into_response();
    };
    let header = account_header(&state, &headers, &me, PATH).await;
    Html(shell_with_header(
        Width::Read,
        "Tout effacer ?",
        &header,
        &clear_body(&lists),
    ))
    .into_response()
}

/// The confirmation's content.
pub(crate) fn clear_body(lists: &Lists) -> String {
    let mut families: Vec<&str> = lists
        .allergies
        .iter()
        .flat_map(|a| a.sharing.iter().map(|s| s.group_name.as_str()))
        .collect();
    families.sort_unstable();
    families.dedup();
    let levels = if families.is_empty() {
        String::new()
    } else {
        format!(
            "<li>Leur niveau de partage dans {} sera supprimé.</li>",
            html_escape(&families.join(", "))
        )
    };
    let consent = if lists.allergies.is_empty() {
        ""
    } else {
        "<li>Votre consentement sera retiré : il vous sera redemandé à la prochaine allergie.</li>"
    };
    format!(
        r#"<hgroup>
<p class="eyebrow">Compte</p>
<h1>Tout effacer ?</h1>
</hgroup>
<section class="card">
<p class="notice warning">Cette action est définitive.</p>
<ul>
<li>Vos allergies ({allergies}) et vos aversions ({dislikes}) seront supprimées.</li>
{levels}
{consent}
</ul>
<form method="post" action="{PATH}/clear" class="actions stacked">
<button type="submit" class="danger">Oui, tout effacer</button>
<a class="btn secondary" href="{PATH}">Annuler</a>
</form>
</section>"#,
        allergies = lists.allergies.len(),
        dislikes = lists.dislikes.len(),
    )
}

/// `POST /account/allergies/clear` — `DELETE /me/dietary-constraints`:
/// every allergy, level and dislike, and so the consent (RGPD art. 7(3)).
pub async fn clear(
    CurrentUser(_me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let cookie = account_cookie(&headers);
    let result = api_request_auth(
        &state,
        reqwest::Method::DELETE,
        "/me/dietary-constraints",
        cookie.as_deref(),
        None,
    )
    .await;
    match result {
        Ok(resp) if resp.status == reqwest::StatusCode::NO_CONTENT => back("notice", "cleared"),
        _ => back("error", "unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn family(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn sharing(n: u128, name: &str, share: &str) -> Sharing {
        Sharing {
            group_id: family(n),
            group_name: name.into(),
            share: share.into(),
        }
    }

    fn allergy(tag: &str, label: &str, day: u32, sharing: Vec<Sharing>) -> Allergy {
        Allergy {
            tag: tag.into(),
            label: label.into(),
            created_at: Utc.with_ymd_and_hms(2026, 11, day, 10, 0, 0).unwrap(),
            sharing,
        }
    }

    fn dislike(tag: &str, label: &str) -> Dislike {
        Dislike {
            tag: tag.into(),
            label: label.into(),
        }
    }

    // -- pure rules ------------------------------------------------------

    #[test]
    fn the_first_allergy_needs_the_box_of_consent() {
        assert_eq!(
            consent_check(Kind::Allergy, false, false),
            Err("consent_required")
        );
        assert_eq!(consent_check(Kind::Allergy, false, true), Ok(()));
    }

    #[test]
    fn with_an_allergy_recorded_consent_is_in_force() {
        assert_eq!(consent_check(Kind::Allergy, true, false), Ok(()));
    }

    #[test]
    fn a_dislike_needs_no_consent() {
        assert_eq!(consent_check(Kind::Dislike, false, false), Ok(()));
    }

    #[test]
    fn the_sharing_form_gives_one_level_per_family() {
        let one = family(1).to_string();
        let two = family(2).to_string();
        let got = sharing_choices(&pairs(&[
            ("tag", "en:peanuts"),
            (&format!("share.{one}"), "public"),
            (&format!("share.{two}"), "private"),
        ]));
        assert_eq!(
            got,
            Ok(vec![
                (family(1), ShareLevel::Public),
                (family(2), ShareLevel::Private)
            ])
        );
        assert_eq!(
            sharing_choices(&pairs(&[("tag", "en:peanuts")])),
            Ok(vec![])
        );
    }

    #[test]
    fn a_forged_sharing_form_saves_nothing() {
        let one = family(1).to_string();
        for bad in [
            vec![(format!("share.{one}"), "secret".to_string())],
            vec![("share.not-a-uuid".to_string(), "public".to_string())],
            vec![
                (format!("share.{one}"), "public".to_string()),
                (format!("share.{one}"), "private".to_string()),
            ],
        ] {
            assert_eq!(sharing_choices(&bad), Err("invalid_share"), "{bad:?}");
        }
    }

    #[test]
    fn without_an_allergy_consent_is_asked_unless_just_given() {
        assert_eq!(consent_state(&Lists::default(), false), Consent::Asked);
        assert_eq!(consent_state(&Lists::default(), true), Consent::JustGiven);
        let only_dislikes = Lists {
            allergies: vec![],
            dislikes: vec![dislike("en:mushrooms", "Champignons")],
        };
        assert_eq!(consent_state(&only_dislikes, false), Consent::Asked);
    }

    #[test]
    fn with_allergies_consent_is_in_force_since_the_oldest() {
        let lists = Lists {
            allergies: vec![
                allergy("en:eggs", "Œufs", 14, vec![]),
                allergy("en:peanuts", "Cacahuètes", 12, vec![]),
            ],
            dislikes: vec![],
        };
        let oldest = Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap();
        for given_now in [false, true] {
            assert_eq!(consent_state(&lists, given_now), Consent::InForce(oldest));
        }
    }

    #[test]
    fn a_date_is_written_out_in_french_on_the_paris_day() {
        let at = Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap();
        assert_eq!(long_date(at), "12 novembre 2026");
        // 23:30 UTC on 30 November is already 1 December in Paris.
        let late = Utc.with_ymd_and_hms(2026, 11, 30, 23, 30, 0).unwrap();
        assert_eq!(long_date(late), "1er décembre 2026");
    }

    #[test]
    fn counts_are_said_in_words() {
        assert_eq!(count(0, "allergie", "allergies"), "0 allergie");
        assert_eq!(count(1, "allergie", "allergies"), "1 allergie");
        assert_eq!(count(2, "allergie", "allergies"), "2 allergies");
    }

    // -- the page ----------------------------------------------------------

    #[test]
    fn before_the_first_allergy_the_page_asks_for_consent() {
        let html = page_body("Camille", &Lists::default(), false, None, None, None);
        assert!(html.contains("<h2>Avant de commencer</h2>"), "{html}");
        // The box and « Continuer » are in the card, in a form of their own.
        let card =
            &html[html.find("Avant de commencer").unwrap()..html.find("<h2>Ajouter</h2>").unwrap()];
        assert!(
            card.contains(r#"<form method="post" action="/account/allergies/consent">"#),
            "{card}"
        );
        assert!(
            card.contains(
                r#"<label class="list-row pick"><input type="checkbox" name="consent" value="on">"#
            ),
            "{card}"
        );
        assert!(
            card.contains(r#"<button type="submit">Continuer</button>"#),
            "{card}"
        );
        // « Ajouter » offers a dislike only, which needs no consent.
        let add = &html[html.find("<h2>Ajouter</h2>").unwrap()..];
        assert!(!add.contains(r#"name="consent""#), "{add}");
        assert!(!add.contains(r#"value="allergy""#), "{add}");
        assert!(
            add.contains(r#"<input type="hidden" name="kind" value="dislike">"#),
            "{add}"
        );
        assert!(
            html.contains(&format!(r#"maxlength="{SEARCH_QUERY_MAX_LEN}""#)),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<button type="button" class="secondary danger" disabled>Tout effacer</button>"#
            ),
            "{html}"
        );
        assert!(
            html.contains("Personne ne voit rien à votre sujet."),
            "{html}"
        );
        assert!(html.contains(r#"href="/privacy-policy""#), "{html}");
    }

    #[test]
    fn the_fourteen_inco_allergens_come_first_then_what_was_found() {
        let search = Search {
            query: "kiwi".into(),
            results: vec![Found {
                tag: "en:kiwifruits".into(),
                label: "Kiwis".into(),
            }],
        };
        let html = page_body(
            "Camille",
            &Lists::default(),
            false,
            Some(&search),
            None,
            None,
        );
        let inco = html
            .find("Les 14 allergènes réglementaires (INCO)")
            .unwrap();
        let found = html
            .find(r#"<option value="en:kiwifruits" selected>Kiwis</option>"#)
            .unwrap();
        assert!(inco < found, "{html}");
        for (tag, label) in INCO_ALLERGENS {
            assert!(
                html.contains(&format!(r#"<option value="{tag}">{label}</option>"#)),
                "{label}"
            );
        }
        assert!(html.contains("1 aliment trouvé"), "{html}");
        // The query stays out of the form's address: it is a POST.
        assert!(
            html.contains(r#"<form method="post" action="/account/allergies/search""#),
            "{html}"
        );
    }

    #[test]
    fn once_an_allergy_is_recorded_the_consent_is_shown_given_and_not_asked_again() {
        let lists = Lists {
            allergies: vec![
                allergy("en:eggs", "Œufs", 14, vec![]),
                allergy("en:peanuts", "Cacahuètes", 12, vec![]),
            ],
            dislikes: vec![],
        };
        let html = page_body("Camille", &lists, false, None, None, None);
        assert!(html.contains("<h2>Mon consentement</h2>"), "{html}");
        assert!(html.contains("Donné"), "{html}");
        assert!(
            html.contains("La plus ancienne date du 12 novembre 2026."),
            "{html}"
        );
        assert!(!html.contains(r#"name="consent""#), "{html}");
        assert!(
            html.contains(r#"<label class="list-row pick"><input type="radio" name="kind" value="allergy" checked>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="btn secondary danger" href="/account/allergies/clear">Tout effacer</a>"#),
            "{html}"
        );
    }

    #[test]
    fn the_consent_just_given_opens_allergies_and_travels_with_both_forms() {
        let html = page_body("Camille", &Lists::default(), true, None, None, None);
        assert!(!html.contains("Avant de commencer"), "{html}");
        assert!(html.contains("<h2>Mon consentement</h2>"), "{html}");
        assert!(html.contains("sans elle, il vous sera redemandé"), "{html}");
        assert!(html.contains(r#"value="allergy" checked"#), "{html}");
        let carried = r#"<input type="hidden" name="consent" value="on">"#;
        let add = html.find(r#"action="/account/allergies/add""#).unwrap();
        let search = html.find(r#"action="/account/allergies/search""#).unwrap();
        assert_eq!(html.matches(carried).count(), 2, "{html}");
        let first = html.find(carried).unwrap();
        let second = html.rfind(carried).unwrap();
        assert!(add < first && first < search && search < second, "{html}");
    }

    #[test]
    fn an_inco_allergen_is_named_as_the_regulation_names_it() {
        let lists = Lists {
            allergies: vec![allergy(
                "en:peanuts",
                "Cacahuètes",
                12,
                vec![sharing(1, "Foyer", "private")],
            )],
            dislikes: vec![dislike("en:coriander-products", "Produits de coriandre")],
        };
        let html = page_body("Camille", &lists, false, None, None, None);
        assert!(html.contains("<strong>Arachides</strong>"), "{html}");
        assert!(html.contains("Retirer « Arachides »"), "{html}");
        assert!(
            html.contains("<strong>Produits de coriandre</strong>"),
            "{html}"
        );
        assert!(html.contains("Aversion · jamais partagée"), "{html}");
        assert!(html.contains("<h2>Ma liste (2)</h2>"), "{html}");
    }

    #[test]
    fn each_family_has_its_three_levels_the_current_one_checked_and_each_effect_said() {
        let lists = Lists {
            allergies: vec![allergy(
                "en:peanuts",
                "Cacahuètes",
                12,
                vec![
                    sharing(1, "Foyer Perrin", "private"),
                    sharing(2, "Famille Martin", "public"),
                    sharing(3, "Famille Roux", "anonymous"),
                ],
            )],
            dislikes: vec![],
        };
        let html = page_body("Camille Perrin", &lists, false, None, None, None);
        for (n, current) in [(1, "private"), (2, "public")] {
            let field = format!("share.{}", family(n));
            for value in ["public", "anonymous", "private"] {
                let checked = if value == current { " checked" } else { "" };
                assert!(
                    html.contains(&format!(
                        r#"<label class="list-row pick"><input type="radio" name="{field}" value="{value}"{checked}>"#
                    )),
                    "{field} {value}: {html}"
                );
            }
        }
        assert!(html.contains("<legend>Foyer Perrin</legend>"), "{html}");
        assert!(
            html.contains("Les membres de Famille Martin verront votre nom et cette allergie, « Camille Perrin : Arachides »"),
            "{html}"
        );
        assert!(
            html.contains("Personne dans Foyer Perrin ne le saura"),
            "{html}"
        );
        // The levels in pills under the name, the word saying each.
        assert!(
            html.contains(r#"<span class="muted">Allergie</span> <span class="badge">Foyer Perrin : privé</span> <span class="badge soon">Famille Martin : public</span> <span class="badge accent">Famille Roux : anonyme</span></summary>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="tag" value="en:peanuts">"#),
            "{html}"
        );
    }

    #[test]
    fn names_are_escaped() {
        let lists = Lists {
            allergies: vec![allergy(
                "en:kiwifruits",
                "<b>Kiwis</b>",
                12,
                vec![sharing(1, "<i>Foyer</i>", "private")],
            )],
            dislikes: vec![],
        };
        let html = page_body("<Camille>", &lists, false, None, None, None);
        assert!(!html.contains("<b>Kiwis</b>"), "{html}");
        assert!(!html.contains("<i>Foyer</i>"), "{html}");
        assert!(!html.contains("<Camille>"), "{html}");
    }

    #[test]
    fn the_confirmation_says_what_goes() {
        let lists = Lists {
            allergies: vec![
                allergy(
                    "en:peanuts",
                    "Cacahuètes",
                    12,
                    vec![sharing(1, "Foyer", "private")],
                ),
                allergy("en:eggs", "Œufs", 13, vec![sharing(1, "Foyer", "public")]),
            ],
            dislikes: vec![dislike("en:mushrooms", "Champignons")],
        };
        let html = clear_body(&lists);
        assert!(html.contains("<h1>Tout effacer ?</h1>"), "{html}");
        assert!(
            html.contains("Vos allergies (2) et vos aversions (1) seront supprimées."),
            "{html}"
        );
        assert!(html.contains("dans Foyer sera supprimé"), "{html}");
        assert!(html.contains("Votre consentement sera retiré"), "{html}");
        assert!(
            html.contains(r#"<form method="post" action="/account/allergies/clear""#),
            "{html}"
        );
    }

    #[test]
    fn every_notice_and_error_code_used_is_in_french() {
        for code in [
            "allergy_added",
            "dislike_added",
            "removed",
            "sharing_saved",
            "cleared",
        ] {
            assert!(notice_text(code).is_some(), "{code}");
        }
        for code in [
            "consent_required",
            "already_listed",
            "unknown_tag",
            "not_found",
            "invalid_share",
            "sharing_failed",
            "query_too_long",
            "unavailable",
        ] {
            assert!(error_text(code).is_some(), "{code}");
        }
    }
}
