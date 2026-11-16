//! A grocery line counted in whole packs (#824): « 380 g → 1 paquet
//! (500 g) ». The pack is the one of an article the family scanned, whose
//! size Open Food Facts gave (`off_products.quantity`, #402) and
//! `parse_pack_size` reads (#805): no network call, the record is already in
//! the database.
//!
//! A line matches a scanned article of its name (trimmed, case-insensitive)
//! or of its category: the article's categories reach the one the line's
//! name maps to (`recipes::taxonomy`), as the recipe suggestions match the
//! stock (`suggestions::stock_entry`). The articles of its name come first,
//! then those of its category, each by id; the first whose pack size reads
//! and whose unit the line's converts into gives the packs
//! (`round_to_packs`). The rounding is shown, never stored: the line keeps
//! its exact quantity.

use manage_our_home_shared::validation::stocks::{round_to_packs, PackRounding};
use uuid::Uuid;

use crate::error::AppResult;
use crate::recipes::suggestions::{stock_entry, StockEntry};
use crate::recipes::taxonomy::{taxonomy, Taxonomy};

/// A scanned article of the family whose Open Food Facts record gives a
/// pack size, as the matching reads it.
pub(crate) struct ScannedPack {
    pub id: Uuid,
    /// Its name and its categories, each with its ancestors.
    pub entry: StockEntry,
    /// Open Food Facts' pack size, as written (`off_products.quantity`).
    pub pack: String,
}

impl ScannedPack {
    pub(crate) fn new(
        taxonomy: &Taxonomy,
        id: Uuid,
        name: &str,
        pack: String,
        off_categories: &[String],
    ) -> Self {
        ScannedPack {
            id,
            entry: stock_entry(taxonomy, name, off_categories),
            pack,
        }
    }
}

/// The packs a grocery line comes to, or `None` when it is shown as it is:
/// see the module documentation.
pub(crate) fn packs_for(
    taxonomy: &Taxonomy,
    name: &str,
    quantity: Option<f64>,
    unit: Option<&str>,
    scanned: &[ScannedPack],
) -> Option<PackRounding> {
    let key = name.trim().to_lowercase();
    let tag = taxonomy.canonical_ingredient(name);
    let mut matching: Vec<(bool, Uuid, &ScannedPack)> = scanned
        .iter()
        .filter_map(|s| {
            let by_name = s.entry.name == key;
            let by_category = tag.is_some_and(|t| s.entry.tags.contains(&t));
            // `false` sorts first: the articles of its name.
            (by_name || by_category).then_some((!by_name, s.id, s))
        })
        .collect();
    matching.sort_by_key(|&(by_category, id, _)| (by_category, id));
    matching
        .into_iter()
        .find_map(|(_, _, s)| round_to_packs(quantity, unit, &s.pack))
}

/// The family's scanned articles with a pack size, by id.
pub(crate) async fn scanned_packs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<ScannedPack>> {
    // `off_products` is the public Open Food Facts cache (no RLS, see
    // migration 0026); the family's rows come from `stock_items`.
    let taxonomy = taxonomy();
    Ok(sqlx::query!(
        r#"
        SELECT s.id, s.name, o.quantity AS "pack!", o.categories_tags
        FROM stock_items s
        JOIN off_products o ON o.code = s.barcode
        WHERE s.group_id = $1 AND o.quantity IS NOT NULL
        ORDER BY s.id
        "#,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| ScannedPack::new(taxonomy, r.id, &r.name, r.pack, &r.categories_tags))
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use manage_our_home_shared::validation::stocks::PackSize;

    const FIXTURE: &str = "\
en:dairies\t\tProduits laitiers
en:milks\ten:dairies\tLaits|lait
en:semi-skimmed-milks\ten:milks\tLaits demi-écrémés
en:pastas\t\tPâtes alimentaires|pâtes
en:sugars\t\tSucres|sucre
";

    fn fixture() -> Taxonomy {
        Taxonomy::parse(FIXTURE)
    }

    fn scanned(t: &Taxonomy, id: u128, name: &str, pack: &str, tags: &[&str]) -> ScannedPack {
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        ScannedPack::new(t, Uuid::from_u128(id), name, pack.to_string(), &tags)
    }

    fn packs(count: u64, quantity: f64, unit: &str) -> Option<PackRounding> {
        Some(PackRounding {
            count,
            size: PackSize {
                quantity,
                unit: unit.to_string(),
            },
        })
    }

    #[test]
    fn a_line_named_as_a_scanned_article_counts_in_its_packs() {
        let t = fixture();
        let stock = [scanned(&t, 1, "Coquillettes", "500 g", &[])];
        assert_eq!(
            packs_for(&t, " coquillettes ", Some(380.0), Some("g"), &stock),
            packs(1, 500.0, "g")
        );
    }

    #[test]
    fn a_line_of_the_category_of_a_scanned_article_counts_in_its_packs() {
        // « Lait » in a recipe, « Lait demi-écrémé Lactel » scanned.
        let t = fixture();
        let stock = [scanned(
            &t,
            1,
            "Lait demi-écrémé Lactel",
            "1 L",
            &["en:semi-skimmed-milks"],
        )];
        assert_eq!(
            packs_for(&t, "Lait", Some(40.0), Some("cl"), &stock),
            packs(1, 1.0, "L")
        );
        // Never the reverse: a broader article does not cover a narrower line.
        let stock = [scanned(&t, 1, "Crèmerie", "1 L", &["en:dairies"])];
        assert_eq!(packs_for(&t, "Lait", Some(2.0), Some("L"), &stock), None);
    }

    #[test]
    fn without_a_matching_scanned_article_the_line_is_unchanged() {
        let t = fixture();
        let stock = [scanned(&t, 1, "Sucre Daddy", "1 kg", &["en:sugars"])];
        assert_eq!(
            packs_for(&t, "Farine", Some(500.0), Some("g"), &stock),
            None
        );
        assert_eq!(packs_for(&t, "Farine", Some(500.0), Some("g"), &[]), None);
    }

    #[test]
    fn the_name_comes_before_the_category_then_the_id() {
        let t = fixture();
        let stock = [
            scanned(&t, 1, "Pâtes Panzani", "1 kg", &["en:pastas"]),
            scanned(&t, 3, "Pâtes", "250 g", &[]),
            scanned(&t, 2, "pâtes", "500 g", &[]),
        ];
        assert_eq!(
            packs_for(&t, "Pâtes", Some(600.0), Some("g"), &stock),
            packs(2, 500.0, "g")
        );
    }

    #[test]
    fn an_article_that_does_not_round_gives_way_to_the_next() {
        let t = fixture();
        let stock = [
            scanned(&t, 1, "Lait", "env. 4 portions", &[]),
            scanned(&t, 2, "Lait", "20 cl", &[]),
            scanned(&t, 3, "Lait entier", "1 L", &["en:milks"]),
        ];
        // 150 cl in 20 cl packs: 7,5, so 8.
        assert_eq!(
            packs_for(&t, "Lait", Some(1.5), Some("L"), &stock),
            packs(8, 20.0, "cl")
        );
        // In grams, no milk pack converts: the line is unchanged.
        assert_eq!(packs_for(&t, "Lait", Some(300.0), Some("g"), &stock), None);
    }

    #[test]
    fn a_zero_or_absent_quantity_is_not_rounded() {
        let t = fixture();
        let stock = [scanned(&t, 1, "Sucre", "1 kg", &[])];
        assert_eq!(packs_for(&t, "Sucre", Some(0.0), Some("g"), &stock), None);
        assert_eq!(packs_for(&t, "Sucre", None, None, &stock), None);
    }
}
