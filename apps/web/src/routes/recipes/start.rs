//! `/recipes/depart` — the household's first questions about meals (#820):
//! three short steps, each one skippable, on a member's first visit to the
//! recipes.
//!
//! 1. How many people usually eat: the family's number of members by
//!    default. The answer is kept in a cookie (`USUAL_COOKIE`), not in a
//!    column — that is out of the issue — and prefills the number of people
//!    of « À réaliser » in that family.
//! 2. The member's allergies and dislikes, and one level of sharing per
//!    family applied to all their allergies in it, Privé by default. The
//!    detail per allergy and the consent stay on `/account/allergies`
//!    (#819), which the step points to.
//! 3. The household's kitchen equipment, which only an owner or an admin
//!    changes, as on the family's settings (#819).
//!
//! « Shown once per member » is the browser's: `enhance.js` sends the list
//! page (`data-depart`) here as long as the member has not finished or
//! skipped the questions, kept in `localStorage`, else in a session cookie.
//! Without JavaScript the questions are never forced on anyone; the page
//! stays reachable at its address.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::validation::dietary::{validate_equipment, ShareLevel, EQUIPMENT};
use manage_our_home_shared::validation::groups::can_manage_group;
use manage_our_home_shared::validation::recipes::{parse_servings, servings_label, SERVINGS_MAX};
use serde::Deserialize;
use uuid::Uuid;

use super::{family_context, recipes_cookie, FamilyContext};
use crate::app::{html_escape, kitchen_segment, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::account::allergies::{fetch_lists, name_of, sharing_choices, Allergy, Lists};
use crate::routes::groups::members::fetch_group_detail;
use crate::state::{api_request_auth, AppState};

pub const PATH: &str = "/recipes/depart";

/// The cookie keeping the answer to the first question: `<family>.<n>`,
/// for a year. One family at a time — answering for another replaces it.
pub const USUAL_COOKIE: &str = "repas_personnes";

/// The number of people the first question proposes: the family's members,
/// within the bounds « À réaliser » takes.
pub(crate) fn proposed_servings(members: usize) -> i32 {
    i32::try_from(members)
        .unwrap_or(SERVINGS_MAX)
        .clamp(1, SERVINGS_MAX)
}

/// The `Set-Cookie` value keeping `servings` for `family`. `HttpOnly`, like
/// the active family's: no script reads it.
pub(crate) fn usual_cookie(family: Uuid, servings: i32) -> String {
    format!("{USUAL_COOKIE}={family}.{servings}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Lax")
}

/// The number of people the cookie keeps for `family`, `None` without a
/// cookie, for another family, or out of bounds.
pub(crate) fn usual_servings(headers: &HeaderMap, family: Uuid) -> Option<i32> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        if name != USUAL_COOKIE {
            return None;
        }
        let (id, n) = value.split_once('.')?;
        if id.parse::<Uuid>().ok()? != family {
            return None;
        }
        parse_servings(n).ok().flatten()
    })
}

/// What « À réaliser » proposes: the household's usual number of people
/// when it was given, the recipe's own base otherwise.
pub(crate) fn prefill(usual: Option<i32>, base: Option<i32>) -> Option<i32> {
    usual.or(base)
}

/// One of the three questions, or the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    People,
    Allergies,
    Equipment,
    Done,
}

impl Step {
    /// `?etape=` and the forms' `etape`: `1`, `2`, `3` or `fin`; anything
    /// else is the first question.
    pub(crate) fn parse(value: Option<&str>) -> Step {
        match value {
            Some("2") => Step::Allergies,
            Some("3") => Step::Equipment,
            Some("fin") => Step::Done,
            _ => Step::People,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Step::People => "1",
            Step::Allergies => "2",
            Step::Equipment => "3",
            Step::Done => "fin",
        }
    }

    fn next(self) -> Step {
        match self {
            Step::People => Step::Allergies,
            Step::Allergies => Step::Equipment,
            Step::Equipment | Step::Done => Step::Done,
        }
    }

    fn href(self) -> String {
        format!("{PATH}?etape={}", self.code())
    }
}

/// One of the member's families and the level their allergies have in it:
/// `None` when they differ, so that no level is ticked for them and none is
/// written unless the member picks one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FamilyShare {
    pub group_id: Uuid,
    pub name: String,
    pub level: Option<ShareLevel>,
}

