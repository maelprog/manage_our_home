//! The recipe a page writes in prose (#475), when it publishes neither
//! JSON-LD (`jsonld`) nor microdata (`microdata`): a blog article with a
//! heading « Ingrédients » over a list, then « Préparation » over the
//! steps. A guess, so a narrow one: it gives a recipe only when it finds
//! at least one ingredient, and the member reviews every line of it.
//!
//! - The name is the page's first `<h1>`, else its `og:title`.
//! - A section starts at a heading (`<h1>`–`<h6>`) or at a paragraph whose
//!   whole text is the label (`<p><strong>Ingrédients :</strong></p>`). A
//!   heading's section runs to the next heading of the same level or
//!   above, so that it keeps its sub-headings; a paragraph's, to the next
//!   heading or label.
//! - Ingredients: the section's list items, or — without a list — its
//!   lines that start with a quantity or read « nom : quantité ».
//! - Steps: the section's lines of text, its sub-headings left out.

use crate::recipes::html::{attribute, heading_level, Document, Token};
use crate::recipes::ingredient_line;
use crate::recipes::jsonld::ExtractedRecipe;

/// The words a section's heading starts with.
const INGREDIENT_LABELS: [&str; 2] = ["ingrédients", "ingredients"];
const STEP_LABELS: [&str; 5] = [
    "préparation",
    "preparation",
    "étapes",
    "etapes",
    "instructions",
];

/// A label longer than this, in tokens or in bytes of text, is not looked
/// at: a heading is short, and a long paragraph is not a label.
const LABEL_MAX_TOKENS: usize = 16;
const LABEL_MAX_LEN: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ingredients,
    Steps,
}

/// Where a section may start or stop: a heading, with the section it
/// names if any, or a paragraph that is a label.
#[derive(Debug, Clone, Copy)]
enum Mark {
    Heading { level: u8, kind: Option<Kind> },
    Label(Kind),
}

/// The recipe the page writes in prose, or `None` when no ingredient is
/// found under an « Ingrédients » heading.
pub fn extract_recipe(html: &str) -> Option<ExtractedRecipe> {
    let document = Document::parse(html);
    let marks = marks(&document);
    let mut ingredients = Vec::new();
    let mut steps = Vec::new();
    for (i, &(at, mark)) in marks.iter().enumerate() {
        let kind = match mark {
            Mark::Heading { kind, .. } => kind,
            Mark::Label(kind) => Some(kind),
        };
        let wanted = match kind {
            Some(Kind::Ingredients) => ingredients.is_empty(),
            Some(Kind::Steps) => steps.is_empty(),
            None => false,
        };
        if !wanted {
            continue;
        }
        let start = (document.end_of(at) + 1).min(document.tokens.len());
        let end = marks[i + 1..]
            .iter()
            .find(|(_, next)| ends_section(mark, *next))
            .map_or(document.tokens.len(), |&(next_at, _)| next_at)
            .max(start);
        match kind {
            Some(Kind::Ingredients) => ingredients = ingredients_in(&document, start..end),
            Some(Kind::Steps) => steps = document.lines_in(start..end),
            None => {}
        }
    }
    if ingredients.is_empty() {
        return None;
    }
    Some(ExtractedRecipe {
        name: name(&document),
        ingredients,
        steps,
    })
}

/// The page's headings and labels, in page order.
fn marks(document: &Document) -> Vec<(usize, Mark)> {
    let label = |at: usize| document.short_text(at, LABEL_MAX_TOKENS, LABEL_MAX_LEN);
    let kind_of = |text: &str, whole: bool| {
        [
            (Kind::Ingredients, &INGREDIENT_LABELS[..]),
            (Kind::Steps, &STEP_LABELS[..]),
        ]
        .into_iter()
        .find(|(_, labels)| {
            after_label(text, labels).is_some_and(|rest| {
                !whole
                    || rest
                        .trim_matches(|c: char| c == ':' || c.is_whitespace())
                        .is_empty()
            })
        })
        .map(|(kind, _)| kind)
    };
    let mut marks = Vec::new();
    for (at, token) in document.tokens.iter().enumerate() {
        let Token::Start { name, .. } = token else {
            continue;
        };
        if let Some(level) = heading_level(name) {
            let kind = label(at).and_then(|text| kind_of(&text, false));
            marks.push((at, Mark::Heading { level, kind }));
        } else if name == "p" {
            if let Some(kind) = label(at).and_then(|text| kind_of(&text, true)) {
                marks.push((at, Mark::Label(kind)));
            }
        }
    }
    marks
}

