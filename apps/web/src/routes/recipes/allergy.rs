//! The allergy warning of a recipe (#825), as the pages say it: what the
//! API computes for the reader (#818), never more.
//!
//! A banner (`.allergy`, `role="status"`, the warning icon and words — never
//! the colour alone), non-blocking, one line per kind: the reader's own
//! allergies, one line per member who shares theirs publicly, that a member
//! of the family is allergic to an ingredient — neither which nor who —, and
//! the ingredients that could not be checked. A recipe with nothing to say
//! shows nothing: never a « sans allergène », since the absence of a warning
//! is no guarantee. The aim, « Aide uniquement… », follows the banners once.
//!
//! The suggestions and « Changer ce plat » carry the box « Éviter les
//! recettes contenant les allergènes de la famille », ticked by default;
//! unticked, `avoid_allergens=false` goes to the API for that request only,
//! and those pages show no banner at all (arbitration of 2026-11-17). The
//! recipe's page, the planning and « Ce soir » always show theirs.

use manage_our_home_shared::dto::recipes::AllergyWarning;
use manage_our_home_shared::validation::dietary::inco_label;

use crate::app::html_escape;
use crate::icons::{icon, Icon};

/// The sentence under the banners: they help, they do not vouch.
pub(crate) const AIM: &str = "Aide uniquement : ne remplace pas la lecture des étiquettes.";

/// The box's label.
pub(crate) const AVOID_LABEL: &str = "Éviter les recettes contenant les allergènes de la famille";

/// Whether the warning says anything.
pub(crate) fn says_something(w: &AllergyWarning) -> bool {
    !(w.mine.is_empty() && w.public.is_empty() && !w.anonymous && w.unverifiable.is_empty())
}

/// An allergen's name in a sentence: the INCO name the account page shows
/// (`account::allergies::name_of`), else the taxonomy's, else the tag;
/// its first letter in lower case.
fn allergen_name(tag: &str, label: &str) -> String {
    let name = inco_label(tag)
        .or((!label.is_empty()).then_some(label))
        .unwrap_or(tag);
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// « a », « a et b », « a, b et c ».
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} et {last}", init.join(", ")),
    }
}

/// The lines of the banner, in HTML, in this order: the reader's own
/// allergies, one line per member who shares theirs publicly (in the API's
/// order, by member), the anonymous one, the ingredients not checked.
pub(crate) fn lines(w: &AllergyWarning) -> Vec<String> {
    let mut lines = Vec::new();
    if !w.mine.is_empty() {
        let names: Vec<String> = w
            .mine
            .iter()
            .map(|m| html_escape(&allergen_name(&m.tag, &m.label)))
            .collect();
        lines.push(format!(
            "Contient {} : vous y êtes allergique.",
            and_list(&names)
        ));
    }
    let mut members: Vec<(&str, Vec<String>)> = Vec::new();
    for p in &w.public {
        let name = html_escape(&allergen_name(&p.tag, &p.label));
        match members.iter_mut().find(|(m, _)| *m == p.member) {
            Some((_, names)) => names.push(name),
            None => members.push((&p.member, vec![name])),
        }
    }
    for (member, names) in members {
        lines.push(format!(
            "{} est allergique à : {}.",
            html_escape(member),
            and_list(&names)
        ));
    }
    if w.anonymous {
        lines.push(
            "Un membre de la famille est allergique à un ingrédient de cette recette.".to_string(),
        );
    }
    if !w.unverifiable.is_empty() {
        let names: Vec<String> = w.unverifiable.iter().map(|n| html_escape(n)).collect();
        lines.push(format!(
            "Certains ingrédients n'ont pas pu être vérifiés pour les allergies : {}.",
            names.join(", ")
        ));
    }
    lines
}