/// The member's families, in the order `GET /me/allergies` first names
/// them, each with the level all their allergies share in it. A level that
/// does not read is `private`, the default. No allergy, no family.
pub(crate) fn family_shares(allergies: &[Allergy]) -> Vec<FamilyShare> {
    let mut shares: Vec<(FamilyShare, ShareLevel)> = Vec::new();
    for sharing in allergies.iter().flat_map(|a| &a.sharing) {
        let level = ShareLevel::parse(&sharing.share).unwrap_or_default();
        match shares
            .iter_mut()
            .find(|(s, _)| s.group_id == sharing.group_id)
        {
            Some((share, first)) => {
                if *first != level {
                    share.level = None;
                }
            }
            None => shares.push((
                FamilyShare {
                    group_id: sharing.group_id,
                    name: sharing.group_name.clone(),
                    level: Some(level),
                },
                level,
            )),
        }
    }
    shares.into_iter().map(|(share, _)| share).collect()
}

/// The `PUT /me/allergies/sharing` to send for the levels chosen, one per
/// family: every allergy of the member whose level in that family differs,
/// as `(tag, family, level)`. A family the member is not in is left out.
pub(crate) fn share_writes(
    allergies: &[Allergy],
    choices: &[(Uuid, ShareLevel)],
) -> Vec<(String, Uuid, ShareLevel)> {
    let mut writes = Vec::new();
    for (group_id, level) in choices {
        for allergy in allergies {
            let Some(sharing) = allergy.sharing.iter().find(|s| s.group_id == *group_id) else {
                continue;
            };
            if ShareLevel::parse(&sharing.share).unwrap_or_default() != *level {
                writes.push((allergy.tag.clone(), *group_id, *level));
            }
        }
    }
    writes
}

/// `family_shares` with the levels of a form sent back in error, so that
/// the page shows what the member chose.
fn with_choices(mut shares: Vec<FamilyShare>, choices: &[(Uuid, ShareLevel)]) -> Vec<FamilyShare> {
    for share in &mut shares {
        if let Some((_, level)) = choices.iter().find(|(g, _)| *g == share.group_id) {
            share.level = Some(*level);
        }
    }
    shares
}

// -- markup ------------------------------------------------------------------

fn heading(step: Step, title: &str) -> String {
    let n = match step {
        Step::People => 1,
        Step::Allergies => 2,
        Step::Equipment => 3,
        Step::Done => 3,
    };
    format!(
        r#"<hgroup>
<p class="eyebrow">Cuisine · Étape {n} sur 3</p>
<h1>{title}</h1>
</hgroup>"#
    )
}

/// The two ways on: the next question without answering this one, and
/// out of all of them, which counts as seen.
fn skip(step: Step) -> String {
    format!(
        r#"<a class="btn secondary" href="{}">Passer cette étape</a>"#,
        step.next().href()
    )
}

fn later(user: Uuid) -> String {
    format!(
        r#"<p><a href="/recipes" data-depart-done="{user}">Plus tard, passer toutes les questions</a></p>"#
    )
}

