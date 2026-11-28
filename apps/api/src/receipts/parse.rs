//! The text Tesseract reads off a till receipt (#1037), turned into the
//! store, the date, the total and the articles bought — for the import's
//! recap, which the member reviews before anything reaches the stocks or
//! the budget.
//!
//! A ticket mixes a header (store, address, phone, date), the articles,
//! the discounts, the sub-total and the total, then the payment, the VAT
//! table and the loyalty points. Only the articles and the total are read;
//! everything after the total is left out, save the date. A line that looks
//! like part of an article but cannot be read whole is reported in
//! `unreadable`, never completed by guess: the OCR misreads a `0` as an `O`
//! often enough that a price is better missing than invented.

use chrono::NaiveDate;
use manage_our_home_shared::validation::stocks::parse_pack_size;

/// What `parse_receipt` read off a ticket.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedReceipt {
    /// The first line of the header that reads as a name.
    pub store: Option<String>,
    /// The first valid date anywhere on the ticket (`28/11/2026`,
    /// `21/11/26`, `05-10-2026`, `05.10.2026`).
    pub purchased_on: Option<NaiveDate>,
    /// The amount on the first total line (`TOTAL`, `NET A PAYER`…), in
    /// cents.
    pub total_cents: Option<i64>,
    /// The articles, in ticket order; empty when none could be read.
    pub items: Vec<ReceiptItem>,
    /// The lines that belong to the articles but could not be read whole,
    /// as printed (cut to `SHOWN` characters).
    pub unreadable: Vec<String>,
    /// The total less the sum of the articles' prices, in cents, when the
    /// two differ — `None` when they agree or when no total was read. A
    /// positive gap is money paid for something `items` misses.
    pub total_mismatch: Option<i64>,
}

/// One article of the ticket.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptItem {
    /// The name as printed, its spaces collapsed, without the count that
    /// may open it (`3 X CITRON` names `CITRON`).
    pub label: String,
    /// The count (`2 x 1,25`, `3 X CITRON`) or the weight (`0,432 kg x
    /// 3,90 €/kg`); 1 when the ticket gives neither.
    pub quantity: f64,
    /// The metric unit of a weighed or measured article, written the way
    /// `parse_pack_size` writes it (`kg`, `g`, `L`, `cl`…); `None` for a
    /// count, which the stock form takes as `stocks::DEFAULT_UNIT`.
    pub unit: Option<String>,
    /// What the line costs, in cents, less the discounts printed under it.
    pub price_cents: i64,
}

/// How many characters of an unreadable line are kept.
const SHOWN: usize = 200;

/// Reads `text`, a ticket as the OCR returns it, line by line:
///
/// - an article ends with its price (`LAIT 1L  0,95 A`: a VAT code and a
///   currency may follow), or stands alone with its price on the next line;
/// - a count or a weight (`2 x 1,25`, `0,432 kg x 3,90 €/kg  1,68`) goes
///   with the article on the same line, with a name alone on the line above
///   it, with the article above it whose price it gives, or else with the
///   article below it whose price it gives;
/// - a discount (`REMISE`, `PROMO`…, or a negative amount) comes off the
///   article above it;
/// - the sub-total, the VAT, the payment, the loyalty points, the phone and
///   the courtesy lines are left out; the total ends the articles.
///
/// Never fails: an empty or meaningless text gives an empty receipt.
pub fn parse_receipt(text: &str) -> ParsedReceipt {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut state = State::default();
    for (at, line) in lines.iter().enumerate() {
        if state.receipt.purchased_on.is_none() {
            state.receipt.purchased_on = find_date(line);
        }
        if state.after_total {
            continue;
        }
        state.read(line, classify(line), lines.get(at + 1).copied());
    }
    state.finish()
}

#[derive(Default)]
struct State<'a> {
    receipt: ParsedReceipt,
    /// Whether the last article already took a count or a weight.
    last_detailed: bool,
    /// A name alone on its line, waiting for its price.
    pending_label: Option<&'a str>,
    /// A count or a weight that went with no article yet, and its line.
    pending_detail: Option<(Detail, &'a str)>,
    after_total: bool,
}

impl<'a> State<'a> {
    /// Whether the articles have begun: before, a name alone on its line is
    /// part of the header and is dropped silently.
    fn in_articles(&self) -> bool {
        !self.receipt.items.is_empty()
    }

    fn report(&mut self, line: &str) {
        let shown = match line.char_indices().nth(SHOWN) {
            Some((at, _)) => format!("{}…", &line[..at]),
            None => line.to_string(),
        };
        self.receipt.unreadable.push(shown);
    }

