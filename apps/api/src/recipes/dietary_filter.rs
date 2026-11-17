//! What a member's food constraints do to the recipes (#818): a hard filter
//! on what is proposed to them, and an allergy warning on every recipe they
//! read.
//!
//! The constraints are #817's (`recipes::constraints`, migration 0057). The
//! reader sees, in one family, their own allergies whatever their level,
//! the allergies another member shares `public` there (who and which), and
//! those another member shares `anonymous` there (which, for the server
//! alone). Another member's `private` allergy is never read: nothing here
//! can depend on it.
//!
//! - **Hard filter** ([`excluded`]), on the suggestions and on the
//!   replacements of a planned recipe (#815): a recipe is not proposed when
//!   one of its required ingredients satisfies ([`Taxonomy::satisfies`]) one
//!   of the reader's dislikes, or when it needs equipment the household
//!   does not have; and, unless the reader unticks `avoid_allergens` for
//!   that request, when a required ingredient satisfies an allergy the
//!   reader sees. It never says why. The scoring of what is left is
//!   `recipes::suggestions`', unchanged.
//! - **Warning** ([`allergy_warning`]), on every ingredient of a recipe,
//!   optional ones included: the reader's own allergies it holds, the
//!   public ones of other members with their name, whether an anonymous
//!   one of another member matches, and the ingredients the taxonomy does
//!   not know, which leave it unverifiable (named since #825: they are the
//!   recipe's, which the reader reads anyway).
//!
//! Computed at each request, from what the reader may read at that moment:
//! nothing is stored, cached or logged.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use uuid::Uuid;

use crate::error::AppResult;
use crate::recipes::taxonomy::{taxonomy, TagId, Taxonomy};

/// The allergies a reader may see in one family, as tags of the taxonomy.
/// A tag the taxonomy no longer knows is left out: it cannot match.
#[derive(Default)]
pub(crate) struct VisibleAllergies {
    /// The reader's own, whatever level they set.
    pub mine: Vec<TagId>,
    /// Shared publicly by another member: their name and the tag.
    pub public: Vec<(String, TagId)>,
    /// Shared anonymously by another member, without who.
    pub anonymous: Vec<TagId>,
}

impl VisibleAllergies {
    fn is_empty(&self) -> bool {
        self.mine.is_empty() && self.public.is_empty() && self.anonymous.is_empty()
    }
}

/// One of the reader's own allergies that a recipe holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
pub struct AllergenMatch {
    /// The allergy, an Open Food Facts category.
    pub tag: String,
    /// Its first French name in the taxonomy (#825), for the page to say.
    pub label: String,
}

/// A public allergy of another member that a recipe holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
pub struct PublicAllergyMatch {
    /// The member's display name.
    pub member: String,
    /// The allergy, an Open Food Facts category.
    pub tag: String,
    /// Its first French name in the taxonomy (#825), for the page to say.
    pub label: String,
}

/// What a recipe holds of the allergies the reader may see (#818).
#[derive(Debug, Default, PartialEq, Serialize, utoipa::ToSchema)]
pub struct AllergyWarning {
    /// The reader's own allergies the recipe holds, whatever their level,
    /// sorted by tag.
    pub mine: Vec<AllergenMatch>,
    /// Allergies another member shares publicly that the recipe holds,
    /// sorted by member then tag. Never the reader.
    pub public: Vec<PublicAllergyMatch>,
    /// Whether an allergy another member shares anonymously matches; which,
    /// and whose, are not said.
    pub anonymous: bool,
    /// When the reader may see an allergy at all, the ingredients of the
    /// recipe that have no category to check it against, as the recipe
    /// names them, sorted, each once (#825); empty otherwise.
    pub unverifiable: Vec<String>,
}

/// One ingredient of a recipe as the filters see it: its name, its
/// category, if the taxonomy knows that name, and whether it is optional.
#[derive(Clone, Copy)]
pub(crate) struct Ingredient<'a> {
    pub name: &'a str,
    pub tag: Option<TagId>,
    pub optional: bool,
}

impl<'a> Ingredient<'a> {
    pub(crate) fn named(taxonomy: &Taxonomy, name: &'a str, optional: bool) -> Self {
        Ingredient {
            name,
            tag: taxonomy.canonical_ingredient(name),
            optional,
        }
    }
}

/// What the hard filter weighs, for one reader in one family.
pub(crate) struct HardFilter<'a> {
    pub allergies: &'a VisibleAllergies,
    /// The reader's dislikes; never another member's.
    pub dislikes: &'a [TagId],
    /// The household's equipment codes.
    pub equipment: &'a HashSet<String>,
    /// The request's `avoid_allergens`: `false` lifts the allergies, and
    /// them only.
    pub avoid_allergens: bool,
}

