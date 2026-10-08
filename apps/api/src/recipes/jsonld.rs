//! The recipe a web page publishes as schema.org JSON-LD (#405): most
//! recipe sites (Marmiton, 750g, Journal des Femmes, CuisineAZ, blogs on
//! WordPress) put a `Recipe` node in a `<script type="application/ld+json">`
//! block, which is what the recipe-scrapers library reads on 400+ sites.
//! No HTML or JSON-LD library: the blocks are found by hand, read with
//! `serde_json`, and only the three fields the import keeps are looked at.

use serde_json::Value;

/// What a page's `Recipe` node gives: its name, its ingredient lines as
/// published, and its steps — all as plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedRecipe {
    pub name: String,
    pub ingredients: Vec<String>,
    pub steps: Vec<String>,
}

/// The first `Recipe` node of the page's JSON-LD blocks that has a name,
/// an ingredient or a step, or `None` when there is none.
pub fn extract_recipe(html: &str) -> Option<ExtractedRecipe> {
    json_ld_blocks(html)
        .into_iter()
        .filter_map(parse_block)
        .find_map(|value| {
            let mut nodes = Vec::new();
            collect_recipes(&value, &mut nodes);
            nodes.into_iter().find_map(read_recipe)
        })
}

/// The contents of every `<script type="application/ld+json">` element,
/// in page order. Tag and attribute names in any case, attributes in any
/// order.
fn json_ld_blocks(html: &str) -> Vec<&str> {
    // ASCII lowercasing keeps every byte offset in place.
    let lower = html.to_ascii_lowercase();
    let mut blocks = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<script") {
        let tag_start = from + at + "<script".len();
        let Some(tag_len) = lower[tag_start..].find('>') else {
            break;
        };
        let attributes = &lower[tag_start..tag_start + tag_len];
        let content_start = tag_start + tag_len + 1;
        let Some(content_len) = lower[content_start..].find("</script") else {
            break;
        };
        if type_attribute(attributes).is_some_and(|t| t.trim() == "application/ld+json") {
            blocks.push(&html[content_start..content_start + content_len]);
        }
        from = content_start + content_len;
    }
    blocks
}

/// The value of a lowercased tag's `type` attribute, quoted or not.
fn type_attribute(attributes: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(at) = attributes[from..].find("type") {
        let start = from + at;
        from = start + "type".len();
        let preceded_by_space = attributes[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let Some(rest) = attributes[from..]
            .trim_start()
            .strip_prefix('=')
            .filter(|_| preceded_by_space)
        else {
            continue;
        };
        let rest = rest.trim_start();
        return Some(match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let value = &rest[1..];
                &value[..value.find(quote).unwrap_or(value.len())]
            }
            _ => &rest[..rest.find(char::is_whitespace).unwrap_or(rest.len())],
        });
    }
    None
}

/// A block as JSON. Some sites publish a literal line break or tab inside
/// a string, which JSON forbids: such a block is read again with every
/// control character turned into a space.
fn parse_block(block: &str) -> Option<Value> {
    serde_json::from_str(block).ok().or_else(|| {
        let spaced: String = block
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        serde_json::from_str(&spaced).ok()
    })
}