    fn flush_label(&mut self) {
        if let Some(label) = self.pending_label.take() {
            if self.in_articles() {
                self.report(label);
            }
        }
    }

    fn flush_detail(&mut self) {
        if let Some((_, line)) = self.pending_detail.take() {
            self.report(line);
        }
    }

    fn push(&mut self, item: ReceiptItem, detailed: bool) {
        self.receipt.items.push(item);
        self.last_detailed = detailed;
    }

    /// An article without a count of its own takes the pending one when
    /// that one gives its price; a pending count that does not is reported.
    fn push_article(&mut self, mut item: ReceiptItem, detailed: bool) {
        let mut detailed = detailed;
        if let Some((detail, line)) = self.pending_detail.take() {
            if !detailed && detail.gives(item.price_cents) {
                item.quantity = detail.quantity;
                item.unit = detail.unit;
                detailed = true;
            } else {
                self.report(line);
            }
        }
        self.push(item, detailed);
    }

    fn read(&mut self, line: &'a str, kind: Kind, next: Option<&str>) {
        match kind {
            Kind::Skip | Kind::Total(None) | Kind::Discount(None) => {}
            Kind::Total(Some(total)) => {
                self.flush_label();
                self.flush_detail();
                self.receipt.total_cents = Some(total);
                self.after_total = true;
            }
            Kind::Discount(Some(amount)) => {
                self.flush_label();
                self.flush_detail();
                match self.receipt.items.last_mut() {
                    Some(last) => last.price_cents = last.price_cents.saturating_add(amount),
                    None => self.report(line),
                }
            }
            Kind::Detail(detail) => self.read_detail(line, detail, next),
            Kind::Article(item, detailed) => {
                self.flush_label();
                self.push_article(item, detailed);
            }
            Kind::PriceOnly(price) => match self.pending_label.take() {
                Some(label) => self.push_article(
                    ReceiptItem {
                        label: collapse(label),
                        quantity: 1.0,
                        unit: None,
                        price_cents: price,
                    },
                    false,
                ),
                None => self.report(line),
            },
            Kind::Label => {
                if self.receipt.store.is_none() && !self.in_articles() {
                    self.receipt.store = Some(collapse(line));
                    return;
                }
                self.flush_label();
                self.flush_detail();
                self.pending_label = Some(line);
            }
            Kind::Unreadable => {
                self.flush_label();
                self.report(line);
            }
            Kind::Other => {
                if self.in_articles() {
                    self.report(line);
                }
            }
        }
    }

    fn read_detail(&mut self, line: &'a str, detail: Detail, next: Option<&str>) {
        if let Some(label) = self.pending_label {
            // A count without its own amount, above an article it prices,
            // is that article's — the name above it then stays unpriced.
            let counts_next = detail.line_price.is_none()
                && next.is_some_and(|next| match classify(next) {
                    Kind::Article(item, false) => detail.gives(item.price_cents),
                    _ => false,
                });
            if !counts_next {
                self.pending_label = None;
                self.flush_detail();
                let price_cents = detail.price();
                self.push(
                    ReceiptItem {
                        label: collapse(label),
                        quantity: detail.quantity,
                        unit: detail.unit,
                        price_cents,
                    },
                    true,
                );
                return;
            }
            self.flush_label();
        }
        self.flush_detail();
        let last_detailed = self.last_detailed;
        if let Some(last) = self.receipt.items.last_mut() {
            if !last_detailed && detail.gives(last.price_cents) {
                last.quantity = detail.quantity;
                last.unit = detail.unit;
                self.last_detailed = true;
                return;
            }
        }
        self.pending_detail = Some((detail, line));
    }

    fn finish(mut self) -> ParsedReceipt {
        self.flush_label();
        self.flush_detail();
        let mut receipt = self.receipt;
        let sum = receipt
            .items
            .iter()
            .fold(0i64, |sum, item| sum.saturating_add(item.price_cents));
        receipt.total_mismatch = receipt
            .total_cents
            .map(|total| total.saturating_sub(sum))
            .filter(|gap| *gap != 0);
        receipt
    }
}

/// A count or a weight: `2 x 1,25`, `0,432 kg x 3,90 €/kg  1,68`.
#[derive(Debug, Clone, PartialEq)]
struct Detail {
    quantity: f64,
    unit: Option<String>,
    /// The amount printed at the end of the line, if any.
    line_price: Option<i64>,
    /// The quantity times the unit price, rounded to the cent.
    computed: i64,
}

impl Detail {
    fn price(&self) -> i64 {
        self.line_price.unwrap_or(self.computed)
    }

