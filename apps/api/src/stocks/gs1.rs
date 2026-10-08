//! Reading a GS1 2D code (#403): a GS1 DataMatrix or QR code carries an
//! element string — Application Identifiers (AI) followed by their data —
//! and a "GS1 Digital Link" QR code carries the same data as a URL. Unlike
//! an EAN-13, both may carry a date: `(17)` the expiry date ("à consommer
//! jusqu'au"), `(15)` the best-before date ("à consommer de préférence
//! avant"). What is read here:
//!
//! - the GTIN `(01)`, 14 digits, brought back to the code an EAN/UPC scan
//!   of the same product gives (`gtin_to_code`), so that it follows the same
//!   path (Open Food Facts, the family's article carrying the code);
//! - the date, `(17)` before `(15)` when both are there.
//!
//! Every other AI, the lot `(10)` among them, is stepped over. Nothing else
//! of the string is kept.

use chrono::{Datelike, NaiveDate};

use crate::stocks::barcode;

/// The Group Separator (ASCII 29): what a decoder hands over for the FNC1
/// that ends a variable-length element.
const GS: char = '\u{1d}';

/// What a GS1 code says: the product code, in the form `barcode::normalize`
/// gives it, and the date it carries, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gs1Read {
    pub code: String,
    pub expires_on: Option<NaiveDate>,
}

/// `raw` read as a GS1 element string or a GS1 Digital Link URL, `None` when
/// it is neither, carries no GTIN, or carries one that is not a consumer
/// unit's EAN/UPC (`gtin_to_code`). `today` places the two-digit years.
/// Whitespace around the string is dropped, as `barcode::normalize` does;
/// the FNC1 separator (ASCII 29) is not whitespace and stays.
pub fn read(raw: &str, today: NaiveDate) -> Option<Gs1Read> {
    let raw = raw.trim();
    let fields = if is_url(raw) {
        digital_link(raw)?
    } else {
        element_string(raw)?
    };
    let code = gtin_to_code(fields.gtin.as_deref()?)?;
    // (17) before (15); an unreadable (17) does not hide a readable (15).
    let expires_on = [fields.expiry, fields.best_before]
        .into_iter()
        .flatten()
        .find_map(|date| gs1_date(&date, today));
    Some(Gs1Read { code, expires_on })
}

/// The values of the AIs read here, as the code carries them.
#[derive(Default)]
struct Fields {
    gtin: Option<String>,
    expiry: Option<String>,
    best_before: Option<String>,
}

impl Fields {
    /// Keeps the first value of each AI read here; ignores every other AI.
    fn set(&mut self, ai: &str, value: &str) {
        let slot = match ai {
            "01" => &mut self.gtin,
            "17" => &mut self.expiry,
            "15" => &mut self.best_before,
            _ => return,
        };
        slot.get_or_insert_with(|| value.to_string());
    }
}

/// The length, AI included, of the elements whose length GS1 predefines,
/// by the AI's first two digits (General Specifications, "element strings
/// with predefined length"). Every other element is ended by an FNC1 —
/// a `GS` once decoded — or by the end of the string.
fn predefined_length(prefix: &str) -> Option<usize> {
    Some(match prefix {
        "00" => 20,
        "01" | "02" | "03" => 16,
        "04" => 18,
        "11" | "12" | "13" | "14" | "15" | "16" | "17" | "18" | "19" => 8,
        "20" => 4,
        "31" | "32" | "33" | "34" | "35" | "36" => 10,
        "41" => 16,
        _ => return None,
    })
}

/// An element string as a decoder hands it over: an optional symbology
/// identifier (`]d2` DataMatrix, `]Q3` QR, `]C1` GS1-128), an optional
/// leading FNC1, then the elements. `None` when an element is cut short or
/// does not start with an AI.
fn element_string(raw: &str) -> Option<Fields> {
    let mut rest = match raw.strip_prefix(']') {
        Some(identified) => identified.get(2..)?,
        None => raw,
    };
    let mut fields = Fields::default();
    loop {
        // A separator after a predefined-length element is allowed too.
        rest = rest.trim_start_matches(GS);
        if rest.is_empty() {
            break;
        }
        let prefix = rest
            .get(..2)
            .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))?;
        let element = match predefined_length(prefix) {
            Some(len) => rest
                .get(..len)
                .filter(|e| e.bytes().all(|b| b.is_ascii_digit()))?,
            None => rest.split(GS).next().unwrap_or(rest),
        };
        fields.set(prefix, &element[2..]);
        rest = &rest[element.len()..];
    }
    Some(fields)
}