fn alert(text: &str) -> String {
    format!(r#"<p class="notice error" role="alert">{text}</p>"#)
}

/// Step 1. `members` is `None` when the family could not be read: no
/// value is proposed then. `value` is what the field holds, `error` that it
/// was refused.
pub(crate) fn people_body(
    user: Uuid,
    family: &str,
    members: Option<usize>,
    value: &str,
    error: bool,
) -> String {
    let family = html_escape(family);
    let hint = match members {
        Some(n) if n <= 1 => format!(
            r#"<p class="muted">Vous êtes seul(e) dans « {family} » : la valeur proposée est 1.</p>"#
        ),
        Some(n) => format!(
            r#"<p class="muted">« {family} » compte {n} membres : c'est la valeur proposée.</p>"#
        ),
        None => String::new(),
    };
    let (alert_html, invalid) = if error {
        (
            alert(&format!(
                "Le nombre de personnes doit être compris entre 1 et {SERVINGS_MAX}."
            )),
            r#" aria-invalid="true""#,
        )
    } else {
        (String::new(), "")
    };
    format!(
        r#"{heading}
<p class="muted">Nous préremplirons le nombre de portions de vos recettes. Vous pourrez le changer à chaque fois.</p>
{alert_html}<section class="card">
<form method="post" action="{PATH}">
<input type="hidden" name="etape" value="1">
<label class="field">Personnes à table
<input type="number" name="servings" min="1" max="{SERVINGS_MAX}" step="1" inputmode="numeric" required value="{value}"{invalid}>
</label>
{hint}
<div class="actions"><button type="submit" data-busy-label="Enregistrement…">Continuer</button>{skip}</div>
</form>
</section>
{later}"#,
        heading = heading(Step::People, "Combien de personnes mangent d'habitude ?"),
        value = html_escape(value),
        skip = skip(Step::People),
        later = later(user),
    )
}

fn family_fieldset(share: &FamilyShare) -> String {
    let family = html_escape(&share.name);
    let field = format!("share.{}", share.group_id);
    let choice = |value: ShareLevel, title: &str, hint: &str| {
        let checked = if share.level == Some(value) {
            " checked"
        } else {
            ""
        };
        format!(
            r#"<label class="list-row pick"><input type="radio" name="{field}" value="{value}"{checked}><span><strong>{title}</strong><br><span class="muted">{hint}</span></span></label>"#,
            value = value.as_str(),
        )
    };
    let mixed = if share.level.is_none() {
        "<p>Vos allergies n'ont pas toutes le même niveau dans cette famille. En choisir un l'appliquera à toutes ; sans choix, rien ne change.</p>\n"
    } else {
        ""
    };
    format!(
        r#"<fieldset class="plain share">
<legend>{family}</legend>
{mixed}{public}
{anonymous}
{private}
<p class="public">Les membres de {family} verront votre nom et vos allergies, et les recettes qui en contiennent leur seront signalées.</p>
<p class="anonymous">Les membres de {family} sauront seulement qu'un membre de la famille a une allergie, sans votre nom ni laquelle.</p>
</fieldset>"#,
        public = choice(
            ShareLevel::Public,
            "Public",
            "Mon nom et l'allergie sont visibles de tous"
        ),
        anonymous = choice(
            ShareLevel::Anonymous,
            "Anonyme",
            "« Un membre de la famille est allergique »"
        ),
        private = choice(ShareLevel::Private, "Privé", "Personne ne le sait"),
    )
}

/// Step 2. `lists` is `None` when the allergies could not be read.
/// `error` is the alert of a form that could not be saved.
pub(crate) fn allergies_body(
    user: Uuid,
    lists: Option<&Lists>,
    shares: &[FamilyShare],
    error: Option<&str>,
) -> String {
    let heading = heading(Step::Allergies, "Vos allergies et aversions");
    let alert_html = error.map(alert).unwrap_or_default();
    let Some(lists) = lists else {
        return format!(
            r#"{heading}
<p class="muted">Elles servent à signaler et écarter les recettes concernées.</p>
{alert}<div class="actions"><a class="btn secondary" href="/account/allergies">Gérer mes allergies et aversions</a>{skip}</div>
{later}"#,
            alert = alert(
                "Vos allergies n'ont pas pu être lues. Passez cette étape, ou réglez-les depuis votre compte."
            ),
            skip = skip(Step::Allergies),
            later = later(user),
        );
    };
    let empty = lists.allergies.is_empty() && lists.dislikes.is_empty();
    let list = if empty {
        r#"<p class="muted">Vous n'avez déclaré aucune allergie ni aversion. Personne ne voit rien à votre sujet.</p>"#
            .to_string()
    } else {
        let allergies = lists.allergies.iter().map(|a| {
            format!(
                r#"<li class="list-row stacked"><span><strong>{}</strong><br><span class="muted">Allergie</span></span></li>"#,
                html_escape(name_of(&a.tag, &a.label))
            )
        });
        let dislikes = lists.dislikes.iter().map(|d| {
            format!(
                r#"<li class="list-row stacked"><span><strong>{}</strong><br><span class="muted">Aversion · jamais partagée</span></span></li>"#,
                html_escape(name_of(&d.tag, &d.label))
            )
        });
        format!(
            r#"<ul class="list">{}</ul>"#,
            allergies.chain(dislikes).collect::<String>()
        )
    };
    let manage = if empty {
        "Ajouter une allergie ou une aversion"
    } else {
        "Gérer mes allergies et aversions"
    };
    let sharing = if shares.is_empty() {
        r#"<p class="muted">Le niveau de partage se règle ici dès que vous aurez déclaré une allergie.</p>"#
            .to_string()
    } else {
        format!(
            r#"<p class="muted">Par défaut, une allergie est <strong>privée</strong> dans toutes vos familles : personne ne le sait. Vous changez cela famille par famille, pour toutes vos allergies ; le détail allergie par allergie reste dans votre compte.</p>{}"#,
            shares.iter().map(family_fieldset).collect::<String>()
        )
    };
    format!(
        r#"{heading}
<p class="muted">Elles servent à signaler et écarter les recettes concernées.</p>
{alert_html}<section class="card">
<h2>Ma liste</h2>
{list}
<div class="actions"><a class="btn secondary" href="/account/allergies">{manage}</a></div>
</section>
<section class="card">
<form method="post" action="{PATH}">
<input type="hidden" name="etape" value="2">
<h2>Partage</h2>
{sharing}
<div class="actions"><button type="submit" data-busy-label="Enregistrement…">Continuer</button>{skip}</div>
</form>
</section>
{later}"#,
        skip = skip(Step::Allergies),
        later = later(user),
    )
}

/// Step 3. `household` is `None` when the list could not be read: no form
/// then, which would empty it. `ticked` overrides it for a form sent back
/// in error.
pub(crate) fn equipment_body(
    user: Uuid,
    family: &str,
    household: Option<&[String]>,
    can_edit: bool,
    error: bool,
) -> String {
    let family = html_escape(family);
    let heading = heading(Step::Equipment, "Les équipements de la cuisine");
    let alert_html = if error {
        alert("Les équipements n'ont pas pu être enregistrés. Réessayez, ou passez cette étape.")
    } else {
        String::new()
    };
    let finish = format!(
        r#"<div class="actions"><a class="btn" href="{}">Terminer</a></div>"#,
        Step::Done.href()
    );
    let body = match household {
        None => format!(
            r#"{}
{finish}"#,
            alert("Les équipements de la famille n'ont pas pu être lus. Passez cette étape, ou réglez-les dans les paramètres de la famille.")
        ),
        Some(household) if can_edit => {
            let boxes: String = EQUIPMENT
                .iter()
                .map(|(code, label)| {
                    let checked = if household.iter().any(|c| c == code) {
                        " checked"
                    } else {
                        ""
                    };
                    format!(
                        r#"<label class="field inline pick"><input type="checkbox" name="equipment" value="{code}"{checked}>{label}</label>"#
                    )
                })
                .collect();
            format!(
                r#"<form method="post" action="{PATH}">
<input type="hidden" name="etape" value="3">
<fieldset class="plain">
<legend class="sr-only">Équipements disponibles</legend>
<div class="picks">{boxes}</div>
</fieldset>
<div class="actions"><button type="submit" data-busy-label="Enregistrement…">Terminer</button>{skip}</div>
</form>"#,
                skip = skip(Step::Equipment),
            )
        }
        Some(household) => {
            let names: Vec<&str> = EQUIPMENT
                .iter()
                .filter(|(code, _)| household.iter().any(|c| c == code))
                .map(|(_, label)| *label)
                .collect();
            let list = if names.is_empty() {
                "Aucun équipement déclaré.".to_string()
            } else {
                html_escape(&names.join(", "))
            };
            format!(
                r#"<p>{list}</p><p class="muted">Seuls le propriétaire et les admins modifient cette liste.</p>{finish}"#
            )
        }
    };
    format!(
        r#"{heading}
<p class="muted">Ce dont dispose « {family} ». Une recette qui exige un équipement absent vous sera signalée.</p>
{alert_html}<section class="card" id="equipements">
<h2>Équipements de la cuisine</h2>
{body}
</section>
{later}"#,
        later = later(user),
    )
}

/// The end, which counts as seen (`data-depart-done` on a card, read when
/// the page loads).
pub(crate) fn done_body(user: Uuid, usual: Option<i32>) -> String {
    let people = match usual {
        Some(n) => format!(
            "Les recettes que vous sélectionnerez partiront de {}.",
            servings_label(n)
        ),
        None => "Les recettes que vous sélectionnerez partiront du nombre de personnes de chacune."
            .to_string(),
    };
    format!(
        r#"<p class="notice success" role="status">C'est noté. Vous retrouverez ces réglages dans votre compte et dans les paramètres de la famille.</p>
<hgroup><p class="eyebrow">Cuisine</p><h1>C'est prêt</h1></hgroup>
<section class="card" data-depart-done="{user}"><p>{people}</p>
<div class="actions"><a class="btn" href="/recipes">Voir mes recettes</a></div></section>"#
    )
}

// -- handlers ----------------------------------------------------------------

fn page(fam: &FamilyContext, body: &str) -> Response {
    Html(shell_with_header(
        Width::Read,
        "Premières questions",
        &fam.header,
        &format!("{}{body}", kitchen_segment("/recipes")),
    ))
    .into_response()
}

async fn members_of(state: &AppState, fam: &FamilyContext, cookie: Option<&str>) -> Option<usize> {
    fetch_group_detail(state, cookie, fam.gid)
        .await
        .ok()
        .flatten()
        .map(|g| g.members.len())
}

async fn people_page(
    state: &AppState,
    fam: &FamilyContext,
    headers: &HeaderMap,
    user: Uuid,
    sent: Option<&str>,
) -> Response {
    let cookie = recipes_cookie(headers);
    let members = members_of(state, fam, cookie.as_deref()).await;
    let value = match sent {
        Some(v) => v.to_string(),
        None => usual_servings(headers, fam.gid)
            .or(members.map(proposed_servings))
            .map(|n| n.to_string())
            .unwrap_or_default(),
    };
    page(
        fam,
        &people_body(user, &fam.name, members, &value, sent.is_some()),
    )
}

async fn allergies_page(
    state: &AppState,
    fam: &FamilyContext,
    headers: &HeaderMap,
    user: Uuid,
    sent: Option<(&[(Uuid, ShareLevel)], &str)>,
) -> Response {
    let cookie = recipes_cookie(headers);
    let lists = fetch_lists(state, cookie.as_deref()).await;
    let shares = lists
        .as_ref()
        .map(|l| family_shares(&l.allergies))
        .unwrap_or_default();
    let (shares, error) = match sent {
        Some((choices, error)) => (with_choices(shares, choices), Some(error)),
        None => (shares, None),
    };
    page(fam, &allergies_body(user, lists.as_ref(), &shares, error))
}

async fn equipment_page(
    state: &AppState,
    fam: &FamilyContext,
    headers: &HeaderMap,
    user: Uuid,
    sent: Option<&[String]>,
) -> Response {
    let cookie = recipes_cookie(headers);
    let household = match sent {
        // What was ticked, for whoever may tick; the list in force otherwise.
        Some(ticked) if can_manage_group(&fam.role) => Some(ticked.to_vec()),
        _ => super::equipment::fetch_household(state, fam.gid, cookie.as_deref()).await,
    };
    page(
        fam,
        &equipment_body(
            user,
            &fam.name,
            household.as_deref(),
            can_manage_group(&fam.role),
            sent.is_some(),
        ),
    )
}

#[derive(Deserialize)]
pub struct StepQuery {
    etape: Option<String>,
}

/// `GET /recipes/depart?etape=1|2|3|fin`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StepQuery>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, PATH).await else {
        return Redirect::to("/groups/new").into_response();
    };
    match Step::parse(query.etape.as_deref()) {
        Step::People => people_page(&state, &fam, &headers, me.user_id, None).await,
        Step::Allergies => allergies_page(&state, &fam, &headers, me.user_id, None).await,
        Step::Equipment => equipment_page(&state, &fam, &headers, me.user_id, None).await,
        Step::Done => page(
            &fam,
            &done_body(me.user_id, usual_servings(&headers, fam.gid)),
        ),
    }
}

