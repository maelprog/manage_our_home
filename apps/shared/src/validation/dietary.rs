//! Pure checks for a member's food constraints and a household's kitchen
//! equipment (#817), used by `apps/api/src/recipes/constraints.rs` and
//! `crud.rs`, and later by `apps/web`'s forms (#819). Written test-first per
//! `.claude/CLAUDE.md`'s TDD process.
//!
//! - A tag names an Open Food Facts category (`en:peanuts`): the same tag
//!   space as `recipes::taxonomy` in apps/api, which decides whether the tag
//!   is *known*. This module only checks its shape: [`validate_tag`].
//! - [`ShareLevel`] is the per-family choice a member makes for one of their
//!   allergies: `public`, `anonymous`, `private` — `private` by default.
//! - [`EQUIPMENT`] is the closed list of equipment codes a household may own
//!   and a recipe may require; [`validate_equipment`] checks a list of them.

/// The longest tag accepted, in bytes. The longest category of the embedded
/// taxonomy (`apps/api/data/off-categories-fr.tsv.gz`) is 127 bytes.
pub const TAG_MAX_LEN: usize = 128;

/// Why a tag, a share level or a list of equipment codes was refused. Each
/// maps to the API's 400 code through [`DietaryError::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DietaryError {
    TagRequired,
    TagTooLong,
    TagMalformed,
    InvalidShare,
    UnknownEquipment,
    DuplicateEquipment,
}

impl DietaryError {
    /// The API's error code.
    pub fn code(&self) -> &'static str {
        match self {
            DietaryError::TagRequired => "tag_required",
            DietaryError::TagTooLong => "tag_too_long",
            DietaryError::TagMalformed => "invalid_tag",
            DietaryError::InvalidShare => "invalid_share",
            DietaryError::UnknownEquipment => "unknown_equipment",
            DietaryError::DuplicateEquipment => "duplicate_equipment",
        }
    }
}

/// Who, in one family, learns of one of a member's allergies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShareLevel {
    /// The member's name and the allergy, to the whole family.
    Public,
    /// That someone in the family has an allergy; nothing else is shown.
    Anonymous,
    /// Nothing, to no one.
    #[default]
    Private,
}

impl ShareLevel {
    /// The value stored in `allergy_sharing.share` and sent on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            ShareLevel::Public => "public",
            ShareLevel::Anonymous => "anonymous",
            ShareLevel::Private => "private",
        }
    }

    /// Reads a share level as sent on the wire: exactly one of the three
    /// values [`ShareLevel::as_str`] writes.
    pub fn parse(value: &str) -> Result<ShareLevel, DietaryError> {
        match value {
            "public" => Ok(ShareLevel::Public),
            "anonymous" => Ok(ShareLevel::Anonymous),
            "private" => Ok(ShareLevel::Private),
            _ => Err(DietaryError::InvalidShare),
        }
    }
}

/// The equipment codes a household may own and a recipe may require, with
/// their French label. Closed: a code outside it is refused.
pub const EQUIPMENT: &[(&str, &str)] = &[
    ("oven", "Four"),
    ("microwave", "Micro-ondes"),
    ("hob", "Plaques de cuisson"),
    ("blender", "Mixeur"),
    ("food_processor", "Robot culinaire"),
    ("stand_mixer", "Batteur sur socle"),
    ("pressure_cooker", "Autocuiseur"),
    ("slow_cooker", "Mijoteuse"),
    ("steamer", "Cuiseur vapeur"),
    ("air_fryer", "Friteuse sans huile"),
    ("deep_fryer", "Friteuse"),
    ("grill", "Gril"),
    ("barbecue", "Barbecue"),
    ("bread_maker", "Machine à pain"),
    ("ice_cream_maker", "Sorbetière"),
];

/// The French label of an equipment code, `None` for a code outside
/// [`EQUIPMENT`].
pub fn equipment_label(code: &str) -> Option<&'static str> {
    EQUIPMENT
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, label)| *label)
}

/// The fourteen allergens of annex II of regulation (EU) 1169/2011 (INCO),
/// in its order, each with the category of the embedded taxonomy that
/// stands for it and its French name (#819). The account page offers them
/// first. The taxonomy names food categories, not substances: gluten is
/// the cereals (`en:cereals-and-their-products`, which rice and maize fall
/// under too), and sulphites, which no category names, are the wines, where
/// a label most often declares them. `recipes::constraints`' tests in
/// apps/api check that each tag is a category of the embedded taxonomy.
pub const INCO_ALLERGENS: &[(&str, &str)] = &[
    ("en:cereals-and-their-products", "Gluten"),
    ("en:crustaceans", "Crustacés"),
    ("en:eggs", "Œufs"),
    ("en:fishes", "Poissons"),
    ("en:peanuts", "Arachides"),
    ("en:soy-beans", "Soja"),
    ("en:milks", "Lait"),
    ("en:nuts", "Fruits à coque"),
    ("en:celery", "Céleri"),
    ("en:mustards", "Moutarde"),
    ("en:sesame", "Sésame"),
    ("en:wines", "Sulfites"),
    ("en:lupin-beans", "Lupin"),
    ("en:mollusc", "Mollusques"),
];

