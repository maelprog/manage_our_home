//! `/groups/{id}/allergies` — « Partager vos allergies » (#827): right after
//! a member creates a family (`new.rs`) or joins one (`invitations.rs`), the
//! level of each of their allergies in it — Public, Anonyme or Privé.
//!
//! Until they choose, nothing is written: an allergy with no level in a
//! family is private there (migration 0057), and joining or creating one
//! writes none. « Valider mes choix » writes only the levels that change,
//! so validating everything « Privé » writes nothing either; « Plus tard »
//! is a link, which writes nothing. A member without an allergy never sees
//! the page. The levels stay editable from `/account/allergies` (#819).
//!
//! What the page shows comes from `GET /me/allergies` only: the member's own
//! allergies, never another member's. A tag travels in the form's body,
//! never in an address, as on the account page.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::validation::dietary::{validate_tag, ShareLevel};
use serde::Deserialize;
use uuid::Uuid;

use super::{cookie_of, header_with_groups};
use crate::app::{html_escape, shell, shell_with_header, Width};
use crate::layout::CurrentUser;
use crate::routes::account::allergies::{name_of, Allergy};
use crate::state::{api_request_auth, AppState};

/// How the member came into the family: what the page says, and where
/// « Plus tard » leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrival {
    Created,
    Joined,
}

impl Arrival {
    pub(crate) fn parse(value: &str) -> Option<Arrival> {
        match value {
            "created" => Some(Arrival::Created),
            "joined" => Some(Arrival::Joined),
            _ => None,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Arrival::Created => "created",
            Arrival::Joined => "joined",
        }
    }

    /// The notice `/groups` gives when the page is skipped.
    fn notice(self) -> &'static str {
        match self {
            Arrival::Created => "group_created",
            Arrival::Joined => "joined",
        }
    }
}

/// Where a member goes once in a family: the sharing page when they hold
/// an allergy, or when that could not be told — the page tells it again and
/// sends them on —, `/groups` with its notice otherwise.
pub(crate) fn after_arrival(
    group_id: Uuid,
    arrival: Arrival,
    has_allergies: Option<bool>,
) -> String {
    match has_allergies {
        Some(false) => format!("/groups?notice={}", arrival.notice()),
        Some(true) | None => format!("/groups/{group_id}/allergies?from={}", arrival.code()),
    }
}

/// One of the member's allergies and its level in the family of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Level {
    pub tag: String,
    pub name: String,
    pub share: ShareLevel,
}

/// The family's name and the member's allergies with their level in it,
/// in the order of `GET /me/allergies`. `None` when no allergy carries the
/// family: the member is not in it, or holds no allergy. A level that does
/// not read is shown as `private`, the default.
pub(crate) fn levels_in(allergies: &[Allergy], group_id: Uuid) -> Option<(String, Vec<Level>)> {
    let mut family = None;
    let mut levels = Vec::new();
    for allergy in allergies {
        let Some(sharing) = allergy.sharing.iter().find(|s| s.group_id == group_id) else {
            continue;
        };
        family.get_or_insert_with(|| sharing.group_name.clone());
        levels.push(Level {
            tag: allergy.tag.clone(),
            name: name_of(&allergy.tag, &allergy.label).to_string(),
            share: ShareLevel::parse(&sharing.share).unwrap_or_default(),
        });
    }
    family.map(|family| (family, levels))
}

/// The levels a « Valider mes choix » form sets: `level.<tag>` = `public`,
/// `anonymous` or `private`. A tag or a value that does not read, or a tag
/// given twice, refuses the whole form (`invalid_share`): nothing is
/// half-saved from a forged form.
pub(crate) fn level_choices(
    pairs: &[(String, String)],
) -> Result<Vec<(String, ShareLevel)>, &'static str> {
    let mut choices: Vec<(String, ShareLevel)> = Vec::new();
    for (key, value) in pairs {
        let Some(tag) = key.strip_prefix("level.") else {
            continue;
        };
        let tag = validate_tag(tag).map_err(|_| "invalid_share")?;
        let share = ShareLevel::parse(value).map_err(|_| "invalid_share")?;
        if choices.iter().any(|(t, _)| t == tag) {
            return Err("invalid_share");
        }
        choices.push((tag.to_string(), share));
    }
    Ok(choices)
}

