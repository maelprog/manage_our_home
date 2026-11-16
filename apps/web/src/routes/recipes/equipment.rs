//! The kitchen equipment of a recipe (#819): what it requires, ticked on its
//! form among the household's, and shown on its page, the equipment the
//! household lacks said in words. The household's list is set in the
//! family settings (`groups::settings`), by the owner and the admins.
//!
//! Codes and labels are `validation::dietary::EQUIPMENT`'s; apps/api
//! refuses any other code (`unknown_equipment`).

use manage_our_home_shared::validation::dietary::{equipment_choices, equipment_label, EQUIPMENT};
use uuid::Uuid;

use crate::app::html_escape;
use crate::state::{api_request_auth, AppState};

/// `GET /groups/:id/equipment`: the household's codes, `None` when it
/// could not be read.
pub(crate) async fn fetch_household(
    state: &AppState,
    gid: Uuid,
    cookie: Option<&str>,
) -> Option<Vec<String>> {
    let resp = api_request_auth(
        state,
        reqwest::Method::GET,
        &format!("/groups/{gid}/equipment"),
        cookie,
        None,
    )
    .await
    .ok()?;
    if resp.status != reqwest::StatusCode::OK {
        return None;
    }
    let codes = resp.body.get("codes")?.as_array()?;
    Some(
        codes
            .iter()
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect(),
    )
}

