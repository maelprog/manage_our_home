# Embedded data

## `off-categories-fr.tsv.gz`

Open Food Facts' category taxonomy, reduced to what the recipe suggestions
need (#406, `apps/api/src/recipes/taxonomy.rs`): every category with a
French name or synonym, and its ancestors. One line per category:

    <tag id> TAB <parent ids, comma-separated> TAB <French names, |-separated>

Built by `build_off_categories.py` (Python 3, standard library only) from
https://static.openfoodfacts.org/data/taxonomies/categories.full.json, and
embedded in the `manage_our_home` binary with `include_bytes!`. Rebuild it
with the script, never by hand; the same taxonomy gives the same bytes.

**Licence.** This file is a database derived from Open Food Facts and is
distributed under the [Open Database License (ODbL)
1.0](https://opendatacommons.org/licenses/odbl/1-0/), not under the
repository's licence (PolyForm Noncommercial 1.0.0). Data: © Open Food Facts contributors,
https://world.openfoodfacts.org.

## `ingredient-aliases-fr.tsv`

French ingredient names that Open Food Facts' taxonomy lacks (#901), each
mapped to one of its category ids. One line per alias:

    <name as a recipe writes it> TAB <Open Food Facts category id>

Read by `Taxonomy::parse_with_aliases` (`apps/api/src/recipes/taxonomy.rs`)
when the embedded taxonomy loads, and embedded with `include_str!`; the
build script and `off-categories-fr.tsv.gz` are unchanged. An alias is one
more French name of its category, under the same rules as Open Food Facts'
names: a name two unrelated categories share is still no tag. A test fails
when a line names a category the taxonomy lacks, repeats a name the taxonomy
already gives that category, or does not end up naming its category.

**Origin.** These aliases are written for manage_our_home, not taken from
Open Food Facts; only the category ids they point to are Open Food Facts'.
