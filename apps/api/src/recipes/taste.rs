//! Taste tags given when a recipe is marked done (#821), and the soft score
//! they add to the suggestions.
//!
//! A closed list (`TasteTag`), stored in `meal_history.taste_tags` under
//! their wire names; none is required. Only « Aimé » and « Pas aimé » weigh
//! on the suggestions: each meal's verdict counts +1 or −1 for its recipe,
//! fully on the recipe itself and at half weight on the recipes that share
//! its ingredient categories (`taxonomy`), in proportion to how many they
//! share ([`similarity`]). The other tags are kept for the household, they
//! score nothing.
//!
//! The term is bounded by [`TASTE_MAX`], well under the stock match
//! (dominant, 0–100) and the variety penalty (30): one opinion is worth at
//! most [`TASTE_VOTE`], as much as one ingredient in season.

use serde::Deserialize;
use std::collections::HashSet;
use std::hash::Hash;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Points one « Aimé » (or « Pas aimé », negative) gives its own recipe.
pub(crate) const TASTE_VOTE: f64 = 5.0;
/// Share of [`TASTE_VOTE`] a recipe with exactly the same ingredient
/// categories receives from a verdict on another recipe.
pub(crate) const SIMILAR_WEIGHT: f64 = 0.5;
/// Bound of the taste term, either way.
pub(crate) const TASTE_MAX: f64 = 15.0;

// Checked at compile time: a similar recipe's share stays under the
// recipe's own; one opinion never outweighs one ingredient out of four in
// stock (25 points); the whole term stays under the variety penalty, and
// so under the stock match (0-100).
const _: () = assert!(
    SIMILAR_WEIGHT < 1.0
        && TASTE_VOTE < 25.0
        && TASTE_MAX < crate::recipes::suggestions::RECENCY_PENALTY
);

/// A taste tag (#821). On the wire and in the database, its snake_case name.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TasteTag {
    /// « Ail »
    Ail,
    /// « Épicé »
    Epice,
    /// « Sucré-salé »
    SucreSale,
    /// « Trop long »
    TropLong,
    /// « Aimé »
    Aime,
    /// « Pas aimé »
    PasAime,
}

impl TasteTag {
    /// The name stored in `meal_history.taste_tags` (migration 0059).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TasteTag::Ail => "ail",
            TasteTag::Epice => "epice",
            TasteTag::SucreSale => "sucre_sale",
            TasteTag::TropLong => "trop_long",
            TasteTag::Aime => "aime",
            TasteTag::PasAime => "pas_aime",
        }
    }
}

/// The tags to store: each once, in the list's order. « Aimé » and « Pas
/// aimé » together say nothing: 400 `conflicting_taste_tags`.
pub(crate) fn taste_tags_to_store(tags: &[TasteTag]) -> AppResult<Vec<&'static str>> {
    if tags.contains(&TasteTag::Aime) && tags.contains(&TasteTag::PasAime) {
        return Err(AppError::BadRequest("conflicting_taste_tags".into()));
    }
    let mut sorted = tags.to_vec();
    sorted.sort();
    sorted.dedup();
    Ok(sorted.into_iter().map(TasteTag::as_str).collect())
}

/// How alike two recipes are by their ingredient categories: the share of
/// the categories either has that both have (0 when either has none).
pub(crate) fn similarity<T: Eq + Hash>(a: &HashSet<T>, b: &HashSet<T>) -> f64 {
    let both = a.intersection(b).count();
    let either = a.union(b).count();
    if either == 0 {
        0.0
    } else {
        both as f64 / either as f64
    }
}

/// A recipe's verdicts: « Aimé » minus « Pas aimé » over its meals, and its
/// ingredient categories.
pub(crate) struct Rated<'a, T> {
    pub recipe_id: Uuid,
    pub net: i64,
    pub categories: &'a HashSet<T>,
}