/// The levels to write: those chosen that differ from the one in force. A
/// tag the member no longer holds is left out — it has no level to set —,
/// and an allergy the form did not carry keeps its own.
pub(crate) fn changes(
    levels: &[Level],
    choices: &[(String, ShareLevel)],
) -> Vec<(String, ShareLevel)> {
    choices
        .iter()
        .filter(|(tag, share)| levels.iter().any(|l| l.tag == *tag && l.share != *share))
        .cloned()
        .collect()
}

fn error_text(code: &str) -> Option<&'static str> {
    match code {
        "invalid_share" => Some("Ce niveau de partage n'est pas proposé."),
        "sharing_failed" => Some(
            "Vos choix n'ont pas tous pu être enregistrés. Les niveaux affichés sont ceux en vigueur : réessayez, ou continuez.",
        ),
        _ => None,
    }
}

/// One allergy's three levels, as a segmented control.
fn allergy_levels(level: &Level) -> String {
    let field = html_escape(&format!("level.{}", level.tag));
    let choice = |value: ShareLevel, title: &str| {
        let checked = if level.share == value { " checked" } else { "" };
        format!(
            r#"<label class="pick level"><input type="radio" name="{field}" value="{value}"{checked}>{title}</label>"#,
            value = value.as_str(),
        )
    };
    format!(
        r#"<li class="list-row stacked"><fieldset class="plain">
<legend><strong>{name}</strong></legend>
<div class="levels">{public}{anonymous}{private}</div>
</fieldset></li>"#,
        name = html_escape(&level.name),
        public = choice(ShareLevel::Public, "Public"),
        anonymous = choice(ShareLevel::Anonymous, "Anonyme"),
        private = choice(ShareLevel::Private, "Privé"),
    )
}