    /// Whether this count gives `price`: the amount printed with it, or
    /// else its product within a cent of rounding.
    fn gives(&self, price: i64) -> bool {
        match self.line_price {
            Some(line_price) => line_price == price,
            None => self.computed.abs_diff(price) <= 1,
        }
    }
}

/// What one line of the ticket is.
enum Kind {
    /// Not part of the articles: sub-total, VAT, payment, loyalty, phone,
    /// courtesy, a date.
    Skip,
    Total(Option<i64>),
    /// A discount, its amount negative.
    Discount(Option<i64>),
    Detail(Detail),
    /// An article, and whether it carries its count or weight.
    Article(ReceiptItem, bool),
    /// An amount alone: the price of the name above it.
    PriceOnly(i64),
    /// A name alone, without a price.
    Label,
    /// An amount after something that cannot be a name.
    Unreadable,
    /// No name, no amount.
    Other,
}

/// The phrases that open a total line, in capitals without accents.
const TOTALS: &[&[&str]] = &[
    &["TOTAL"],
    &["TOT"],
    &["NET", "A", "PAYER"],
    &["A", "PAYER"],
    &["MONTANT", "A", "PAYER"],
    &["MONTANT", "DU"],
    &["MONTANT", "TTC"],
];

/// Words that make a total line some other total.
const NOT_THE_TOTAL: &[&str] = &[
    "HT",
    "TVA",
    "REMISE",
    "REMISES",
    "ECONOMIE",
    "ECONOMIES",
    "POINTS",
    "ARTICLE",
    "ARTICLES",
];

/// The phrases that open a discount line.
const DISCOUNTS: &[&[&str]] = &[
    &["REMISE"],
    &["REMISES"],
    &["REM"],
    &["PROMO"],
    &["PROMOTION"],
    &["REDUCTION"],
    &["RED"],
    &["RABAIS"],
    &["COUPON"],
    &["AVANTAGE"],
    &["BON", "DE", "REDUCTION"],
    &["BON", "REDUCTION"],
];

/// The phrases that open a line the articles leave out. A word that may
/// open a name (`CARTE D'OR`, `BON MAMAN`) only counts with the word after.
const SKIPPED: &[&[&str]] = &[
    &["SOUS-TOTAL"],
    &["SOUS", "TOTAL"],
    &["S/TOTAL"],
    &["STOTAL"],
    &["S/T"],
    &["HT"],
    &["TTC"],
    &["TAUX"],
    &["BASE"],
    &["CB"],
    &["CARTE", "BANCAIRE"],
    &["CARTE", "BLEUE"],
    &["CARTE", "CREDIT"],
    &["CARTE", "DE", "CREDIT"],
    &["CARTE", "FIDELITE"],
    &["VISA"],
    &["MASTERCARD"],
    &["SANS", "CONTACT"],
    &["PAIEMENT"],
    &["ESPECES"],
    &["CHEQUE"],
    &["RENDU"],
    &["MONNAIE"],
    &["FIDELITE"],
    &["POINTS"],
    &["VOS", "POINTS"],
    &["CAGNOTTE"],
    &["SOLDE"],
    &["NB"],
    &["NOMBRE"],
    &["ARTICLES"],
    &["ECONOMIE"],
    &["ECONOMIES"],
    &["TEL"],
    &["SIRET"],
    &["TICKET"],
    &["CAISSE"],
    &["VOTRE"],
    &["MERCI"],
    &["A", "BIENTOT"],
    &["BONNE", "JOURNEE"],
];