/// The banner of a recipe, or nothing when the warning says nothing.
/// `dense` is the planning's, the offers' and « Ce soir »'s (`.allergy.sm`);
/// `aim` puts « Aide uniquement… » inside it, where it is the page's only
/// banner (the recipe's page, « Ce soir »).
pub(crate) fn banner(w: &AllergyWarning, dense: bool, aim: bool) -> String {
    let lines = lines(w);
    if lines.is_empty() {
        return String::new();
    }
    let items: String = lines.iter().map(|l| format!("<li>{l}</li>")).collect();
    format!(
        r#"<div class="allergy{sm}" role="status">{icon}<div><span class="sr-only">Avertissement d'allergie. </span><ul>{items}</ul>{aim}</div></div>"#,
        sm = if dense { " sm" } else { "" },
        icon = icon(Icon::Warning),
        aim = if aim {
            format!(r#"<p class="aim">{AIM}</p>"#)
        } else {
            String::new()
        },
    )
}

/// « Aide uniquement… » once for a page of several banners, or nothing
/// when `any` is false: no banner, no aim.
pub(crate) fn aim_line(any: bool) -> String {
    if any {
        format!(r#"<p class="allergy-aim">{AIM}</p>"#)
    } else {
        String::new()
    }
}

/// Whether the request asks to keep the family's allergens out: yes unless
/// the box's form was sent (`form`) without the box ticked (`ticked`). A
/// link that does not come from the form keeps them out.
pub(crate) fn avoid_from(form: Option<&str>, ticked: Option<&str>) -> bool {
    form.is_none() || ticked.is_some()
}

/// The box, in a `GET` form of its own to `action` with `hidden` fields:
/// `enhance.js` sends it as soon as it changes (`data-submit-on-change`);
/// without JavaScript, `button` does.
pub(crate) fn avoid_form(
    action: &str,
    hidden: &[(&str, &str)],
    avoid: bool,
    button: &str,
) -> String {
    let hidden: String = hidden
        .iter()
        .map(|(name, value)| {
            format!(
                r#"<input type="hidden" name="{name}" value="{}"/>"#,
                html_escape(value)
            )
        })
        .collect();
    format!(
        r#"<form class="avoid" method="get" action="{action}">{hidden}<input type="hidden" name="avoid_form" value="1"/><label class="field inline"><input type="checkbox" name="avoid_allergens" value="true" data-submit-on-change{checked}/> {AVOID_LABEL}</label><button type="submit" class="secondary">{button}</button></form>"#,
        action = html_escape(action),
        checked = if avoid { " checked" } else { "" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::dto::recipes::{AllergenMatch, PublicAllergyMatch};

    fn mine(tags: &[(&str, &str)]) -> Vec<AllergenMatch> {
        tags.iter()
            .map(|(tag, label)| AllergenMatch {
                tag: tag.to_string(),
                label: label.to_string(),
            })
            .collect()
    }

    fn public(member: &str, tag: &str, label: &str) -> PublicAllergyMatch {
        PublicAllergyMatch {
            member: member.into(),
            tag: tag.into(),
            label: label.into(),
        }
    }

    fn all_four() -> AllergyWarning {
        AllergyWarning {
            mine: mine(&[("en:milks", "Laits"), ("en:peanuts", "Cacahuètes")]),
            public: vec![
                public("Léa", "en:milks", "Laits"),
                public("Léa", "en:eggs", "Œufs"),
                public("Jonas", "en:peanuts", "Cacahuètes"),
            ],
            anonymous: true,
            unverifiable: vec!["sauce soja".into(), "vinaigre balsamique".into()],
        }
    }

    #[test]
    fn nothing_to_say_draws_nothing_and_never_a_free_from_badge() {
        let none = AllergyWarning::default();
        assert!(!says_something(&none));
        assert!(lines(&none).is_empty());
        assert_eq!(banner(&none, false, true), "");
        assert_eq!(banner(&none, true, false), "");
        assert_eq!(aim_line(false), "");
    }

    #[test]
    fn mine_names_every_allergen_by_its_nature() {
        let w = AllergyWarning {
            mine: mine(&[("en:milks", "Laits"), ("en:peanuts", "Cacahuètes")]),
            ..Default::default()
        };
        assert!(says_something(&w));
        // The INCO names first, as the account page says them.
        assert_eq!(
            lines(&w),
            ["Contient lait et arachides : vous y êtes allergique."]
        );
        // Another allergen: the taxonomy's name; none, the tag.
        let w = AllergyWarning {
            mine: mine(&[("en:kiwis", "Kiwis"), ("en:x", ""), ("en:eggs", "Œufs")]),
            ..Default::default()
        };
        assert_eq!(
            lines(&w),
            ["Contient kiwis, en:x et œufs : vous y êtes allergique."]
        );
    }

    #[test]
    fn public_says_one_line_per_member() {
        let w = AllergyWarning {
            public: vec![
                public("Léa", "en:milks", "Laits"),
                public("Léa", "en:eggs", "Œufs"),
                public("Jonas <3", "en:peanuts", "Cacahuètes"),
            ],
            ..Default::default()
        };
        assert_eq!(
            lines(&w),
            [
                "Léa est allergique à : lait et œufs.",
                "Jonas &lt;3 est allergique à : arachides.",
            ]
        );
    }

    #[test]
    fn anonymous_says_neither_which_nor_who() {
        let w = AllergyWarning {
            anonymous: true,
            ..Default::default()
        };
        assert_eq!(
            lines(&w),
            ["Un membre de la famille est allergique à un ingrédient de cette recette."]
        );
    }

    #[test]
    fn unverifiable_names_the_ingredients_not_checked() {
        let w = AllergyWarning {
            unverifiable: vec!["sauce soja".into(), "vinaigre <balsamique>".into()],
            ..Default::default()
        };
        assert_eq!(
            lines(&w),
            ["Certains ingrédients n'ont pas pu être vérifiés pour les allergies : sauce soja, vinaigre &lt;balsamique&gt;."]
        );
    }

    #[test]
    fn the_lines_come_mine_public_anonymous_unverifiable() {
        assert_eq!(
            lines(&all_four()),
            [
                "Contient lait et arachides : vous y êtes allergique.",
                "Léa est allergique à : lait et œufs.",
                "Jonas est allergique à : arachides.",
                "Un membre de la famille est allergique à un ingrédient de cette recette.",
                "Certains ingrédients n'ont pas pu être vérifiés pour les allergies : sauce soja, vinaigre balsamique.",
            ]
        );
    }

    #[test]
    fn the_banner_is_a_status_with_its_icon_its_words_and_its_aim() {
        let html = banner(&all_four(), false, true);
        assert!(
            html.starts_with(r#"<div class="allergy" role="status"><svg class="icon""#),
            "{html}"
        );
        assert!(html.contains(icon(Icon::Warning)), "{html}");
        assert!(
            html.contains(r#"<span class="sr-only">Avertissement d'allergie. </span>"#),
            "{html}"
        );
        assert_eq!(html.matches("<li>").count(), 5, "{html}");
        assert!(
            html.contains(&format!(r#"<p class="aim">{AIM}</p>"#)),
            "{html}"
        );
        assert!(!html.contains("role=\"alert\""), "{html}");
        // Dense, without the aim: the planning's, the offers'.
        let html = banner(&all_four(), true, false);
        assert!(
            html.starts_with(r#"<div class="allergy sm" role="status">"#),
            "{html}"
        );
        assert!(!html.contains(AIM), "{html}");
        assert_eq!(
            aim_line(true),
            format!(r#"<p class="allergy-aim">{AIM}</p>"#)
        );
    }

    #[test]
    fn an_anonymous_banner_carries_no_allergen_and_no_member() {
        // What another member shares anonymously reaches the page as a
        // boolean: the rendered string can hold neither which nor who.
        let html = banner(
            &AllergyWarning {
                anonymous: true,
                ..Default::default()
            },
            false,
            true,
        );
        for secret in ["arachide", "Cacahuètes", "en:", "Léa", "member", "data-"] {
            assert!(!html.contains(secret), "{secret} in {html}");
        }
    }

    #[test]
    fn no_text_says_free_from_allergens() {
        for html in [
            banner(&all_four(), false, true),
            aim_line(true),
            avoid_form("/recipes", &[], true, "Appliquer"),
        ] {
            for word in ["sans allergène", "Sans allergène", "garanti"] {
                assert!(!html.contains(word), "{word} in {html}");
            }
        }
    }

    #[test]
    fn the_box_avoids_unless_its_form_came_unticked() {
        // No form: a plain link to the page, the default.
        assert!(avoid_from(None, None));
        assert!(avoid_from(None, Some("true")));
        // The form, ticked or not.
        assert!(avoid_from(Some("1"), Some("true")));
        assert!(!avoid_from(Some("1"), None));
    }

    #[test]
    fn the_box_is_ticked_by_default_and_sends_itself() {
        let html = avoid_form("/recipes", &[("vue", "possibles")], true, "Appliquer");
        assert!(
            html.starts_with(r#"<form class="avoid" method="get" action="/recipes">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="vue" value="possibles"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="hidden" name="avoid_form" value="1"/>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="checkbox" name="avoid_allergens" value="true" data-submit-on-change checked/> Éviter les recettes contenant les allergènes de la famille</label>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit" class="secondary">Appliquer</button>"#),
            "{html}"
        );
        let html = avoid_form(
            "/recipes/x/replace",
            &[("back", "week@2026-11-27")],
            false,
            "Actualiser",
        );
        assert!(!html.contains(" checked"), "{html}");
        assert!(html.contains(r#"value="week@2026-11-27""#), "{html}");
    }
}
