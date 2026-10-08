//! The expiry date a scan proposes when the code carries none (#404): a
//! default shelf life per Open Food Facts category, counted from the day of
//! the scan.
//!
//! An EAN-13 carries no date, and reading the one printed on the pack (dot
//! matrix, inkjet) is not reliable without a vision model, left for later.
//! So the scan proposes an order of magnitude the member corrects in one
//! glance, rather than an empty field. The date is only ever proposed:
//! `POST /groups/:id/stock-items/scan` writes nothing, and the page shows
//! where the date comes from.
//!
//! The order of what is proposed (`proposed_expiry`): the date a GS1 code
//! carries (#403), else the category's shelf life, else nothing.

use chrono::{Days, NaiveDate};
use manage_our_home_shared::dto::stocks::ExpiresOnSource;

/// A category's default shelf life, and its name as Open Food Facts gives it
/// in French, for the page to say where the date comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShelfLife {
    pub days: Days,
    pub label: &'static str,
}

/// One row of `SHELF_LIVES`.
struct Row {
    tag: &'static str,
    days: u64,
    label: &'static str,
}

const fn row(tag: &'static str, days: u64, label: &'static str) -> Row {
    Row { tag, days, label }
}

/// The categories a shelf life is known for, **in order of priority**: the
/// first row whose tag a product carries wins. A product's
/// `categories_tags` hold its categories *and all their ancestors* (a
/// yogurt also carries `en:dairies`), in no order to rely on, so the
/// specificity lives here:
///
/// - a way of keeping before any kind of food (`en:canned-fishes` carries
///   both `en:canned-foods` and `en:fishes`: the tin decides);
/// - a sub-category before its parent (`en:fresh-cheeses` before
///   `en:cheeses`), checked against the taxonomy at
///   `static.openfoodfacts.org/data/taxonomies/categories.json`, where every
///   tag below exists and the labels are its French names.
///
/// The values are an order of magnitude of the date printed on the pack on
/// the day it is bought, rounded **down**: a date proposed too soon makes
/// the member look at the pack, one proposed too late lets food spoil.
/// Categories whose members differ too much for one figure (`en:milks`
/// holds fresh and UHT milk, `en:pastas` fresh and dry pasta) get a row
/// only through a sub-category that settles it. A category without a row
/// gets no date: an empty field beats a wrong one.
const SHELF_LIVES: &[Row] = &[
    // --- Ways of keeping, before what they keep ---
    // Commercial frozen food is labelled a year or more ahead; six months is
    // the low end, short enough to never outrun the pack.
    row("en:frozen-foods", 180, "Surgelés"),
    // Semi-preserves (anchovies, fish roe) sit under `en:canned-foods` in
    // the taxonomy but keep in the fridge, labelled weeks ahead, not years:
    // before it.
    row("en:semi-preserved-foods", 21, "Semi-conserves"),
    // Tins are labelled two to five years ahead: the low end.
    row("en:canned-foods", 730, "Conserves"),
    // --- Meat and fish: the shortest dates ---
    // Minced meat spoils first of all fresh meats: a day.
    row("en:ground-meats", 1, "Viandes hachées"),
    // Pre-packed fresh meat is sold a few days from its date.
    row("en:fresh-meats", 3, "Viandes fraîches"),
    // Raw poultry keeps less than red meat.
    row("en:poultries", 2, "Volailles"),
    // Sliced ham, the bulk of the category, keeps about a week sealed.
    row("en:hams", 5, "Jambons"),
    // Smoked fish (smoked salmon) is labelled two to three weeks ahead;
    // before `en:fishes`, its parent.
    row("en:smoked-fishes", 10, "Poissons fumés"),
    // Fresh fish: a day or two.
    row("en:fishes", 2, "Poissons"),
    // --- Dairy and eggs ---
    // Yogurts are labelled about a month ahead of production; three weeks
    // left when bought.
    row("en:yogurts", 21, "Yaourts"),
    // Fresh cheeses keep less than ripened ones; before `en:cheeses`.
    row("en:fresh-cheeses", 10, "Fromages à pâte fraîche"),
    row("en:cheeses", 21, "Fromages"),
    // UHT milk is labelled about three months ahead; before `en:milks`,
    // which has no row of its own (fresh milk is labelled about a week).
    row("en:uht-milks", 60, "Laits UHT"),
    // Butter is labelled about two months ahead.
    row("en:butters", 30, "Beurres"),
    // Eggs: at most 28 days after laying (Regulation (EC) No 589/2008,
    // art. 13), minus a week between laying and the shelf.
    row("en:eggs", 21, "Œufs"),
    // --- Bakery and dry goods ---
    // Fresh bread is stale in a few days; packaged sliced bread lasts
    // longer, and the member says so.
    row("en:breads", 3, "Pains"),
    // Dry pasta and rice are labelled one to three years ahead: the low end.
    row("en:dry-pastas", 365, "Pâtes sèches"),
    row("en:rices", 365, "Riz"),
    // Breakfast cereals, biscuits and chocolate: months, the low end.
    row("en:breakfast-cereals", 180, "Céréales pour petit-déjeuner"),
    row("en:biscuits", 90, "Biscuits"),
    row("en:chocolates", 180, "Chocolats"),
];