fn classify(line: &str) -> Kind {
    if find_date(line).is_some() {
        return Kind::Skip;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let words: Vec<String> = tokens.iter().take(3).map(|w| fold(w)).collect();
    let opens = |phrases: &[&[&str]]| {
        phrases.iter().any(|phrase| {
            phrase.len() <= words.len() && phrase.iter().zip(&words).all(|(p, w)| p == w)
        })
    };
    if opens(TOTALS) {
        let other = tokens
            .iter()
            .take(4)
            .any(|t| NOT_THE_TOTAL.contains(&fold(t).as_str()));
        if other {
            return Kind::Skip;
        }
        return Kind::Total(trailing_price(&tokens).map(|(price, _)| price));
    }
    if opens(DISCOUNTS) {
        return Kind::Discount(trailing_price(&tokens).map(|(price, _)| -price.abs()));
    }
    let tva = tokens.iter().any(|t| {
        t.trim_matches(|c: char| !c.is_alphanumeric())
            .eq_ignore_ascii_case("tva")
    });
    if opens(SKIPPED) || tva || line.contains('%') {
        return Kind::Skip;
    }

    if let Some(detail) = parse_detail(&tokens) {
        return Kind::Detail(detail);
    }
    for at in tokens.len().saturating_sub(MAX_DETAIL_TOKENS).max(1)..tokens.len() {
        if !tokens[at].starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        if let Some(detail) = parse_detail(&tokens[at..]) {
            let label = &tokens[..at];
            if letters(label) < 2 {
                return Kind::Unreadable;
            }
            let price_cents = detail.price();
            return Kind::Article(
                ReceiptItem {
                    label: label.join(" "),
                    quantity: detail.quantity,
                    unit: detail.unit,
                    price_cents,
                },
                true,
            );
        }
    }
    match trailing_price(&tokens) {
        Some((price, _)) if price < 0 => Kind::Discount(Some(price)),
        Some((price, 0)) => Kind::PriceOnly(price),
        Some((price, end)) => {
            let (quantity, label) = counted(&tokens[..end]);
            if letters(label) < 2 {
                return Kind::Unreadable;
            }
            Kind::Article(
                ReceiptItem {
                    label: label.join(" "),
                    quantity,
                    unit: None,
                    price_cents: price,
                },
                quantity != 1.0,
            )
        }
        None if letters(&tokens) >= 2 => Kind::Label,
        None if line.chars().any(char::is_alphanumeric) => Kind::Other,
        None => Kind::Skip,
    }
}

/// A count or a weight takes at most this many words: `0,432 kg x 3,90
/// EUR / kg 1,68 € A`.
const MAX_DETAIL_TOKENS: usize = 10;

/// `tokens`, all of them, as a count or a weight (see `Detail`).
fn parse_detail(tokens: &[&str]) -> Option<Detail> {
    if tokens.is_empty() || tokens.len() > MAX_DETAIL_TOKENS {
        return None;
    }
    let t = split_times(tokens);
    let (count, rest) = number(t[0])?;
    let mut k = 1;
    let (quantity, unit) = if rest.is_empty() {
        match t.get(1).filter(|u| is_metric(u)) {
            Some(unit) => {
                k = 2;
                let size = parse_pack_size(&format!("{} {unit}", t[0]))?;
                (size.quantity, Some(size.unit))
            }
            None if count > 0.0 && count <= MAX_COUNT => (count, None),
            None => return None,
        }
    } else if is_metric(rest) {
        let size = parse_pack_size(t[0])?;
        (size.quantity, Some(size.unit))
    } else {
        return None;
    };
    if !t.get(k).is_some_and(|x| is_times(x)) {
        return None;
    }
    k += 1;
    let (unit_price, rest) = money(t.get(k)?)?;
    if unit_price <= 0 || !is_per_unit(rest) {
        return None;
    }
    k += 1;
    while t.get(k).is_some_and(|s| is_per_unit(s)) {
        k += 1;
    }
    let line_price = t.get(k).and_then(|s| price_token(s));
    if line_price.is_some() {
        k += 1;
    }
    while t.get(k).is_some_and(|s| is_currency(s)) {
        k += 1;
    }
    if t.get(k).is_some_and(|s| is_vat_code(s)) {
        k += 1;
    }
    if k != t.len() {
        return None;
    }
    Some(Detail {
        quantity,
        unit,
        line_price,
        computed: (quantity * unit_price as f64).round() as i64,
    })
}

/// The largest count read without a unit.
const MAX_COUNT: f64 = 9999.0;

/// `tokens` with a multiplication sign glued to a number split out:
/// `2x1,25` reads `2`, `x`, `1,25`; `0,432kgx3,90` reads `0,432kg`, `x`,
/// `3,90`; `2X` reads `2`, `x`.
fn split_times<'t>(tokens: &[&'t str]) -> Vec<&'t str> {
    let mut out = Vec::with_capacity(tokens.len() + 2);
    for &token in tokens {
        let at = token.starts_with(|c: char| c.is_ascii_digit()).then(|| {
            let mut previous = ' ';
            token.char_indices().find_map(|(at, c)| {
                let glued = matches!(c, 'x' | 'X' | '×' | '*')
                    && (previous.is_ascii_digit() || matches!(previous, 'g' | 'G' | 'l' | 'L'));
                previous = c;
                glued.then_some((at, c.len_utf8()))
            })
        });
        match at.flatten() {
            Some((at, len)) => {
                out.push(&token[..at]);
                out.push("x");
                if at + len < token.len() {
                    out.push(&token[at + len..]);
                }
            }
            None => out.push(token),
        }
    }
    out
}

