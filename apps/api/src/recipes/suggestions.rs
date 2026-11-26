use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::auth::session::{scoped_tx, AuthUser};
use crate::error::AppResult;
use crate::groups::require_role;
use crate::recipes::dietary_filter::{
    allergy_warning, excluded, household_equipment, reader_dislikes, visible_allergies,
    AllergyWarning, HardFilter, Ingredient,
};
use crate::recipes::taste::{taste_score, Rated, Taste};
use crate::recipes::taxonomy::{same_ingredient_name, taxonomy, TagId, Taxonomy};
use crate::stocks::ingredient_links::{family_links, link_key};
use crate::stocks::items::today;
use crate::AppState;

/// Rule-based suggestion algorithm (architecture.md: "a concrete v1 rule,
/// not ML"). Scores each family recipe on three inputs named in idea.md,
/// and a fourth, the household's taste (#821):
///
/// 1. **Stock match** — how many of the recipe's required (non-optional)
///    ingredients are currently in stock. Dominant term (0-100 points):
///    lets the family cook what they already have.
/// 2. **Variety** — a flat penalty if the recipe was logged in
///    `meal_history` within the last [`VARIETY_WINDOW_DAYS`] (28 since
///    #821, 14 before), so the same meal doesn't get resuggested right
///    after being eaten.
/// 3. **Season** — a small bonus per ingredient whose `seasonal_months`
///    includes the current month, nudging suggestions toward what's in
///    season without overriding the stock-match term.
/// 4. **Taste** — the « Aimé » / « Pas aimé » given when a recipe was
///    marked done, on the recipe and, weaker, on the recipes sharing its
///    ingredient categories ([`taste_score`]), bounded by
///    [`crate::recipes::taste::TASTE_MAX`] (15). Without such a verdict
///    (« Neutre », #822) the term is 0 and the score is the three others'.
///
/// Ingredient/stock matching goes through Open Food Facts' category
/// taxonomy (#406, `recipes::taxonomy`): an article covers an ingredient
/// when its category is the ingredient's or descends from it. An article
/// takes its categories from the family's link for its name (#903), else
/// from its scanned product (`off_products`), else from its name; an
/// ingredient from its name. Equal names (case-insensitive,
/// trimmed) match too, whether or not either side has a category. It stays a
/// heuristic, not an exact reservation check (unit conversion is out of
/// scope, same as the low_stock heuristic in stocks::items).
///
/// Before the scoring, a hard filter (#818, `recipes::dietary_filter`)
/// keeps out what the reader may not be proposed: their dislikes, the
/// equipment the household lacks and, unless `avoid_allergens=false`, the
/// allergies they may see. It changes no score of what it keeps.
pub(crate) const VARIETY_WINDOW_DAYS: i64 = 28;
pub(crate) const RECENCY_PENALTY: f64 = 30.0;
const SEASONAL_BONUS: f64 = 5.0;

struct RecipeRow {
    id: Uuid,
    name: String,
    required_equipment: Vec<String>,
    duration_minutes: Option<i32>,
}

