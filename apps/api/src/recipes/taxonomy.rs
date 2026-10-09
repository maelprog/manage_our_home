//! Ingredient taxonomy for the recipe suggestions (#406).
//!
//! A recipe's ingredient and an article in stock are matched by category,
//! not only by name: "Lait demi-écrémé Lactel" in stock satisfies "lait" in
//! a recipe. The categories are Open Food Facts' (ODbL 1.0), the same tags a
//! scanned product carries in `off_products.categories_tags` (#402), so a
//! scanned article and a recipe land in one tag space. The data is
//! `apps/api/data/off-categories-fr.tsv.gz`, built by
//! `apps/api/data/build_off_categories.py` and embedded in the binary:
//! one line per category, its parents and its French names.
//!
//! Two rules, both pure:
//! - [`Taxonomy::canonical_ingredient`] maps a name to a tag. The name is
//!   normalized (case, `œ`/`æ`, plural `-s`/`-x` on each word, `de`/`d'`/
//!   `du`/`des` anywhere, a leading `le`/`la`/`les`/`l'`), then looked up
//!   among the French names, first with its accents, then without. The
//!   whole name must match: no prefix or substring guess, so a brand after
//!   the product name ("… Lactel") gives no tag rather than a wrong one.
//!   A name that two unrelated categories share is no tag; when one of the
//!   candidates is an ancestor of all the others, it is the tag.
//! - [`Taxonomy::satisfies`]: an article in stock satisfies a recipe's
//!   ingredient when its tag is the recipe's, or descends from it
//!   (`en:semi-skimmed-milks` satisfies `en:milks`), never the reverse.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::LazyLock;

/// A category of the taxonomy: an index into [`Taxonomy`]'s tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TagId(u32);

pub struct Taxonomy {
    ids: Vec<String>,
    by_id: HashMap<String, TagId>,
    parents: Vec<Vec<TagId>>,
    /// Normalized name, accents kept → tag.
    exact: HashMap<String, TagId>,
    /// Normalized name, accents folded → tag.
    folded: HashMap<String, TagId>,
}

/// The embedded data: Open Food Facts' categories with a French name, and
/// their ancestors (see `build_off_categories.py`).
const EMBEDDED_GZ: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/off-categories-fr.tsv.gz"
));

static EMBEDDED: LazyLock<Taxonomy> = LazyLock::new(|| {
    let mut tsv = String::new();
    flate2::read::GzDecoder::new(EMBEDDED_GZ)
        .read_to_string(&mut tsv)
        .expect("embedded taxonomy is valid gzipped UTF-8");
    Taxonomy::parse(&tsv)
});

/// The taxonomy embedded in the binary, decompressed and indexed on first use.
pub fn taxonomy() -> &'static Taxonomy {
    &EMBEDDED
}