/// The shelf life of the most specific category of `SHELF_LIVES` a product
/// carries, `None` when it carries none of them — the date field then
/// stays empty.
pub fn default_shelf_life(categories_tags: &[String]) -> Option<ShelfLife> {
    SHELF_LIVES
        .iter()
        .find(|row| categories_tags.iter().any(|tag| tag == row.tag))
        .map(|row| ShelfLife {
            days: Days::new(row.days),
            label: row.label,
        })
}

/// The date a scan proposes, and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedExpiry {
    pub on: NaiveDate,
    pub source: ExpiresOnSource,
    /// The category's French name when `source` is `Category`.
    pub category: Option<&'static str>,
}

/// The date a GS1 code carries, else `today` plus the category's shelf
/// life, else `None`. `today` is the Paris day of the scan, injected.
pub fn proposed_expiry(
    gs1: Option<NaiveDate>,
    categories_tags: &[String],
    today: NaiveDate,
) -> Option<ProposedExpiry> {
    if let Some(on) = gs1 {
        return Some(ProposedExpiry {
            on,
            source: ExpiresOnSource::Gs1,
            category: None,
        });
    }
    let shelf_life = default_shelf_life(categories_tags)?;
    Some(ProposedExpiry {
        on: today.checked_add_days(shelf_life.days)?,
        source: ExpiresOnSource::Category,
        category: Some(shelf_life.label),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| t.to_string()).collect()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_known_category_gives_its_shelf_life_and_name() {
        assert_eq!(
            default_shelf_life(&tags(&["en:dairies", "en:yogurts"])),
            Some(ShelfLife {
                days: Days::new(21),
                label: "Yaourts"
            })
        );
        assert_eq!(
            default_shelf_life(&tags(&["en:meats", "en:fresh-meats"])).map(|s| s.days),
            Some(Days::new(3))
        );
    }

    #[test]
    fn an_unknown_category_or_none_gives_nothing() {
        assert_eq!(default_shelf_life(&[]), None);
        assert_eq!(
            default_shelf_life(&tags(&["en:spreads", "en:sweet-spreads"])),
            None
        );
        // `en:milks` holds fresh and UHT milk alike: no figure for it.
        assert_eq!(default_shelf_life(&tags(&["en:dairies", "en:milks"])), None);
        // A tag in another language, or another case, is not one of ours.
        assert_eq!(
            default_shelf_life(&tags(&["fr:yaourts", "EN:YOGURTS"])),
            None
        );
    }

    #[test]
    fn the_most_specific_category_wins_whatever_the_order_of_the_tags() {
        let fresh = ["en:dairies", "en:cheeses", "en:fresh-cheeses"];
        let mut reversed = fresh;
        reversed.reverse();
        for list in [fresh, reversed] {
            assert_eq!(
                default_shelf_life(&tags(&list)).map(|s| s.label),
                Some("Fromages à pâte fraîche"),
                "{list:?}"
            );
        }
        // Smoked salmon carries `en:fishes` too.
        assert_eq!(
            default_shelf_life(&tags(&[
                "en:fishes",
                "en:smoked-fishes",
                "en:smoked-salmons"
            ]))
            .map(|s| s.days),
            Some(Days::new(10))
        );
        assert_eq!(
            default_shelf_life(&tags(&["en:fishes", "en:fresh-meats", "en:ground-meats"]))
                .map(|s| s.days),
            Some(Days::new(1))
        );
    }

    #[test]
    fn the_way_of_keeping_wins_over_the_kind_of_food() {
        // A tin of tuna, a bag of frozen fish.
        assert_eq!(
            default_shelf_life(&tags(&["en:fishes", "en:canned-fishes", "en:canned-foods"]))
                .map(|s| s.label),
            Some("Conserves")
        );
        assert_eq!(
            default_shelf_life(&tags(&["en:fishes", "en:frozen-fishes", "en:frozen-foods"]))
                .map(|s| s.label),
            Some("Surgelés")
        );
    }

    #[test]
    fn a_semi_preserve_is_not_a_tin() {
        // Anchovies, lumpfish roe: `en:semi-preserved-foods` sits under
        // `en:canned-foods` but keeps in the fridge, for weeks.
        let shelf_life = default_shelf_life(&tags(&[
            "en:canned-foods",
            "en:fresh-foods",
            "en:semi-preserved-foods",
            "en:fishes",
        ]))
        .unwrap();
        assert_eq!(shelf_life.label, "Semi-conserves");
        assert!(shelf_life.days <= Days::new(30), "{shelf_life:?}");
    }

    #[test]
    fn the_table_holds_each_tag_once_with_a_positive_duration() {
        for (i, row) in SHELF_LIVES.iter().enumerate() {
            assert!(row.days > 0, "{}", row.tag);
            assert!(row.tag.starts_with("en:"), "{}", row.tag);
            assert!(!row.label.is_empty(), "{}", row.tag);
            assert!(
                SHELF_LIVES[i + 1..]
                    .iter()
                    .all(|other| other.tag != row.tag),
                "{} twice",
                row.tag
            );
            // Each row is reachable: a product carrying only its tag gets it.
            assert_eq!(
                default_shelf_life(&tags(&[row.tag])).map(|s| s.label),
                Some(row.label)
            );
        }
    }

    #[test]
    fn a_gs1_date_wins_over_the_category() {
        assert_eq!(
            proposed_expiry(
                Some(day(2027, 1, 31)),
                &tags(&["en:yogurts"]),
                day(2026, 10, 8)
            ),
            Some(ProposedExpiry {
                on: day(2027, 1, 31),
                source: ExpiresOnSource::Gs1,
                category: None,
            })
        );
        // A GS1 date with no category known, too.
        assert_eq!(
            proposed_expiry(Some(day(2027, 1, 31)), &[], day(2026, 10, 8)).map(|p| p.source),
            Some(ExpiresOnSource::Gs1)
        );
    }

    #[test]
    fn without_a_gs1_date_the_category_counts_from_today() {
        assert_eq!(
            proposed_expiry(None, &tags(&["en:dairies", "en:yogurts"]), day(2026, 10, 8)),
            Some(ProposedExpiry {
                on: day(2026, 10, 29),
                source: ExpiresOnSource::Category,
                category: Some("Yaourts"),
            })
        );
        // Across a month and a year end.
        assert_eq!(
            proposed_expiry(None, &tags(&["en:breads"]), day(2026, 12, 30)).map(|p| p.on),
            Some(day(2027, 1, 2))
        );
    }

    #[test]
    fn neither_a_gs1_date_nor_a_known_category_proposes_nothing() {
        assert_eq!(
            proposed_expiry(None, &tags(&["en:spreads"]), day(2026, 10, 8)),
            None
        );
        assert_eq!(proposed_expiry(None, &[], day(2026, 10, 8)), None);
    }
}