struct IngredientRow {
    recipe_id: Uuid,
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    is_optional: bool,
    seasonal_months: Option<Vec<i32>>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct MissingIngredient {
    pub name: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RecipeSuggestion {
    pub recipe_id: Uuid,
    pub name: String,
    pub score: f64,
    pub matched_ingredients: usize,
    pub total_required_ingredients: usize,
    pub missing_ingredients: Vec<MissingIngredient>,
    pub recently_eaten: bool,
    pub last_eaten_on: Option<NaiveDate>,
    /// What the recipe holds of the allergies the reader may see (#818).
    pub allergy_warning: AllergyWarning,
    /// How long the recipe takes, when it says (#539), for the page of the
    /// replacements (#816).
    pub duration_minutes: Option<i32>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestionsQuery {
    pub limit: Option<i64>,
    /// `true` keeps only the recipes the stock covers entirely
    /// ([`is_possible`], #539), before `limit` applies.
    #[serde(default)]
    pub possible: bool,
    /// `true` (the default) keeps out the recipes holding an allergy the
    /// reader may see (#818); `false` lifts that, for this request only.
    /// Dislikes and equipment exclude either way.
    #[serde(default = "avoid_allergens_default")]
    pub avoid_allergens: bool,
}

pub(crate) fn avoid_allergens_default() -> bool {
    true
}

#[utoipa::path(
    get,
    path = "/groups/{id}/recipes/suggestions",
    tag = "recipes",
    params(
        ("id" = uuid::Uuid, Path, description = "Group id"),
        SuggestionsQuery,
    ),
    responses(
        (status = 200, description = "Recipes ranked by what the stock covers", body = crate::api_docs::schemas::SuggestionList),
        crate::api_docs::MemberErrors,
    ),
    security(("session_cookie" = [])),
)]
pub async fn suggest_recipes(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(group_id): Path<Uuid>,
    Query(query): Query<SuggestionsQuery>,
) -> AppResult<impl IntoResponse> {
    let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;
    require_role(&mut tx, group_id, auth.user_id).await?;
    let mut suggestions =
        score_recipes(&mut tx, group_id, auth.user_id, query.avoid_allergens).await?;
    tx.commit().await?;

    if query.possible {
        suggestions.retain(|s| is_possible(s.matched_ingredients, s.total_required_ingredients));
    }
    suggestions.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

    let limit = query.limit.unwrap_or(20).max(0) as usize;
    suggestions.truncate(limit);

    Ok(Json(json!({ "suggestions": suggestions })))
}

/// Every recipe of the group the hard filter (#818) leaves to `reader`,
/// scored, in the order the rows came in. Shared by the suggestions and the
/// replacements of a planned selection (#815).
pub(crate) async fn score_recipes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    reader: Uuid,
    avoid_allergens: bool,
) -> AppResult<Vec<RecipeSuggestion>> {
    let recipe_rows = sqlx::query_as!(
        RecipeRow,
        "SELECT id, name, required_equipment, duration_minutes FROM recipes WHERE group_id = $1",
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?;

    let ingredient_rows = sqlx::query_as!(
        IngredientRow,
        r#"
        SELECT recipe_id, name, quantity, unit, is_optional, seasonal_months
        FROM recipe_ingredients
        WHERE group_id = $1
        "#,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?;

    let taxonomy = taxonomy();
    let stock: Vec<StockEntry> = tagged_stock(tx, group_id)
        .await?
        .into_iter()
        .filter(|a| a.quantity > 0.0)
        .map(|a| a.entry)
        .collect();

    let recent_meals: Vec<(Uuid, NaiveDate)> = sqlx::query!(
        r#"
        SELECT recipe_id, MAX(eaten_on) as "last_eaten_on!"
        FROM meal_history
        WHERE group_id = $1 AND eaten_on >= $3::date - $2::bigint * INTERVAL '1 day'
        GROUP BY recipe_id
        "#,
        group_id,
        VARIETY_WINDOW_DAYS,
        today(),
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.recipe_id, r.last_eaten_on))
    .collect();
    let recent_meals: HashMap<Uuid, NaiveDate> = recent_meals.into_iter().collect();

    // Every meal's verdict, whenever it was eaten: « Aimé » minus « Pas
    // aimé », per recipe (#821).
    let verdicts: Vec<(Uuid, i64)> = sqlx::query!(
        r#"
        SELECT recipe_id,
               COUNT(*) FILTER (WHERE taste = $2)
                 - COUNT(*) FILTER (WHERE taste = $3) AS "net!"
        FROM meal_history
        WHERE group_id = $1
        GROUP BY recipe_id
        "#,
        group_id,
        Taste::Aime.as_str(),
        Taste::PasAime.as_str(),
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .filter(|r| r.net != 0)
    .map(|r| (r.recipe_id, r.net))
    .collect();

    let allergies = visible_allergies(tx, group_id, reader).await?;
    let dislikes = reader_dislikes(tx, reader).await?;
    let equipment = household_equipment(tx, group_id).await?;
    let filter = HardFilter {
        allergies: &allergies,
        dislikes: &dislikes,
        equipment: &equipment,
        avoid_allergens,
    };

    let mut ingredients_by_recipe: HashMap<Uuid, Vec<&IngredientRow>> = HashMap::new();
    for ing in &ingredient_rows {
        ingredients_by_recipe
            .entry(ing.recipe_id)
            .or_default()
            .push(ing);
    }

    // A recipe's ingredient categories, for the taste term: the canonical
    // tag of each required ingredient the taxonomy knows.
    let categories: HashMap<Uuid, HashSet<TagId>> = recipe_rows
        .iter()
        .map(|recipe| {
            let tags = ingredients_by_recipe
                .get(&recipe.id)
                .into_iter()
                .flatten()
                .filter(|i| !i.is_optional)
                .filter_map(|i| taxonomy.canonical_ingredient(&i.name))
                .collect();
            (recipe.id, tags)
        })
        .collect();
    let no_categories = HashSet::new();
    let rated: Vec<Rated<'_, TagId>> = verdicts
        .iter()
        .map(|&(recipe_id, net)| Rated {
            recipe_id,
            net,
            categories: categories.get(&recipe_id).unwrap_or(&no_categories),
        })
        .collect();

    // The month of today in Paris, like the variety window: on the first
    // day of a month, the UTC month is still the previous one until 1–2 a.m.
    let current_month = today().month() as i32;

    let suggestions: Vec<RecipeSuggestion> = recipe_rows
        .into_iter()
        .filter_map(|recipe| {
            let ingredients = ingredients_by_recipe
                .get(&recipe.id)
                .cloned()
                .unwrap_or_default();
            let tagged: Vec<Ingredient> = ingredients
                .iter()
                .map(|i| Ingredient::named(taxonomy, &i.name, i.is_optional))
                .collect();
            if excluded(taxonomy, &filter, &tagged, &recipe.required_equipment) {
                return None;
            }

            let required: Vec<&&IngredientRow> =
                ingredients.iter().filter(|i| !i.is_optional).collect();
            let covered: Vec<bool> = required
                .iter()
                .map(|i| in_stock(&i.name, taxonomy.canonical_ingredient(&i.name), &stock))
                .collect();
            let matched = covered.iter().filter(|&&c| c).count();
            let missing_ingredients: Vec<MissingIngredient> = required
                .iter()
                .zip(&covered)
                .filter(|(_, &c)| !c)
                .map(|(i, _)| MissingIngredient {
                    name: i.name.clone(),
                    quantity: i.quantity,
                    unit: i.unit.clone(),
                })
                .collect();

            let match_ratio = if required.is_empty() {
                1.0
            } else {
                matched as f64 / required.len() as f64
            };

            let last_eaten_on = recent_meals.get(&recipe.id).copied();
            let recently_eaten = last_eaten_on.is_some();

            let seasonal_bonus = ingredients
                .iter()
                .filter(|i| {
                    i.seasonal_months
                        .as_ref()
                        .is_some_and(|months| months.contains(&current_month))
                })
                .count() as f64
                * SEASONAL_BONUS;

            let taste = taste_score(
                recipe.id,
                categories.get(&recipe.id).unwrap_or(&no_categories),
                &rated,
            );

            let score = match_ratio * 100.0 - if recently_eaten { RECENCY_PENALTY } else { 0.0 }
                + seasonal_bonus
                + taste;

            Some(RecipeSuggestion {
                recipe_id: recipe.id,
                name: recipe.name,
                score,
                matched_ingredients: matched,
                total_required_ingredients: required.len(),
                missing_ingredients,
                recently_eaten,
                last_eaten_on,
                allergy_warning: allergy_warning(taxonomy, &allergies, &tagged),
                duration_minutes: recipe.duration_minutes,
            })
        })
        .collect();
    Ok(suggestions)
}

/// An article in stock as the matching sees it: its name, trimmed and
/// lowercased, and the categories it belongs to, each with all its
/// ancestors (empty when it has none).
pub(crate) struct StockEntry {
    pub(crate) name: String,
    pub(crate) tags: HashSet<TagId>,
}

/// The categories come first from the family's link for the article's name
/// (`linked`, #903, `stocks::ingredient_links`), when the taxonomy knows
/// its category; otherwise from Open Food Facts' `categories_tags` of a
/// scanned article (#402), the ones the taxonomy knows; otherwise from the
/// article's name.
pub(crate) fn stock_entry(
    taxonomy: &Taxonomy,
    name: &str,
    linked: Option<&str>,
    off_categories: &[String],
) -> StockEntry {
    let mut direct: Vec<TagId> = match linked.and_then(|id| taxonomy.tag(id)) {
        Some(tag) => vec![tag],
        None => off_categories
            .iter()
            .filter_map(|id| taxonomy.tag(id))
            .collect(),
    };
    if direct.is_empty() {
        direct.extend(taxonomy.canonical_ingredient(name));
    }
    StockEntry {
        name: name.trim().to_lowercase(),
        tags: direct
            .into_iter()
            .flat_map(|tag| taxonomy.ancestors_or_self(tag))
            .collect(),
    }
}

/// A stock line of the group with its categories ([`stock_entry`]).
pub(crate) struct StockArticle {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) unit: String,
    pub(crate) quantity: f64,
    pub(crate) expires_on: Option<NaiveDate>,
    pub(crate) entry: StockEntry,
}

/// Every stock line of the group, empty ones included, with its categories:
/// those of the family's link for its name (#903), else those of its
/// scanned product, else those of its name ([`stock_entry`]).
/// Shared by the suggestions and what the planned recipes lack
/// (`availability::planned_availability`, #900).
pub(crate) async fn tagged_stock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<StockArticle>> {
    // `off_products` is the public Open Food Facts cache (no RLS, see
    // migration 0026); the family's rows come from `stock_items`. A cached
    // record past its TTL still names the product's categories.
    let taxonomy = taxonomy();
    let links = family_links(tx, group_id).await?;
    Ok(sqlx::query!(
        r#"
        SELECT s.id, s.name, s.unit, s.quantity, s.expires_on,
               o.categories_tags AS "categories_tags?"
        FROM stock_items s
        LEFT JOIN off_products o ON o.code = s.barcode
        WHERE s.group_id = $1
        "#,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| StockArticle {
        entry: stock_entry(
            taxonomy,
            &r.name,
            links.get(&link_key(&r.name)).map(String::as_str),
            &r.categories_tags.unwrap_or_default(),
        ),
        id: r.id,
        name: r.name,
        unit: r.unit,
        quantity: r.quantity,
        expires_on: r.expires_on,
    })
    .collect())
}