fn is_url(raw: &str) -> bool {
    let scheme = raw.split_once("://").map(|(scheme, _)| scheme);
    scheme.is_some_and(|s| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"))
}

/// A GS1 Digital Link URI: `…/01/<gtin>[/10/<lot>…][?17=YYMMDD&15=…]`, on
/// any domain. The GTIN is the segment after the last `01` segment that a
/// GTIN follows — 8, 12, 13 or 14 digits whose check digit holds — so a
/// path prefix may hold a `01` of its own (`/2026/01/galette/01/<gtin>`),
/// the reading apps/web's `photo::is_gs1_2d` makes of the same URL. A GTIN
/// of 8, 12 or 13 digits is padded to 14. Only the numeric AI keys of the
/// query are read.
fn digital_link(raw: &str) -> Option<Fields> {
    let raw = raw.split('#').next().unwrap_or(raw);
    let (path, query) = raw.split_once('?').unwrap_or((raw, ""));
    let (_, after_scheme) = path.split_once("://")?;
    // The first segment is the host.
    let segments: Vec<&str> = after_scheme.split('/').skip(1).collect();
    let gtin = segments
        .windows(2)
        .rev()
        .find(|pair| pair[0] == "01" && is_gtin(pair[1]))
        .map(|pair| pair[1])?;
    let mut fields = Fields::default();
    fields.set("01", &format!("{gtin:0>14}"));
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            fields.set(key, value);
        }
    }
    Some(fields)
}

/// 8, 12, 13 or 14 digits whose GS1 check digit holds.
fn is_gtin(segment: &str) -> bool {
    matches!(segment.len(), 8 | 12 | 13 | 14)
        && segment.bytes().all(|b| b.is_ascii_digit())
        && barcode::check_digit_holds(segment)
}