/// The taste term of a recipe's score, in [−[`TASTE_MAX`], [`TASTE_MAX`]]:
/// its own verdicts at [`TASTE_VOTE`] each, the other recipes' at
/// [`SIMILAR_WEIGHT`] × their [`similarity`] to it. 0 without verdicts.
pub(crate) fn taste_score<T: Eq + Hash>(
    recipe_id: Uuid,
    categories: &HashSet<T>,
    rated: &[Rated<'_, T>],
) -> f64 {
    let raw: f64 = rated
        .iter()
        .map(|r| {
            let weight = if r.recipe_id == recipe_id {
                1.0
            } else {
                SIMILAR_WEIGHT * similarity(categories, r.categories)
            };
            r.net as f64 * TASTE_VOTE * weight
        })
        .sum();
    raw.clamp(-TASTE_MAX, TASTE_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn cats(names: &[&'static str]) -> HashSet<&'static str> {
        names.iter().copied().collect()
    }

    // -- taste_tags_to_store ------------------------------------------------

    #[test]
    fn no_tag_stores_nothing() {
        assert!(taste_tags_to_store(&[]).unwrap().is_empty());
    }

    #[test]
    fn tags_are_stored_once_in_the_list_order() {
        use TasteTag::*;
        assert_eq!(
            taste_tags_to_store(&[TropLong, Ail, Aime, Ail]).unwrap(),
            ["ail", "trop_long", "aime"]
        );
    }

    #[test]
    fn every_tag_has_its_wire_name() {
        use TasteTag::*;
        for tag in [Ail, Epice, SucreSale, TropLong, Aime, PasAime] {
            let wire: TasteTag = serde_json::from_value(serde_json::json!(tag.as_str())).unwrap();
            assert_eq!(wire, tag);
        }
        assert!(serde_json::from_value::<TasteTag>(serde_json::json!("Aimé")).is_err());
    }

    #[test]
    fn liked_and_disliked_together_are_refused() {
        use TasteTag::*;
        match taste_tags_to_store(&[Aime, Epice, PasAime]) {
            Err(AppError::BadRequest(code)) => assert_eq!(code, "conflicting_taste_tags"),
            other => panic!("expected 400, got {:?}", other.map(|_| ())),
        }
    }

    // -- similarity ----------------------------------------------------------

    #[test]
    fn similarity_is_the_share_of_categories_in_common() {
        assert_eq!(similarity(&cats(&["a", "b"]), &cats(&["a", "b"])), 1.0);
        assert_eq!(
            similarity(&cats(&["a", "b"]), &cats(&["b", "c"])),
            1.0 / 3.0
        );
        assert_eq!(similarity(&cats(&["a"]), &cats(&["b"])), 0.0);
    }

    #[test]
    fn a_recipe_without_categories_is_like_none() {
        assert_eq!(similarity(&cats(&[]), &cats(&["a"])), 0.0);
        assert_eq!(similarity(&cats(&[]), &cats(&[])), 0.0);
    }

    // -- taste_score ---------------------------------------------------------

    #[test]
    fn without_verdicts_the_term_is_zero() {
        let mine = cats(&["a", "b"]);
        assert_eq!(taste_score(id(1), &mine, &[]), 0.0);
        // A verdict of zero (as many likes as dislikes) neither.
        let rated = [Rated {
            recipe_id: id(1),
            net: 0,
            categories: &mine,
        }];
        assert_eq!(taste_score(id(1), &mine, &rated), 0.0);
    }

    #[test]
    fn liked_raises_the_recipe_itself() {
        let mine = cats(&["a"]);
        let rated = [Rated {
            recipe_id: id(1),
            net: 1,
            categories: &mine,
        }];
        assert_eq!(taste_score(id(1), &mine, &rated), TASTE_VOTE);
    }

    #[test]
    fn disliked_lowers_the_recipe_itself() {
        // Even without categories: the verdict is on this very recipe.
        let mine = cats(&[]);
        let rated = [Rated {
            recipe_id: id(1),
            net: -1,
            categories: &mine,
        }];
        assert_eq!(taste_score(id(1), &mine, &rated), -TASTE_VOTE);
    }

    #[test]
    fn a_similar_recipe_receives_a_weaker_share() {
        let liked = cats(&["a", "b"]);
        let same = cats(&["a", "b"]);
        let half = cats(&["a", "b", "c", "d"]);
        let other = cats(&["e"]);
        let rated = [Rated {
            recipe_id: id(1),
            net: 1,
            categories: &liked,
        }];
        assert_eq!(
            taste_score(id(2), &same, &rated),
            TASTE_VOTE * SIMILAR_WEIGHT
        );
        assert_eq!(
            taste_score(id(3), &half, &rated),
            TASTE_VOTE * SIMILAR_WEIGHT * 0.5
        );
        assert_eq!(taste_score(id(4), &other, &rated), 0.0);
        let disliked = [Rated {
            recipe_id: id(1),
            net: -1,
            categories: &liked,
        }];
        assert_eq!(
            taste_score(id(2), &same, &disliked),
            -TASTE_VOTE * SIMILAR_WEIGHT
        );
    }

    #[test]
    fn verdicts_add_up_within_the_bounds() {
        let mine = cats(&["a"]);
        let many = [Rated {
            recipe_id: id(1),
            net: 10,
            categories: &mine,
        }];
        assert_eq!(taste_score(id(1), &mine, &many), TASTE_MAX);
        let many = [Rated {
            recipe_id: id(1),
            net: -10,
            categories: &mine,
        }];
        assert_eq!(taste_score(id(1), &mine, &many), -TASTE_MAX);
        let two = [
            Rated {
                recipe_id: id(1),
                net: 1,
                categories: &mine,
            },
            Rated {
                recipe_id: id(2),
                net: 1,
                categories: &mine,
            },
        ];
        assert_eq!(
            taste_score(id(1), &mine, &two),
            TASTE_VOTE * (1.0 + SIMILAR_WEIGHT)
        );
    }
}