/// Whether `next` ends the section `mark` starts.
fn ends_section(mark: Mark, next: Mark) -> bool {
    match (mark, next) {
        (Mark::Heading { level, .. }, Mark::Heading { level: next, .. }) => next <= level,
        (Mark::Heading { .. }, Mark::Label(_)) => false,
        (Mark::Label(_), _) => true,
    }
}

/// The ingredients of a section: its list items, or without a list its
/// lines that give a quantity.
fn ingredients_in(document: &Document, range: std::ops::Range<usize>) -> Vec<String> {
    let items = document.leaf_items_in(range.clone());
    if !items.is_empty() {
        return items;
    }
    document
        .lines_in(range)
        .into_iter()
        .filter(|line| ingredient_line::parse_line(line).quantity.is_some())
        .collect()
}

/// The text of the page's first `<h1>`, else its `og:title`.
fn name(document: &Document) -> String {
    let heading = document
        .tokens
        .iter()
        .position(|token| matches!(token, Token::Start { name, .. } if name == "h1"))
        .map(|at| document.text(at))
        .filter(|text| !text.is_empty());
    heading
        .or_else(|| {
            document.tokens.iter().find_map(|token| match token {
                Token::Start { name, attributes } if name == "meta" => {
                    (attribute(attributes, "property").as_deref() == Some("og:title"))
                        .then(|| attribute(attributes, "content"))
                        .flatten()
                }
                _ => None,
            })
        })
        .unwrap_or_default()
}

/// What follows the label of `labels` a heading's text starts with —
/// whatever the case, after any leading number, sign or emoji
/// (`2. Préparation`, `🥕 Ingrédients pour 4`) — or `None` when it starts
/// with none of them.
fn after_label<'a>(text: &'a str, labels: &[&str]) -> Option<&'a str> {
    let text = text.trim_start_matches(|c: char| !c.is_alphabetic());
    let word_len = text
        .find(|c: char| !c.is_alphabetic())
        .unwrap_or(text.len());
    let word = text[..word_len].to_lowercase();
    labels.contains(&word.as_str()).then(|| &text[word_len..])
}

