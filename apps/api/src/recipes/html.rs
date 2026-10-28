//! A page's HTML as a flat list of tags and texts, with each element's
//! extent (#475): what the microdata and prose extractors walk. No HTML
//! library, like `jsonld`: a tokenizer by hand, which skips comments,
//! `<script>` and `<style>`, and a matching of end tags that tolerates
//! the unclosed and misnested elements real pages carry.
//!
//! Everything here is linear in the page's size: a `<` or a quote that
//! never closes ends the scan instead of being searched for again, and
//! each element is opened and closed once in the matching.

use std::collections::HashMap;

use crate::recipes::jsonld::plain_text;

/// One piece of a page, in page order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token<'a> {
    /// `<name attributes>`, the name lowercased, the attributes as written.
    Start { name: String, attributes: &'a str },
    /// `</name>`, the name lowercased.
    End { name: String },
    /// Text between tags, entities not decoded.
    Text(&'a str),
}

/// A page cut into tokens, with the extent of every element.
pub struct Document<'a> {
    pub tokens: Vec<Token<'a>>,
    /// For a `Start` token, the index of the token that ends the element:
    /// its end tag, an ancestor's end tag that closes it implicitly, or
    /// `tokens.len()` when nothing does. For a void element, its own index.
    /// Unused for the other tokens.
    ends: Vec<usize>,
}

/// Elements without content or end tag.
const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements that end a line of text.
const BLOCKS: [&str; 22] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "li",
    "ol",
    "p",
    "ul",
];

/// Elements a new block closes when it opens right inside them: the
/// start of one of these ends an open `<p>` (`<p>un<p>deux`, `<p>texte<ul>`).
const CLOSES_P: [&str; 16] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "div",
    "dl",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ol",
    "p",
    "ul",
];

/// Whether `name` is a heading, `h1` to `h6`; its level if so.
pub fn heading_level(name: &str) -> Option<u8> {
    match name.as_bytes() {
        [b'h', level @ b'1'..=b'6'] => Some(level - b'0'),
        _ => None,
    }
}

impl<'a> Document<'a> {
    pub fn parse(html: &'a str) -> Self {
        let tokens = tokenize(html);
        let ends = match_ends(&tokens);
        Self { tokens, ends }
    }

    /// The index of the token that ends the element started at `start`.
    pub fn end_of(&self, start: usize) -> usize {
        self.ends[start]
    }

    /// The tokens inside the element started at `start`.
    pub fn inside(&self, start: usize) -> std::ops::Range<usize> {
        (start + 1).min(self.end_of(start))..self.end_of(start)
    }

    /// The element's text, on one line: tags dropped, entities decoded,
    /// whitespace collapsed.
    pub fn text(&self, start: usize) -> String {
        self.text_in(self.inside(start))
    }

    /// The text of `range`, on one line.
    pub fn text_in(&self, range: std::ops::Range<usize>) -> String {
        let mut raw = String::new();
        for token in &self.tokens[range] {
            match token {
                Token::Text(text) => raw.push_str(text),
                Token::Start { name, .. } | Token::End { name } if is_block(name) => raw.push(' '),
                _ => {}
            }
        }
        plain_text(&raw)
    }

    /// The text of the element started at `start`, on one line, or `None`
    /// when it holds more than `max_tokens` tokens or `max_len` bytes of
    /// text: what a label is checked on, at a bounded cost however long
    /// the element.
    pub fn short_text(&self, start: usize, max_tokens: usize, max_len: usize) -> Option<String> {
        let range = self.inside(start);
        if range.len() > max_tokens {
            return None;
        }
        let length: usize = self.tokens[range.clone()]
            .iter()
            .map(|t| match t {
                Token::Text(text) => text.len(),
                _ => 0,
            })
            .sum();
        (length <= max_len).then(|| self.text_in(range))
    }

    /// The element's non-blank lines of text — a block element ends a
    /// line — leaving out the text of the headings it contains.
    pub fn lines(&self, start: usize) -> Vec<String> {
        self.lines_in(self.inside(start))
    }