/// The count that may open an article's name (`3 X CITRON`, `2X JUS`),
/// or 1, and the name.
fn counted<'s, 't>(label: &'s [&'t str]) -> (f64, &'s [&'t str]) {
    let small = |s: &str| {
        (1..=3).contains(&s.len())
            && s.bytes().all(|b| b.is_ascii_digit())
            && s != "0"
            && !s.starts_with('0')
    };
    match label {
        [count, times, rest @ ..] if small(count) && is_times(times) => {
            (count.parse().unwrap_or(1.0), rest)
        }
        [glued, rest @ ..] => {
            let count = glued
                .strip_suffix(['x', 'X', '×', '*'])
                .filter(|count| small(count));
            match count {
                Some(count) => (count.parse().unwrap_or(1.0), rest),
                None => (1.0, label),
            }
        }
        [] => (1.0, label),
    }
}

/// The price that ends `tokens`, after which only a currency and a VAT
/// code may come, and where the name before it ends.
fn trailing_price(tokens: &[&str]) -> Option<(i64, usize)> {
    let before = |mut end: usize| {
        while end > 0 && is_currency(tokens[end - 1]) {
            end -= 1;
        }
        let at = end.checked_sub(1)?;
        Some((price_token(tokens[at])?, at))
    };
    let n = tokens.len();
    if n > 0 && is_vat_code(tokens[n - 1]) {
        if let Some(found) = before(n - 1) {
            return Some(found);
        }
    }
    before(n)
}

/// An amount alone or with its currency: `1,25`, `1,25€`, `-0,50`, `0,50-`.
fn price_token(token: &str) -> Option<i64> {
    let (cents, rest) = money(token)?;
    (rest.is_empty() || is_currency(rest)).then_some(cents)
}

/// An amount in euros — at most seven digits, a comma or a point, exactly
/// two decimals, a minus before or after — in cents, and what follows it.
fn money(token: &str) -> Option<(i64, &str)> {
    let bytes = token.as_bytes();
    let leading_minus = bytes.first() == Some(&b'-');
    let start = usize::from(leading_minus);
    let digits = bytes[start..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits == 0 || digits > 7 {
        return None;
    }
    let sep = start + digits;
    if !matches!(bytes.get(sep), Some(b',' | b'.')) {
        return None;
    }
    let (tens, units) = (*bytes.get(sep + 1)?, *bytes.get(sep + 2)?);
    if !tens.is_ascii_digit() || !units.is_ascii_digit() {
        return None;
    }
    if bytes.get(sep + 3).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let euros: i64 = token[start..sep].parse().ok()?;
    let cents = euros * 100 + i64::from(tens - b'0') * 10 + i64::from(units - b'0');
    let rest = &token[sep + 3..];
    let (trailing_minus, rest) = match rest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    if leading_minus && trailing_minus {
        return None;
    }
    let cents = if leading_minus || trailing_minus {
        -cents
    } else {
        cents
    };
    Some((cents, rest))
}

/// A quantity: digits, and up to three decimals after a comma or a
/// point; and what follows it.
fn number(token: &str) -> Option<(f64, &str)> {
    let whole = token.bytes().take_while(u8::is_ascii_digit).count();
    if whole == 0 || whole > 9 {
        return None;
    }
    let mut end = whole;
    if let Some(b',' | b'.') = token.as_bytes().get(whole) {
        let decimals = token.as_bytes()[whole + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if decimals == 0 || decimals > 3 {
            return None;
        }
        end = whole + 1 + decimals;
    }
    let value = token[..end].replace(',', ".").parse().ok()?;
    Some((value, &token[end..]))
}

fn is_times(token: &str) -> bool {
    matches!(token, "x" | "X" | "×" | "*")
}

fn is_metric(token: &str) -> bool {
    ["g", "kg", "mg", "l", "cl", "ml", "dl"]
        .iter()
        .any(|unit| token.eq_ignore_ascii_case(unit))
}

fn is_currency(token: &str) -> bool {
    token == "€" || token.eq_ignore_ascii_case("eur") || token.eq_ignore_ascii_case("euros")
}

/// What may follow a unit price: a currency, `/kg`, both (`€/kg`,
/// `EUR/kg`), or one of the pieces when the OCR spaced them.
fn is_per_unit(token: &str) -> bool {
    let rest = token.strip_prefix('€').unwrap_or(token);
    let rest = match rest.get(..3) {
        Some(eur) if eur.eq_ignore_ascii_case("eur") => &rest[3..],
        _ => rest,
    };
    rest.is_empty()
        || rest == "/"
        || is_metric(rest)
        || rest.strip_prefix('/').is_some_and(is_metric)
}

/// The VAT code a ticket prints after a price: one letter, one digit or `*`.
fn is_vat_code(token: &str) -> bool {
    let mut chars = token.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphanumeric() || c == '*')
}

/// How many letters `tokens` hold.
fn letters(tokens: &[&str]) -> usize {
    tokens
        .iter()
        .map(|t| t.chars().filter(|c| c.is_alphabetic()).count())
        .sum()
}

/// `line` with its runs of spaces made one.
fn collapse(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `word` in capitals without accents, its closing `:` or `.` dropped, to
/// compare with the phrases above.
fn fold(word: &str) -> String {
    word.trim_end_matches([':', '.'])
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'À' | 'Â' | 'Ä' => 'A',
            'ç' | 'Ç' => 'C',
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'E',
            'î' | 'ï' | 'Î' | 'Ï' => 'I',
            'ô' | 'ö' | 'Ô' | 'Ö' => 'O',
            'ù' | 'û' | 'ü' | 'Ù' | 'Û' | 'Ü' => 'U',
            c => c.to_ascii_uppercase(),
        })
        .collect()
}

