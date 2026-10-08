//! One ingredient line as recipe sites write it in French (#405) — `200 g
//! de farine`, `1/2 citron`, `2 c. à soupe d'huile`, `sel` — split into
//! quantity, unit and name for the import's draft. A line this does not
//! understand is kept whole as the name, without quantity: the member
//! reviews every line before anything is saved.

use std::sync::LazyLock;

use manage_our_home_shared::dto::recipes::DraftIngredient;

/// `line` as quantity, unit and name: a quantity first (`200`, `1,5`,
/// `0.5`, `1/2`, `1 1/2`, `½`, `1½`, `un`, `une`), then an optional unit
/// from `UNITS` or a spoon (`c. à soupe`, `c. à café`), then an optional
/// `de` / `d'`, then the name. Anything else — no quantity, a range
/// (`2 à 3`, `2-3`), nothing left for the name — is kept whole as the name.
pub fn parse_line(line: &str) -> DraftIngredient {
    let line = line.trim();
    understood(line).unwrap_or_else(|| DraftIngredient {
        quantity: None,
        unit: None,
        name: line.to_string(),
    })
}

fn understood(line: &str) -> Option<DraftIngredient> {
    let (quantity, rest) = quantity(line)?;
    let rest = rest.trim_start();
    if is_range(rest) {
        return None;
    }
    let (unit, rest) = match unit(rest) {
        Some((unit, rest)) => (Some(unit.to_string()), rest.trim_start()),
        None => (None, rest),
    };
    let name = ["de ", "d'", "d’"]
        .iter()
        .find_map(|of| strip_prefix_ci(rest, of))
        .unwrap_or(rest)
        .trim();
    if name.is_empty() {
        return None;
    }
    Some(DraftIngredient {
        quantity: Some((quantity * 1000.0).round() / 1000.0),
        unit,
        name: name.to_string(),
    })
}

/// The vulgar fractions recipe sites use.
const FRACTIONS: [(char, f64); 6] = [
    ('½', 0.5),
    ('¼', 0.25),
    ('¾', 0.75),
    ('⅓', 1.0 / 3.0),
    ('⅔', 2.0 / 3.0),
    ('⅛', 0.125),
];

fn fraction(c: char) -> Option<f64> {
    FRACTIONS.iter().find(|(f, _)| *f == c).map(|&(_, v)| v)
}

/// The quantity `line` starts with, and what follows it. The quantity must
/// end at a space, a letter (`75g`) or the end of the line.
fn quantity(line: &str) -> Option<(f64, &str)> {
    for (word, value) in [("une", 1.0), ("un", 1.0)] {
        if let Some(rest) = strip_prefix_ci(line, word) {
            return rest
                .starts_with(char::is_whitespace)
                .then_some((value, rest));
        }
    }
    let (value, rest) = match line.chars().next().and_then(fraction) {
        Some(value) => (value, &line[line.chars().next()?.len_utf8()..]),
        None => number(line)?,
    };
    match rest.chars().next() {
        None => Some((value, rest)),
        Some(c) if c.is_whitespace() || c.is_alphabetic() => Some((value, rest)),
        _ => None,
    }
}

/// A decimal number (`.` or `,`), a fraction `a/b`, a whole number and a
/// fraction (`1 1/2`), or a whole number and a vulgar fraction (`1½`,
/// `1 ½`).
fn number(line: &str) -> Option<(f64, &str)> {
    let (whole, rest) = digits(line)?;
    if let Some(rest) = rest.strip_prefix('/') {
        let (denominator, rest) = digits(rest)?;
        return (denominator != 0.0).then(|| (whole / denominator, rest));
    }
    if let Some(decimals) = rest.strip_prefix(['.', ',']) {
        if let Some((_, after)) = digits(decimals) {
            let text = &line[..line.len() - after.len()];
            return Some((text.replace(',', ".").parse().ok()?, after));
        }
    }
    let spaced = rest.trim_start();
    if let Some(value) = spaced.chars().next().and_then(fraction) {
        return Some((whole + value, &spaced[spaced.chars().next()?.len_utf8()..]));
    }
    if spaced.len() < rest.len() {
        if let Some((numerator, after)) = digits(spaced) {
            if let Some(after) = after.strip_prefix('/') {
                let (denominator, after) = digits(after)?;
                return (denominator != 0.0).then(|| (whole + numerator / denominator, after));
            }
        }
    }
    Some((whole, rest))
}