    /// The non-blank lines of `range`, its headings left out.
    pub fn lines_in(&self, range: std::ops::Range<usize>) -> Vec<String> {
        let mut raw = String::new();
        let mut at = range.start;
        while at < range.end {
            match &self.tokens[at] {
                Token::Text(text) => raw.push_str(text),
                Token::Start { name, .. } if heading_level(name).is_some() => {
                    raw.push('\n');
                    at = self.end_of(at).max(at);
                }
                Token::Start { name, .. } | Token::End { name } if is_block(name) => raw.push('\n'),
                _ => {}
            }
            at += 1;
        }
        raw.lines()
            .map(plain_text)
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// The text, one line, of every `li` inside the element that holds no
    /// other `li`: a nested list gives its own items, not one item made of
    /// all of them.
    pub fn leaf_items(&self, start: usize) -> Vec<String> {
        self.leaf_items_in(self.inside(start))
    }

    /// The text of every `li` of `range` that holds no other `li`.
    pub fn leaf_items_in(&self, range: std::ops::Range<usize>) -> Vec<String> {
        // The open items, innermost last: where each ends, its text so
        // far, and whether it holds another item.
        let mut open: Vec<(usize, String, bool)> = Vec::new();
        let mut items = Vec::new();
        let mut close = |open: &mut Vec<(usize, String, bool)>, at: usize| {
            while open.last().is_some_and(|(end, _, _)| *end <= at) {
                let (_, raw, parent) = open.pop().unwrap();
                if !parent {
                    let text = plain_text(&raw);
                    if !text.is_empty() {
                        items.push(text);
                    }
                }
            }
        };
        for at in range.clone() {
            close(&mut open, at);
            match &self.tokens[at] {
                Token::Start { name, .. } if name == "li" => {
                    if let Some(parent) = open.last_mut() {
                        parent.2 = true;
                    }
                    open.push((self.end_of(at), String::new(), false));
                }
                Token::Text(text) => {
                    if let Some((_, raw, _)) = open.last_mut() {
                        raw.push_str(text);
                    }
                }
                Token::Start { name, .. } | Token::End { name } if is_block(name) => {
                    if let Some((_, raw, _)) = open.last_mut() {
                        raw.push(' ');
                    }
                }
                _ => {}
            }
        }
        close(&mut open, usize::MAX);
        items
    }
}

fn is_block(name: &str) -> bool {
    BLOCKS.contains(&name)
}

/// The tokens of `html`, without its comments, scripts and styles. A tag
/// that never closes, a comment that never ends or a `<script>` without
/// its end tag ends the scan: what follows is kept as text, or dropped for
/// a comment or a script.
fn tokenize<'a>(html: &'a str) -> Vec<Token<'a>> {
    // ASCII lowercasing keeps every byte offset in place.
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut tokens = Vec::new();
    let mut text_from = 0;
    let mut from = 0;
    while let Some(found) = html[from..].find('<') {
        let at = from + found;
        let next = bytes.get(at + 1).copied();
        let push_text = |tokens: &mut Vec<Token<'a>>| {
            if text_from < at {
                tokens.push(Token::Text(&html[text_from..at]));
            }
        };
        match next {
            Some(b'!') => {
                push_text(&mut tokens);
                let close = if lower[at..].starts_with("<!--") {
                    lower[at + 4..].find("-->").map(|e| at + 4 + e + 3)
                } else {
                    lower[at..].find('>').map(|e| at + e + 1)
                };
                let Some(after) = close else {
                    return tokens;
                };
                from = after;
                text_from = after;
            }
            Some(b'/') | Some(b'a'..=b'z') | Some(b'A'..=b'Z') => {
                let closing = next == Some(b'/');
                let name_from = at + 1 + usize::from(closing);
                let name_len = lower[name_from..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .unwrap_or(lower.len() - name_from);
                let name_end = name_from + name_len;
                let Some(gt) = tag_end(&html[name_end..]).map(|e| name_end + e) else {
                    tokens.push(Token::Text(&html[text_from..]));
                    return tokens;
                };
                push_text(&mut tokens);
                let name = lower[name_from..name_end].to_string();
                from = gt + 1;
                text_from = from;
                if closing {
                    if !name.is_empty() {
                        tokens.push(Token::End { name });
                    }
                    continue;
                }
                if name == "script" || name == "style" {
                    // Not HTML, and nothing a recipe is read from: the
                    // element is skipped, up to the end of its end tag.
                    let Some(close) = lower[from..].find(&format!("</{name}")) else {
                        return tokens;
                    };
                    let close = from + close;
                    let Some(gt) = lower[close..].find('>') else {
                        return tokens;
                    };
                    from = close + gt + 1;
                    text_from = from;
                    continue;
                }
                let attributes = html[name_end..gt].trim_end_matches('/');
                tokens.push(Token::Start { name, attributes });
            }
            _ => from = at + 1,
        }
    }
    if text_from < html.len() {
        tokens.push(Token::Text(&html[text_from..]));
    }
    tokens
}

