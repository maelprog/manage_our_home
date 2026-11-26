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
//!   `apps/api/data/ingredient-aliases-fr.tsv` (#901) adds French names
//!   the taxonomy lacks, under the same rules: an alias is one more name of
//!   its tag, so a name shared by unrelated categories stays no tag, and an
//!   alias to an ancestor of the name's category widens the name to it.
//! - [`Taxonomy::satisfies`]: an article in stock satisfies a recipe's
//!   ingredient when its tag is the recipe's, or descends from it
//!   (`en:semi-skimmed-milks` satisfies `en:milks`), never the reverse.
//!
//! A name the taxonomy does not know is still matched by its name
//! ([`same_ingredient_name`], #1023): normalized as above, without its
//! accents, so `tomme fermiere` finds `Tommes fermières`. When both sides
//! have a category, the category alone decides, so `pâtes` (`en:pastas`)
//! never finds `Pâtés` (`en:pate`) by their name without accents.

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
    /// Each tag's first French name, as written (#819).
    labels: Vec<String>,
    /// Each tag's names, normalized with their accents folded: what
    /// [`Taxonomy::search`] reads (#819).
    searchable: Vec<Vec<String>>,
}

/// The embedded data: Open Food Facts' categories with a French name, and
/// their ancestors (see `build_off_categories.py`).
const EMBEDDED_GZ: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/off-categories-fr.tsv.gz"
));

const EMBEDDED_ALIASES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/ingredient-aliases-fr.tsv"
));

/// An alias line that does not name its tag (see
/// [`Taxonomy::parse_with_aliases`]); `line` counts from 1.
#[derive(Debug, PartialEq, Eq)]
pub enum AliasError {
    /// No tab, or an empty name or tag.
    Malformed { line: usize },
    /// The taxonomy has no such tag: the alias is dropped.
    UnknownTag { line: usize, tag: String },
    /// The taxonomy already gives the name that tag.
    Redundant { line: usize },
    /// With the alias, the name, with its accents or without them,
    /// resolves to another tag, or to none (`resolved`: the first of the
    /// two that does).
    Ambiguous {
        line: usize,
        resolved: Option<String>,
    },
}

impl AliasError {
    fn line(&self) -> usize {
        match self {
            AliasError::Malformed { line }
            | AliasError::UnknownTag { line, .. }
            | AliasError::Redundant { line }
            | AliasError::Ambiguous { line, .. } => *line,
        }
    }
}

/// The embedded data, with the aliases of `ingredient-aliases-fr.tsv`. An
/// alias in error is not fatal here: a test checks the embedded file has
/// none.
static EMBEDDED: LazyLock<Taxonomy> =
    LazyLock::new(|| Taxonomy::parse_with_aliases(&embedded_tsv(), EMBEDDED_ALIASES).0);

fn embedded_tsv() -> String {
    let mut tsv = String::new();
    flate2::read::GzDecoder::new(EMBEDDED_GZ)
        .read_to_string(&mut tsv)
        .expect("embedded taxonomy is valid gzipped UTF-8");
    tsv
}

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