/// Every `Recipe` node in `value`, in document order: the value itself, an
/// array's items, and what `@graph` and `mainEntity` hold.
fn collect_recipes<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| collect_recipes(item, out)),
        Value::Object(node) => {
            if node.get("@type").is_some_and(is_recipe_type) {
                out.push(value);
            } else {
                for key in ["@graph", "mainEntity"] {
                    if let Some(inner) = node.get(key) {
                        collect_recipes(inner, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// `Recipe`, `schema:Recipe`, `http://schema.org/Recipe`, alone or in an
/// array of types.
fn is_recipe_type(t: &Value) -> bool {
    let named = |s: &str| s == "Recipe" || s.ends_with("/Recipe") || s.ends_with(":Recipe");
    match t {
        Value::String(s) => named(s),
        Value::Array(types) => types.iter().filter_map(Value::as_str).any(named),
        _ => false,
    }
}

fn read_recipe(node: &Value) -> Option<ExtractedRecipe> {
    let name = node
        .get("name")
        .and_then(Value::as_str)
        .map(plain_text)
        .unwrap_or_default();
    let lines: Vec<&str> = match node
        .get("recipeIngredient")
        .or_else(|| node.get("ingredients"))
    {
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(line)) => vec![line.as_str()],
        _ => Vec::new(),
    };
    let ingredients: Vec<String> = lines
        .into_iter()
        .map(plain_text)
        .filter(|l| !l.is_empty())
        .collect();
    let mut steps = Vec::new();
    if let Some(instructions) = node.get("recipeInstructions") {
        collect_steps(instructions, &mut steps);
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

/// `recipeInstructions` as steps: a text (one step per line), a list of
/// texts, of `HowToStep` (its `text`, else its `name`) or of
/// `HowToSection` (its steps, in order; the section's own name is not a
/// step).
fn collect_steps(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.extend(lines_of(text)),
        Value::Array(items) => items.iter().for_each(|item| collect_steps(item, out)),
        Value::Object(node) => {
            if let Some(inner) = node.get("itemListElement") {
                collect_steps(inner, out);
            } else if let Some(text) = ["text", "name"]
                .iter()
                .filter_map(|key| node.get(*key).and_then(Value::as_str))
                .map(plain_text)
                .find(|t| !t.is_empty())
            {
                out.push(text);
            }
        }
        _ => {}
    }
}

/// A text's non-blank lines, as plain text: a line ends at a line break or
/// at a `<br>`, `<p>`, `<li>` or `<div>` tag.
fn lines_of(text: &str) -> Vec<String> {
    strip_tags(text, true)
        .lines()
        .map(plain_text)
        .filter(|l| !l.is_empty())
        .collect()
}

/// `text` as plain text: tags dropped, HTML entities decoded, runs of
/// whitespace collapsed to one space, trimmed.
pub fn plain_text(text: &str) -> String {
    decode_entities(&strip_tags(text, false))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tags that end a line of text.
const BLOCK_TAGS: [&str; 6] = ["br", "p", "li", "div", "ol", "ul"];

/// `text` without its tags; a block tag becomes a line break when
/// `blocks_as_lines`. A `<` that does not open a tag (`4 < 5`) stays.
fn strip_tags(text: &str, blocks_as_lines: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let opens_tag = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        match after.find('>').filter(|_| opens_tag) {
            Some(end) => {
                let name = after[..end]
                    .trim_start_matches('/')
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_ascii_lowercase();
                if blocks_as_lines && BLOCK_TAGS.contains(&name.as_str()) {
                    out.push('\n');
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Named entities decoded besides the numeric ones: XML's five, the
/// no-break space, and the letters and signs of French text.
const NAMED_ENTITIES: [(&str, char); 39] = [
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{a0}'),
    ("agrave", 'à'),
    ("acirc", 'â'),
    ("ccedil", 'ç'),
    ("eacute", 'é'),
    ("egrave", 'è'),
    ("ecirc", 'ê'),
    ("euml", 'ë'),
    ("icirc", 'î'),
    ("iuml", 'ï'),
    ("ocirc", 'ô'),
    ("oelig", 'œ'),
    ("ugrave", 'ù'),
    ("ucirc", 'û'),
    ("uuml", 'ü'),
    ("Agrave", 'À'),
    ("Ccedil", 'Ç'),
    ("Eacute", 'É'),
    ("Egrave", 'È'),
    ("Ecirc", 'Ê'),
    ("OElig", 'Œ'),
    ("deg", '°'),
    ("laquo", '«'),
    ("raquo", '»'),
    ("lsquo", '‘'),
    ("rsquo", '’'),
    ("ldquo", '“'),
    ("rdquo", '”'),
    ("hellip", '…'),
    ("ndash", '–'),
    ("mdash", '—'),
    ("frac12", '½'),
    ("frac14", '¼'),
    ("frac34", '¾'),
];

/// `text` with its HTML entities decoded. An unknown or invalid one stays
/// as written.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let decoded = after
            .find(';')
            .filter(|&end| end <= 12)
            .and_then(|end| entity(&after[..end]).map(|c| (c, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse::<u32>().ok()?,
        };
        return char::from_u32(code);
    }
    NAMED_ENTITIES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/recipe_pages/",
                $name
            ))
        };
    }

    fn page(blocks: &[&str]) -> String {
        let scripts: String = blocks
            .iter()
            .map(|b| format!("<script type=\"application/ld+json\">{b}</script>\n"))
            .collect();
        format!("<!DOCTYPE html><html><head>{scripts}</head><body></body></html>")
    }

    // -- real pages (anonymized: only the recipe fields are kept) ----------

    #[test]
    fn marmiton_recipe_inside_a_graph() {
        let recipe = extract_recipe(fixture!("marmiton_graph.html")).unwrap();
        assert_eq!(recipe.name, "Pâte à crêpes simple : la meilleure recette");
        assert_eq!(
            recipe.ingredients,
            [
                "5 oeufs",
                "500 g de farine",
                "1 l de lait demi-écrémé",
                "3 cuillères à soupe d'huile",
                "0.5 verre de bière",
            ]
        );
        assert_eq!(recipe.steps.len(), 3);
        assert_eq!(
            recipe.steps[1], "Enfin rajouter l'huile et la bière.",
            "trailing space trimmed"
        );
    }

    #[test]
    fn entities_in_750g_steps_are_decoded() {
        let recipe = extract_recipe(fixture!("750g_entities.html")).unwrap();
        assert_eq!(recipe.name, "Bento craquant");
        assert_eq!(recipe.ingredients.len(), 37);
        assert_eq!(recipe.ingredients[1], "1 poignée de haricots verts");
        assert_eq!(recipe.steps.len(), 5);
        assert!(
            recipe.steps[0].contains("dans de l'eau bouillante"),
            "{}",
            recipe.steps[0]
        );
        assert!(recipe.steps.iter().all(|s| !s.contains("&#039;")));
    }

    #[test]
    fn journal_des_femmes_recipe_in_a_top_level_array_among_two_blocks() {
        let recipe = extract_recipe(fixture!("jdf_array_two_blocks.html")).unwrap();
        assert_eq!(recipe.name, "Crêpes : la meilleure recette rapide");
        assert_eq!(recipe.ingredients[2], "½ litre lait");
        assert_eq!(recipe.steps.len(), 7);
    }

    #[test]
    fn cuisineaz_steps_take_their_text_not_their_name() {
        let recipe = extract_recipe(fixture!("cuisineaz_steps_named.html")).unwrap();
        assert_eq!(recipe.name, "Rougail saucisses facile minute");
        assert_eq!(recipe.steps.len(), 3);
        assert!(recipe.steps[0].starts_with("Piquez les saucisses"));
    }

    #[test]
    fn a_page_without_a_recipe_node_gives_none() {
        assert_eq!(extract_recipe(fixture!("no_recipe.html")), None);
        assert_eq!(extract_recipe("<html><body>Rien ici</body></html>"), None);
        assert_eq!(extract_recipe(""), None);
    }

    // -- shapes (constructed) ----------------------------------------------

    #[test]
    fn type_given_as_an_array_or_with_a_prefix() {
        for t in [
            r#"["Recipe", "NewsArticle"]"#,
            r#""schema:Recipe""#,
            r#""http://schema.org/Recipe""#,
        ] {
            let html = page(&[&format!(
                r#"{{"@type": {t}, "name": "Soupe", "recipeIngredient": ["1 poireau"]}}"#
            )]);
            let recipe = extract_recipe(&html).unwrap_or_else(|| panic!("{t}"));
            assert_eq!(recipe.name, "Soupe");
        }
    }

    #[test]
    fn a_recipe_under_main_entity_is_found() {
        let html = page(&[
            r#"{"@type": "WebPage", "mainEntity": {"@type": "Recipe", "name": "Tarte", "recipeInstructions": "Cuire."}}"#,
        ]);
        assert_eq!(extract_recipe(&html).unwrap().steps, ["Cuire."]);
    }

    #[test]
    fn the_first_recipe_wins_and_a_broken_block_is_skipped() {
        let html = page(&[
            r#"{"@type": "Recipe", "name": "Cassé", "#,
            r#"{"@type": "Organization", "name": "Blog"}"#,
            r#"[{"@type": "Recipe", "name": "Première"}, {"@type": "Recipe", "name": "Seconde"}]"#,
        ]);
        assert_eq!(extract_recipe(&html).unwrap().name, "Première");
    }

    #[test]
    fn raw_newlines_inside_strings_do_not_lose_the_block() {
        // Invalid JSON that some sites publish: a literal line break inside
        // a string.
        let html = page(&["{\"@type\": \"Recipe\", \"name\": \"Gratin\n dauphinois\"}"]);
        assert_eq!(extract_recipe(&html).unwrap().name, "Gratin dauphinois");
    }

    #[test]
    fn script_tag_matching_ignores_case_and_extra_attributes() {
        let html = r#"<SCRIPT class="yoast" Type = 'application/ld+json' data-x="1">{"@type":"Recipe","name":"Flan"}</SCRIPT>
<script type="text/javascript">var x = {"@type":"Recipe","name":"Pas lui"};</script>"#;
        assert_eq!(extract_recipe(html).unwrap().name, "Flan");
        let only_js = r#"<script>{"@type":"Recipe","name":"Pas lui"}</script>"#;
        assert_eq!(extract_recipe(only_js), None);
    }

    #[test]
    fn instructions_as_one_text_are_split_on_line_breaks() {
        let html = page(&[
            r#"{"@type": "Recipe", "name": "Riz", "recipeInstructions": "Rincer le riz.\nCuire 12 min.<br>Servir.<p>Bon appétit</p>"}"#,
        ]);
        assert_eq!(
            extract_recipe(&html).unwrap().steps,
            ["Rincer le riz.", "Cuire 12 min.", "Servir.", "Bon appétit"]
        );
    }

    #[test]
    fn instructions_as_a_list_of_strings_or_steps() {
        let html = page(&[
            r#"{"@type": "Recipe", "name": "Pâtes", "recipeInstructions": ["Bouillir l'eau.", {"@type": "HowToStep", "text": "Cuire <b>10</b> min."}, {"@type": "HowToStep", "name": "Égoutter."}, "  "]}"#,
        ]);
        assert_eq!(
            extract_recipe(&html).unwrap().steps,
            ["Bouillir l'eau.", "Cuire 10 min.", "Égoutter."]
        );
    }

    #[test]
    fn sections_are_flattened_into_their_steps() {
        let html = page(&[
            r#"{"@type": "Recipe", "name": "Tarte", "recipeInstructions": [
            {"@type": "HowToSection", "name": "La pâte", "itemListElement": [
                {"@type": "HowToStep", "text": "Mélanger farine et beurre."},
                {"@type": "HowToStep", "text": "Étaler."}
            ]},
            {"@type": "HowToSection", "name": "La garniture", "itemListElement": [
                {"@type": "HowToStep", "text": "Couper les pommes."}
            ]}
        ]}"#,
        ]);
        assert_eq!(
            extract_recipe(&html).unwrap().steps,
            [
                "Mélanger farine et beurre.",
                "Étaler.",
                "Couper les pommes."
            ]
        );
    }

    #[test]
    fn a_recipe_with_nothing_to_import_is_none() {
        let html = page(&[r#"{"@type": "Recipe", "name": "  ", "recipeIngredient": []}"#]);
        assert_eq!(extract_recipe(&html), None);
    }

    #[test]
    fn ingredient_lines_are_cleaned_and_blank_ones_dropped() {
        let html = page(&[
            r#"{"@type": "Recipe", "name": "Crème &amp; fruits", "recipeIngredient": [" 200&nbsp;g  de <i>fraises</i> ", "", "1 pot de cr&egrave;me", 42]}"#,
        ]);
        let recipe = extract_recipe(&html).unwrap();
        assert_eq!(recipe.name, "Crème & fruits");
        assert_eq!(recipe.ingredients, ["200 g de fraises", "1 pot de crème"]);
    }

    #[test]
    fn plain_text_decodes_entities_and_strips_tags() {
        assert_eq!(plain_text("l&#039;eau &amp; le sel"), "l'eau & le sel");
        assert_eq!(plain_text("l&#x27;huile&apos;"), "l'huile'");
        assert_eq!(plain_text("&lt;b&gt; reste du texte"), "<b> reste du texte");
        assert_eq!(plain_text("<p>Un\n\t deux</p>"), "Un deux");
        assert_eq!(plain_text("4 &lt; 5 &unknown; &"), "4 < 5 &unknown; &");
        assert_eq!(plain_text("&#9999999999;"), "&#9999999999;");
    }
}