/// Whether a heading's text names a section of `labels`.
#[cfg(test)]
fn names_section(text: &str, labels: &[&str]) -> bool {
    after_label(text, labels).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WOOCHIENS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/recipe_pages/woochiens_prose.html"
    ));

    #[test]
    fn the_woochiens_article_gives_its_recipe() {
        let recipe = extract_recipe(WOOCHIENS).unwrap();
        assert_eq!(
            recipe.name,
            "Tajine de poulet aux épices : des saveurs orientales"
        );
        assert_eq!(recipe.ingredients.len(), 18, "{:#?}", recipe.ingredients);
        assert_eq!(
            recipe.ingredients[0],
            "Poulet entier ou morceaux : 1,2 kg (cuisses, pilons ou blanc selon préférence)"
        );
        assert_eq!(recipe.ingredients[12], "Bouillon de volaille : 250 ml");
        assert_eq!(
            recipe.ingredients[17],
            "Optionnel : légumes comme carottes, courgettes ou navets"
        );
        // The six paragraphs of the six numbered sub-headings, and the
        // two tips and the serving suggestion between them; not the
        // sub-headings, nor the « Astuces » section that follows.
        assert_eq!(recipe.steps.len(), 9, "{:#?}", recipe.steps);
        assert!(recipe.steps[0].starts_with("Commencez par nettoyer le poulet"));
        assert!(recipe.steps[8].starts_with("Accompagnement : Le tajine"));
        assert!(recipe.steps.iter().all(|s| !s.starts_with("1. ")));
    }

    #[test]
    fn a_paragraph_label_and_lines_without_a_list() {
        let html =
            "<html><head><meta property=\"og:title\" content=\"Pâtes au beurre\"></head><body>\
            <p>Une recette rapide.</p>\
            <p><strong>Ingrédients :</strong></p>\
            <p>Pour deux personnes :<br>200 g de pâtes<br>Beurre : 20 g<br>sel</p>\
            <p><b>Préparation</b></p>\
            <p>Cuire les pâtes.</p><p>Ajouter le beurre.</p>\
            </body></html>";
        let recipe = extract_recipe(html).unwrap();
        assert_eq!(recipe.name, "Pâtes au beurre", "og:title without an h1");
        assert_eq!(recipe.ingredients, ["200 g de pâtes", "Beurre : 20 g"]);
        assert_eq!(recipe.steps, ["Cuire les pâtes.", "Ajouter le beurre."]);
    }

    #[test]
    fn a_heading_s_section_keeps_its_sub_headings_and_stops_at_the_next() {
        let html = "<h1>Tarte</h1>\
            <h2>Ingrédients</h2>\
            <h3>Pour la pâte</h3><ul><li>250 g de farine</li></ul>\
            <h3>Pour la garniture</h3><ul><li>4 pommes</li></ul>\
            <h2>Matériel</h2><ul><li>1 moule</li></ul>\
            <h2>Instructions</h2><ol><li>Étaler la pâte.</li><li>Cuire.</li></ol>\
            <h2>Commentaires</h2><p>Super !</p>";
        let recipe = extract_recipe(html).unwrap();
        assert_eq!(recipe.name, "Tarte");
        assert_eq!(recipe.ingredients, ["250 g de farine", "4 pommes"]);
        assert_eq!(recipe.steps, ["Étaler la pâte.", "Cuire."]);
    }

    #[test]
    fn steps_are_optional() {
        let html = "<h1>Vinaigrette</h1><h2>Ingrédients</h2><ul><li>3 c. à soupe d'huile</li></ul>";
        let recipe = extract_recipe(html).unwrap();
        assert_eq!(recipe.ingredients, ["3 c. à soupe d'huile"]);
        assert!(recipe.steps.is_empty());
    }

    #[test]
    fn an_empty_ingredients_section_is_passed_over_for_a_later_one() {
        let html = "<h1>Soupe</h1><h2>Ingrédients</h2><p>Voir plus bas.</p>\
            <h2>Ingrédients de la soupe</h2><ul><li>1 poireau</li></ul>";
        assert_eq!(extract_recipe(html).unwrap().ingredients, ["1 poireau"]);
    }

    #[test]
    fn no_recognizable_section_gives_none() {
        for html in [
            "",
            "<html><body><h1>Mon voyage</h1><p>Il faisait beau.</p></body></html>",
            // A heading but nothing that reads as an ingredient.
            "<h1>Soupe</h1><h2>Ingrédients</h2><p>Des légumes de saison.</p>\
             <h2>Préparation</h2><p>Tout mixer.</p>",
            // A label in the middle of a sentence is not a heading.
            "<h1>Soupe</h1><p><strong>Ingrédients</strong> : 1 poireau, 2 carottes</p>",
            // Steps without ingredients.
            "<h1>Soupe</h1><h2>Préparation</h2><ol><li>1 heure de cuisson.</li></ol>",
        ] {
            assert_eq!(extract_recipe(html), None, "{html}");
        }
        let no_recipe = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/recipe_pages/no_recipe.html"
        ));
        assert_eq!(extract_recipe(no_recipe), None);
    }

    #[test]
    fn section_names() {
        for text in [
            "Ingrédients",
            "INGRÉDIENTS",
            "Ingredients:",
            "Ingrédients :",
            "Ingrédients pour 4 personnes",
            "2. Ingrédients",
            "🥕 Ingrédients",
        ] {
            assert!(names_section(text, &INGREDIENT_LABELS), "{text}");
        }
        for text in [
            "Les ingrédients",
            "Ingrédientsmagiques",
            "Temps de préparation et cuisson",
            "",
        ] {
            assert!(!names_section(text, &INGREDIENT_LABELS), "{text}");
        }
        assert!(names_section(
            "Étapes de préparation détaillées",
            &STEP_LABELS
        ));
        assert!(!names_section("Temps de préparation", &STEP_LABELS));
    }

    /// Sections as many as a page at the size limit holds.
    #[test]
    fn many_sections_are_read_in_linear_time() {
        for unit in [
            "<h2>Ingrédients</h2>",
            "<h6>Ingrédients</h6><h5>x</h5>",
            "<p>Ingrédients</p>",
        ] {
            let html = unit.repeat(crate::recipes::import::MAX_PAGE_BYTES / unit.len());
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                assert_eq!(extract_recipe(&html), None);
                let _ = done.send(());
            });
            finished
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("still running past the limit: not linear");
        }
    }
}