/// The date of a GS1 `YYMMDD` field. The century follows GS1's sliding
/// window (General Specifications, "Determination of century in dates"):
/// a year 51 to 99 years ahead of `today`'s is in the previous century, one
/// 50 to 99 years behind it in the next, any other in the current one. A
/// day `00` is the last day of the month. A date that does not exist is
/// `None`.
pub fn gs1_date(yymmdd: &str, today: NaiveDate) -> Option<NaiveDate> {
    if yymmdd.len() != 6 || !yymmdd.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = |at: usize| yymmdd[at..at + 2].parse::<i32>().ok();
    let (yy, month, dd) = (number(0)?, number(2)?, number(4)?);
    let current = today.year().rem_euclid(100);
    let century = today.year() - current;
    let year = yy
        + match yy - current {
            51.. => century - 100,
            ..=-50 => century + 100,
            _ => century,
        };
    let month = u32::try_from(month).ok()?;
    if dd == 0 {
        // The day before the first of the next month.
        if !(1..=12).contains(&month) {
            return None;
        }
        let (year, month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        return NaiveDate::from_ymd_opt(year, month, 1)?.pred_opt();
    }
    NaiveDate::from_ymd_opt(year, month, u32::try_from(dd).ok()?)
}

/// The product code behind a GTIN-14: the leading `0` (indicator digit of a
/// consumer unit) dropped gives the EAN-13 form `barcode::normalize` stores
/// (a GTIN-12 padded to 14 keeps one more `0`, as a UPC-A does there), and a
/// GTIN-8 padded to 14 gives back its EAN-8. `None` for an indicator digit
/// other than `0` (a case or a pallet, which no product database lists),
/// a wrong length or check digit.
pub fn gtin_to_code(gtin: &str) -> Option<String> {
    if gtin.len() != 14 || !gtin.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let thirteen = gtin.strip_prefix('0')?;
    match thirteen.strip_prefix("00000") {
        Some(eight) => barcode::normalize(eight),
        None => barcode::normalize(thirteen),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The day these tests are run "on".
    fn today() -> NaiveDate {
        day(2026, 10, 8)
    }

    // --- dates ---

    #[test]
    fn a_yymmdd_date_is_read() {
        assert_eq!(gs1_date("270131", today()), Some(day(2027, 1, 31)));
        assert_eq!(gs1_date("261008", today()), Some(day(2026, 10, 8)));
    }

    #[test]
    fn day_00_is_the_last_day_of_the_month() {
        assert_eq!(gs1_date("270200", today()), Some(day(2027, 2, 28)));
        assert_eq!(gs1_date("280200", today()), Some(day(2028, 2, 29)));
        assert_eq!(gs1_date("261200", today()), Some(day(2026, 12, 31)));
        assert_eq!(gs1_date("270400", today()), Some(day(2027, 4, 30)));
    }

    #[test]
    fn the_century_follows_the_sliding_window() {
        // 50 years ahead: still this century.
        assert_eq!(gs1_date("760101", today()), Some(day(2076, 1, 1)));
        // 51 years ahead: the previous one.
        assert_eq!(gs1_date("770101", today()), Some(day(1977, 1, 1)));
        // Behind, within 49 years: this century.
        assert_eq!(gs1_date("000101", today()), Some(day(2000, 1, 1)));
        // Late in a century, a small year is in the next one.
        assert_eq!(gs1_date("010101", day(2098, 6, 1)), Some(day(2101, 1, 1)));
        assert_eq!(gs1_date("480101", day(2098, 6, 1)), Some(day(2148, 1, 1)));
        assert_eq!(gs1_date("490101", day(2098, 6, 1)), Some(day(2049, 1, 1)));
    }

    #[test]
    fn a_date_that_does_not_exist_is_none() {
        for bad in [
            "270230", "270132", "271301", "270001", "2701", "2701311", "27O131", "", "+70131",
        ] {
            assert_eq!(gs1_date(bad, today()), None, "{bad:?}");
        }
    }

    // --- GTIN ---

    #[test]
    fn a_gtin_14_with_a_leading_zero_is_its_ean_13() {
        assert_eq!(
            gtin_to_code("03017620422003").as_deref(),
            Some("3017620422003")
        );
    }

    #[test]
    fn a_padded_upc_a_keeps_the_form_a_upc_a_scan_gives() {
        assert_eq!(
            gtin_to_code("00036000291452"),
            barcode::normalize("036000291452")
        );
    }

    #[test]
    fn a_padded_gtin_8_is_its_ean_8() {
        assert_eq!(gtin_to_code("00000096385074").as_deref(), Some("96385074"));
    }

    #[test]
    fn a_case_or_a_wrong_gtin_is_none() {
        for bad in [
            // Indicator digit 1: a case of the product, not the product.
            "13017620422000",
            // Wrong check digit.
            "03017620422004",
            // Lengths.
            "3017620422003",
            "003017620422003",
            "0301762042200a",
        ] {
            assert_eq!(gtin_to_code(bad), None, "{bad}");
        }
    }

    // --- element strings ---

    #[test]
    fn gtin_and_expiry_date_are_read_from_an_element_string() {
        assert_eq!(
            read("010301762042200317270131", today()),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: Some(day(2027, 1, 31)),
            })
        );
    }

    #[test]
    fn a_lot_ended_by_a_separator_is_stepped_over() {
        // (01)(10)LOT-42<GS>(17): the lot is variable-length, the FNC1 ends it.
        assert_eq!(
            read("010301762042200310LOT-42\u{1d}17270131", today()),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: Some(day(2027, 1, 31)),
            })
        );
    }

    #[test]
    fn a_lot_at_the_end_needs_no_separator() {
        assert_eq!(
            read("01030176204220031527011510A1B2", today()),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: Some(day(2027, 1, 15)),
            })
        );
    }

    #[test]
    fn the_expiry_date_wins_over_the_best_before_date() {
        // Either order.
        let expected = Some(Gs1Read {
            code: "3017620422003".into(),
            expires_on: Some(day(2027, 1, 31)),
        });
        assert_eq!(read("01030176204220031527011517270131", today()), expected);
        assert_eq!(read("01030176204220031727013115270115", today()), expected);
    }

    #[test]
    fn a_best_before_date_alone_is_used() {
        assert_eq!(
            read("01030176204220031527011510LOT", today()).and_then(|r| r.expires_on),
            Some(day(2027, 1, 15))
        );
    }

    #[test]
    fn an_invalid_date_leaves_the_code_and_no_date() {
        assert_eq!(
            read("010301762042200317271332", today()),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: None,
            })
        );
        // An unreadable (17) does not hide a readable (15).
        assert_eq!(
            read("01030176204220031727133215270115", today()).and_then(|r| r.expires_on),
            Some(day(2027, 1, 15))
        );
    }

    #[test]
    fn the_first_occurrence_of_an_ai_wins() {
        // Two (17), two (01): the first of each is read.
        assert_eq!(
            read("010301762042200317270131172801310100036000291452", today()),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: Some(day(2027, 1, 31)),
            })
        );
        assert_eq!(
            read(
                "https://id.gs1.org/01/03017620422003?17=270131&17=280131",
                today()
            )
            .and_then(|r| r.expires_on),
            Some(day(2027, 1, 31))
        );
    }

    #[test]
    fn whitespace_around_the_string_is_dropped() {
        let expected = Some(Gs1Read {
            code: "3017620422003".into(),
            expires_on: Some(day(2027, 1, 31)),
        });
        assert_eq!(
            read(
                " https://id.gs1.org/01/03017620422003?17=270131 \n",
                today()
            ),
            expected
        );
        assert_eq!(read("  010301762042200317270131\n", today()), expected);
    }

    #[test]
    fn a_gtin_alone_is_read_without_a_date() {
        assert_eq!(
            read("0103017620422003", today()),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: None,
            })
        );
    }

    #[test]
    fn a_leading_fnc1_a_symbology_identifier_and_a_separator_after_a_fixed_element_are_tolerated() {
        let expected = Some(Gs1Read {
            code: "3017620422003".into(),
            expires_on: Some(day(2027, 1, 31)),
        });
        assert_eq!(read("\u{1d}010301762042200317270131", today()), expected);
        assert_eq!(read("]d2010301762042200317270131", today()), expected);
        assert_eq!(read("]Q3010301762042200317270131", today()), expected);
        assert_eq!(read("0103017620422003\u{1d}17270131", today()), expected);
    }

    #[test]
    fn other_fixed_and_variable_elements_are_stepped_over() {
        // (11) production date, (3103) net weight in kg, (21) serial, then (17).
        assert_eq!(
            read(
                "0103017620422003112609013103000400\u{1d}21SER1\u{1d}17270131",
                today()
            )
            .and_then(|r| r.expires_on),
            Some(day(2027, 1, 31))
        );
    }

    #[test]
    fn a_string_without_a_usable_gtin_is_none() {
        for bad in [
            // No (01).
            "17270131",
            "10LOT",
            // (01) cut short.
            "01030176204220",
            // A case (indicator 1).
            "011301762042200017270131",
            // A fixed element cut short.
            "010301762042200317270",
            // Not an element string at all.
            "",
            "hello",
            "3017620422003",
        ] {
            assert_eq!(read(bad, today()), None, "{bad:?}");
        }
    }

    // --- Digital Link ---

    #[test]
    fn a_digital_link_url_gives_its_gtin_and_date() {
        assert_eq!(
            read(
                "https://id.gs1.org/01/03017620422003/10/LOT42?17=270131",
                today()
            ),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: Some(day(2027, 1, 31)),
            })
        );
    }

    #[test]
    fn a_digital_link_on_a_brand_domain_with_a_path_prefix_is_read() {
        assert_eq!(
            read(
                "HTTPS://example.com/p/01/3017620422003?15=270115&17=270131#x",
                today()
            ),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: Some(day(2027, 1, 31)),
            })
        );
    }

    #[test]
    fn a_path_prefix_holding_a_01_segment_is_stepped_over() {
        // `01` as a month: the GTIN is behind the `01` a GTIN follows, as
        // apps/web's `is_gs1_2d` judges the URL.
        assert_eq!(
            read(
                "https://brand.com/2026/01/galette/01/03017620422003?17=270131",
                today()
            ),
            Some(Gs1Read {
                code: "3017620422003".into(),
                expires_on: Some(day(2027, 1, 31)),
            })
        );
        // A lot that reads `01` after the GTIN changes nothing.
        assert_eq!(
            read("https://id.gs1.org/01/03017620422003/10/01", today()).map(|r| r.code),
            Some("3017620422003".into())
        );
    }

    #[test]
    fn a_digital_link_without_a_date_or_with_a_short_gtin_is_read() {
        assert_eq!(
            read("http://id.gs1.org/01/036000291452", today()),
            Some(Gs1Read {
                code: "0036000291452".into(),
                expires_on: None,
            })
        );
        assert_eq!(
            read("https://id.gs1.org/01/03017620422003?15=270115", today())
                .and_then(|r| r.expires_on),
            Some(day(2027, 1, 15))
        );
    }

    #[test]
    fn a_url_that_is_no_digital_link_is_none() {
        for bad in [
            "https://example.com/",
            "https://example.com/products/3017620422003",
            "https://id.gs1.org/01/",
            "https://id.gs1.org/01/03017620422004?17=270131",
            "https://id.gs1.org/01/abc",
            "ftp://id.gs1.org/01/03017620422003",
        ] {
            assert_eq!(read(bad, today()), None, "{bad}");
        }
    }
}
