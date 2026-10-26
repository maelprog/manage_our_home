//! The recipe a page marks up as schema.org microdata (#475): an element
//! `itemscope itemtype=".../Recipe"` whose descendants carry `itemprop`
//! — what recipe plugins emitted before JSON-LD took over, and what some
//! sites still publish alone. The import reads it when the page has no
//! JSON-LD recipe (`jsonld`), and keeps the same three fields.

use crate::recipes::html::{attribute, Document, Token};
use crate::recipes::jsonld::{names_recipe, ExtractedRecipe};

/// The first `Recipe` item of the page that has a name, an ingredient or a
/// step, or `None` when there is none. An item inside another one is not
/// read on its own: the page is walked once, each item's extent skipped
/// once read, so that items nested in each other's properties are not
/// read again for each of them.
pub fn extract_recipe(html: &str) -> Option<ExtractedRecipe> {
    let document = Document::parse(html);
    let mut at = 0;
    while at < document.tokens.len() {
        if is_recipe_item(&document.tokens[at]) {
            if let Some(recipe) = read_item(&document, at) {
                return Some(recipe);
            }
            at = document.end_of(at).max(at);
        }
        at += 1;
    }
    None
}

/// An element `itemscope` whose `itemtype` lists `Recipe`.
fn is_recipe_item(token: &Token) -> bool {
    let Token::Start { attributes, .. } = token else {
        return false;
    };
    attribute(attributes, "itemscope").is_some()
        && attribute(attributes, "itemtype")
            .is_some_and(|types| types.split_whitespace().any(names_recipe))
}

/// The recipe's name, ingredients and steps: the properties of the item
/// started at `item`. A property's value is its `content` attribute, else
/// its text. What a property element holds is not looked into further,
/// nor what a nested item holds (an author's `name` is not the recipe's).
fn read_item(document: &Document, item: usize) -> Option<ExtractedRecipe> {
    let mut name = String::new();
    let mut ingredients = Vec::new();
    let mut steps = Vec::new();
    let inside = document.inside(item);
    let mut at = inside.start;
    while at < inside.end {
        let Token::Start { attributes, .. } = &document.tokens[at] else {
            at += 1;
            continue;
        };
        let properties = attribute(attributes, "itemprop");
        if properties.is_none() && attribute(attributes, "itemscope").is_none() {
            at += 1;
            continue;
        }
        let content = attribute(attributes, "content");
        for property in properties.iter().flat_map(|p| p.split_whitespace()) {
            match property {
                "name" if name.is_empty() => {
                    name = content.clone().unwrap_or_else(|| document.text(at));
                }
                "recipeIngredient" | "ingredients" => {
                    let line = content.clone().unwrap_or_else(|| document.text(at));
                    if !line.is_empty() {
                        ingredients.push(line);
                    }
                }
                "recipeInstructions" => match &content {
                    Some(text) if !text.is_empty() => steps.push(text.clone()),
                    _ => steps.extend(document.lines(at)),
                },
                _ => {}
            }
        }
        at = document.end_of(at).max(at) + 1;
    }
    if name.is_empty() && ingredients.is_empty() && steps.is_empty() {
        return None;
    }
    Some(ExtractedRecipe {
        name,
        ingredients,
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/recipe_pages/microdata_recipe.html"
    ));

    #[test]
    fn a_microdata_recipe_gives_its_name_ingredients_and_steps() {
        let recipe = extract_recipe(FIXTURE).unwrap();
        assert_eq!(
            recipe.name, "Clafoutis aux cerises",
            "not the author's name"
        );
        assert_eq!(
            recipe.ingredients,
            [
                "500 g de cerises",
                "3 oeufs",
                "100 g de sucre",
                "25 cl de lait"
            ]
        );
        assert_eq!(
            recipe.steps,
            [
                "Préchauffer le four à 180 °C.",
                "Battre les oeufs avec le sucre, puis ajouter le lait.",
                "Verser sur les cerises et cuire 35 minutes.",
            ]
        );
    }

    #[test]
    fn a_property_in_a_nested_item_is_not_the_recipe_s() {
        let html = r#"<div itemscope itemtype="http://schema.org/Recipe">
            <div itemscope itemtype="http://schema.org/Person"><span itemprop="name">Auteur</span></div>
            <span itemprop="recipeIngredient">1 poireau</span>
        </div>"#;
        let recipe = extract_recipe(html).unwrap();
        assert_eq!(recipe.name, "");
        assert_eq!(recipe.ingredients, ["1 poireau"]);
    }

    #[test]
    fn a_content_attribute_is_the_value() {
        let html = r#"<div itemscope itemtype="https://schema.org/Recipe">
            <meta itemprop="name" content="Soupe &amp; pain">
            <span itemprop="recipeIngredient" content="2 carottes">deux carottes</span>
        </div>"#;
        let recipe = extract_recipe(html).unwrap();
        assert_eq!(recipe.name, "Soupe & pain");
        assert_eq!(recipe.ingredients, ["2 carottes"]);
    }

    #[test]
    fn the_type_may_be_one_of_several_and_written_in_any_case() {
        let html = r#"<DIV ItemScope ItemType="https://schema.org/NewsArticle https://schema.org/Recipe">
            <h1 ItemProp="name">Flan</h1></DIV>"#;
        assert_eq!(extract_recipe(html).unwrap().name, "Flan");
    }

    #[test]
    fn the_first_recipe_with_something_wins() {
        let html = r#"<div itemscope itemtype="https://schema.org/Recipe"></div>
            <div itemscope itemtype="https://schema.org/Recipe"><b itemprop="name">Seconde</b></div>"#;
        assert_eq!(extract_recipe(html).unwrap().name, "Seconde");
    }

    #[test]
    fn a_page_without_a_recipe_item_gives_none() {
        let other = r#"<div itemscope itemtype="https://schema.org/Article">
            <span itemprop="name">Article</span>
            <span itemprop="recipeIngredient">1 poireau</span></div>"#;
        assert_eq!(extract_recipe(other), None);
        let not_a_scope =
            r#"<div itemtype="https://schema.org/Recipe"><span itemprop="name">X</span></div>"#;
        assert_eq!(extract_recipe(not_a_scope), None);
        assert_eq!(extract_recipe(""), None);
        let jsonld_only = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/recipe_pages/marmiton_graph.html"
        ));
        assert_eq!(extract_recipe(jsonld_only), None);
    }

    #[test]
    fn an_empty_recipe_item_gives_none() {
        let html = r#"<div itemscope itemtype="https://schema.org/Recipe"><span itemprop="name"> </span></div>"#;
        assert_eq!(extract_recipe(html), None);
    }

    /// Recipe items nested as deep as a page at the size limit allows,
    /// bare or each inside an empty property of the one around it: every
    /// property reads the rest of the page, so reading it again for each
    /// item would be quadratic.
    #[test]
    fn nested_items_are_read_in_linear_time() {
        let item = r#"<div itemscope itemtype="https://schema.org/Recipe">"#;
        for property in ["", "recipeInstructions", "recipeIngredient", "name"] {
            let unit = if property.is_empty() {
                item.to_string()
            } else {
                format!(r#"{item}<div itemprop="{property}">"#)
            };
            let html = unit.repeat(crate::recipes::import::MAX_PAGE_BYTES / unit.len());
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                assert_eq!(extract_recipe(&html), None);
                let _ = done.send(());
            });
            finished
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_else(|_| {
                    panic!("{property:?}: still running past the limit: not linear")
                });
        }
    }
}