/// `POST /recipes/depart`: the answer to the step named by `etape`, then
/// the next one; a refused or failed answer comes back on its step, as
/// sent.
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let Some(fam) = family_context(&state, &headers, &me, PATH).await else {
        return Redirect::to("/groups/new").into_response();
    };
    let field = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let step = Step::parse(field("etape"));
    let next = Redirect::to(&step.next().href());
    let cookie = recipes_cookie(&headers);
    match step {
        Step::People => {
            let sent = field("servings").unwrap_or_default();
            match parse_servings(sent) {
                Ok(Some(n)) => {
                    ([(header::SET_COOKIE, usual_cookie(fam.gid, n))], next).into_response()
                }
                _ => people_page(&state, &fam, &headers, me.user_id, Some(sent)).await,
            }
        }
        Step::Allergies => {
            let failed = "Le partage n'a pas pu être enregistré. Vos réponses sont conservées : réessayez, ou passez cette étape et réglez-le plus tard depuis votre compte.";
            let Ok(choices) = sharing_choices(&pairs) else {
                return allergies_page(&state, &fam, &headers, me.user_id, Some((&[], failed)))
                    .await;
            };
            let Some(lists) = fetch_lists(&state, cookie.as_deref()).await else {
                return allergies_page(
                    &state,
                    &fam,
                    &headers,
                    me.user_id,
                    Some((&choices, failed)),
                )
                .await;
            };
            for (tag, group_id, share) in share_writes(&lists.allergies, &choices) {
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
                if !matches!(result, Ok(ref resp) if resp.status == reqwest::StatusCode::OK) {
                    return allergies_page(
                        &state,
                        &fam,
                        &headers,
                        me.user_id,
                        Some((&choices, failed)),
                    )
                    .await;
                }
            }
            next.into_response()
        }
        Step::Equipment => {
            let ticked: Vec<String> = pairs
                .iter()
                .filter(|(k, _)| k == "equipment")
                .map(|(_, v)| v.clone())
                .collect();
            let Ok(codes) = validate_equipment(&ticked) else {
                return equipment_page(&state, &fam, &headers, me.user_id, Some(&ticked)).await;
            };
            let result = api_request_auth(
                &state,
                reqwest::Method::PUT,
                &format!("/groups/{}/equipment", fam.gid),
                cookie.as_deref(),
                Some(serde_json::json!({ "codes": codes })),
            )
            .await;
            if matches!(result, Ok(ref resp) if resp.status == reqwest::StatusCode::OK) {
                next.into_response()
            } else {
                equipment_page(&state, &fam, &headers, me.user_id, Some(&ticked)).await
            }
        }
        Step::Done => Redirect::to(&Step::Done.href()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::account::allergies::{Dislike, Sharing};
    use chrono::{TimeZone, Utc};

    fn family(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn allergy(tag: &str, sharing: &[(u128, &str, &str)]) -> Allergy {
        Allergy {
            tag: tag.into(),
            label: tag.into(),
            created_at: Utc.with_ymd_and_hms(2026, 11, 17, 10, 0, 0).unwrap(),
            sharing: sharing
                .iter()
                .map(|(n, name, share)| Sharing {
                    group_id: family(*n),
                    group_name: (*name).into(),
                    share: (*share).into(),
                })
                .collect(),
        }
    }

    fn cookie_headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        headers
    }

    #[test]
    fn the_proposed_number_of_people_is_the_number_of_members() {
        assert_eq!(proposed_servings(3), 3);
        assert_eq!(proposed_servings(1), 1);
        // A family is never empty to its member; a count of 0 still
        // proposes a valid number, and a crowd stays within the bounds.
        assert_eq!(proposed_servings(0), 1);
        assert_eq!(proposed_servings(80), SERVINGS_MAX);
    }

    #[test]
    fn the_answer_is_kept_for_one_family_and_read_back_for_it_alone() {
        let set = usual_cookie(family(1), 4);
        assert!(
            set.starts_with(&format!("{USUAL_COOKIE}={}.4;", family(1))),
            "{set}"
        );
        for attr in ["Path=/", "Max-Age=31536000", "HttpOnly", "SameSite=Lax"] {
            assert!(set.contains(attr), "{set} lacks {attr}");
        }
        let pair = set.split(';').next().unwrap();
        let headers = cookie_headers(&format!("session=s; {pair}"));
        assert_eq!(usual_servings(&headers, family(1)), Some(4));
        assert_eq!(usual_servings(&headers, family(2)), None);
        assert_eq!(usual_servings(&HeaderMap::new(), family(1)), None);
    }

    #[test]
    fn a_cookie_that_does_not_read_or_is_out_of_bounds_is_no_answer() {
        for value in [
            format!("{}.0", family(1)),
            format!("{}.51", family(1)),
            format!("{}.trois", family(1)),
            format!("{}", family(1)),
            "pas-un-id.3".to_string(),
        ] {
            let headers = cookie_headers(&format!("{USUAL_COOKIE}={value}"));
            assert_eq!(usual_servings(&headers, family(1)), None, "{value}");
        }
    }

    #[test]
    fn a_realiser_starts_from_the_households_number_then_the_recipes() {
        assert_eq!(prefill(Some(3), Some(6)), Some(3));
        assert_eq!(prefill(Some(3), None), Some(3));
        assert_eq!(prefill(None, Some(6)), Some(6));
        assert_eq!(prefill(None, None), None);
    }

    #[test]
    fn the_step_comes_from_the_address_and_defaults_to_the_first() {
        assert_eq!(Step::parse(None), Step::People);
        assert_eq!(Step::parse(Some("1")), Step::People);
        assert_eq!(Step::parse(Some("2")), Step::Allergies);
        assert_eq!(Step::parse(Some("3")), Step::Equipment);
        assert_eq!(Step::parse(Some("fin")), Step::Done);
        assert_eq!(Step::parse(Some("4")), Step::People);
        assert_eq!(Step::Equipment.next(), Step::Done);
    }

    #[test]
    fn each_family_gets_the_level_its_allergies_share_or_none_when_they_differ() {
        let allergies = [
            allergy(
                "en:peanuts",
                &[(1, "Notre maison", "public"), (2, "Martin", "private")],
            ),
            allergy(
                "en:kiwis",
                &[(1, "Notre maison", "public"), (2, "Martin", "anonymous")],
            ),
        ];
        assert_eq!(
            family_shares(&allergies),
            vec![
                FamilyShare {
                    group_id: family(1),
                    name: "Notre maison".into(),
                    level: Some(ShareLevel::Public),
                },
                FamilyShare {
                    group_id: family(2),
                    name: "Martin".into(),
                    level: None,
                },
            ]
        );
        assert!(family_shares(&[]).is_empty());
    }

    #[test]
    fn a_level_that_does_not_read_is_private() {
        let shares = family_shares(&[allergy("en:peanuts", &[(1, "Notre maison", "???")])]);
        assert_eq!(shares[0].level, Some(ShareLevel::Private));
    }

    #[test]
    fn one_level_per_family_is_written_to_each_allergy_that_differs() {
        let allergies = [
            allergy("en:peanuts", &[(1, "A", "private"), (2, "B", "private")]),
            allergy("en:kiwis", &[(1, "A", "public"), (2, "B", "private")]),
        ];
        let writes = share_writes(
            &allergies,
            &[
                (family(1), ShareLevel::Public),
                (family(2), ShareLevel::Private),
                (family(9), ShareLevel::Public),
            ],
        );
        assert_eq!(
            writes,
            vec![("en:peanuts".to_string(), family(1), ShareLevel::Public)]
        );
        assert!(share_writes(&allergies, &[]).is_empty());
    }

    #[test]
    fn the_first_question_proposes_the_members_and_says_why() {
        let html = people_body(family(7), "Notre <maison>", Some(3), "3", false);
        assert!(html.contains("Étape 1 sur 3"), "{html}");
        assert!(html.contains(r#"name="servings""#), "{html}");
        assert!(html.contains(r#"value="3""#), "{html}");
        assert!(
            html.contains("« Notre &lt;maison&gt; » compte 3 membres"),
            "{html}"
        );
        assert!(html.contains(r#"href="/recipes/depart?etape=2">Passer cette étape"#));
        assert!(html.contains(&format!(
            r#"<a href="/recipes" data-depart-done="{}">Plus tard"#,
            family(7)
        )));
        assert!(!html.contains("role=\"alert\""), "{html}");
        let alone = people_body(family(7), "Solo", Some(1), "1", false);
        assert!(alone.contains("Vous êtes seul(e) dans « Solo »"), "{alone}");
    }

    #[test]
    fn a_refused_number_comes_back_as_typed_with_an_alert() {
        let html = people_body(family(7), "Notre maison", Some(3), "0", true);
        assert!(html.contains(r#"role="alert""#), "{html}");
        assert!(html.contains("entre 1 et 50"), "{html}");
        assert!(html.contains(r#"value="0" aria-invalid="true""#), "{html}");
    }

    fn lists() -> Lists {
        Lists {
            allergies: vec![allergy("en:peanuts", &[(1, "Notre maison", "private")])],
            dislikes: vec![Dislike {
                tag: "en:coriander".into(),
                label: "Coriandre".into(),
            }],
        }
    }

    #[test]
    fn the_second_question_lists_both_and_ticks_private_by_default() {
        let lists = lists();
        let shares = family_shares(&lists.allergies);
        let html = allergies_body(family(7), Some(&lists), &shares, None);
        assert!(html.contains("Étape 2 sur 3"), "{html}");
        assert!(html.contains("Aversion · jamais partagée"), "{html}");
        assert!(html.contains(r#"href="/account/allergies""#), "{html}");
        assert!(html.contains("<legend>Notre maison</legend>"), "{html}");
        let field = format!("share.{}", family(1));
        assert!(
            html.contains(&format!(
                r#"<input type="radio" name="{field}" value="private" checked>"#
            )),
            "{html}"
        );
        assert_eq!(html.matches(" checked>").count(), 1, "{html}");
    }

    #[test]
    fn mixed_levels_tick_nothing_and_say_so() {
        let share = FamilyShare {
            group_id: family(1),
            name: "Martin".into(),
            level: None,
        };
        let html = allergies_body(family(7), Some(&lists()), &[share], None);
        assert!(!html.contains(" checked"), "{html}");
        assert!(html.contains("pas toutes le même niveau"), "{html}");
    }

    #[test]
    fn without_any_allergy_the_second_question_has_no_level() {
        let html = allergies_body(family(7), Some(&Lists::default()), &[], None);
        assert!(html.contains("aucune allergie ni aversion"), "{html}");
        assert!(!html.contains(r#"type="radio""#), "{html}");
        assert!(
            html.contains("Ajouter une allergie ou une aversion"),
            "{html}"
        );
    }

    #[test]
    fn unread_allergies_offer_no_form() {
        let html = allergies_body(family(7), None, &[], None);
        assert!(html.contains(r#"role="alert""#), "{html}");
        assert!(!html.contains("<form"), "{html}");
        assert!(html.contains("Passer cette étape"), "{html}");
    }

    #[test]
    fn the_third_question_is_a_form_for_owners_and_a_list_for_the_others() {
        let household = vec!["oven".to_string(), "hob".to_string()];
        let editable = equipment_body(family(7), "Notre maison", Some(&household), true, false);
        assert!(editable.contains("Étape 3 sur 3"), "{editable}");
        assert!(
            editable.contains(r#"value="oven" checked>Four"#),
            "{editable}"
        );
        assert_eq!(
            editable.matches(r#"type="checkbox""#).count(),
            EQUIPMENT.len(),
            "{editable}"
        );
        assert!(
            editable.contains("?etape=fin\">Passer cette étape"),
            "{editable}"
        );

        let read = equipment_body(family(7), "Notre maison", Some(&household), false, false);
        assert!(!read.contains("<form"), "{read}");
        assert!(read.contains("<p>Four, Plaques de cuisson</p>"), "{read}");
        assert!(
            read.contains("Seuls le propriétaire et les admins"),
            "{read}"
        );

        let unread = equipment_body(family(7), "Notre maison", None, true, false);
        assert!(!unread.contains("<form"), "{unread}");
        assert!(unread.contains(r#"role="alert""#), "{unread}");
    }

    #[test]
    fn enhance_js_reads_the_marks_these_pages_and_the_list_carry() {
        // The list's `data-depart`, the end's and « Plus tard »'s
        // `data-depart-done`, kept in localStorage or else a session cookie.
        for needle in [
            "[data-depart]",
            "a[data-depart-done]",
            "[data-depart-done]:not(a)",
            "localStorage.setItem",
            "document.cookie = MEAL_START_COOKIE",
            "location.replace(\"/recipes/depart\")",
        ] {
            assert!(
                crate::app::ENHANCE_SCRIPT.contains(needle),
                "enhance.js lacks {needle}"
            );
        }
    }

    #[test]
    fn the_end_counts_as_seen_and_names_the_number_of_people() {
        let html = done_body(family(7), Some(3));
        assert!(
            html.contains(&format!(r#"data-depart-done="{}""#, family(7))),
            "{html}"
        );
        assert!(html.contains("partiront de 3 personnes"), "{html}");
        assert!(!done_body(family(7), None).contains("partiront de 1"));
    }
}