/// The ASCII digits `text` starts with, as a number, and what follows.
fn digits(text: &str) -> Option<(f64, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    Some((text[..end].parse().ok()?, &text[end..]))
}

/// `à 3`, `a 3`, `ou 3`, `-3`: the line gives a range, not a quantity.
fn is_range(rest: &str) -> bool {
    let after = ["-", "à ", "a ", "ou "]
        .iter()
        .find_map(|sep| strip_prefix_ci(rest, sep));
    after.is_some_and(|a| a.trim_start().starts_with(|c: char| c.is_ascii_digit()))
}

/// Units read after a quantity: spellings, the unit written in the draft,
/// and whether a plural ending (`s`, `x`, `(s)`) may follow.
const UNITS: [(&[&str], &str, bool); 24] = [
    (
        &["kilogrammes", "kilogramme", "kilos", "kilo", "kg"],
        "kg",
        false,
    ),
    (&["grammes", "gramme", "gr", "g"], "g", false),
    (&["milligrammes", "milligramme", "mg"], "mg", false),
    (&["litres", "litre", "l"], "l", false),
    (&["décilitres", "décilitre", "dl"], "dl", false),
    (&["centilitres", "centilitre", "cl"], "cl", false),
    (&["millilitres", "millilitre", "ml"], "ml", false),
    (&["pincée", "pincee"], "pincée", true),
    (&["sachet"], "sachet", true),
    (&["tranche"], "tranche", true),
    (&["gousse"], "gousse", true),
    (&["boîte", "boite"], "boîte", true),
    (&["pot"], "pot", true),
    (&["verre"], "verre", true),
    (&["tasse"], "tasse", true),
    (&["brin"], "brin", true),
    (&["botte"], "botte", true),
    (&["feuille"], "feuille", true),
    (&["morceau"], "morceau", true),
    (&["poignée", "poignee"], "poignée", true),
    (&["goutte"], "goutte", true),
    (&["branche"], "branche", true),
    (&["paquet"], "paquet", true),
    (&["bol"], "bol", true),
];

/// Every spelling of the two spoons, with the unit written in the draft.
static SPOONS: LazyLock<Vec<(String, &'static str)>> = LazyLock::new(|| {
    let heads = [
        "cuillères",
        "cuillère",
        "cuilleres",
        "cuillere",
        "cuillerées",
        "cuillerée",
        "cuil.",
        "cuil",
        "c.",
        "c",
    ];
    let joins = [" à ", " a ", "à.", "a."];
    let mut spoons = Vec::new();
    for (tails, unit) in [
        (["soupe", "s.", "s"], "c. à soupe"),
        (["café", "cafe", "c"], "c. à café"),
    ] {
        for head in heads {
            for join in joins {
                for tail in tails {
                    spoons.push((format!("{head}{join}{tail}"), unit));
                }
            }
        }
    }
    for (compact, unit) in [
        ("càs", "c. à soupe"),
        ("cs", "c. à soupe"),
        ("càc", "c. à café"),
        ("cc", "c. à café"),
    ] {
        spoons.push((compact.to_string(), unit));
    }
    spoons
});

