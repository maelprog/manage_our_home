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
repository's MIT licence. Data: © Open Food Facts contributors,
https://world.openfoodfacts.org.