impl StockEntry {
    /// Whether the article covers a recipe's ingredient of this name and
    /// tag: by category when both sides have one ([`Taxonomy::satisfies`]);
    /// by the same name, trimmed and case-insensitive, always; and, when
    /// one side has no category, by the same name normalized without its
    /// accents (#1023, [`same_ingredient_name`]).
    pub(crate) fn covers(&self, ingredient_name: &str, ingredient_tag: Option<TagId>) -> bool {
        ingredient_tag.is_some_and(|tag| self.tags.contains(&tag))
            || same_ingredient_name(
                ingredient_name,
                &self.name,
                ingredient_tag.is_some() && !self.tags.is_empty(),
            )
    }
}

/// Whether some article in stock covers a recipe's ingredient
/// ([`StockEntry::covers`]).
fn in_stock(ingredient_name: &str, ingredient_tag: Option<TagId>, stock: &[StockEntry]) -> bool {
    stock
        .iter()
        .any(|item| item.covers(ingredient_name, ingredient_tag))
}

/// Whether a recipe can be cooked from the stock (#539): it names at least
/// one required ingredient, and the stock covers every one of them.
/// Optional ingredients never count. A recipe with no required ingredient
/// says nothing of what it takes from the stock, so it is not « possible ».
fn is_possible(matched: usize, total_required: usize) -> bool {
    total_required > 0 && matched == total_required
}