/// The unit `rest` starts with — the longest spelling that ends at a space
/// or at the end of the line — and what follows it.
fn unit(rest: &str) -> Option<(&'static str, &str)> {
    let mut best: Option<(usize, &'static str, &str)> = None;
    let mut consider = |spelling: &str, unit: &'static str, plural: bool| {
        let Some(mut after) = strip_prefix_ci(rest, spelling) else {
            return;
        };
        if plural {
            after = ["(s)", "s", "x"]
                .iter()
                .find_map(|ending| after.strip_prefix(ending))
                .unwrap_or(after);
        }
        let ends =
            spelling.ends_with('.') || after.is_empty() || after.starts_with(char::is_whitespace);
        let length = rest.len() - after.len();
        if ends && best.is_none_or(|(l, _, _)| length > l) {
            best = Some((length, unit, after));
        }
    };
    for (spellings, unit, plural) in UNITS {
        for spelling in spellings {
            consider(spelling, unit, plural);
        }
    }
    for (spelling, unit) in SPOONS.iter() {
        consider(spelling, unit, false);
    }
    best.map(|(_, unit, after)| (unit, after))
}

/// `text` without `prefix`, compared without regard to case.
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut chars = text.char_indices();
    let mut end = 0;
    for expected in prefix.chars() {
        let (at, c) = chars.next()?;
        if !c.to_lowercase().eq(expected.to_lowercase()) {
            return None;
        }
        end = at + c.len_utf8();
    }
    Some(&text[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(line: &str) -> (Option<f64>, Option<&'static str>, String) {
        let d = parse_line(line);
        // Leak the unit so the expectations below read as literals.
        let unit = d.unit.map(|u| &*Box::leak(u.into_boxed_str()));
        (d.quantity, unit, d.name)
    }

    fn is(line: &str, quantity: Option<f64>, unit: Option<&str>, name: &str) {
        let (q, u, n) = parsed(line);
        assert_eq!((q, u, n.as_str()), (quantity, unit, name), "{line:?}");
    }

    #[test]
    fn the_five_lines_of_the_issue() {
        is("200 g de farine", Some(200.0), Some("g"), "farine");
        is("1/2 citron", Some(0.5), None, "citron");
        is(
            "2 c. à soupe d'huile",
            Some(2.0),
            Some("c. à soupe"),
            "huile",
        );
        is("sel", None, None, "sel");
        is("1,5 l de lait", Some(1.5), Some("l"), "lait");
    }

    #[test]
    fn lines_from_the_real_page_fixtures() {
        is("5 oeufs", Some(5.0), None, "oeufs");
        is("500 g de farine", Some(500.0), Some("g"), "farine");
        is(
            "1 l de lait demi-écrémé",
            Some(1.0),
            Some("l"),
            "lait demi-écrémé",
        );
        is(
            "3 cuillères à soupe d'huile",
            Some(3.0),
            Some("c. à soupe"),
            "huile",
        );
        is("0.5 verre de bière", Some(0.5), Some("verre"), "bière");
        is(
            "1 poignée de haricots verts",
            Some(1.0),
            Some("poignée"),
            "haricots verts",
        );
        is(
            "1 c à s de sauce soja",
            Some(1.0),
            Some("c. à soupe"),
            "sauce soja",
        );
        is("75g de sucre", Some(75.0), Some("g"), "sucre");
        is("250g mascarpone", Some(250.0), Some("g"), "mascarpone");
        is("1/4 de reblochon", Some(0.25), None, "reblochon");
        is("3 gouttes de tabasco", Some(3.0), Some("goutte"), "tabasco");
        is("250 g farine", Some(250.0), Some("g"), "farine");
        is("½ litre lait", Some(0.5), Some("l"), "lait");
        is("1 pincée sel", Some(1.0), Some("pincée"), "sel");
        is("2 c à s sucre", Some(2.0), Some("c. à soupe"), "sucre");
        is("6 gousse(s) Ail", Some(6.0), Some("gousse"), "Ail");
        is("1 branche(s) Thym", Some(1.0), Some("branche"), "Thym");
        is("2 c. à soupe Huile", Some(2.0), Some("c. à soupe"), "Huile");
        is(
            "6 Saucisse(s) fumée(s)",
            Some(6.0),
            None,
            "Saucisse(s) fumée(s)",
        );
        is("Du sésame noir", None, None, "Du sésame noir");
        is("l Sel poivre", None, None, "l Sel poivre");
    }

    #[test]
    fn units_are_read_in_their_usual_spellings() {
        for (line, unit) in [
            ("1 kg de pommes de terre", "kg"),
            ("2 kilos de pommes de terre", "kg"),
            ("100 grammes de pommes de terre", "g"),
            ("100 gr de pommes de terre", "g"),
            ("10 cl de pommes de terre", "cl"),
            ("10 ml de pommes de terre", "ml"),
            ("2 dl de pommes de terre", "dl"),
            ("1 litre de pommes de terre", "l"),
            ("2 litres de pommes de terre", "l"),
            ("1 cuillère à soupe de pommes de terre", "c. à soupe"),
            ("1 cuillere a soupe de pommes de terre", "c. à soupe"),
            ("1 cuil. à soupe de pommes de terre", "c. à soupe"),
            ("1 càs de pommes de terre", "c. à soupe"),
            ("1 c.à.s de pommes de terre", "c. à soupe"),
            ("1 c. à s. de pommes de terre", "c. à soupe"),
            ("1 cs de pommes de terre", "c. à soupe"),
            ("1 cuillère à café de pommes de terre", "c. à café"),
            ("1 c. à café de pommes de terre", "c. à café"),
            ("1 c à c de pommes de terre", "c. à café"),
            ("1 càc de pommes de terre", "c. à café"),
            ("1 cc de pommes de terre", "c. à café"),
            ("2 sachets de pommes de terre", "sachet"),
            ("2 tranches de pommes de terre", "tranche"),
            ("2 boîtes de pommes de terre", "boîte"),
            ("2 pots de pommes de terre", "pot"),
            ("2 tasses de pommes de terre", "tasse"),
            ("2 brins de pommes de terre", "brin"),
            ("2 bottes de pommes de terre", "botte"),
            ("2 feuilles de pommes de terre", "feuille"),
            ("2 morceaux de pommes de terre", "morceau"),
            ("2 pincées de pommes de terre", "pincée"),
            ("2 Pincées de pommes de terre", "pincée"),
        ] {
            is(
                line,
                Some(line_quantity(line)),
                Some(unit),
                "pommes de terre",
            );
        }
    }

    fn line_quantity(line: &str) -> f64 {
        line.split(' ').next().unwrap().parse().unwrap()
    }

    #[test]
    fn a_unit_needs_a_word_boundary() {
        // `l` of `lardons`, `g` of `gésiers`, `pot` of `potiron`.
        is("200 lardons", Some(200.0), None, "lardons");
        is("300 gésiers", Some(300.0), None, "gésiers");
        is("1 potiron", Some(1.0), None, "potiron");
        is("2 cs", None, None, "2 cs");
    }

    #[test]
    fn quantities_in_every_written_form() {
        is("1 1/2 tasse de farine", Some(1.5), Some("tasse"), "farine");
        is("1½ tasse de farine", Some(1.5), Some("tasse"), "farine");
        is("1 ½ tasse de farine", Some(1.5), Some("tasse"), "farine");
        is("¼ de chou", Some(0.25), None, "chou");
        is("¾ l d'eau", Some(0.75), Some("l"), "eau");
        is("1/3 des nouilles", Some(0.333), None, "des nouilles");
        is("⅔ tasse de lait", Some(0.667), Some("tasse"), "lait");
        is("une pincée de sel", Some(1.0), Some("pincée"), "sel");
        is("un oignon", Some(1.0), None, "oignon");
        is("2 d’échalotes", Some(2.0), None, "échalotes");
    }

    #[test]
    fn a_line_that_is_not_understood_is_kept_whole() {
        for line in [
            "2 à 3 tomates",
            "2-3 tomates",
            "200 g",
            "1/0 citron",
            "Farine, panko (sorte de chapelure)",
            "Quelques raisins secs",
            "",
        ] {
            is(line, None, None, line);
        }
        is("  sel et poivre  ", None, None, "sel et poivre");
    }
}