/// The offset of the `>` that ends a tag whose attributes `rest` starts
/// with: a `>` inside a quoted value does not. `None` when there is none.
fn tag_end(rest: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    let mut after_equals = false;
    for (i, &b) in rest.as_bytes().iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'>' => return Some(i),
                b'"' | b'\'' if after_equals => quote = Some(b),
                b'=' => after_equals = true,
                b if b.is_ascii_whitespace() => {}
                _ => after_equals = false,
            },
        }
        if quote.is_some() {
            after_equals = false;
        }
    }
    None
}

/// Where each element ends (`Document::ends`). An end tag closes the
/// innermost open element of its name and every element opened inside it;
/// one that matches no open element is ignored. A `<li>` closes the open
/// `<li>` of the same list, and a block closes a `<p>` it opens right
/// inside of. Each element is pushed and popped once.
fn match_ends(tokens: &[Token<'_>]) -> Vec<usize> {
    let mut ends = vec![0; tokens.len()];
    // The open elements, innermost last, and how many of each name.
    let mut stack: Vec<(usize, &str)> = Vec::new();
    let mut open: HashMap<&str, Vec<usize>> = HashMap::new();
    let close_down_to = |stack: &mut Vec<(usize, &str)>,
                         open: &mut HashMap<&str, Vec<usize>>,
                         ends: &mut Vec<usize>,
                         depth: usize,
                         at: usize| {
        while stack.len() > depth {
            let (start, name) = stack.pop().unwrap();
            ends[start] = at;
            open.get_mut(name).and_then(Vec::pop);
        }
    };
    let innermost = |open: &HashMap<&str, Vec<usize>>, name: &str| {
        open.get(name).and_then(|depths| depths.last().copied())
    };
    for (at, token) in tokens.iter().enumerate() {
        match token {
            Token::Start { name, .. } => {
                if VOID.contains(&name.as_str()) {
                    ends[at] = at;
                    continue;
                }
                if name == "li" {
                    let list = innermost(&open, "ul").max(innermost(&open, "ol"));
                    if let Some(li) = innermost(&open, "li").filter(|&li| Some(li) > list) {
                        close_down_to(&mut stack, &mut open, &mut ends, li, at);
                    }
                }
                if CLOSES_P.contains(&name.as_str())
                    && stack.last().is_some_and(|(_, top)| *top == "p")
                {
                    let depth = stack.len() - 1;
                    close_down_to(&mut stack, &mut open, &mut ends, depth, at);
                }
                open.entry(name.as_str()).or_default().push(stack.len());
                stack.push((at, name.as_str()));
            }
            Token::End { name } => {
                if let Some(depth) = innermost(&open, name) {
                    close_down_to(&mut stack, &mut open, &mut ends, depth, at);
                }
            }
            Token::Text(_) => {}
        }
    }
    close_down_to(&mut stack, &mut open, &mut ends, 0, tokens.len());
    ends
}

/// The value of attribute `name` in a tag's attributes, entities decoded;
/// `Some("")` for an attribute without a value. Names in any case.
pub fn attribute(attributes: &str, name: &str) -> Option<String> {
    let mut rest = attributes;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return None;
        }
        let name_len = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let found = &rest[..name_len];
        rest = rest[name_len..].trim_start();
        let value = match rest.strip_prefix('=') {
            Some(after) => {
                let after = after.trim_start();
                let (value, remaining) = match after.chars().next() {
                    Some(quote @ ('"' | '\'')) => {
                        let inner = &after[1..];
                        let end = inner.find(quote).unwrap_or(inner.len());
                        (&inner[..end], &inner[(end + 1).min(inner.len())..])
                    }
                    _ => {
                        let end = after.find(char::is_whitespace).unwrap_or(after.len());
                        (&after[..end], &after[end..])
                    }
                };
                rest = remaining;
                value
            }
            None => "",
        };
        if found.eq_ignore_ascii_case(name) {
            return Some(plain_text(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(document: &Document, name: &str) -> usize {
        document
            .tokens
            .iter()
            .position(|t| matches!(t, Token::Start { name: n, .. } if n == name))
            .unwrap_or_else(|| panic!("no <{name}>"))
    }

    #[test]
    fn tags_and_texts_in_page_order() {
        let d = Document::parse(r#"<P Class="x">Un <b>deux</b></p>"#);
        assert_eq!(
            d.tokens,
            [
                Token::Start {
                    name: "p".into(),
                    attributes: r#" Class="x""#
                },
                Token::Text("Un "),
                Token::Start {
                    name: "b".into(),
                    attributes: ""
                },
                Token::Text("deux"),
                Token::End { name: "b".into() },
                Token::End { name: "p".into() },
            ]
        );
    }

    #[test]
    fn comments_scripts_and_styles_are_skipped() {
        let d = Document::parse(
            "<!DOCTYPE html><!-- <h1>non</h1> --><script>if (a<b) x = '</p>';</script>\
             <STYLE>p > a {}</STYLE><p>oui</p>",
        );
        let p = start(&d, "p");
        assert_eq!(d.text(p), "oui");
        assert!(!d
            .tokens
            .iter()
            .any(|t| matches!(t, Token::Start { name, .. } if name == "h1" || name == "script")));
    }

    #[test]
    fn a_quoted_greater_than_does_not_end_the_tag() {
        let d = Document::parse(r#"<img alt="a > b" src='x>y'><p>texte</p>"#);
        assert_eq!(
            attribute(
                match &d.tokens[0] {
                    Token::Start { attributes, .. } => attributes,
                    other => panic!("{other:?}"),
                },
                "alt"
            ),
            Some("a > b".into())
        );
        assert_eq!(d.text(start(&d, "p")), "texte");
    }

    #[test]
    fn a_less_than_that_opens_no_tag_is_text() {
        let d = Document::parse("<p>4 < 5 et 3 <= 4</p>");
        assert_eq!(d.text(start(&d, "p")), "4 < 5 et 3 <= 4");
    }

    #[test]
    fn an_unclosed_tag_ends_the_page_as_text() {
        let d = Document::parse("<p>avant</p><a href=\"x");
        assert_eq!(d.text(start(&d, "p")), "avant");
        assert_eq!(d.tokens.last(), Some(&Token::Text("<a href=\"x")));
    }

    #[test]
    fn elements_end_at_their_end_tag_or_an_ancestor_s() {
        // `<li>` and `<p>` left open, a stray `</span>`, a void `<br>`.
        let d = Document::parse("<ul><li>un<li>deux</span><br></ul><p>après");
        let ul = start(&d, "ul");
        assert!(matches!(&d.tokens[d.end_of(ul)], Token::End { name } if name == "ul"));
        let first_li = start(&d, "li");
        let second_li = first_li + 2;
        assert_eq!(d.end_of(first_li), second_li, "closed by the next <li>");
        assert_eq!(d.end_of(second_li), d.end_of(ul), "closed by </ul>");
        let br = start(&d, "br");
        assert_eq!(d.end_of(br), br);
        assert_eq!(d.end_of(start(&d, "p")), d.tokens.len());
        assert_eq!(d.leaf_items(ul), ["un", "deux"]);
    }

    #[test]
    fn a_block_closes_an_open_paragraph_and_a_list_item_its_sibling() {
        let d = Document::parse("<div><p>un<p>deux<ul><li>a<ul><li>b</ul><li>c</ul></div>");
        assert_eq!(d.lines(0), ["un", "deux", "a", "b", "c"]);
        assert_eq!(d.leaf_items(0), ["b", "c"]);
        let first_p = start(&d, "p");
        assert_eq!(d.text(first_p), "un");
    }

    #[test]
    fn text_decodes_entities_and_collapses_whitespace() {
        let d = Document::parse("<div>\n  Cr&egrave;me&nbsp;:  <strong>200 g</strong>\n</div>");
        assert_eq!(d.text(0), "Crème : 200 g");
    }

    #[test]
    fn lines_break_at_blocks_and_leave_headings_out() {
        let d = Document::parse(
            "<section><h3>1. Préparer</h3><p>Couper.</p><p>Cuire<br>Servir</p>\
             <ul><li>Astuce</li></ul></section>",
        );
        assert_eq!(d.lines(0), ["Couper.", "Cuire", "Servir", "Astuce"]);
    }

    #[test]
    fn leaf_items_take_each_innermost_li() {
        let d = Document::parse(
            "<div><ul><li><p><b>Sel</b> : 1 pincée</p></li>\
             <li>Pour la pâte<ul><li>200 g de farine</li><li>1 oeuf</li></ul></li></ul></div>",
        );
        assert_eq!(
            d.leaf_items(0),
            ["Sel : 1 pincée", "200 g de farine", "1 oeuf"]
        );
    }

    #[test]
    fn attributes_quoted_or_not_in_any_case() {
        let attrs = r#" ItemProp="recipeIngredient"  content='1 l' data-x=3 hidden itemscope"#;
        assert_eq!(
            attribute(attrs, "itemprop"),
            Some("recipeIngredient".into())
        );
        assert_eq!(attribute(attrs, "content"), Some("1 l".into()));
        assert_eq!(attribute(attrs, "data-x"), Some("3".into()));
        assert_eq!(attribute(attrs, "itemscope"), Some(String::new()));
        assert_eq!(attribute(attrs, "scope"), None);
        assert_eq!(attribute(attrs, "item"), None);
        assert_eq!(
            attribute(r#"content="Tajine &amp; olives""#, "content"),
            Some("Tajine & olives".into())
        );
    }

    #[test]
    fn heading_levels() {
        assert_eq!(heading_level("h1"), Some(1));
        assert_eq!(heading_level("h6"), Some(6));
        for other in ["h7", "h", "header", "hr", "p"] {
            assert_eq!(heading_level(other), None, "{other}");
        }
    }

    /// Runs `work` on another thread and fails if it has not returned
    /// within `limit`.
    fn within(limit: std::time::Duration, work: impl FnOnce() + Send + 'static) {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            work();
            let _ = done.send(());
        });
        finished
            .recv_timeout(limit)
            .expect("still running past the limit: not linear");
    }

    /// Pages at the import's size limit made of openings that never close,
    /// of nesting as deep as it goes, or of end tags matching nothing.
    #[test]
    fn hostile_pages_are_read_in_linear_time() {
        let size = crate::recipes::import::MAX_PAGE_BYTES;
        for unit in [
            "<a",
            "<a b=\"",
            "<!--",
            "<script>",
            "<li>",
            "<ul><li>",
            "</li>",
            "<div></span>",
        ] {
            let html = unit.repeat(size / unit.len());
            within(std::time::Duration::from_secs(5), move || {
                let d = Document::parse(&html);
                let first = d
                    .tokens
                    .iter()
                    .position(|t| matches!(t, Token::Start { .. }));
                if let Some(first) = first {
                    let _ = d.text(first);
                    let _ = d.lines(first);
                    let _ = d.leaf_items(first);
                }
            });
        }
    }
}