/// Equipment as pills: one `.badge` per code, in the order of `EQUIPMENT`
/// — the order of the form's boxes, not apps/api's, which sorts by code —,
/// a code outside the list left out. With the household's list, an
/// equipment it lacks is a `.badge.soon` that says so in words, not by its
/// colour alone.
pub(crate) fn badges(codes: &[String], household: Option<&[String]>) -> String {
    let pills: String = EQUIPMENT
        .iter()
        .filter(|(code, _)| codes.iter().any(|c| c == code))
        .map(|(code, label)| {
            let label = html_escape(label);
            let missing = household.is_some_and(|h| !h.iter().any(|c| c == code));
            if missing {
                format!(r#"<span class="badge soon">{label} · manquant dans cette famille</span>"#)
            } else {
                format!(r#"<span class="badge">{label}</span>"#)
            }
        })
        .collect();
    format!(r#"<p class="picks">{pills}</p>"#)
}

/// « Équipements requis » on a recipe's page.
pub(crate) fn required_section(required: &[String], household: Option<&[String]>) -> String {
    let body = if required.iter().any(|c| equipment_label(c).is_some()) {
        badges(required, household)
    } else {
        r#"<p class="muted">Aucun équipement requis.</p>"#.to_string()
    };
    format!(
        r#"<section class="card">
<h2>Équipements requis</h2>
{body}
</section>"#
    )
}

/// « Équipements requis » on a recipe's form: a box per equipment of the
/// household, and per one the recipe already requires that the household
/// lacks (said so beside it), so that saving does not drop it. `household`
/// is `None` when the list could not be read: the form then offers what is
/// ticked alone, and with nothing ticked says the list was not read.
/// Without anything to offer otherwise, the way to the settings.
pub(crate) fn fieldset(group_id: Uuid, household: Option<&[String]>, ticked: &[String]) -> String {
    let choices = equipment_choices(household.unwrap_or_default(), ticked);
    let body = if choices.is_empty() && household.is_none() {
        r#"<p class="notice error" role="alert">Les équipements de la famille n'ont pas pu être lus : rechargez la page pour en choisir.</p>"#
            .to_string()
    } else if choices.is_empty() {
        format!(
            r#"<p class="notice">La famille n'a encore déclaré aucun équipement. <a href="/groups/{group_id}/settings#equipements">Les déclarer dans les paramètres de la famille</a> (propriétaire ou admin).</p>"#
        )
    } else {
        let boxes: String = choices
            .iter()
            .map(|c| {
                let checked = if c.ticked { " checked" } else { "" };
                let lacking = if c.owned || household.is_none() {
                    ""
                } else {
                    " · manquant dans cette famille"
                };
                format!(
                    r#"<label class="field inline pick"><input type="checkbox" name="equipment" value="{code}"{checked}/>{label}{lacking}</label>"#,
                    code = c.code,
                    label = html_escape(c.label),
                )
            })
            .collect();
        format!(
            r#"<p class="muted">Facultatif. Choisissez parmi les équipements de la famille.</p>
<div class="picks">{boxes}</div>"#
        )
    };
    format!(
        r#"<fieldset class="card">
<legend>Équipements requis</legend>
{body}
</fieldset>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(list: &[&str]) -> Vec<String> {
        list.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn a_required_equipment_the_household_lacks_is_said_in_words() {
        let html = required_section(&codes(&["oven", "barbecue"]), Some(&codes(&["oven"])));
        assert!(html.contains("<h2>Équipements requis</h2>"), "{html}");
        assert!(
            html.contains(r#"<span class="badge">Four</span>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<span class="badge soon">Barbecue · manquant dans cette famille</span>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn without_the_households_list_nothing_is_said_missing() {
        let html = required_section(&codes(&["barbecue"]), None);
        assert!(
            html.contains(r#"<span class="badge">Barbecue</span>"#),
            "{html}"
        );
        assert!(!html.contains("manquant"), "{html}");
    }

    #[test]
    fn a_recipe_without_equipment_says_so() {
        for required in [codes(&[]), codes(&["teleporter"])] {
            let html = required_section(&required, Some(&[]));
            assert!(html.contains("Aucun équipement requis."), "{html}");
        }
    }

    #[test]
    fn the_form_offers_the_households_equipment_and_keeps_what_it_lacks() {
        let html = fieldset(
            Uuid::nil(),
            Some(&codes(&["oven", "hob"])),
            &codes(&["oven", "barbecue"]),
        );
        assert!(
            html.contains("<legend>Équipements requis</legend>"),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<input type="checkbox" name="equipment" value="oven" checked/>Four</label>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<input type="checkbox" name="equipment" value="hob"/>Plaques de cuisson</label>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"value="barbecue" checked/>Barbecue · manquant dans cette famille</label>"#
            ),
            "{html}"
        );
        assert_eq!(html.matches(r#"type="checkbox""#).count(), 3, "{html}");
    }

    #[test]
    fn a_household_without_equipment_is_pointed_to_its_settings() {
        let html = fieldset(Uuid::nil(), Some(&[]), &[]);
        assert!(!html.contains("checkbox"), "{html}");
        assert!(
            html.contains(&format!(
                r#"href="/groups/{}/settings#equipements""#,
                Uuid::nil()
            )),
            "{html}"
        );
    }

    #[test]
    fn the_pills_follow_the_lists_order_not_the_codes() {
        // apps/api returns codes sorted by code: « barbecue » before « oven ».
        let html = badges(&codes(&["barbecue", "hob", "oven"]), None);
        let four = html.find(">Four<").unwrap();
        let hob = html.find(">Plaques de cuisson<").unwrap();
        let bbq = html.find(">Barbecue<").unwrap();
        assert!(four < hob && hob < bbq, "{html}");
    }

    #[test]
    fn an_unread_household_list_with_nothing_ticked_says_it_was_not_read() {
        let html = fieldset(Uuid::nil(), None, &[]);
        assert!(!html.contains("n'a encore déclaré"), "{html}");
        assert!(
            html.contains(r#"<p class="notice error" role="alert">"#),
            "{html}"
        );
        assert!(html.contains("n'ont pas pu être lus"), "{html}");
        assert!(!html.contains("checkbox"), "{html}");
    }

    #[test]
    fn an_unread_household_list_keeps_what_is_ticked() {
        let html = fieldset(Uuid::nil(), None, &codes(&["oven"]));
        assert!(
            html.contains(r#"value="oven" checked/>Four</label>"#),
            "{html}"
        );
        assert!(!html.contains("manquant"), "{html}");
    }
}