/// Whether two ingredient names designate one ingredient by their name
/// (#1023): the same name trimmed and lowercased, as always; or, when one
/// side at least has no category (`both_categorised` false), the same name
/// once normalized without its accents ([`normalize`]). When both sides
/// have a category, the category alone decides beyond the identical name.
/// An empty name, or one that normalizes to nothing (`de`), designates
/// nothing.
pub fn same_ingredient_name(a: &str, b: &str, both_categorised: bool) -> bool {
    let (a, b) = (a.trim().to_lowercase(), b.trim().to_lowercase());
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    if both_categorised {
        return false;
    }
    let a = normalize(&a, true);
    !a.is_empty() && a == normalize(&b, true)
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
        Self::parse_with_aliases(tsv, "").0
    }

    /// [`Taxonomy::parse`], plus the aliases of `ingredient-aliases-fr.tsv`
    /// (#901): one `<name> TAB <tag id>` per line, `#` for comments. An
    /// alias is one more French name of its tag, under the same rules: a
    /// name shared by unrelated categories is still no tag. Every alias that
    /// does not end up naming its tag is reported: a malformed line, a tag
    /// the taxonomy lacks (the alias is then dropped), a name the taxonomy
    /// already gives that tag, or a name that, once the alias joins it,
    /// resolves to another tag, or to none, with its accents or without
    /// them. An alias only joins those two keys, so an alias without error
    /// changes no other name.
    pub fn parse_with_aliases(tsv: &str, aliases: &str) -> (Taxonomy, Vec<AliasError>) {
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

        let labels = rows
            .iter()
            .map(|(_, _, names)| names.split('|').next().unwrap_or_default().to_string())
            .collect();
        let searchable = rows
            .iter()
            .map(|(_, _, names)| {
                names
                    .split('|')
                    .map(|name| normalize(name, true))
                    .filter(|key| !key.is_empty())
                    .collect()
            })
            .collect();
        let mut taxonomy = Taxonomy {
            ids,
            by_id,
            parents,
            exact: HashMap::new(),
            folded: HashMap::new(),
            labels,
            searchable,
        };
        let mut errors = Vec::new();
        // (line, name, tag) of each alias whose tag the taxonomy has.
        let mut valid: Vec<(usize, &str, TagId)> = Vec::new();
        for (i, line) in aliases.lines().enumerate() {
            let line_no = i + 1;
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, id)) = line.split_once('\t') else {
                errors.push(AliasError::Malformed { line: line_no });
                continue;
            };
            let id = id.trim();
            if normalize(name, false).is_empty() || id.is_empty() {
                errors.push(AliasError::Malformed { line: line_no });
                continue;
            }
            match taxonomy.tag(id) {
                Some(tag) => valid.push((line_no, name, tag)),
                None => errors.push(AliasError::UnknownTag {
                    line: line_no,
                    tag: id.to_string(),
                }),
            }
        }

        // What each alias's name resolves to without the aliases, accents
        // kept then folded, as `canonical_ingredient` reads it.
        let mut before: Vec<[Option<TagId>; 2]> = vec![[None; 2]; valid.len()];
        for fold in [false, true] {
            let mut candidates: HashMap<String, Vec<TagId>> = HashMap::new();
            fn add(candidates: &mut HashMap<String, Vec<TagId>>, key: String, tag: TagId) {
                let tags = candidates.entry(key).or_default();
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            for (i, (_, _, names)) in rows.iter().enumerate() {
                for name in names.split('|') {
                    let key = normalize(name, fold);
                    if !key.is_empty() {
                        add(&mut candidates, key, TagId(i as u32));
                    }
                }
            }
            for (j, (_, name, _)) in valid.iter().enumerate() {
                before[j][fold as usize] = candidates
                    .get(&normalize(name, fold))
                    .and_then(|tags| taxonomy.common_ancestor_among(tags));
            }
            for &(_, name, tag) in &valid {
                add(&mut candidates, normalize(name, fold), tag);
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

        for (j, &(line, name, tag)) in valid.iter().enumerate() {
            let [exact, folded] = before[j];
            if exact.or(folded) == Some(tag) {
                errors.push(AliasError::Redundant { line });
            } else {
                // Both keys the alias joins, accents kept and folded, must
                // now name its tag: otherwise a name that had a tag, or the
                // alias itself, lost it to the shared-name rule.
                let exact = taxonomy.exact.get(&normalize(name, false)).copied();
                let folded = taxonomy.folded.get(&normalize(name, true)).copied();
                let wrong = [exact, folded].into_iter().find(|&r| r != Some(tag));
                if let Some(resolved) = wrong {
                    errors.push(AliasError::Ambiguous {
                        line,
                        resolved: resolved.map(|t| taxonomy.id(t).to_string()),
                    });
                }
            }
        }
        errors.sort_by_key(AliasError::line);
        (taxonomy, errors)
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

    /// The tag's first French name, as the taxonomy writes it (#819).
    pub fn label(&self, tag: TagId) -> &str {
        &self.labels[tag.0 as usize]
    }

    /// At most `limit` tags whose French names match `query` (#819), the
    /// query read as a name is ([`normalize`], accents folded): first the
    /// tags one of whose names is the query, then those one of whose names
    /// starts with it, then those one of whose names has a word starting
    /// with it. A match inside a word is none. Within a rank, the shorter
    /// label first, then the label, then the id: the same query always
    /// answers the same list.
    pub fn search(&self, query: &str, limit: usize) -> Vec<TagId> {
        let query = normalize(query, true);
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let word_start = format!(" {query}");
        let mut found: Vec<(u8, TagId)> = self
            .searchable
            .iter()
            .enumerate()
            .filter_map(|(i, names)| {
                let rank = names
                    .iter()
                    .filter_map(|name| {
                        if *name == query {
                            Some(0)
                        } else if name.starts_with(&query) {
                            Some(1)
                        } else if name.contains(&word_start) {
                            Some(2)
                        } else {
                            None
                        }
                    })
                    .min()?;
                Some((rank, TagId(i as u32)))
            })
            .collect();
        found.sort_by(|(ra, a), (rb, b)| {
            let (la, lb) = (self.label(*a), self.label(*b));
            ra.cmp(rb)
                .then(la.chars().count().cmp(&lb.chars().count()))
                .then(la.cmp(lb))
                .then(self.id(*a).cmp(self.id(*b)))
        });
        found.into_iter().take(limit).map(|(_, tag)| tag).collect()
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

    // -- same_ingredient_name (#1023) --------------------------------------

    #[test]
    fn without_a_category_a_name_finds_its_accent_and_plural_variant() {
        assert!(same_ingredient_name(
            "tomme fermiere",
            "Tommes fermières",
            false
        ));
        assert!(same_ingredient_name(
            "Tommes fermières",
            "tomme fermiere",
            false
        ));
        assert!(same_ingredient_name("l'échalote", "Échalotes", false));
    }

    #[test]
    fn with_both_categories_the_name_without_accents_matches_nothing() {
        // `pâtes` is en:pastas, `Pâtés` en:pate: the category decides.
        assert!(!same_ingredient_name("pâtes", "Pâtés", true));
        assert!(same_ingredient_name("pâtes", "Pâtés", false));
    }

    #[test]
    fn the_same_name_lowercased_still_matches_as_before() {
        assert!(same_ingredient_name("Lait", " lait ", true));
        assert!(same_ingredient_name("Lait", " lait ", false));
        assert!(same_ingredient_name("Crème fraîche", "CRÈME FRAÎCHE", true));
    }

    #[test]
    fn different_names_do_not_match() {
        assert!(!same_ingredient_name("tomme", "comté", false));
        assert!(!same_ingredient_name("tomme fermiere", "tomme", false));
    }

    #[test]
    fn an_empty_name_matches_nothing() {
        assert!(!same_ingredient_name("", "", false));
        assert!(!same_ingredient_name("  ", "", true));
        assert!(!same_ingredient_name("", "lait", false));
        assert!(!same_ingredient_name("lait", " ", false));
        // A name of nothing but `de`/`du`/`des` or an article normalizes to
        // nothing: it is no name either.
        assert!(!same_ingredient_name("de", "du", false));
        assert!(!same_ingredient_name("les", "la", false));
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

    fn found<'t>(t: &'t Taxonomy, query: &str, limit: usize) -> Vec<&'t str> {
        t.search(query, limit)
            .into_iter()
            .map(|tag| t.id(tag))
            .collect()
    }

    #[test]
    fn a_label_is_the_first_french_name() {
        let t = fixture();
        assert_eq!(t.label(tag(&t, "en:milks")), "Laits");
        assert_eq!(t.label(tag(&t, "en:eggs")), "Œufs");
        assert_eq!(t.label(tag(&t, "en:potatoes")), "Pommes de terre");
    }

    #[test]
    fn search_ranks_a_whole_name_then_a_name_start_then_a_word_start() {
        let t = fixture();
        assert_eq!(
            found(&t, "lait", 10),
            vec!["en:milks", "en:semi-skimmed-milks", "en:dairies"]
        );
        assert_eq!(found(&t, "lait", 1), vec!["en:milks"]);
    }

    #[test]
    fn search_reads_the_query_as_a_name_is_read() {
        let t = fixture();
        assert_eq!(found(&t, "ŒUFS", 10), vec!["en:eggs"]);
        assert_eq!(found(&t, " oeuf ", 10), vec!["en:eggs"]);
        assert_eq!(found(&t, "terre", 10), vec!["en:potatoes"]);
        assert_eq!(found(&t, "creme fraiche", 10), vec!["en:creme-fraiche"]);
    }

    #[test]
    fn search_finds_nothing_for_an_empty_query_or_the_inside_of_a_word() {
        let t = fixture();
        assert!(found(&t, "", 10).is_empty());
        assert!(found(&t, "  de ", 10).is_empty());
        assert!(found(&t, "aits", 10).is_empty());
        assert!(found(&t, "licorne", 10).is_empty());
        assert!(found(&t, "lait", 0).is_empty());
    }

    fn with_aliases(aliases: &str) -> (Taxonomy, Vec<AliasError>) {
        Taxonomy::parse_with_aliases(FIXTURE, aliases)
    }

    #[test]
    fn an_alias_names_a_tag_the_taxonomy_has_no_name_for() {
        let (t, errors) = with_aliases(
            "# comment line\n\
             Lait de vache\ten:milks\n\
             \n\
             Œuf frais\ten:eggs\n\
             pâte fraîche\ten:pastas\n",
        );
        assert_eq!(errors, vec![]);
        assert_eq!(canon(&t, "lait de vache").as_deref(), Some("en:milks"));
        // The alias is read as a name is: plurals, case, ligatures, accents.
        assert_eq!(canon(&t, "Laits de vache").as_deref(), Some("en:milks"));
        assert_eq!(canon(&t, "OEUFS FRAIS").as_deref(), Some("en:eggs"));
        assert_eq!(canon(&t, "pâtes fraîches").as_deref(), Some("en:pastas"));
        assert_eq!(canon(&t, "pates fraiches").as_deref(), Some("en:pastas"));
        // Nothing else moves: the whole name still has to match.
        assert_eq!(canon(&t, "lait").as_deref(), Some("en:milks"));
        assert_eq!(canon(&t, "lait de vache Lactel"), None);
        assert_eq!(canon(&t, "lait de coco"), None);
        // `parse` alone knows no alias.
        assert_eq!(canon(&fixture(), "lait de vache"), None);
    }

    #[test]
    fn an_alias_may_widen_a_name_to_an_ancestor_of_its_category() {
        // The taxonomy's "Laits demi-écrémés" is en:semi-skimmed-milks; an
        // alias to en:milks, its ancestor, makes the name en:milks: a name
        // shared by a category and its subcategory names the category.
        let (t, errors) = with_aliases("lait demi-écrémé\ten:milks\n");
        assert_eq!(errors, vec![]);
        let milks = tag(&t, "en:milks");
        assert_eq!(t.canonical_ingredient("Laits demi-écrémés"), Some(milks));
        assert!(t.satisfies(tag(&t, "en:semi-skimmed-milks"), milks));
    }

    #[test]
    fn an_alias_to_a_tag_the_taxonomy_lacks_is_an_error_and_ignored() {
        let (t, errors) = with_aliases("licorne\ten:unicorns\n");
        assert_eq!(
            errors,
            vec![AliasError::UnknownTag {
                line: 1,
                tag: "en:unicorns".into()
            }]
        );
        assert_eq!(canon(&t, "licorne"), None);
    }

    #[test]
    fn an_alias_does_not_bypass_the_shared_name_rule() {
        // "Maquereau" already names two unrelated categories: an alias to
        // one of them leaves the name without a tag, and says so.
        let (t, errors) = with_aliases("maquereau\ten:mackerel\n");
        assert_eq!(
            errors,
            vec![AliasError::Ambiguous {
                line: 1,
                resolved: None
            }]
        );
        assert_eq!(canon(&t, "maquereau"), None);

        // A name the taxonomy gives to another, unrelated category: both
        // lose it, the alias does not win.
        let (t, errors) = with_aliases("Pâtés\ten:pastas\n");
        assert_eq!(
            errors,
            vec![AliasError::Ambiguous {
                line: 1,
                resolved: None
            }]
        );
        assert_eq!(canon(&t, "pâtés"), None);

        // Narrowing a name to a subcategory: the ancestor keeps it.
        let (t, errors) = with_aliases("lait\ten:semi-skimmed-milks\n");
        assert_eq!(
            errors,
            vec![AliasError::Ambiguous {
                line: 1,
                resolved: Some("en:milks".into())
            }]
        );
        assert_eq!(canon(&t, "lait").as_deref(), Some("en:milks"));

        // Two aliases giving one name to unrelated categories.
        let (t, errors) = with_aliases("beurre\ten:eggs\nbeurres\ten:milks\n");
        assert_eq!(
            errors,
            vec![
                AliasError::Ambiguous {
                    line: 1,
                    resolved: None
                },
                AliasError::Ambiguous {
                    line: 2,
                    resolved: None
                },
            ]
        );
        assert_eq!(canon(&t, "beurre"), None);
    }

    #[test]
    fn an_alias_that_takes_its_folded_spelling_from_another_tag_is_ambiguous() {
        // The accented spelling is new, but without accents it is the
        // taxonomy's "Crème fraiche": that name would lose its tag.
        let (t, errors) = with_aliases("crème fraîche\ten:eggs\n");
        assert_eq!(
            errors,
            vec![AliasError::Ambiguous {
                line: 1,
                resolved: None
            }]
        );
        assert_eq!(canon(&t, "creme fraiche"), None);
    }

    #[test]
    fn an_alias_the_taxonomy_already_answers_is_redundant() {
        let (t, errors) = with_aliases("Laits\ten:milks\noeuf\ten:eggs\n");
        assert_eq!(
            errors,
            vec![
                AliasError::Redundant { line: 1 },
                AliasError::Redundant { line: 2 }
            ]
        );
        assert_eq!(canon(&t, "lait").as_deref(), Some("en:milks"));
    }

    #[test]
    fn an_alias_the_taxonomy_answers_once_accents_are_folded_is_redundant() {
        // "creme fraiche" is not the taxonomy's spelling ("Crème fraiche"),
        // but the lookup without accents already finds en:creme-fraiche.
        let (_, errors) = with_aliases("creme fraiche\ten:creme-fraiche\n");
        assert_eq!(errors, vec![AliasError::Redundant { line: 1 }]);
    }

    #[test]
    fn a_malformed_alias_line_is_an_error() {
        let (_, errors) = with_aliases("sans tabulation\n\ten:milks\nde\ten:milks\nlait\t\n");
        assert_eq!(
            errors,
            vec![
                AliasError::Malformed { line: 1 },
                AliasError::Malformed { line: 2 },
                AliasError::Malformed { line: 3 },
                AliasError::Malformed { line: 4 },
            ]
        );
    }

    #[test]
    fn every_embedded_alias_names_an_existing_tag_and_takes_effect() {
        let (_, errors) = Taxonomy::parse_with_aliases(&embedded_tsv(), EMBEDDED_ALIASES);
        assert_eq!(errors, vec![]);
        let aliases = EMBEDDED_ALIASES
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .count();
        assert!(aliases >= 40, "{aliases} aliases");
    }

    #[test]
    fn embedded_creme_fraiche_is_satisfied_by_every_thick_cream() {
        let t = taxonomy();
        let fermented = t.tag("en:fermented-creams").expect("embedded");
        let tag = t
            .canonical_ingredient("crème fraîche")
            .expect("crème fraîche has a tag");
        assert_eq!(t.canonical_ingredient("Crèmes fraîches"), Some(tag));
        assert_eq!(t.canonical_ingredient("creme fraiche"), Some(tag));
        assert_eq!(t.canonical_ingredient("crème fraîche entière"), Some(tag));
        for id in [
            "en:fermented-creams",
            "en:cremes-fraiches",
            "en:refrigerated-30-fat-thick-cream",
        ] {
            let stock = t.tag(id).expect("embedded");
            assert!(t.satisfies(stock, tag), "{id} satisfies crème fraîche");
        }
        // Never the reverse: a cream that is not a thick one does not.
        for id in ["en:creams", "en:unfermented-creams"] {
            let stock = t.tag(id).expect("embedded");
            assert!(!t.satisfies(stock, tag), "{id} does not satisfy");
        }
        assert!(t.satisfies(tag, fermented));
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