/// The French name of an INCO allergen's tag, `None` for any other tag.
pub fn inco_label(tag: &str) -> Option<&'static str> {
    INCO_ALLERGENS
        .iter()
        .find(|(t, _)| *t == tag)
        .map(|(_, label)| *label)
}

/// One equipment a form offers to tick (#819).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquipmentChoice {
    pub code: &'static str,
    pub label: &'static str,
    /// Ticked when the form is drawn.
    pub ticked: bool,
    /// The household has it.
    pub owned: bool,
}

/// The equipment a recipe's form offers: the household's, and whatever the
/// recipe already requires that the household lacks, so that saving the
/// form does not drop it. In [`EQUIPMENT`]'s order; a code outside it is
/// left out.
pub fn equipment_choices(household: &[String], ticked: &[String]) -> Vec<EquipmentChoice> {
    EQUIPMENT
        .iter()
        .filter_map(|(code, label)| {
            let owned = household.iter().any(|c| c == code);
            let ticked = ticked.iter().any(|c| c == code);
            (owned || ticked).then_some(EquipmentChoice {
                code,
                label,
                ticked,
                owned,
            })
        })
        .collect()
}

/// Checks a tag's shape and returns it trimmed: a language prefix of two
/// lowercase ASCII letters, a colon, then a name with no whitespace, no
/// control character and no upper-case letter, the whole at most
/// [`TAG_MAX_LEN`] bytes.
pub fn validate_tag(tag: &str) -> Result<&str, DietaryError> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Err(DietaryError::TagRequired);
    }
    if tag.len() > TAG_MAX_LEN {
        return Err(DietaryError::TagTooLong);
    }
    let Some((lang, name)) = tag.split_once(':') else {
        return Err(DietaryError::TagMalformed);
    };
    let lang_ok = lang.len() == 2 && lang.bytes().all(|b| b.is_ascii_lowercase());
    let name_ok = !name.is_empty()
        && name
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !c.is_uppercase());
    if lang_ok && name_ok {
        Ok(tag)
    } else {
        Err(DietaryError::TagMalformed)
    }
}