/// The page's content.
pub(crate) fn page_body(
    group_id: Uuid,
    family: &str,
    arrival: Option<Arrival>,
    levels: &[Level],
    error: Option<&str>,
) -> String {
    let family = html_escape(family);
    let all_private = levels.iter().all(|l| l.share == ShareLevel::Private);
    let error = error
        .and_then(error_text)
        .map(|e| format!(r#"<p class="notice error" role="alert">{e}</p>"#))
        .unwrap_or_default();
    let opening = match arrival {
        Some(Arrival::Created) => "Votre groupe est créé. ".to_string(),
        Some(Arrival::Joined) => format!("Vous venez de rejoindre {family}. "),
        None => String::new(),
    };
    let nothing_yet = if all_private {
        " Rien n'est partagé tant que vous n'avez pas validé : tout reste privé."
    } else {
        ""
    };
    let rows: String = levels.iter().map(allergy_levels).collect();
    let later_href = match arrival {
        Some(arrival) => format!("/groups?notice={}", arrival.notice()),
        None => "/groups".to_string(),
    };
    let later = if all_private {
        "Plus tard, tout laisser privé"
    } else {
        "Plus tard"
    };
    let from = arrival
        .map(|a| format!(r#"<input type="hidden" name="from" value="{}">"#, a.code()))
        .unwrap_or_default();
    format!(
        r#"<h1>Partager vos allergies avec {family}</h1>
<p>{opening}Choisissez ce que ses membres voient de vos allergies.{nothing_yet}</p>
{error}<ul class="key">
<li><strong>Public</strong><span>Les membres voient l'allergie et que c'est la vôtre.</span></li>
<li><strong>Anonyme</strong><span>Les membres savent qu'un membre a une allergie, sans votre nom ni laquelle.</span></li>
<li><strong>Privé</strong><span>Vous seul la voyez.</span></li>
</ul>
<form method="post" action="/groups/{group_id}/allergies">
{from}<section class="card"><ul class="list">{rows}</ul></section>
<p class="muted">Vous pourrez modifier ces choix à tout moment depuis <a href="/account/allergies">Mes allergies et aversions</a>. Une allergie ajoutée plus tard est d'abord privée dans toutes vos familles.</p>
<div class="actions stacked">
<button type="submit" data-busy-label="Enregistrement…">Valider mes choix</button>
<a class="btn secondary" href="{later_href}">{later}</a>
</div>
</form>"#
    )
}

#[derive(Deserialize)]
struct AllergyList {
    allergies: Vec<Allergy>,
}

/// `GET /me/allergies`, `None` on any failure.
async fn fetch_allergies(state: &AppState, cookie: Option<&str>) -> Option<Vec<Allergy>> {
    let resp = api_request_auth(state, reqwest::Method::GET, "/me/allergies", cookie, None)
        .await
        .ok()
        .filter(|r| r.status == reqwest::StatusCode::OK)?;
    serde_json::from_value::<AllergyList>(resp.body)
        .ok()
        .map(|l| l.allergies)
}

/// Whether the member holds an allergy, `None` when that could not be told.
pub(crate) async fn has_allergies(state: &AppState, cookie: Option<&str>) -> Option<bool> {
    fetch_allergies(state, cookie).await.map(|a| !a.is_empty())
}

/// Says nothing of the levels in force: `post()` reaches it after a level
/// may already have been written.
fn unavailable_page() -> Html<String> {
    let body = r#"<h1>Service momentanément indisponible</h1>
<p>Vos allergies n'ont pas pu être lues. Merci de réessayer dans quelques instants.</p>
<a class="btn secondary" href="/groups">Retour à mes groupes</a>"#;
    Html(shell(Width::Form, "Service indisponible", body))
}

/// The page with the levels in force, or where the member goes instead:
/// `/groups` when the family holds none of their allergies.
async fn render(
    state: &AppState,
    headers: &HeaderMap,
    me: &MeResponse,
    group_id: Uuid,
    arrival: Option<Arrival>,
    error: Option<&str>,
) -> Response {
    let cookie = cookie_of(headers);
    let Some(allergies) = fetch_allergies(state, cookie.as_deref()).await else {
        return unavailable_page().into_response();
    };
    let Some((family, levels)) = levels_in(&allergies, group_id) else {
        let to = match arrival {
            Some(arrival) => format!("/groups?notice={}", arrival.notice()),
            None => "/groups".to_string(),
        };
        return Redirect::to(&to).into_response();
    };
    let path = format!("/groups/{group_id}/allergies");
    let (_groups, header) = header_with_groups(state, headers, me, &path).await;
    Html(shell_with_header(
        Width::Form,
        "Partager vos allergies",
        &header,
        &page_body(group_id, &family, arrival, &levels, error),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub struct PageQuery {
    from: Option<String>,
}

/// `GET /groups/{id}/allergies?from=created|joined`.
pub async fn get(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Query(query): Query<PageQuery>,
) -> Response {
    let arrival = query.from.as_deref().and_then(Arrival::parse);
    render(&state, &headers, &me, group_id, arrival, None).await
}

/// `POST /groups/{id}/allergies` — one `PUT /me/allergies/sharing` per
/// level that changes, then `/groups`. On a failure the page comes back
/// with the levels in force, read again.
pub async fn post(
    CurrentUser(me): CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(group_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let arrival = pairs
        .iter()
        .find(|(k, _)| k == "from")
        .and_then(|(_, v)| Arrival::parse(v));
    let choices = match level_choices(&pairs) {
        Ok(choices) => choices,
        Err(code) => return render(&state, &headers, &me, group_id, arrival, Some(code)).await,
    };
    let cookie = cookie_of(&headers);
    let Some(allergies) = fetch_allergies(&state, cookie.as_deref()).await else {
        return unavailable_page().into_response();
    };
    let Some((_, levels)) = levels_in(&allergies, group_id) else {
        return Redirect::to("/groups").into_response();
    };
    for (tag, share) in changes(&levels, &choices) {
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
            return render(
                &state,
                &headers,
                &me,
                group_id,
                arrival,
                Some("sharing_failed"),
            )
            .await;
        }
    }
    Redirect::to("/groups?notice=allergy_sharing_saved").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::account::allergies::Sharing;
    use chrono::{TimeZone, Utc};

    fn family(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn allergy(tag: &str, label: &str, sharing: &[(u128, &str, &str)]) -> Allergy {
        Allergy {
            tag: tag.into(),
            label: label.into(),
            created_at: Utc.with_ymd_and_hms(2026, 11, 16, 10, 0, 0).unwrap(),
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

    fn level(tag: &str, name: &str, share: ShareLevel) -> Level {
        Level {
            tag: tag.into(),
            name: name.into(),
            share,
        }
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // -- where a member goes ----------------------------------------------

    #[test]
    fn an_arrival_reads_from_its_code_only() {
        assert_eq!(Arrival::parse("created"), Some(Arrival::Created));
        assert_eq!(Arrival::parse("joined"), Some(Arrival::Joined));
        assert_eq!(Arrival::parse("Created"), None);
        assert_eq!(Arrival::parse(""), None);
    }

    #[test]
    fn with_an_allergy_the_member_is_asked_before_the_list() {
        assert_eq!(
            after_arrival(family(7), Arrival::Created, Some(true)),
            format!("/groups/{}/allergies?from=created", family(7))
        );
        assert_eq!(
            after_arrival(family(7), Arrival::Joined, Some(true)),
            format!("/groups/{}/allergies?from=joined", family(7))
        );
    }

    #[test]
    fn without_an_allergy_there_is_no_screen() {
        assert_eq!(
            after_arrival(family(7), Arrival::Created, Some(false)),
            "/groups?notice=group_created"
        );
        assert_eq!(
            after_arrival(family(7), Arrival::Joined, Some(false)),
            "/groups?notice=joined"
        );
    }

    #[test]
    fn when_it_cannot_be_told_the_page_decides() {
        assert_eq!(
            after_arrival(family(7), Arrival::Joined, None),
            format!("/groups/{}/allergies?from=joined", family(7))
        );
    }

    // -- the levels in one family -----------------------------------------

    #[test]
    fn each_allergy_comes_with_its_level_in_this_family_only() {
        let allergies = [
            allergy(
                "en:peanuts",
                "Cacahuètes",
                &[(1, "Maison", "private"), (2, "Ailleurs", "public")],
            ),
            allergy(
                "en:cereals-and-their-products",
                "Céréales et dérivés",
                &[(1, "Maison", "anonymous"), (2, "Ailleurs", "private")],
            ),
        ];
        let (name, levels) = levels_in(&allergies, family(1)).unwrap();
        assert_eq!(name, "Maison");
        assert_eq!(
            levels,
            vec![
                level("en:peanuts", "Arachides", ShareLevel::Private),
                level(
                    "en:cereals-and-their-products",
                    "Gluten",
                    ShareLevel::Anonymous
                ),
            ]
        );
    }

    #[test]
    fn a_family_the_member_is_not_in_has_no_levels() {
        let allergies = [allergy(
            "en:peanuts",
            "Cacahuètes",
            &[(1, "Maison", "private")],
        )];
        assert_eq!(levels_in(&allergies, family(9)), None);
        assert_eq!(levels_in(&[], family(1)), None);
    }

    #[test]
    fn a_level_that_does_not_read_is_private() {
        let allergies = [allergy("en:kiwis", "Kiwis", &[(1, "Maison", "secret")])];
        let (_, levels) = levels_in(&allergies, family(1)).unwrap();
        assert_eq!(
            levels,
            vec![level("en:kiwis", "Kiwis", ShareLevel::Private)]
        );
    }

    // -- the form ---------------------------------------------------------

    #[test]
    fn the_form_reads_one_level_per_allergy() {
        assert_eq!(
            level_choices(&pairs(&[
                ("from", "created"),
                ("level.en:peanuts", "public"),
                ("level.en:gluten", "private"),
            ])),
            Ok(vec![
                ("en:peanuts".to_string(), ShareLevel::Public),
                ("en:gluten".to_string(), ShareLevel::Private),
            ])
        );
        assert_eq!(level_choices(&pairs(&[("from", "joined")])), Ok(vec![]));
    }

    #[test]
    fn a_forged_form_is_refused_whole() {
        for form in [
            vec![
                ("level.en:peanuts", "public"),
                ("level.en:gluten", "secret"),
            ],
            vec![("level.peanuts", "public")],
            vec![("level.", "public")],
            vec![
                ("level.en:peanuts", "public"),
                ("level.en:peanuts", "private"),
            ],
        ] {
            assert_eq!(
                level_choices(&pairs(&form)),
                Err("invalid_share"),
                "{form:?}"
            );
        }
    }

    #[test]
    fn only_the_levels_that_change_are_written() {
        let levels = [
            level("en:peanuts", "Arachides", ShareLevel::Private),
            level("en:gluten", "Gluten", ShareLevel::Anonymous),
            level("en:eggs", "Œufs", ShareLevel::Private),
        ];
        assert_eq!(
            changes(
                &levels,
                &[
                    ("en:peanuts".into(), ShareLevel::Public),
                    ("en:gluten".into(), ShareLevel::Anonymous),
                    ("en:eggs".into(), ShareLevel::Private),
                ]
            ),
            vec![("en:peanuts".to_string(), ShareLevel::Public)]
        );
    }

    #[test]
    fn validating_everything_private_writes_nothing() {
        let levels = [
            level("en:peanuts", "Arachides", ShareLevel::Private),
            level("en:eggs", "Œufs", ShareLevel::Private),
        ];
        let all_private: Vec<(String, ShareLevel)> = levels
            .iter()
            .map(|l| (l.tag.clone(), ShareLevel::Private))
            .collect();
        assert_eq!(changes(&levels, &all_private), vec![]);
    }

    #[test]
    fn a_tag_no_longer_held_is_not_written() {
        let levels = [level("en:peanuts", "Arachides", ShareLevel::Private)];
        assert_eq!(
            changes(&levels, &[("en:kiwis".into(), ShareLevel::Public)]),
            vec![]
        );
    }

    // -- the page ---------------------------------------------------------

    #[test]
    fn every_allergy_is_offered_three_levels_private_checked_by_default() {
        let levels = [
            level("en:peanuts", "Arachides", ShareLevel::Private),
            level("en:gluten", "Gluten", ShareLevel::Private),
        ];
        let html = page_body(family(1), "Maison", Some(Arrival::Created), &levels, None);
        assert!(
            html.contains("<h1>Partager vos allergies avec Maison</h1>"),
            "{html}"
        );
        assert_eq!(html.matches(r#"type="radio""#).count(), 6);
        assert_eq!(html.matches(" checked").count(), 2);
        assert!(html.contains(
            r#"<input type="radio" name="level.en:peanuts" value="private" checked>Privé"#
        ));
        assert!(
            html.contains(r#"<input type="radio" name="level.en:peanuts" value="public">Public"#)
        );
        assert!(html.contains(r#"<input type="hidden" name="from" value="created">"#));
        assert!(html.contains(&format!(r#"action="/groups/{}/allergies""#, family(1))));
        assert!(html.contains("Rien n'est partagé tant que vous n'avez pas validé"));
        assert!(html.contains(r#"<a class="btn secondary" href="/groups?notice=group_created">Plus tard, tout laisser privé</a>"#));
    }

    #[test]
    fn a_level_already_set_is_the_one_checked_and_nothing_claims_all_private() {
        let levels = [level("en:peanuts", "Arachides", ShareLevel::Public)];
        let html = page_body(family(1), "Maison", Some(Arrival::Joined), &levels, None);
        assert!(html.contains(r#"value="public" checked>"#));
        assert_eq!(html.matches(" checked").count(), 1);
        assert!(!html.contains("tout reste privé"));
        assert!(!html.contains("tout laisser privé"));
        assert!(html.contains(r#"href="/groups?notice=joined">Plus tard</a>"#));
        assert!(html.contains("<p>Vous venez de rejoindre Maison. Choisissez"));
    }

    #[test]
    fn the_unavailable_page_claims_nothing_about_the_levels() {
        // `post()` reaches it after a level may already be written.
        let Html(page) = unavailable_page();
        assert!(!page.contains("restent"), "{page}");
        assert!(page.contains("Vos allergies n'ont pas pu être lues."));
    }

    #[test]
    fn names_and_family_are_escaped() {
        let levels = [level("en:peanuts", "<b>", ShareLevel::Private)];
        let html = page_body(family(1), "A & <B>", None, &levels, None);
        assert!(html.contains("avec A &amp; &lt;B&gt;</h1>"));
        assert!(html.contains("<strong>&lt;b&gt;</strong>"));
        assert!(!html.contains(r#"name="from""#));
        assert!(html.contains(r#"href="/groups">"#));
    }

    #[test]
    fn a_failure_is_said_in_an_alert() {
        let levels = [level("en:peanuts", "Arachides", ShareLevel::Private)];
        let html = page_body(family(1), "Maison", None, &levels, Some("sharing_failed"));
        assert!(html.contains(
            r#"<p class="notice error" role="alert">Vos choix n'ont pas tous pu être enregistrés."#
        ));
        // Under the opening sentence, above the key, as in the mockup.
        let lead = html.find("Choisissez ce que").unwrap();
        let alert = html.find("notice error").unwrap();
        let key = html.find(r#"class="key""#).unwrap();
        assert!(lead < alert && alert < key, "{html}");
        let html = page_body(family(1), "Maison", None, &levels, Some("nope"));
        assert!(!html.contains("notice error"));
    }
}