/// How many recipes a planned selection is offered in its place (#815).
pub(crate) const REPLACEMENTS_MAX: usize = 3;

/// The recipes offered in place of a planned one (#815): neither the
/// replaced recipe nor a recipe already planned, the best score first;
/// equal scores by name, then id, so that the offer does not depend on the
/// order the rows came in. At most [`REPLACEMENTS_MAX`], none when no
/// other recipe is left.
pub(crate) fn replacement_candidates(
    ranked: Vec<RecipeSuggestion>,
    replaced: Uuid,
    planned: &HashSet<Uuid>,
) -> Vec<RecipeSuggestion> {
    let mut candidates: Vec<RecipeSuggestion> = ranked
        .into_iter()
        .filter(|s| s.recipe_id != replaced && !planned.contains(&s.recipe_id))
        .collect();
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.recipe_id.cmp(&b.recipe_id))
    });
    candidates.truncate(REPLACEMENTS_MAX);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- replacement_candidates (#815) -------------------------------------

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn suggestion(n: u128, name: &str, score: f64) -> RecipeSuggestion {
        RecipeSuggestion {
            recipe_id: id(n),
            name: name.to_string(),
            score,
            matched_ingredients: 0,
            total_required_ingredients: 0,
            missing_ingredients: vec![],
            recently_eaten: false,
            last_eaten_on: None,
            allergy_warning: AllergyWarning::default(),
            duration_minutes: None,
        }
    }

    fn offered(ranked: Vec<RecipeSuggestion>, replaced: u128, planned: &[u128]) -> Vec<u128> {
        let planned: HashSet<Uuid> = planned.iter().copied().map(id).collect();
        replacement_candidates(ranked, id(replaced), &planned)
            .into_iter()
            .map(|s| s.recipe_id.as_u128())
            .collect()
    }

    #[test]
    fn neither_the_replaced_recipe_nor_a_planned_one_is_offered() {
        let ranked = vec![
            suggestion(1, "Gratin", 100.0),
            suggestion(2, "Soupe", 90.0),
            suggestion(3, "Tarte", 80.0),
            suggestion(4, "Quiche", 70.0),
        ];
        // 1 is replaced, 1 and 3 are planned.
        assert_eq!(offered(ranked, 1, &[1, 3]), [2, 4]);
    }

    #[test]
    fn the_best_score_comes_first_whatever_the_order_given() {
        let ranked = vec![
            suggestion(1, "Gratin", 10.0),
            suggestion(2, "Soupe", 95.0),
            suggestion(3, "Tarte", -20.0),
            suggestion(4, "Quiche", 50.0),
        ];
        assert_eq!(offered(ranked, 9, &[9]), [2, 4, 1]);
    }

    #[test]
    fn equal_scores_go_by_name_then_id() {
        let ranked = vec![
            suggestion(3, "Tarte", 60.0),
            suggestion(5, "Gratin", 60.0),
            suggestion(4, "Gratin", 60.0),
        ];
        assert_eq!(offered(ranked, 9, &[9]), [4, 5, 3]);
    }

    #[test]
    fn at_most_three_are_offered() {
        let ranked: Vec<_> = (1..=6)
            .map(|n| suggestion(n, &format!("R{n}"), 100.0 - n as f64))
            .collect();
        assert_eq!(offered(ranked, 9, &[9]), [1, 2, 3]);
        assert_eq!(REPLACEMENTS_MAX, 3);
    }

    #[test]
    fn fewer_or_none_when_few_recipes_are_left() {
        let ranked = vec![suggestion(1, "Gratin", 50.0), suggestion(2, "Soupe", 40.0)];
        assert_eq!(offered(ranked, 1, &[1]), [2]);
        let ranked = vec![suggestion(1, "Gratin", 50.0), suggestion(2, "Soupe", 40.0)];
        assert!(offered(ranked, 1, &[1, 2]).is_empty());
        assert!(offered(vec![], 1, &[1]).is_empty());
    }

    fn entry(name: &str, off: &[&str]) -> StockEntry {
        linked_entry(name, None, off)
    }

    fn linked_entry(name: &str, link: Option<&str>, off: &[&str]) -> StockEntry {
        let off: Vec<String> = off.iter().map(|s| s.to_string()).collect();
        stock_entry(taxonomy(), name, link, &off)
    }

    // -- the family's link first (#903) --------------------------------------

    #[test]
    fn a_link_gives_a_category_to_a_name_the_taxonomy_does_not_know() {
        // The issue's example: « mon p'tit plaisir » is crème fraîche.
        assert!(!covered(
            "crème fraîche",
            &[entry("Mon p'tit plaisir", &[])]
        ));
        let stock = [linked_entry(
            "Mon p'tit plaisir",
            Some("en:fermented-creams"),
            &[],
        )];
        assert!(covered("crème fraîche", &stock));
        // With the category's ancestors: crème covers « crème ».
        assert!(covered("crème", &stock));
        assert!(!covered("beurre", &stock));
    }

    #[test]
    fn a_link_comes_before_the_scanned_categories_and_the_name() {
        // Scanned as milk, linked to oat drinks: the link wins, the scan's
        // categories no longer count.
        let stock = [linked_entry(
            "Boisson",
            Some("en:oat-based-drinks"),
            &["en:milks"],
        )];
        assert!(!covered("lait", &stock));
        // A name the taxonomy reads as milk, linked elsewhere: the link wins.
        let stock = [linked_entry("Lait", Some("en:butters"), &[])];
        assert!(covered("beurre", &stock));
        assert!(!covered("lait demi-écrémé", &stock));
        // The same name still matches by name, link or not.
        assert!(covered("lait", &stock));
    }

    #[test]
    fn a_link_to_a_category_the_taxonomy_lost_is_no_link() {
        let stock = [linked_entry(
            "Lait demi-écrémé Lactel",
            Some("en:gone"),
            &["en:semi-skimmed-milks"],
        )];
        assert!(covered("lait", &stock));
        let stock = [linked_entry("Lait", Some("en:gone"), &[])];
        assert!(covered("laits", &stock));
    }

    fn covered(ingredient: &str, stock: &[StockEntry]) -> bool {
        in_stock(
            ingredient,
            taxonomy().canonical_ingredient(ingredient),
            stock,
        )
    }

    // -- by name, normalized (#1023) -----------------------------------------

    #[test]
    fn a_name_the_taxonomy_lacks_finds_its_accent_and_plural_variant() {
        assert_eq!(taxonomy().canonical_ingredient("tomme fermiere"), None);
        assert!(covered("tomme fermiere", &[entry("Tommes fermières", &[])]));
        assert!(!covered("tomme fermiere", &[entry("Tommes", &[])]));
    }

    #[test]
    fn two_categories_decide_over_the_name_without_accents() {
        // `pâtes` is en:pastas, `Pâtés` en:pate.
        assert!(taxonomy().canonical_ingredient("pâtes").is_some());
        assert!(!entry("Pâtés", &[]).tags.is_empty());
        assert!(!covered("pâtes", &[entry("Pâtés", &[])]));
    }

    #[test]
    fn the_same_name_lowercased_still_covers() {
        assert!(covered("Tomme fermière", &[entry(" tomme FERMIÈRE ", &[])]));
    }

    #[test]
    fn scanned_article_satisfies_its_ancestor_category() {
        // The issue's example: a scanned "Lait demi-écrémé Lactel" carries
        // Open Food Facts' categories down to en:semi-skimmed-milks.
        let stock = [entry(
            "Lait demi-écrémé Lactel",
            &["en:dairies", "en:milks", "en:semi-skimmed-milks"],
        )];
        assert!(covered("lait", &stock));
        assert!(covered("Lait demi-écrémé", &stock));
        assert!(!covered("lait de coco", &stock));
        assert!(!covered("beurre", &stock));
    }

    #[test]
    fn article_without_categories_is_tagged_from_its_name() {
        let stock = [entry("Lait demi-écrémé", &[])];
        assert!(covered("lait", &stock));
        // Never the reverse: plain milk does not cover semi-skimmed.
        let stock = [entry("Lait", &[])];
        assert!(!covered("lait demi-écrémé", &stock));
        assert!(covered("Laits", &stock));
    }

    #[test]
    fn unknown_categories_fall_back_to_the_name() {
        let stock = [entry("Oeufs", &["xx:not-a-category"])];
        assert!(covered("œufs", &stock));
    }

    #[test]
    fn category_or_name_when_both_sides_have_a_category() {
        // Categories that diverge, names equal: the name the family typed
        // wins (arbitrage of 2026-10-09: the name stays a fallback even
        // when both sides have a category). Guards against a return to a
        // category-only rule.
        let stock = [entry("Lait", &["en:oat-based-drinks"])];
        assert!(covered("lait", &stock));
        // Category satisfied, names different: the category covers it.
        let stock = [entry("Lait demi-écrémé Lactel", &["en:semi-skimmed-milks"])];
        assert!(covered("lait", &stock));
        // Categories that diverge, names different: no match.
        let stock = [entry("Boisson avoine", &["en:oat-based-drinks"])];
        assert!(!covered("lait", &stock));
    }

    #[test]
    fn name_match_when_either_side_has_no_category() {
        // Neither side in the taxonomy: the previous name match.
        let stock = [entry("  Sauce Maison ", &[])];
        assert!(covered("sauce maison", &stock));
        assert!(!covered("sauce", &stock));
        // Only the article has a category: name match too.
        let stock = [entry("Lait", &[])];
        assert!(!covered("lait entier bio de la ferme", &stock));
        let stock = [entry("lait entier bio de la ferme", &[])];
        assert!(covered("Lait entier bio de la ferme", &stock));
    }

    #[test]
    fn possible_when_every_required_ingredient_is_in_stock() {
        assert!(is_possible(2, 2));
        assert!(is_possible(1, 1));
    }

    #[test]
    fn not_possible_when_a_required_ingredient_is_missing() {
        assert!(!is_possible(1, 2));
        assert!(!is_possible(0, 3));
    }

    #[test]
    fn not_possible_without_a_required_ingredient() {
        // Nothing to take from the stock: the recipe says nothing about
        // what it needs, so it is not offered as cookable from it.
        assert!(!is_possible(0, 0));
    }

    #[test]
    fn nothing_in_stock_covers_nothing() {
        assert!(!covered("lait", &[]));
    }
}
