//! Reading the string a barcode decoder hands over (#402). Every decision
//! about `raw` is taken here, in Rust, or in `gs1` for a GS1 2D code
//! (#403): apps/web decodes the photo of the barcode on its server
//! (`routes::stocks::photo`) and passes the text on, and the manual
//! "Code-barres" field sends what was typed through the same path.
//!
//! Accepted: EAN-13, EAN-8 and UPC-A, each with a valid check digit. A UPC-A
//! is an EAN-13 with a leading `0` — the same product, which a decoder may
//! report under either form — so it is stored and looked up in its 13-digit
//! form, and a rescan matches whichever form the first scan produced.
//! UPC-E (8 digits whose check digit is computed on the expanded UPC-A) is
//! not accepted: read as an EAN-8 its check digit would be judged wrongly.

/// The normalized code for `raw`, or `None` when `raw` is not an EAN-13,
/// EAN-8 or UPC-A with a valid check digit. Whitespace anywhere is dropped
/// (a code typed by hand as printed under the bars, `3 017620 422003`);
/// any other character refuses the whole string.
pub fn normalize(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let code = match digits.len() {
        8 | 13 => digits,
        12 => format!("0{digits}"),
        _ => return None,
    };
    check_digit_holds(&code).then_some(code)
}

/// GS1's mod-10 check: from the rightmost digit leftwards, weights 1, 3, 1,
/// 3…, the check digit included (weight 1), and the sum a multiple of 10.
/// The same rule for every GTIN length: EAN-8, UPC-A, EAN-13 and the
/// GTIN-14 of a GS1 Digital Link (`gs1`).
pub(crate) fn check_digit_holds(code: &str) -> bool {
    let sum: u32 = code
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    sum.is_multiple_of(10)
}

/// Whether a normalized code is a store's in-house label (GS1 prefixes
/// 20–29 and 020–029, "restricted circulation" — the second, once
/// normalized, is a UPC-A of number system 2): the scales' weighed-goods stickers, which
/// encode an article number and a price or weight of the store's own
/// choosing. No public database knows them, and the same code names a
/// different product in the next shop, so Open Food Facts is not asked.
/// Only the 13-digit form is judged: EAN-8 restricted codes are not weighing
/// labels.
pub fn is_weighed(code: &str) -> bool {
    code.len() == 13 && (code.starts_with('2') || code.starts_with("02"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_ean_13_is_kept_as_is() {
        // Nutella 400 g, the code Open Food Facts documents its API with.
        assert_eq!(normalize("3017620422003").as_deref(), Some("3017620422003"));
    }

    #[test]
    fn a_wrong_check_digit_is_refused() {
        for bad in ["3017620422004", "3017620422000", "96385073"] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_valid_ean_8_is_kept_as_is() {
        assert_eq!(normalize("96385074").as_deref(), Some("96385074"));
    }

    #[test]
    fn a_upc_a_becomes_its_ean_13_form() {
        // 036000291452: the UPC-A of GS1 US's own examples.
        assert_eq!(normalize("036000291452").as_deref(), Some("0036000291452"));
        assert_eq!(normalize("036000291452"), normalize("0036000291452"));
    }

    #[test]
    fn a_upc_a_with_a_wrong_check_digit_is_refused() {
        assert_eq!(normalize("036000291453"), None);
    }

    #[test]
    fn whitespace_typed_between_the_digit_groups_is_dropped() {
        assert_eq!(
            normalize("  3 017620 422003\n").as_deref(),
            Some("3017620422003")
        );
    }

    #[test]
    fn anything_but_digits_is_refused() {
        for bad in [
            "",
            "   ",
            "301762042200a",
            "3017620-422003",
            "https://example.test/3017620422003",
            "+3017620422003",
            "３０１７６２０４２２００３",
        ] {
            assert_eq!(normalize(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn other_lengths_are_refused() {
        // 7, 9, 11, 14 digits: a GTIN-14 or a truncated read.
        for bad in ["9638507", "963850730", "03600029145", "03017620422003"] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_store_weighing_label_is_recognised_by_its_prefix_2() {
        // 2 + article + weight/price + check digit, as a scale prints it.
        let label = normalize("2123456012347").expect("a valid EAN-13");
        assert!(is_weighed(&label));
        assert!(is_weighed("2900000000001"));
    }

    #[test]
    fn a_product_code_is_not_a_weighing_label() {
        assert!(!is_weighed("3017620422003"));
        assert!(!is_weighed("0036000291452"));
        // An EAN-8 is never judged a weighing label, prefix 2 or not.
        assert!(!is_weighed("20000004"));
    }

    #[test]
    fn a_upc_a_of_number_system_2_is_a_weighing_label_too() {
        // GS1 prefixes 020–029: restricted circulation, the UPC-A of a
        // scale's sticker (number system 2), stored as 02… once normalized.
        let label = normalize("212345678992").expect("a valid UPC-A");
        assert_eq!(label, "0212345678992");
        assert!(is_weighed(&label));
        // Prefixes 00–01 and 03–09 are ordinary products.
        assert!(!is_weighed("0012345678905"));
        assert!(!is_weighed("0312345678906"));
    }
}
