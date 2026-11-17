//! The opinion given when a recipe is marked done (#821, amended by #822),
//! and the soft score it adds to the suggestions.
//!
//! One choice ([`Taste`]): « Aimé », « Neutre » (the default) or « Pas
//! aimé », stored in `meal_history.taste`, and a free note of at most
//! [`TASTE_NOTE_MAX_CHARS`] characters, `meal_history.taste_note`
//! (migration 0061, which replaced #821's closed list of tags). Neither is
//! required. Only the choice weighs on the suggestions: each meal's verdict
//! counts +1 (« Aimé ») or −1 (« Pas aimé ») for its recipe, fully on the
//! recipe itself and at half weight on the recipes that share its
//! ingredient categories (`taxonomy`), in proportion to how many they share
//! ([`similarity`]); « Neutre » counts 0. The note is kept for the
//! household, it scores nothing.
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
/// The longest note, in characters (Unicode scalar values, as Postgres'
/// `char_length` counts them in migration 0061's check), once trimmed and
/// each line break counted as one.
pub(crate) const TASTE_NOTE_MAX_CHARS: usize =
    manage_our_home_shared::dto::recipes::TASTE_NOTE_MAX_CHARS;

// Checked at compile time: a similar recipe's share stays under the
// recipe's own; one opinion never outweighs one ingredient out of four in
// stock (25 points); the whole term stays under the variety penalty, and
// so under the stock match (0-100).
const _: () = assert!(
    SIMILAR_WEIGHT < 1.0
        && TASTE_VOTE < 25.0
        && TASTE_MAX < crate::recipes::suggestions::RECENCY_PENALTY
);

/// The opinion on a meal (#822). On the wire and in the database, its
/// snake_case name; « Neutre » when none is given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Taste {
    /// « Aimé »: +1 for the recipe.
    Aime,
    /// « Neutre »: changes nothing.
    #[default]
    Neutre,
    /// « Pas aimé »: −1 for the recipe.
    PasAime,
}

impl Taste {
    /// The name stored in `meal_history.taste` (migration 0061).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Taste::Aime => "aime",
            Taste::Neutre => "neutre",
            Taste::PasAime => "pas_aime",
        }
    }
}

/// The note to store: its line breaks as `\n`, trimmed, `None` when empty
/// or absent. A browser posts a textarea's line breaks as `\r\n` but its
/// `maxlength` counts each as one character: so does this bound. Longer
/// than [`TASTE_NOTE_MAX_CHARS`] once trimmed: 400 `taste_note_too_long`.
pub(crate) fn taste_note_to_store(note: Option<&str>) -> AppResult<Option<String>> {
    let Some(note) = note.map(|n| n.replace("\r\n", "\n")) else {
        return Ok(None);
    };
    let note = note.trim();
    if note.is_empty() {
        return Ok(None);
    }
    if note.chars().count() > TASTE_NOTE_MAX_CHARS {
        return Err(AppError::BadRequest("taste_note_too_long".into()));
    }
    Ok(Some(note.to_string()))
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

    // -- Taste / taste_note_to_store -----------------------------------------

    #[test]
    fn every_taste_has_its_wire_name_and_neutral_is_the_default() {
        use Taste::*;
        for (taste, wire) in [(Aime, "aime"), (Neutre, "neutre"), (PasAime, "pas_aime")] {
            assert_eq!(taste.as_str(), wire);
            let read: Taste = serde_json::from_value(serde_json::json!(wire)).unwrap();
            assert_eq!(read, taste);
        }
        assert_eq!(Taste::default(), Neutre);
        assert!(serde_json::from_value::<Taste>(serde_json::json!("Aimé")).is_err());
        // #821's other tags are gone.
        assert!(serde_json::from_value::<Taste>(serde_json::json!("trop_long")).is_err());
    }

    #[test]
    fn no_note_or_a_blank_one_stores_nothing() {
        assert_eq!(taste_note_to_store(None).unwrap(), None);
        assert_eq!(taste_note_to_store(Some("")).unwrap(), None);
        assert_eq!(taste_note_to_store(Some("  \r\n\t ")).unwrap(), None);
    }

    #[test]
    fn a_note_is_stored_trimmed() {
        assert_eq!(
            taste_note_to_store(Some("  À refaire.\n"))
                .unwrap()
                .as_deref(),
            Some("À refaire.")
        );
    }

    #[test]
    fn a_note_of_the_maximum_length_in_characters_is_kept() {
        // 200 « é » are 400 bytes: the bound counts characters, and the
        // spaces around are not counted.
        let longest = "é".repeat(TASTE_NOTE_MAX_CHARS);
        let padded = format!("  {longest} ");
        assert_eq!(
            taste_note_to_store(Some(&padded)).unwrap().as_deref(),
            Some(longest.as_str())
        );
    }

    #[test]
    fn a_line_break_sent_as_crlf_counts_one_and_is_stored_as_lf() {
        // What a browser sends for a textarea its `maxlength` held to 200:
        // 201 characters on the wire, 200 as typed.
        let typed = format!("{}\n{}", "é".repeat(100), "é".repeat(99));
        let sent = typed.replace('\n', "\r\n");
        assert_eq!(
            taste_note_to_store(Some(&sent)).unwrap().as_deref(),
            Some(typed.as_str())
        );
        // A line of its own, as any other character, once trimmed.
        assert_eq!(
            taste_note_to_store(Some("\r\n Bon.\r\n\r\nÀ refaire.\r\n"))
                .unwrap()
                .as_deref(),
            Some("Bon.\n\nÀ refaire.")
        );
    }

    #[test]
    fn a_longer_note_is_refused() {
        let long = "é".repeat(TASTE_NOTE_MAX_CHARS + 1);
        match taste_note_to_store(Some(&long)) {
            Err(AppError::BadRequest(code)) => assert_eq!(code, "taste_note_too_long"),
            other => panic!("expected 400, got {other:?}"),
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