/// The first valid date among the words of `line`: day, month and year
/// separated twice by the same `/`, `-` or `.`, a year of two digits being
/// in the 2000s.
fn find_date(line: &str) -> Option<NaiveDate> {
    line.split_whitespace().find_map(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_digit());
        ['/', '-', '.'].iter().find_map(|&sep| {
            let mut parts = word.split(sep);
            let (d, m, y) = (parts.next()?, parts.next()?, parts.next()?);
            if parts.next().is_some() {
                return None;
            }
            let digits = |s: &str, lens: &[usize]| {
                (lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| s.parse::<u32>().ok())
                    .flatten()
            };
            let (d, m) = (digits(d, &[1, 2])?, digits(m, &[1, 2])?);
            let y = digits(y, &[2, 4])?;
            let y = if y < 100 { 2000 + y } else { y };
            NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m, d)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, quantity: f64, unit: Option<&str>, price_cents: i64) -> ReceiptItem {
        ReceiptItem {
            label: label.to_string(),
            quantity,
            unit: unit.map(str::to_string),
            price_cents,
        }
    }

    fn date(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, d)
    }

    /// A hypermarket ticket: the price, then a VAT code; the count or the
    /// weight on the line below the article; a discount on its own line;
    /// the date at the foot, after payment and the VAT table.
    const HYPER: &str = "\
HYPER DES TILLEULS
12 RUE DES LILAS
69000 LYON
TEL 04 00 00 00 00
LAIT DEMI ECR 1L              0,95 A
YAOURT NAT. X4                2,50 A
   2 x 1,25
TOMATES GRAPPE                1,68 A
   0,432 kg x 3,90 EUR/kg