/// Normalizes a name for lookup: see the module documentation.
/// `fold_accents` also maps accented letters to their base letter.
pub fn normalize(name: &str, fold_accents: bool) -> String {
    let mut text = String::with_capacity(name.len());
    for c in name.chars().flat_map(char::to_lowercase) {
        match c {
            'œ' => text.push_str("oe"),
            'æ' => text.push_str("ae"),
            _ if fold_accents => text.push(fold_accent(c)),
            _ => text.push(c),
        }
    }
    // Apostrophes (straight or typographic), hyphens and every other
    // non-alphanumeric character separate words: "d'olive" is "d" "olive".
    let mut words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !matches!(*w, "d" | "de" | "du" | "des"))
        .collect();
    while matches!(words.first(), Some(&("l" | "le" | "la" | "les"))) {
        words.remove(0);
    }
    words
        .iter()
        .map(|w| match w.strip_suffix(['s', 'x']) {
            Some(stem) if w.chars().count() > 3 => stem,
            _ => w,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn fold_accent(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
        'ç' => 'c',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'î' | 'ï' | 'í' | 'ì' => 'i',
        'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
        'ù' | 'û' | 'ü' | 'ú' => 'u',
        'ÿ' | 'ý' => 'y',
        'ñ' => 'n',
        _ => c,
    }
}

impl Taxonomy {
    /// Reads the format `build_off_categories.py` writes. Lines starting
    /// with `#` are comments; a parent the file does not define is ignored.
    pub fn parse(tsv: &str) -> Taxonomy {
        let rows: Vec<(&str, &str, &str)> = tsv
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                Some((fields.next()?, fields.next()?, fields.next().unwrap_or("")))
            })
            .collect();

        let ids: Vec<String> = rows.iter().map(|(id, _, _)| id.to_string()).collect();
        let by_id: HashMap<String, TagId> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), TagId(i as u32)))
            .collect();
        let parents: Vec<Vec<TagId>> = rows
            .iter()
            .map(|(_, parents, _)| {
                parents
                    .split(',')
                    .filter_map(|p| by_id.get(p).copied())
                    .collect()
            })
            .collect();

        let mut taxonomy = Taxonomy {
            ids,
            by_id,
            parents,
            exact: HashMap::new(),
            folded: HashMap::new(),
        };
        for fold in [false, true] {
            let mut candidates: HashMap<String, Vec<TagId>> = HashMap::new();
            for (i, (_, _, names)) in rows.iter().enumerate() {
                for name in names.split('|') {
                    let key = normalize(name, fold);
                    if key.is_empty() {
                        continue;
                    }
                    let tags = candidates.entry(key).or_default();
                    if !tags.contains(&TagId(i as u32)) {
                        tags.push(TagId(i as u32));
                    }
                }
            }
            let index: HashMap<String, TagId> = candidates
                .into_iter()
                .filter_map(|(key, tags)| taxonomy.common_ancestor_among(&tags).map(|t| (key, t)))
                .collect();
            if fold {
                taxonomy.folded = index;
            } else {
                taxonomy.exact = index;
            }
        }
        taxonomy
    }

    /// The one candidate every other candidate descends from, if any: a
    /// name shared by a category and its subcategories names the category.
    fn common_ancestor_among(&self, tags: &[TagId]) -> Option<TagId> {
        tags.iter()
            .copied()
            .find(|&a| tags.iter().all(|&t| self.satisfies(t, a)))
    }

    /// The tag for an Open Food Facts category id (`en:milks`), if the
    /// taxonomy has it.
    pub fn tag(&self, id: &str) -> Option<TagId> {
        self.by_id.get(id).copied()
    }

    /// The Open Food Facts id of a tag.
    pub fn id(&self, tag: TagId) -> &str {
        &self.ids[tag.0 as usize]
    }

    pub fn canonical_ingredient(&self, name: &str) -> Option<TagId> {
        self.exact
            .get(&normalize(name, false))
            .or_else(|| self.folded.get(&normalize(name, true)))
            .copied()
    }

    /// The tag and every category it descends from.
    pub fn ancestors_or_self(&self, tag: TagId) -> HashSet<TagId> {
        let mut seen = HashSet::from([tag]);
        let mut stack = vec![tag];
        while let Some(t) = stack.pop() {
            for &p in &self.parents[t.0 as usize] {
                if seen.insert(p) {
                    stack.push(p);
                }
            }
        }
        seen
    }

    pub fn satisfies(&self, stock_tag: TagId, recipe_tag: TagId) -> bool {
        stock_tag == recipe_tag || self.ancestors_or_self(stock_tag).contains(&recipe_tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# comment line
en:dairies\t\tProduits laitiers
en:milks\ten:dairies\tLaits|lait
en:semi-skimmed-milks\ten:milks,en:not-in-file\tLaits demi-écrémés
en:eggs\t\tŒufs
en:pastas\t\tPâtes alimentaires|pâtes
en:pate\t\tPâtés
en:olive-oils\t\tHuiles d'olive
en:hams\t\tJambons
en:diced-ham\ten:hams\tJambon|Dés de jambon
en:mackerel\t\tMaquereau
en:mackerels\t\tMaquereaux
en:creme-fraiche\t\tCrème fraiche
en:potatoes\t\tPommes de terre
";

    fn fixture() -> Taxonomy {
        Taxonomy::parse(FIXTURE)
    }

    fn tag(t: &Taxonomy, id: &str) -> TagId {
        t.tag(id).unwrap_or_else(|| panic!("{id} in fixture"))
    }

    fn canon(t: &Taxonomy, name: &str) -> Option<String> {
        t.canonical_ingredient(name)
            .map(|tag| t.id(tag).to_string())
    }

    #[test]
    fn normalize_folds_case_plurals_articles_and_ligatures() {
        assert_eq!(normalize("  Laits ", false), "lait");
        assert_eq!(normalize("Laits demi-écrémés", false), "lait demi écrémé");
        assert_eq!(normalize("Laits demi-écrémés", true), "lait demi ecreme");
        assert_eq!(normalize("Œufs", false), "oeuf");
        assert_eq!(normalize("Huiles d’olive", false), "huile olive");
        assert_eq!(normalize("huile d'olive", false), "huile olive");
        assert_eq!(normalize("Pommes de terre", false), "pomme terre");
        assert_eq!(normalize("Dés du jambon", false), "dés jambon");
        assert_eq!(normalize("Le lait", false), "lait");
        assert_eq!(normalize("l'ail", false), "ail");
        assert_eq!(normalize("Choux", false), "chou");
        // Words of three letters or fewer keep their final letter: "riz"
        // and "jus" are not plurals.
        assert_eq!(normalize("riz", false), "riz");
        assert_eq!(normalize("jus", false), "jus");
        assert_eq!(normalize("Crème fraîche", true), "creme fraiche");
        assert_eq!(normalize("", false), "");
        assert_eq!(normalize("de", false), "");
    }

    #[test]
    fn tag_lookup_by_id() {
        let t = fixture();
        assert_eq!(t.id(tag(&t, "en:milks")), "en:milks");
        assert_eq!(t.tag("en:unknown"), None);
        assert_eq!(t.tag("en:not-in-file"), None);
    }

    #[test]
    fn canonical_ingredient_matches_names_and_synonyms() {
        let t = fixture();
        assert_eq!(canon(&t, "lait").as_deref(), Some("en:milks"));
        assert_eq!(canon(&t, "LAITS").as_deref(), Some("en:milks"));
        assert_eq!(
            canon(&t, "Lait demi-écrémé").as_deref(),
            Some("en:semi-skimmed-milks")
        );
        assert_eq!(
            canon(&t, "lait demi ecreme").as_deref(),
            Some("en:semi-skimmed-milks")
        );
        assert_eq!(canon(&t, "oeufs").as_deref(), Some("en:eggs"));
        assert_eq!(canon(&t, "œuf").as_deref(), Some("en:eggs"));
        assert_eq!(canon(&t, "huile d'olive").as_deref(), Some("en:olive-oils"));
        assert_eq!(canon(&t, "pomme de terre").as_deref(), Some("en:potatoes"));
        assert_eq!(
            canon(&t, "Produits laitiers").as_deref(),
            Some("en:dairies")
        );
    }

    #[test]
    fn canonical_ingredient_tries_accents_before_folding_them() {
        let t = fixture();
        // "pâtes" and "pâtés" differ by their accents only: kept, they
        // tell the two apart.
        assert_eq!(canon(&t, "pâtes").as_deref(), Some("en:pastas"));
        assert_eq!(canon(&t, "Pâtés").as_deref(), Some("en:pate"));
        // Without accents the name could be either: no tag.
        assert_eq!(canon(&t, "pates"), None);
        // The taxonomy's spelling lacks an accent the user typed.
        assert_eq!(
            canon(&t, "crème fraîche").as_deref(),
            Some("en:creme-fraiche")
        );
    }

    #[test]
    fn canonical_ingredient_resolves_shared_names_only_through_ancestry() {
        let t = fixture();
        // "Jambon" names both en:hams (plural folded) and en:diced-ham,
        // which descends from it: the ancestor is the tag.
        assert_eq!(canon(&t, "jambon").as_deref(), Some("en:hams"));
        // Two unrelated categories: no tag.
        assert_eq!(canon(&t, "maquereau"), None);
    }

    #[test]
    fn canonical_ingredient_needs_the_whole_name() {
        let t = fixture();
        assert_eq!(canon(&t, "Lait demi-écrémé Lactel"), None);
        assert_eq!(canon(&t, "lait de coco"), None);
        assert_eq!(canon(&t, ""), None);
        assert_eq!(canon(&t, "  "), None);
        assert_eq!(canon(&t, "de"), None);
    }

    #[test]
    fn satisfies_is_equality_or_descent_never_the_reverse() {
        let t = fixture();
        let dairies = tag(&t, "en:dairies");
        let milks = tag(&t, "en:milks");
        let semi = tag(&t, "en:semi-skimmed-milks");
        let eggs = tag(&t, "en:eggs");
        assert!(t.satisfies(milks, milks));
        assert!(t.satisfies(semi, milks));
        assert!(t.satisfies(semi, dairies));
        assert!(!t.satisfies(milks, semi));
        assert!(!t.satisfies(dairies, milks));
        assert!(!t.satisfies(eggs, milks));
        assert!(!t.satisfies(milks, eggs));
    }

    #[test]
    fn ancestors_or_self_walks_every_parent() {
        let t = fixture();
        let semi = tag(&t, "en:semi-skimmed-milks");
        let expected: HashSet<TagId> = ["en:semi-skimmed-milks", "en:milks", "en:dairies"]
            .iter()
            .map(|id| tag(&t, id))
            .collect();
        assert_eq!(t.ancestors_or_self(semi), expected);
    }

    #[test]
    fn embedded_taxonomy_loads_and_answers_the_issue_example() {
        let t = taxonomy();
        let milks = t.tag("en:milks").expect("en:milks embedded");
        let semi = t.tag("en:semi-skimmed-milks").expect("embedded");
        assert_eq!(t.canonical_ingredient("lait"), Some(milks));
        assert_eq!(t.canonical_ingredient("Lait demi-écrémé"), Some(semi));
        assert!(t.satisfies(semi, milks));
        assert!(!t.satisfies(milks, semi));
        assert_eq!(
            t.canonical_ingredient("oeufs").map(|tag| t.id(tag)),
            Some("en:eggs")
        );
    }
}