/// Whether a recipe is kept out of what is proposed to the reader.
pub(crate) fn excluded(
    taxonomy: &Taxonomy,
    filter: &HardFilter,
    ingredients: &[Ingredient],
    required_equipment: &[String],
) -> bool {
    if required_equipment
        .iter()
        .any(|code| !filter.equipment.contains(code))
    {
        return true;
    }
    let tags: Vec<TagId> = ingredients
        .iter()
        .filter(|i| !i.optional)
        .filter_map(|i| i.tag)
        .collect();
    let held = |constraint: &TagId| tags.iter().any(|&t| taxonomy.satisfies(t, *constraint));
    if filter.dislikes.iter().any(held) {
        return true;
    }
    let a = filter.allergies;
    filter.avoid_allergens
        && a.mine
            .iter()
            .chain(a.public.iter().map(|(_, tag)| tag))
            .chain(&a.anonymous)
            .any(held)
}

/// What the reader may see of the allergies in the family the transaction
/// is scoped to (`scoped_tx`): their own, read under `user_allergies`' RLS;
/// the others', through the functions of 0057 and 0058, which never read a
/// `private` row.
pub(crate) async fn visible_allergies(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    reader: Uuid,
) -> AppResult<VisibleAllergies> {
    let taxonomy = taxonomy();
    let mine = sqlx::query_scalar!("SELECT tag FROM user_allergies WHERE user_id = $1", reader)
        .fetch_all(&mut **tx)
        .await?;
    let public = sqlx::query!(
        r#"SELECT user_id AS "user_id!", display_name AS "display_name!", tag AS "tag!"
           FROM group_public_allergies($1)"#,
        group_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let anonymous = sqlx::query_scalar!(
        r#"SELECT tag AS "tag!" FROM group_other_anonymous_allergy_tags($1) AS tag"#,
        group_id
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(VisibleAllergies {
        mine: mine.iter().filter_map(|t| taxonomy.tag(t)).collect(),
        public: public
            .into_iter()
            .filter(|p| p.user_id != reader)
            .filter_map(|p| taxonomy.tag(&p.tag).map(|tag| (p.display_name, tag)))
            .collect(),
        anonymous: anonymous.iter().filter_map(|t| taxonomy.tag(t)).collect(),
    })
}

/// The reader's dislikes, as tags. Their own only (`user_dislikes`' RLS).
pub(crate) async fn reader_dislikes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    reader: Uuid,
) -> AppResult<Vec<TagId>> {
    let taxonomy = taxonomy();
    let tags = sqlx::query_scalar!("SELECT tag FROM user_dislikes WHERE user_id = $1", reader)
        .fetch_all(&mut **tx)
        .await?;
    Ok(tags.iter().filter_map(|t| taxonomy.tag(t)).collect())
}

/// The household's equipment codes.
pub(crate) async fn household_equipment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<HashSet<String>> {
    Ok(sqlx::query_scalar!(
        "SELECT code FROM group_equipment WHERE group_id = $1",
        group_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect())
}

/// The allergy warning of each recipe named, for the reader; a recipe with
/// no ingredient warns of nothing.
pub(crate) async fn warnings_for(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    reader: Uuid,
    recipe_ids: &[Uuid],
) -> AppResult<HashMap<Uuid, AllergyWarning>> {
    let taxonomy = taxonomy();
    let allergies = visible_allergies(tx, group_id, reader).await?;
    let rows = sqlx::query!(
        r#"SELECT recipe_id, name, is_optional FROM recipe_ingredients
           WHERE group_id = $1 AND recipe_id = ANY($2)"#,
        group_id,
        recipe_ids,
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut ingredients: HashMap<Uuid, Vec<Ingredient>> = HashMap::new();
    for row in &rows {
        ingredients
            .entry(row.recipe_id)
            .or_default()
            .push(Ingredient::named(taxonomy, &row.name, row.is_optional));
    }
    Ok(recipe_ids
        .iter()
        .map(|id| {
            let of_recipe = ingredients.get(id).map_or(&[][..], Vec::as_slice);
            (*id, allergy_warning(taxonomy, &allergies, of_recipe))
        })
        .collect())
}

/// The allergy warning of a recipe for the reader.
pub(crate) fn allergy_warning(
    taxonomy: &Taxonomy,
    allergies: &VisibleAllergies,
    ingredients: &[Ingredient],
) -> AllergyWarning {
    let tags: Vec<TagId> = ingredients.iter().filter_map(|i| i.tag).collect();
    let held = |constraint: TagId| tags.iter().any(|&t| taxonomy.satisfies(t, constraint));

    let mut mine: Vec<AllergenMatch> = allergies
        .mine
        .iter()
        .filter(|&&tag| held(tag))
        .map(|&tag| AllergenMatch {
            tag: taxonomy.id(tag).to_string(),
            label: taxonomy.label(tag).to_string(),
        })
        .collect();
    mine.sort();
    mine.dedup();
    let mut public: Vec<PublicAllergyMatch> = allergies
        .public
        .iter()
        .filter(|(_, tag)| held(*tag))
        .map(|(member, tag)| PublicAllergyMatch {
            member: member.clone(),
            tag: taxonomy.id(*tag).to_string(),
            label: taxonomy.label(*tag).to_string(),
        })
        .collect();
    public.sort();
    public.dedup();
    let mut unverifiable: Vec<String> = if allergies.is_empty() {
        Vec::new()
    } else {
        ingredients
            .iter()
            .filter(|i| i.tag.is_none())
            .map(|i| i.name.to_string())
            .collect()
    };
    unverifiable.sort();
    unverifiable.dedup();
    AllergyWarning {
        mine,
        public,
        anonymous: allergies.anonymous.iter().any(|&tag| held(tag)),
        unverifiable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
en:dairies\t\tProduits laitiers
en:milks\ten:dairies\tLaits|lait
en:semi-skimmed-milks\ten:milks\tLaits demi-écrémés
en:nuts\t\tFruits à coque
en:peanuts\ten:nuts\tCacahuètes
en:eggs\t\tŒufs
en:mushrooms\t\tChampignons
en:flours\t\tFarines
";

    fn t() -> Taxonomy {
        Taxonomy::parse(FIXTURE)
    }

    fn tag(t: &Taxonomy, id: &str) -> TagId {
        t.tag(id).unwrap()
    }

    fn required<'a>(t: &Taxonomy, name: &'a str) -> Ingredient<'a> {
        Ingredient::named(t, name, false)
    }

    fn optional<'a>(t: &Taxonomy, name: &'a str) -> Ingredient<'a> {
        Ingredient::named(t, name, true)
    }

    fn mine(t: &Taxonomy, ids: &[&str]) -> VisibleAllergies {
        VisibleAllergies {
            mine: ids.iter().map(|id| tag(t, id)).collect(),
            ..Default::default()
        }
    }

    fn equipment(codes: &[&str]) -> HashSet<String> {
        codes.iter().map(|c| c.to_string()).collect()
    }

    fn strings(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|c| c.to_string()).collect()
    }

    /// Excluded for a reader with `allergies` and `dislikes`, in a household
    /// that owns `owned`.
    fn out(
        t: &Taxonomy,
        allergies: &VisibleAllergies,
        dislikes: &[&str],
        avoid_allergens: bool,
        ingredients: &[Ingredient],
        needs: &[&str],
    ) -> bool {
        let dislikes: Vec<TagId> = dislikes.iter().map(|id| tag(t, id)).collect();
        let owned = equipment(&["oven", "blender"]);
        excluded(
            t,
            &HardFilter {
                allergies,
                dislikes: &dislikes,
                equipment: &owned,
                avoid_allergens,
            },
            ingredients,
            &strings(needs),
        )
    }

    // -- excluded ----------------------------------------------------------

    #[test]
    fn an_allergen_among_the_required_ingredients_excludes() {
        let t = t();
        let a = mine(&t, &["en:peanuts"]);
        assert!(out(
            &t,
            &a,
            &[],
            true,
            &[required(&t, "farine"), required(&t, "cacahuètes")],
            &[]
        ));
        assert!(!out(&t, &a, &[], true, &[required(&t, "farine")], &[]));
    }

    #[test]
    fn a_descendant_of_the_allergen_excludes_never_the_reverse() {
        let t = t();
        // Allergic to milk: semi-skimmed milk is milk.
        let milk = mine(&t, &["en:milks"]);
        assert!(out(
            &t,
            &milk,
            &[],
            true,
            &[required(&t, "lait demi-écrémé")],
            &[]
        ));
        // Allergic to nuts: peanuts are filed under them.
        let nuts = mine(&t, &["en:nuts"]);
        assert!(out(
            &t,
            &nuts,
            &[],
            true,
            &[required(&t, "cacahuètes")],
            &[]
        ));
        // Allergic to semi-skimmed milk only: plain milk does not satisfy it.
        let semi = mine(&t, &["en:semi-skimmed-milks"]);
        assert!(!out(&t, &semi, &[], true, &[required(&t, "lait")], &[]));
    }

    #[test]
    fn an_optional_allergen_does_not_exclude() {
        let t = t();
        let a = mine(&t, &["en:peanuts"]);
        assert!(!out(
            &t,
            &a,
            &[],
            true,
            &[required(&t, "farine"), optional(&t, "cacahuètes")],
            &[]
        ));
    }

    #[test]
    fn an_ingredient_without_a_tag_does_not_exclude() {
        let t = t();
        let a = mine(&t, &["en:peanuts"]);
        assert!(!out(
            &t,
            &a,
            &["en:mushrooms"],
            true,
            &[required(&t, "sauce maison")],
            &[]
        ));
    }

    #[test]
    fn public_and_anonymous_allergies_of_others_exclude_too() {
        let t = t();
        let public = VisibleAllergies {
            public: vec![("Léa".into(), tag(&t, "en:eggs"))],
            ..Default::default()
        };
        assert!(out(&t, &public, &[], true, &[required(&t, "œufs")], &[]));
        let anonymous = VisibleAllergies {
            anonymous: vec![tag(&t, "en:milks")],
            ..Default::default()
        };
        assert!(out(&t, &anonymous, &[], true, &[required(&t, "lait")], &[]));
    }

    #[test]
    fn unticking_avoid_allergens_lifts_the_allergies_only() {
        let t = t();
        let a = VisibleAllergies {
            mine: vec![tag(&t, "en:peanuts")],
            public: vec![("Léa".into(), tag(&t, "en:eggs"))],
            anonymous: vec![tag(&t, "en:milks")],
        };
        for ingredient in ["cacahuètes", "œufs", "lait"] {
            assert!(out(&t, &a, &[], true, &[required(&t, ingredient)], &[]));
            assert!(
                !out(&t, &a, &[], false, &[required(&t, ingredient)], &[]),
                "{ingredient}"
            );
        }
        // A dislike and a missing piece of equipment still exclude.
        assert!(out(
            &t,
            &a,
            &["en:mushrooms"],
            false,
            &[required(&t, "champignons")],
            &[]
        ));
        assert!(out(
            &t,
            &a,
            &[],
            false,
            &[required(&t, "farine")],
            &["thermomix"]
        ));
    }

    #[test]
    fn a_dislike_of_the_reader_excludes_by_category() {
        let t = t();
        let none = VisibleAllergies::default();
        assert!(out(
            &t,
            &none,
            &["en:milks"],
            true,
            &[required(&t, "lait demi-écrémé")],
            &[]
        ));
        assert!(!out(
            &t,
            &none,
            &["en:milks"],
            true,
            &[optional(&t, "lait")],
            &[]
        ));
        assert!(!out(
            &t,
            &none,
            &["en:milks"],
            true,
            &[required(&t, "farine")],
            &[]
        ));
    }

    #[test]
    fn missing_equipment_excludes_owned_equipment_does_not() {
        let t = t();
        let none = VisibleAllergies::default();
        let flour = [required(&t, "farine")];
        assert!(!out(&t, &none, &[], true, &flour, &[]));
        assert!(!out(&t, &none, &[], true, &flour, &["oven", "blender"]));
        assert!(out(&t, &none, &[], true, &flour, &["oven", "thermomix"]));
        // A recipe without ingredients is judged on its equipment too.
        assert!(out(&t, &none, &[], true, &[], &["thermomix"]));
    }

    #[test]
    fn nothing_to_weigh_excludes_nothing() {
        let t = t();
        let none = VisibleAllergies::default();
        assert!(!out(
            &t,
            &none,
            &[],
            true,
            &[required(&t, "cacahuètes"), required(&t, "lait")],
            &[]
        ));
    }

    // -- allergy_warning ---------------------------------------------------

    fn warn(t: &Taxonomy, a: &VisibleAllergies, ingredients: &[Ingredient]) -> AllergyWarning {
        allergy_warning(t, a, ingredients)
    }

    fn public(member: &str, tag: &str, label: &str) -> PublicAllergyMatch {
        PublicAllergyMatch {
            member: member.into(),
            tag: tag.into(),
            label: label.into(),
        }
    }

    fn allergen(tag: &str, label: &str) -> AllergenMatch {
        AllergenMatch {
            tag: tag.into(),
            label: label.into(),
        }
    }

    #[test]
    fn the_readers_own_allergies_are_named_by_their_tag() {
        let t = t();
        let a = mine(&t, &["en:milks", "en:peanuts", "en:eggs"]);
        let w = warn(
            &t,
            &a,
            &[
                required(&t, "lait demi-écrémé"),
                optional(&t, "cacahuètes"),
                required(&t, "farine"),
            ],
        );
        assert_eq!(
            w,
            AllergyWarning {
                mine: vec![
                    allergen("en:milks", "Laits"),
                    allergen("en:peanuts", "Cacahuètes"),
                ],
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_public_allergy_names_its_member_and_tag() {
        let t = t();
        let a = VisibleAllergies {
            public: vec![
                ("Zoé".into(), tag(&t, "en:eggs")),
                ("Léa".into(), tag(&t, "en:milks")),
                ("Léa".into(), tag(&t, "en:eggs")),
                ("Léa".into(), tag(&t, "en:peanuts")),
            ],
            ..Default::default()
        };
        let w = warn(&t, &a, &[required(&t, "œufs"), required(&t, "lait")]);
        assert_eq!(
            w,
            AllergyWarning {
                public: vec![
                    public("Léa", "en:eggs", "Œufs"),
                    public("Léa", "en:milks", "Laits"),
                    public("Zoé", "en:eggs", "Œufs"),
                ],
                ..Default::default()
            }
        );
    }

    #[test]
    fn an_anonymous_allergy_says_only_that_one_matches() {
        let t = t();
        let a = VisibleAllergies {
            anonymous: vec![tag(&t, "en:nuts"), tag(&t, "en:eggs")],
            ..Default::default()
        };
        assert_eq!(
            warn(&t, &a, &[required(&t, "cacahuètes")]),
            AllergyWarning {
                anonymous: true,
                ..Default::default()
            }
        );
        assert_eq!(
            warn(&t, &a, &[required(&t, "lait")]),
            AllergyWarning::default()
        );
    }

    #[test]
    fn the_reader_and_another_member_allergic_to_the_same() {
        let t = t();
        let a = VisibleAllergies {
            mine: vec![tag(&t, "en:peanuts")],
            public: vec![("Léa".into(), tag(&t, "en:peanuts"))],
            anonymous: vec![tag(&t, "en:peanuts")],
        };
        assert_eq!(
            warn(&t, &a, &[required(&t, "cacahuètes")]),
            AllergyWarning {
                mine: vec![allergen("en:peanuts", "Cacahuètes")],
                public: vec![public("Léa", "en:peanuts", "Cacahuètes")],
                anonymous: true,
                unverifiable: Vec::new(),
            }
        );
    }

    #[test]
    fn an_untagged_ingredient_is_unverifiable_only_when_an_allergy_is_visible() {
        let t = t();
        let untagged = [required(&t, "sauce maison"), required(&t, "farine")];
        // Nothing the reader may see: another member's private allergy
        // never reaches here, so it cannot set the flag.
        assert_eq!(
            warn(&t, &VisibleAllergies::default(), &untagged),
            AllergyWarning::default()
        );
        for a in [
            mine(&t, &["en:eggs"]),
            VisibleAllergies {
                public: vec![("Léa".into(), tag(&t, "en:eggs"))],
                ..Default::default()
            },
            VisibleAllergies {
                anonymous: vec![tag(&t, "en:eggs")],
                ..Default::default()
            },
        ] {
            assert_eq!(
                warn(&t, &a, &untagged),
                AllergyWarning {
                    unverifiable: strings(&["sauce maison"]),
                    ..Default::default()
                }
            );
            // Every ingredient tagged: verifiable.
            assert!(warn(&t, &a, &[required(&t, "farine")])
                .unverifiable
                .is_empty());
        }
        // An optional ingredient counts as much as a required one.
        let a = mine(&t, &["en:eggs"]);
        assert_eq!(
            warn(&t, &a, &[optional(&t, "sauce maison")]).unverifiable,
            strings(&["sauce maison"])
        );
    }

    #[test]
    fn the_unverifiable_ingredients_are_named_sorted_once_each() {
        // #825: the warning names what could not be checked, as the recipe
        // writes it.
        let t = t();
        let a = mine(&t, &["en:eggs"]);
        let w = warn(
            &t,
            &a,
            &[
                required(&t, "vinaigre balsamique"),
                required(&t, "farine"),
                optional(&t, "sauce soja"),
                required(&t, "sauce soja"),
            ],
        );
        assert_eq!(
            w.unverifiable,
            strings(&["sauce soja", "vinaigre balsamique"])
        );
    }

    #[test]
    fn a_recipe_without_ingredients_warns_of_nothing() {
        let t = t();
        let a = mine(&t, &["en:eggs"]);
        assert_eq!(warn(&t, &a, &[]), AllergyWarning::default());
    }
}