REMISE IMMEDIATE             -0,50
BIERE BLONDE 6X25CL           5,40 B
SOUS-TOTAL                   10,03
TOTAL                        10,03
CB                           10,03
TVA A  5,50%  HT 4,34  TVA 0,24
TVA B 20,00%  HT 4,50  TVA 0,90
28/11/2026 10:42 CAISSE 003 TICKET 0815
MERCI DE VOTRE VISITE
";

    #[test]
    fn a_hypermarket_ticket() {
        let r = parse_receipt(HYPER);
        assert_eq!(r.store.as_deref(), Some("HYPER DES TILLEULS"));
        assert_eq!(r.purchased_on, date(2026, 11, 28));
        assert_eq!(r.total_cents, Some(1003));
        assert_eq!(
            r.items,
            vec![
                item("LAIT DEMI ECR 1L", 1.0, None, 95),
                item("YAOURT NAT. X4", 2.0, None, 250),
                // 1,68 less the 0,50 discount that follows it.
                item("TOMATES GRAPPE", 0.432, Some("kg"), 118),
                item("BIERE BLONDE 6X25CL", 1.0, None, 540),
            ]
        );
        assert_eq!(r.total_mismatch, None);
        assert!(r.unreadable.is_empty(), "{:?}", r.unreadable);
    }

    /// A discounter ticket: the euro sign after each price, mixed case,
    /// the count printed *before* its article, a weighed article whose name
    /// stands alone above its weight line, a two-digit year.
    const DISCOUNT: &str = "\
MARCHE DISCOUNT
Zone artisanale du Moulin
21/11/26  18:05
PAIN COMPLET                1,19 €  A
2 x  0,89 €
COMPOTE POMME               1,78 €  A
BANANES
0,850 kg x 1,99 €/kg        1,69 €  A
Total                       4,66 €
Carte bancaire              4,66
Points fidelite cumules     12
";

    #[test]
    fn a_discounter_ticket() {
        let r = parse_receipt(DISCOUNT);
        assert_eq!(r.store.as_deref(), Some("MARCHE DISCOUNT"));
        assert_eq!(r.purchased_on, date(2026, 11, 21));
        assert_eq!(r.total_cents, Some(466));
        assert_eq!(
            r.items,
            vec![
                item("PAIN COMPLET", 1.0, None, 119),
                item("COMPOTE POMME", 2.0, None, 178),
                item("BANANES", 0.85, Some("kg"), 169),
            ]
        );
        assert_eq!(r.total_mismatch, None);
        assert!(r.unreadable.is_empty(), "{:?}", r.unreadable);
    }

    /// A greengrocer's ticket: the count before the name (`3 X`), the
    /// weight inline with a glued unit and euro sign, `NET A PAYER` for the
    /// total, cash and change — and a price the OCR misread (`1,1O`).
    const GREENGROCER: &str = "\
PRIMEUR LE PANIER VERT
Le 05-10-2026 a 09h12
3 X CITRON JAUNE            1,50
SALADE BATAVIA              1,1O
POMMES GOLDEN 1,234kg x 2,49€/kg  3,07
NET A PAYER                 5,67
ESPECES                    10,00
RENDU                       4,33
";

    #[test]
    fn a_greengrocer_ticket_with_an_unreadable_line() {
        let r = parse_receipt(GREENGROCER);
        assert_eq!(r.store.as_deref(), Some("PRIMEUR LE PANIER VERT"));
        assert_eq!(r.purchased_on, date(2026, 10, 5));
        assert_eq!(r.total_cents, Some(567));
        assert_eq!(
            r.items,
            vec![
                item("CITRON JAUNE", 3.0, None, 150),
                item("POMMES GOLDEN", 1.234, Some("kg"), 307),
            ]
        );
        // The salad is reported, never priced by guess.
        assert_eq!(r.unreadable, vec!["SALADE BATAVIA              1,1O"]);
        // 5,67 read against 1,50 + 3,07: the missing salad shows.
        assert_eq!(r.total_mismatch, Some(110));
    }

    #[test]
    fn a_weight_line_without_its_total_is_priced_from_the_unit_price() {
        let r = parse_receipt("ENSEIGNE\nCAROTTES\n1,500 kg x 1,29 €/kg\nTOTAL 1,94\n");
        // 1,5 × 1,29 = 1,935, rounded to the cent.
        assert_eq!(r.items, vec![item("CAROTTES", 1.5, Some("kg"), 194)]);
        assert_eq!(r.total_mismatch, None);
    }

    #[test]
    fn a_count_above_the_first_article_is_not_bound_to_the_header() {
        // `2 x 0,89` gives the compote's price: the address line above it
        // is not an article.
        let r = parse_receipt("ENSEIGNE\nZone artisanale\n2 x 0,89\nCOMPOTE 1,78\nTOTAL 1,78\n");
        assert_eq!(r.items, vec![item("COMPOTE", 2.0, None, 178)]);
        assert!(r.unreadable.is_empty(), "{:?}", r.unreadable);
    }

    #[test]
    fn a_count_glued_to_its_x_and_a_trailing_minus_discount() {
        let r = parse_receipt("ENSEIGNE\n2X JUS ORANGE 1L  3,00\nPROMO  0,60-\nTOTAL 2,40\n");
        assert_eq!(r.items, vec![item("JUS ORANGE 1L", 2.0, None, 240)]);
        assert_eq!(r.total_mismatch, None);
    }

    #[test]
    fn a_discount_with_no_article_before_it_is_reported() {
        let r = parse_receipt("ENSEIGNE\nREMISE FIDELITE -1,00\nTOTAL 0,00\n");
        assert!(r.items.is_empty());
        assert_eq!(r.unreadable, vec!["REMISE FIDELITE -1,00"]);
    }

    #[test]
    fn a_count_line_that_matches_nothing_is_reported() {
        let r = parse_receipt("ENSEIGNE\nPAIN 1,00\n3 x 0,70\nTOTAL 1,00\n");
        assert_eq!(r.items, vec![item("PAIN", 1.0, None, 100)]);
        assert_eq!(r.unreadable, vec!["3 x 0,70"]);
    }

    #[test]
    fn a_name_with_its_price_on_the_next_line() {
        let r = parse_receipt("ENSEIGNE\nCAFE MOULU\n3,45 A\nTOTAL 3,45\n");
        assert_eq!(r.items, vec![item("CAFE MOULU", 1.0, None, 345)]);
    }

    #[test]
    fn a_name_that_never_gets_a_price_is_reported() {
        let r = parse_receipt("ENSEIGNE\nPAIN 1,00\nFROMAGE RAPE\nTOTAL 1,00\n");
        assert_eq!(r.items, vec![item("PAIN", 1.0, None, 100)]);
        assert_eq!(r.unreadable, vec!["FROMAGE RAPE"]);
        assert_eq!(r.total_mismatch, None);
    }

    #[test]
    fn a_word_that_opens_a_payment_line_does_not_hide_an_article_inside_a_name() {
        // `CARTE` alone opens nothing: only `CARTE BANCAIRE` and the like do.
        let r = parse_receipt("ENSEIGNE\nCARTE D'OR VANILLE 4,50\nTOTAL 4,50\n");
        assert_eq!(r.items, vec![item("CARTE D'OR VANILLE", 1.0, None, 450)]);
    }

    #[test]
    fn total_tva_and_total_ht_are_not_the_total() {
        let r =
            parse_receipt("ENSEIGNE\nPAIN 1,00\nTOTAL HT 0,95\nTOTAL TVA 0,05\nTOTAL TTC 1,00\n");
        assert_eq!(r.total_cents, Some(100));
    }

    #[test]
    fn a_ticket_without_any_article_gives_an_empty_list() {
        let r = parse_receipt("BOULANGERIE\n01/02/2026\nTOTAL 0,00\nMERCI\n");
        assert!(r.items.is_empty());
        assert_eq!(r.total_cents, Some(0));
        assert_eq!(r.total_mismatch, None);
        assert_eq!(r.purchased_on, date(2026, 2, 1));
    }

    #[test]
    fn a_missing_total_exposes_no_mismatch() {
        let r = parse_receipt("ENSEIGNE\nPAIN 1,00\n");
        assert_eq!(r.total_cents, None);
        assert_eq!(r.total_mismatch, None);
    }

    #[test]
    fn an_impossible_date_is_not_read() {
        let r = parse_receipt("ENSEIGNE\n31/02/2026\n12/03/2026\n");
        assert_eq!(r.purchased_on, date(2026, 3, 12));
    }

    #[test]
    fn empty_and_blank_text() {
        assert_eq!(parse_receipt(""), ParsedReceipt::default());
        assert_eq!(parse_receipt(" \n\t\n  "), ParsedReceipt::default());
    }

    /// Deterministic noise: printable ASCII, the characters a ticket's
    /// grammar turns on (digits, `,`, `x`, `-`, `€`, `/`), accented and
    /// multi-byte characters, and line breaks.
    fn noise(len: usize, seed: u64) -> String {
        const ALPHABET: &[char] = &[
            'A', 'Z', 'x', 'X', '×', '*', '0', '1', '9', ',', '.', '-', '/', '€', 'é', 'É', '%',
            ' ', ' ', ' ', '\n', '\t', 'k', 'g', 'L', '😀', '\u{202E}', '\0',
        ];
        let mut state = seed;
        let mut out = String::with_capacity(len);
        while out.len() < len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push(ALPHABET[(state >> 33) as usize % ALPHABET.len()]);
        }
        out
    }

    #[test]
    fn a_megabyte_of_noise_does_not_panic() {
        for seed in 1..=3 {
            let r = parse_receipt(&noise(1 << 20, seed));
            for unreadable in &r.unreadable {
                assert!(unreadable.chars().count() <= 201);
            }
        }
    }

    #[test]
    fn lines_of_a_hundred_thousand_characters_do_not_panic() {
        let long = 100_000;
        let lines = [
            "A".repeat(long),
            format!("{} 1,00", "B".repeat(long)),
            "1,00 ".repeat(long / 5),
            "2 x ".repeat(long / 4),
            format!("{} 0,432 kg x 3,90 €/kg 1,68", "é".repeat(long / 2)),
            "9".repeat(long),
            format!("9{}", ",9".repeat(long / 2)),
            "-".repeat(long),
            "😀".repeat(long / 4),
            format!("TOTAL {}", "9".repeat(long)),
        ];
        for line in &lines {
            let text = format!("ENSEIGNE\nPAIN 1,00\n{line}\nTOTAL 1,00\n");
            let r = parse_receipt(&text);
            for unreadable in &r.unreadable {
                assert!(unreadable.chars().count() <= 201);
            }
        }
        // A single line, no header, no break.
        for line in &lines {
            parse_receipt(line);
        }
    }

    #[test]
    fn a_long_name_keeps_its_article() {
        let name = "B".repeat(100_000);
        let r = parse_receipt(&format!("ENSEIGNE\n{name} 1,00\nTOTAL 1,00\n"));
        assert_eq!(r.items, vec![item(&name, 1.0, None, 100)]);
    }

    #[test]
    fn an_amount_too_large_to_be_a_price_is_not_one() {
        let r = parse_receipt("ENSEIGNE\nPAIN 99999999999999999999,00\nTOTAL 1,00\n");
        assert!(r.items.is_empty());
        assert_eq!(r.total_cents, Some(100));
    }
}