/// Checks a list of equipment codes: every one in [`EQUIPMENT`], none twice.
/// Returns them sorted, so that the same set is always stored and answered
/// the same way.
pub fn validate_equipment(codes: &[String]) -> Result<Vec<String>, DietaryError> {
    let mut sorted: Vec<String> = Vec::with_capacity(codes.len());
    for code in codes {
        if equipment_label(code).is_none() {
            return Err(DietaryError::UnknownEquipment);
        }
        if sorted.contains(code) {
            return Err(DietaryError::DuplicateEquipment);
        }
        sorted.push(code.clone());
    }
    sorted.sort();
    Ok(sorted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(list: &[&str]) -> Vec<String> {
        list.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn a_category_tag_is_accepted_and_trimmed() {
        assert_eq!(validate_tag("en:peanuts"), Ok("en:peanuts"));
        assert_eq!(validate_tag("  en:milks \n"), Ok("en:milks"));
        assert_eq!(validate_tag("fr:crème-fraîche"), Ok("fr:crème-fraîche"));
    }

    #[test]
    fn an_empty_tag_is_required() {
        assert_eq!(validate_tag(""), Err(DietaryError::TagRequired));
        assert_eq!(validate_tag("   "), Err(DietaryError::TagRequired));
    }

    #[test]
    fn a_tag_longer_than_the_bound_is_refused() {
        let at_bound = format!("en:{}", "a".repeat(TAG_MAX_LEN - 3));
        assert_eq!(validate_tag(&at_bound), Ok(at_bound.as_str()));
        let above = format!("en:{}", "a".repeat(TAG_MAX_LEN - 2));
        assert_eq!(validate_tag(&above), Err(DietaryError::TagTooLong));
    }

    #[test]
    fn a_tag_without_its_language_prefix_or_name_is_malformed() {
        for tag in [
            "peanuts",
            ":peanuts",
            "e:peanuts",
            "eng:peanuts",
            "EN:peanuts",
            "en:",
            "1n:peanuts",
        ] {
            assert_eq!(validate_tag(tag), Err(DietaryError::TagMalformed), "{tag}");
        }
    }

    #[test]
    fn a_tag_with_a_space_a_control_or_a_capital_is_malformed() {
        for tag in [
            "en:pea nuts",
            "en:pea\tnuts",
            "en:pea\u{7}nuts",
            "en:Peanuts",
        ] {
            assert_eq!(
                validate_tag(tag),
                Err(DietaryError::TagMalformed),
                "{tag:?}"
            );
        }
    }

    #[test]
    fn the_three_share_levels_read_back_as_written() {
        for level in [
            ShareLevel::Public,
            ShareLevel::Anonymous,
            ShareLevel::Private,
        ] {
            assert_eq!(ShareLevel::parse(level.as_str()), Ok(level));
        }
        assert_eq!(ShareLevel::Public.as_str(), "public");
        assert_eq!(ShareLevel::Anonymous.as_str(), "anonymous");
        assert_eq!(ShareLevel::Private.as_str(), "private");
    }

    #[test]
    fn private_is_the_default_share_level() {
        assert_eq!(ShareLevel::default(), ShareLevel::Private);
    }

    #[test]
    fn any_other_share_level_is_refused() {
        for value in ["", "Public", " public", "secret", "anonyme"] {
            assert_eq!(
                ShareLevel::parse(value),
                Err(DietaryError::InvalidShare),
                "{value:?}"
            );
        }
    }

    #[test]
    fn known_equipment_codes_are_accepted_and_sorted() {
        assert_eq!(
            validate_equipment(&codes(&["oven", "blender"])),
            Ok(codes(&["blender", "oven"]))
        );
        assert_eq!(validate_equipment(&[]), Ok(Vec::new()));
    }

    #[test]
    fn an_equipment_code_outside_the_list_is_refused() {
        assert_eq!(
            validate_equipment(&codes(&["oven", "teleporter"])),
            Err(DietaryError::UnknownEquipment)
        );
        assert_eq!(
            validate_equipment(&codes(&["Oven"])),
            Err(DietaryError::UnknownEquipment)
        );
    }

    #[test]
    fn an_equipment_code_given_twice_is_refused() {
        assert_eq!(
            validate_equipment(&codes(&["oven", "blender", "oven"])),
            Err(DietaryError::DuplicateEquipment)
        );
    }

    #[test]
    fn every_listed_code_is_unique_has_a_label_and_is_snake_case() {
        assert!(!EQUIPMENT.is_empty());
        for (i, (code, label)) in EQUIPMENT.iter().enumerate() {
            assert!(
                !EQUIPMENT[..i].iter().any(|(c, _)| c == code),
                "{code} listed twice"
            );
            assert!(
                code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{code}"
            );
            assert!(!label.trim().is_empty(), "{code}");
            assert_eq!(equipment_label(code), Some(*label));
        }
        assert_eq!(equipment_label("teleporter"), None);
    }

    #[test]
    fn the_fourteen_inco_allergens_each_have_one_tag_and_a_label() {
        assert_eq!(INCO_ALLERGENS.len(), 14);
        for (i, (tag, label)) in INCO_ALLERGENS.iter().enumerate() {
            assert_eq!(validate_tag(tag), Ok(*tag), "{label}");
            assert!(
                !INCO_ALLERGENS[..i].iter().any(|(t, _)| t == tag),
                "{tag} listed twice"
            );
            assert!(
                !INCO_ALLERGENS[..i].iter().any(|(_, l)| l == label),
                "{label} listed twice"
            );
            assert_eq!(inco_label(tag), Some(*label));
        }
        assert_eq!(inco_label("en:peanuts"), Some("Arachides"));
        assert_eq!(inco_label("en:kiwifruits"), None);
    }

    #[test]
    fn the_equipment_offered_is_the_households_and_what_is_ticked_in_list_order() {
        let choices = equipment_choices(&codes(&["oven", "hob"]), &codes(&["barbecue", "oven"]));
        assert_eq!(
            choices,
            vec![
                EquipmentChoice {
                    code: "oven",
                    label: "Four",
                    ticked: true,
                    owned: true
                },
                EquipmentChoice {
                    code: "hob",
                    label: "Plaques de cuisson",
                    ticked: false,
                    owned: true
                },
                EquipmentChoice {
                    code: "barbecue",
                    label: "Barbecue",
                    ticked: true,
                    owned: false
                },
            ]
        );
    }

    #[test]
    fn an_unknown_code_is_never_offered() {
        let choices = equipment_choices(&codes(&["teleporter"]), &codes(&["Oven"]));
        assert!(choices.is_empty(), "{choices:?}");
        assert!(equipment_choices(&[], &[]).is_empty());
    }

    #[test]
    fn each_error_has_its_api_code() {
        assert_eq!(DietaryError::TagRequired.code(), "tag_required");
        assert_eq!(DietaryError::TagTooLong.code(), "tag_too_long");
        assert_eq!(DietaryError::TagMalformed.code(), "invalid_tag");
        assert_eq!(DietaryError::InvalidShare.code(), "invalid_share");
        assert_eq!(DietaryError::UnknownEquipment.code(), "unknown_equipment");
        assert_eq!(
            DietaryError::DuplicateEquipment.code(),
            "duplicate_equipment"
        );
    }
}
