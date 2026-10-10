//! The Content-Security-Policy guard (#141, #325). Test-only, like
//! `design_journal`: it embeds `infra/Caddyfile`, which the shipped binary
//! has no use for.
//!
//! The policy is set by Caddy, not by this application (one `header` block
//! covers apps/web and apps/api at once), and Caddy's configuration is
//! static: no per-response nonce. Until #325 the few inline scripts apps/web
//! emitted were allowed by the hash of their text, event-handler attributes
//! included under `'unsafe-hashes'`. Since then there are none: every script
//! is a file served under `/assets` by `crate::assets` (`assets::Script`),
//! named by the digest of its content like the stylesheet, and the policy
//! allows script files from the site's own origin and nothing inline at
//! all — `script-src 'self'`.
//!
//! That holds only as long as nobody writes an inline script again: behind
//! Caddy it would silently not run, and nothing in the application's own
//! tests would notice, because they never go through Caddy (CI's e2e job
//! does since #381, but for a few pages only: `caddy-https.spec.ts`,
//! `sessions.spec.ts`). This module is what notices. Its tests hold together:
//!
//! - the markup: no event-handler attribute, no inline `<script>` and no
//!   `javascript:` URL in the production code of `src/`;
//! - the scripts: every `assets::Script` is still loaded by some page;
//! - the Caddyfile: the policy, directive by directive, set by a `header`
//!   that applies to every response, and the endpoint it reports to.

use crate::assets::Script;

const CADDYFILE: &str = include_str!("../../../infra/Caddyfile");

/// The value of the header `name` that the Caddyfile sets on **every**
/// response: inside a `header { … }` block or as `header <name> <value>`,
/// on an uncommented line, the value either `"…"`, `` `…` `` or a bare token.
/// `None` when no value holds for every response, or when the guard cannot
/// tell which one does.
///
/// The `header` directives are played in the order Caddy applies them, as
/// checked against caddy:2.11.4 (`caddy_header_follows_caddys_order_of_operations`):
///
/// 1. those with a matcher (`header @api …`, `header /login …`), which
///    Caddy sorts before the others — `*`, the matcher of every request,
///    counts as none;
/// 2. those without, in the order of the file;
/// 3. the deferred ones — holding a `-<field>`, `?<field>` or `><field>`
///    line, or `defer` — when the response is written, the first one of the
///    file last.
///
/// Inside one directive the sets come first, then the replacements
/// (`<name> <search> <replace>`), whatever the order of the lines; of several
/// sets the last one counts. A replacement leaves the value unknown (the
/// guard does not compute it), until a set in a directive applied later; a
/// replacement finding no value does nothing. `?<name>` sets a value only
/// where there is none; `+<name>` adds a second one, which leaves the value
/// unknown. A directive with a matcher that touches the field leaves it
/// unknown too — its responses get something else — unless a set without a
/// matcher applied later covers them all; deferred, nothing applies after it.
///
/// Field names match in any case, and `<name>-Report-Only` is another field:
/// it reports and blocks nothing. Once a `-<name>` anywhere (matcher or not,
/// deferred or not) deletes the field — the name in any case, or a `*`
/// wildcard covering it (`deletes`) — no value holds.
///
/// What it does **not** see — a reading of lines, not Caddy's parser: a
/// `header` nested in a `handle`, `route` or snippet (scoped by that block,
/// not by a matcher of its own) and its place among other directives, a
/// value split across lines, a block holding a `match` of response matchers,
/// the relative order of several directives with matchers, and what a
/// replacement turns the value into. What Caddy really sends is checked by
/// the `e2e` job of `ci.yml`, which goes through this Caddyfile over HTTPS
/// (`e2e/tests/caddy-https.spec.ts`, #381).
fn caddy_header(caddyfile: &str, name: &str) -> Option<String> {
    let mut directives: Vec<Directive> = Vec::new();
    let mut deleted = false;
    // Inside a `header … {` block: the directive it is.
    let mut block: Option<Directive> = None;
    for line in caddyfile.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        let field = if let Some(open) = block.as_mut() {
            if line.starts_with('}') {
                directives.extend(block.take());
                continue;
            }
            if line == "defer" {
                open.deferred = true;
                continue;
            }
            line
        } else {
            let Some(rest) = line
                .strip_prefix("header")
                .filter(|r| r.starts_with(char::is_whitespace))
            else {
                continue;
            };
            let rest = rest.trim_start();
            let scoped = rest.starts_with(['@', '/']);
            let every_request = rest.split_whitespace().next() == Some("*");
            let rest = if scoped || every_request {
                rest.split_once(char::is_whitespace)
                    .map_or("", |(_, r)| r.trim_start())
            } else {
                rest
            };
            let directive = Directive {
                scoped,
                ..Directive::default()
            };
            if rest.starts_with('{') {
                block = Some(directive);
                continue;
            }
            directives.push(directive);
            rest
        };
        let directive = block
            .as_mut()
            .or(directives.last_mut())
            .expect("a directive");
        let (prefix, rest) = match field.chars().next() {
            Some(c @ ('-' | '?' | '>' | '+')) => (Some(c), &field[1..]),
            _ => (None, field),
        };
        if matches!(prefix, Some('-' | '?' | '>')) {
            directive.deferred = true;
        }
        if prefix == Some('-') {
            // `-<name>` removes the header from the responses it applies to,
            // matcher or not: some response, then, goes without it.
            deleted |= rest
                .split_whitespace()
                .next()
                .is_some_and(|pattern| deletes(pattern, name));
            continue;
        }
        directive
            .ops
            .extend(field_value(rest, name).map(|field| match (prefix, field) {
                (None | Some('>'), Field::Set(value)) => Op::Set(value),
                (None | Some('>'), Field::Replaced) => Op::Replace,
                (Some('?'), Field::Set(value)) => Op::Default(value),
                _ => Op::Add,
            }));
    }
    if deleted {
        return None;
    }
    let (deferred, now): (Vec<_>, Vec<_>) = directives.into_iter().partition(|d| d.deferred);
    let (scoped, unscoped): (Vec<_>, Vec<_>) = now.into_iter().partition(|d| d.scoped);
    let mut value = if scoped.iter().any(|d| !d.ops.is_empty()) {
        Value::Unknown
    } else {
        Value::Absent
    };
    for directive in &unscoped {
        value = directive.apply(value);
    }
    for directive in deferred.iter().rev() {
        if directive.scoped && !directive.ops.is_empty() {
            return None;
        }
        value = directive.apply(value);
    }
    match value {
        Value::Known(value) => Some(value),
        Value::Absent | Value::Unknown => None,
    }
}

/// One `header` directive, as far as one field goes.
#[derive(Debug, Default)]
struct Directive {
    /// It carries a matcher other than `*`.
    scoped: bool,
    /// Applied when the response is written, not when the request passes.
    deferred: bool,
    /// What it does to the field, in the order of its lines.
    ops: Vec<Op>,
}

/// One line of a directive on the field.
#[derive(Debug)]
enum Op {
    /// `<name> <value>` or `><name> <value>`.
    Set(String),
    /// `?<name> <value>`: only where there is none.
    Default(String),
    /// `+<name> …`, or any form the guard does not read.
    Add,
    /// `<name> <search> <replace>`.
    Replace,
}

/// What the field holds at one point of the response's way out.
#[derive(Debug, PartialEq)]
enum Value {
    Absent,
    Known(String),
    /// Set, but to something the guard does not compute.
    Unknown,
}

impl Directive {
    /// `value` once this directive has run: its sets, then its defaults,
    /// then its replacements.
    fn apply(&self, value: Value) -> Value {
        let mut sets = self.ops.iter().filter_map(|op| match op {
            Op::Set(v) => Some(v),
            _ => None,
        });
        let defaults: Vec<_> = self
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Default(v) => Some(v),
                _ => None,
            })
            .collect();
        let added = self.ops.iter().any(|op| matches!(op, Op::Add));
        let replaced = self.ops.iter().any(|op| matches!(op, Op::Replace));
        let last_set = sets.next_back();
        if added || defaults.len() > 1 || (last_set.is_some() && !defaults.is_empty()) {
            return Value::Unknown;
        }
        let value = match (last_set, defaults.first(), value) {
            (Some(set), _, _) => Value::Known(set.clone()),
            (None, Some(default), Value::Absent) => Value::Known((*default).clone()),
            (None, _, value) => value,
        };
        match value {
            Value::Known(_) if replaced => Value::Unknown,
            value => value,
        }
    }
}

/// What a `<name> …` line does to the field.
#[derive(Debug, PartialEq)]
enum Field {
    /// `<name> <value>`: sets it.
    Set(String),
    /// `<name> <search> <replace>`: edits the value already there.
    Replaced,
}

/// What `line` does to the field `name`, if it starts with it: one value
/// (`"…"`, in which `\"` is a quote, `` `…` `` or a bare token) sets it, a
/// second one makes it Caddy's replacement form. A `#` token ends the line,
/// as in Caddy.
fn field_value(line: &str, name: &str) -> Option<Field> {
    let rest = strip_name(line, name)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let value = rest.trim();
    let (first, after) = if let Some(quoted) = value.strip_prefix('"') {
        let mut first = String::new();
        let mut chars = quoted.char_indices();
        let end = loop {
            match chars.next()? {
                (_, '\\') if quoted[chars.offset()..].starts_with('"') => {
                    first.push('"');
                    chars.next();
                }
                (at, '"') => break at,
                (_, c) => first.push(c),
            }
        };
        (first, &quoted[end + 1..])
    } else if let Some(quoted) = value.strip_prefix('`') {
        let end = quoted.find('`')?;
        (quoted[..end].to_string(), &quoted[end + 1..])
    } else {
        let (first, after) = value.split_at(value.find(char::is_whitespace).unwrap_or(value.len()));
        (first.to_string(), after)
    };
    let after = after.trim_start();
    if after.is_empty() || after.starts_with('#') {
        Some(Field::Set(first))
    } else {
        Some(Field::Replaced)
    }
}

/// `line` after `name`, matched as HTTP matches field names: in any case.
fn strip_name<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let head = line.get(..name.len())?;
    head.eq_ignore_ascii_case(name).then(|| &line[name.len()..])
}

/// Whether `-<pattern>` deletes the field `name` in Caddy: the same name in
/// any case, or a `*` wildcard — prefix (`-Content-*`), suffix
/// (`-*-Policy`) or both, a substring (`-*Security*`).
fn deletes(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.to_ascii_lowercase(), name.to_ascii_lowercase());
    if let Some(inner) = pattern.strip_prefix('*').and_then(|p| p.strip_suffix('*')) {
        name.contains(inner)
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        name.starts_with(prefix)
    } else if let Some(suffix) = pattern.strip_prefix('*') {
        name.ends_with(suffix)
    } else {
        pattern == name
    }
}

/// The policy `infra/Caddyfile` enforces on every response.
fn caddy_csp(caddyfile: &str) -> Option<String> {
    caddy_header(caddyfile, "Content-Security-Policy")
}

/// The source list of one directive, or `None` if the policy lacks it.
fn directive<'a>(policy: &'a str, name: &str) -> Option<Vec<&'a str>> {
    policy.split(';').find_map(|d| {
        let mut words = d.split_whitespace();
        (words.next()? == name).then(|| words.collect())
    })
}

/// What precedes a file's first inline `#[cfg(test)] mod … {` block.
pub(crate) fn production_code(source: &str) -> &str {
    let tests = source.match_indices("#[cfg(test)]").find(|(at, attr)| {
        let item = source[at + attr.len()..].trim_start();
        item.starts_with("mod ") && item.lines().next().is_some_and(|l| l.contains('{'))
    });
    tests.map_or(source, |(at, _)| &source[..at])
}

/// One way of running JavaScript from the markup that `script-src 'self'`
/// refuses, with the 60 characters of source it starts.
#[derive(Debug, PartialEq)]
enum Inline {
    /// `on<event>=…`.
    Handler(String),
    /// A `<script` element without a `src` — its text is the script.
    Script(String),
    /// `href="javascript:…"`, or any other attribute given such a URL.
    JsUrl(String),
}

/// Every inline script in one source file's production code
/// (`production_code`), comment lines left out. Names are matched
/// case-insensitively, as HTML reads them.
///
/// - Event handlers: `on<letters>=`, whatever follows the `=` (quoted or
///   not), where the name starts the line or follows whitespace, a quote (an
///   attribute glued to the previous one's value, `class="z"onclick=`) or a
///   `/` (`<svg/onload=`) — the separators HTML's tokenizer accepts.
/// - `<script` not followed by whitespace and `src=`: an external script is
///   the one form allowed.
/// - `javascript:` right after an `=`, an optional quote and optional
///   spaces and C0 control characters, tabs and line breaks removed first
///   (browsers drop them from a URL: `java<TAB>script:` and
///   `<U+0001>javascript:` run).
///
/// Each line is read with its escapes decoded (`unescape`): `\n`, `\t`,
/// `\x20`, `\u{20}`, `\x6f`… count as the character they compile to.
///
/// What it does **not** see — a textual scan, not an HTML parser: spaces
/// around a handler's `=`, markup assembled from pieces (`"on" + "click"`,
/// a `<script` split across two literals), entity-encoded URLs
/// (`javascript&colon;`), and Leptos `view!` attributes, which SSR does not emit as
/// handlers anyway. It also errs the other way on any attribute whose value
/// starts with `javascript:` — `title="javascript: guide"` is reported,
/// though it runs nothing. One that slipped through would run nowhere
/// behind Caddy, and show up as a broken page, not as a hole in the policy.
fn inline_scripts(source: &str) -> Vec<Inline> {
    let excerpt = |s: &str| s.chars().take(60).collect::<String>();
    let mut found = Vec::new();
    for line in production_code(source).lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let line = &unescape(line);
        // Same byte offsets as `line`: ASCII lowering keeps every length.
        let lower = line.to_ascii_lowercase();
        // Every match is recorded at its offset, then sorted, so that the
        // list reads in source order whatever its kinds.
        let mut on_line: Vec<(usize, Inline)> = Vec::new();
        for (at, _) in lower.match_indices('=') {
            let before = &lower[..at];
            let start = before
                .rfind(|c: char| !c.is_ascii_lowercase())
                .map_or(0, |i| i + 1);
            let delimited = before[..start]
                .chars()
                .next_back()
                .is_none_or(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '/'));
            if before.len() - start > 2 && before[start..].starts_with("on") && delimited {
                on_line.push((start, Inline::Handler(excerpt(&line[start..]))));
            }
        }
        // Browsers drop every tab and line break from a URL, and strip its
        // leading spaces: `href=" java<TAB>script:` runs.
        let squeezed_line: String = line
            .chars()
            .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
            .collect();
        let squeezed = squeezed_line.to_ascii_lowercase();
        for (at, _) in squeezed.match_indices("javascript:") {
            // Leading C0 controls and spaces are stripped from a URL too.
            let before =
                squeezed[..at].trim_end_matches(|c: char| c <= ' ' || c == '"' || c == '\'');
            if before.ends_with('=') {
                on_line.push((at, Inline::JsUrl(excerpt(&squeezed_line[at..]))));
            }
        }
        for (at, _) in lower.match_indices("<script") {
            let rest = &lower[at + "<script".len()..];
            let external =
                rest.starts_with(char::is_whitespace) && rest.trim_start().starts_with("src=");
            if !external {
                on_line.push((at, Inline::Script(excerpt(&line[at..]))));
            }
        }
        on_line.sort_by_key(|(at, _)| *at);
        found.extend(on_line.into_iter().map(|(_, inline)| inline));
    }
    found
}

/// `line` with the escapes of a Rust literal decoded — `\n`, `\t`, `\r`,
/// `\0`, `\\`, `\"`, `\'`, `\xHH`, `\u{H…}` — as the compiler would in a
/// non-raw literal. Applied to raw literals and code too, where it can only
/// add matches, never hide one. Anything else is kept as written.
fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let decoded = match after.chars().next() {
            Some('n') => Some(('\n', 1)),
            Some('t') => Some(('\t', 1)),
            Some('r') => Some(('\r', 1)),
            Some('0') => Some(('\0', 1)),
            Some(c @ ('\\' | '"' | '\'')) => Some((c, 1)),
            Some('x') => after
                .get(1..3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .filter(u8::is_ascii)
                .map(|b| (b as char, 3)),
            Some('u') => after
                .strip_prefix("u{")
                .and_then(|r| r.find('}').map(|end| (&r[..end], end)))
                .and_then(|(hex, end)| {
                    u32::from_str_radix(&hex.replace('_', ""), 16)
                        .ok()
                        .and_then(char::from_u32)
                        .map(|c| (c, end + 3))
                }),
            _ => None,
        };
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &after[len..];
            }
            None => {
                out.push('\\');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The scripts no production code loads: no `Script::<Variant>.tag()` in a
/// file's `production_code` once its comments and literals are removed
/// (`code_only`), `assets.rs` (which defines them) left out.
fn scripts_not_loaded(sources: &[(String, String)]) -> Vec<Script> {
    let code: Vec<String> = sources
        .iter()
        .filter(|(path, _)| !path.ends_with("/assets.rs"))
        .map(|(_, text)| code_only(production_code(text)))
        .collect();
    Script::ALL
        .into_iter()
        .filter(|script| {
            let call = format!("Script::{script:?}.tag()");
            !code.iter().any(|text| text.contains(&call))
        })
        .collect()
}

/// `source` as the compiler reads it as code: `//` comments, `/* … */`
/// comments (nested, as Rust's nest, and across lines) and the contents of
/// string literals (`"…"`, `b"…"`, raw `r#"…"#`) and char literals removed.
/// A `'` opens a char literal only when one character or one escape and a
/// closing `'` follow; otherwise it is a lifetime or a label, and kept.
fn code_only(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while let Some(c) = at(i) {
        // A raw string: `r`, or `br`, not inside an identifier, then
        // `#`s and a `"`. It ends at a `"` followed by as many `#`s.
        let r = if c == 'b' { i + 1 } else { i };
        if !ident(i.checked_sub(1).and_then(at)) && at(r) == Some('r') {
            let hashes = (r + 1..).take_while(|&j| at(j) == Some('#')).count();
            if at(r + 1 + hashes) == Some('"') {
                let mut j = r + 2 + hashes;
                while j < chars.len()
                    && !(chars[j] == '"' && (1..=hashes).all(|k| at(j + k) == Some('#')))
                {
                    j += 1;
                }
                out.push_str("\"\"");
                i = j + 1 + hashes;
                continue;
            }
        }
        match (c, at(i + 1)) {
            ('/', Some('/')) => {
                while at(i).is_some_and(|c| c != '\n') {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                let mut depth = 0;
                while i < chars.len() {
                    match (chars[i], at(i + 1)) {
                        ('/', Some('*')) => {
                            depth += 1;
                            i += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => i += 1,
                    }
                }
                out.push(' ');
            }
            ('"', _) => {
                i += 1;
                while let Some(c) = at(i) {
                    i += if c == '\\' { 2 } else { 1 };
                    if c == '"' {
                        break;
                    }
                }
                out.push_str("\"\"");
            }
            ('\'', Some('\\')) => {
                i += 3;
                while at(i).is_some_and(|c| c != '\'') {
                    i += 1;
                }
                i += 1;
                out.push_str("' '");
            }
            ('\'', Some(_)) if at(i + 2) == Some('\'') => {
                i += 3;
                out.push_str("' '");
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                out.push((path.display().to_string(), text));
            }
        }
    }
    let mut out = Vec::new();
    walk(
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &mut out,
    );
    // This file quotes the forms it looks for.
    out.retain(|(path, _)| !path.ends_with("/csp.rs"));
    out
}

#[test]
fn caddy_csp_reads_the_quoted_value_and_skips_comments() {
    let file = "# header Content-Security-Policy \"commented\"\n\
                \theader {\n\
                \t\tContent-Security-Policy \"default-src 'self'; script-src 'self'\"\n\
                \t}\n";
    assert_eq!(
        caddy_csp(file).as_deref(),
        Some("default-src 'self'; script-src 'self'")
    );
    assert_eq!(caddy_csp("header X-Frame-Options DENY\n").as_deref(), None);
}

#[test]
fn caddy_csp_reads_the_enforced_header_only() {
    // Report-only reports and blocks nothing: it must not satisfy the guard.
    let report_only = "\theader {\n\
                       \t\tContent-Security-Policy-Report-Only \"default-src 'self'\"\n\
                       \t}\n";
    assert_eq!(caddy_csp(report_only).as_deref(), None);
    let one_line = "\theader Content-Security-Policy \"default-src 'none'\"\n";
    assert_eq!(caddy_csp(one_line).as_deref(), Some("default-src 'none'"));
    let other = "\theader X-Content-Security-Policy \"default-src 'none'\"\n";
    assert_eq!(caddy_csp(other).as_deref(), None);
}

/// A matcher scopes a `header` to the responses it matches: a policy set
/// that way protects one path and leaves every other page without one.
#[test]
fn caddy_csp_ignores_a_header_scoped_by_a_matcher() {
    for file in [
        "\theader @api {\n\t\tContent-Security-Policy \"default-src 'none'\"\n\t}\n",
        "\theader /login {\n\t\tContent-Security-Policy \"default-src 'none'\"\n\t}\n",
        "\theader @api Content-Security-Policy \"default-src 'none'\"\n",
        "\theader /login Content-Security-Policy \"default-src 'none'\"\n",
    ] {
        assert_eq!(caddy_csp(file).as_deref(), None, "{file}");
    }
    // The block after a scoped one is read again, and a line outside any
    // `header` is not a header.
    let file = "\theader @api {\n\t\tContent-Security-Policy \"scoped\"\n\t}\n\
                \tContent-Security-Policy \"loose\"\n\
                \theader {\n\t\tContent-Security-Policy \"global\"\n\t}\n";
    assert_eq!(caddy_csp(file).as_deref(), Some("global"));
}

/// `*` is the matcher of every request: a `header *` sets the field on every
/// response, as a `header` without any matcher does.
#[test]
fn caddy_csp_reads_a_header_on_the_matcher_of_every_request() {
    let block = "\theader * {\n\t\tContent-Security-Policy \"default-src 'none'\"\n\t}\n";
    assert_eq!(caddy_csp(block).as_deref(), Some("default-src 'none'"));
    let one_line = "\theader * Content-Security-Policy \"default-src 'self'\"\n";
    assert_eq!(caddy_csp(one_line).as_deref(), Some("default-src 'self'"));
    let deleted = "\theader Content-Security-Policy \"default-src 'self'\"\n\theader * -Content-Security-Policy\n";
    assert_eq!(caddy_csp(deleted).as_deref(), None);
}

/// Caddy's replacement form, `<name> <search> <replace>`, edits the value
/// sent rather than setting one: what goes out is no longer what the file
/// wrote, on the responses it applies to, matcher or not.
#[test]
fn caddy_csp_is_none_once_a_replacement_edits_it() {
    for file in [
        "\theader {\n\t\tContent-Security-Policy \"script-src 'self'\"\n\t}\n\theader Content-Security-Policy \"'self'\" \"'self' 'unsafe-inline'\"\n",
        "\theader {\n\t\tContent-Security-Policy \"script-src 'self'\"\n\t\tContent-Security-Policy `'self'` `*`\n\t}\n",
        // Deferred, the one with a matcher edits the value on the responses
        // it matches (not deferred, it would run first and edit nothing).
        "\theader {\n\t\tContent-Security-Policy \"script-src 'self'\"\n\t}\n\theader @api {\n\t\t-Server\n\t\tContent-Security-Policy self none\n\t}\n",
    ] {
        assert_eq!(caddy_csp(file).as_deref(), None, "{file}");
    }
    // A set in a later `header` directive sets the value anew: Caddy sends
    // that one.
    let file = "\theader Content-Security-Policy \"a\" \"b\"\n\
                \theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n";
    assert_eq!(caddy_csp(file).as_deref(), Some("default-src 'self'"));
    // Within one `header` block Caddy applies every set before any
    // replacement, whatever the order of the lines: the replacement edits
    // the value set after it (seen on caddy:2.11.4).
    let file = "\theader {\n\
                \t\tContent-Security-Policy \"script-src 'self'\" \"script-src 'self' 'unsafe-inline'\"\n\
                \t\tContent-Security-Policy \"default-src 'self'; script-src 'self'\"\n\
                \t}\n";
    assert_eq!(caddy_csp(file).as_deref(), None);
    // An escaped quote inside a quoted value is part of the value, not its
    // end followed by a second token.
    let file = "\theader X-A \"a\\\"b\"\n";
    assert_eq!(caddy_header(file, "X-A").as_deref(), Some("a\"b"));
    // One value, quoted or bare, is a value, whatever it holds.
    let file = "\theader {\n\t\tContent-Security-Policy \"a b\"\n\t\tX-Frame-Options DENY\n\t}\n";
    assert_eq!(caddy_csp(file).as_deref(), Some("a b"));
    assert_eq!(
        caddy_header(file, "X-Frame-Options").as_deref(),
        Some("DENY")
    );
}

/// The order in which Caddy applies `header` directives, each case checked
/// against caddy:2.11.4 (what it sent is the expected value, or `None` where
/// the guard declines to tell). The directives with a matcher run before the
/// others; those without a `-`, `?`, `>` or `defer` run when the request
/// passes, in the order of the file; the deferred ones when the response is
/// written, the first one of the file last. Inside one directive the sets
/// come before the replacements.
#[test]
fn caddy_header_follows_caddys_order_of_operations() {
    let x = |file: &str| caddy_header(file, "X-T");
    let wrong = [
        // A deferred directive applies after every directive that is not.
        (
            "header {\n-Server\nX-T \"first\"\n}\nheader X-T \"second\"\n",
            Some("first"),
        ),
        ("header >X-T \"a\"\nheader X-T \"b\"\n", Some("a")),
        (
            "header {\nX-T \"a\"\ndefer\n}\nheader X-T \"b\"\n",
            Some("a"),
        ),
        (
            "header {\n-Server\nX-T \"a\"\n}\nheader X-T \"a\" \"z\"\n",
            Some("a"),
        ),
        (
            "header X-T \"a\"\nheader {\n-Server\nX-T \"a\" \"z\"\n}\n",
            None,
        ),
        // Of two deferred directives, the first of the file applies last.
        (
            "header {\n-Server\nX-T \"first\"\n}\nheader {\n-Foo\nX-T \"second\"\n}\n",
            Some("first"),
        ),
        ("header >X-T \"a\"\nheader >X-T \"b\"\n", Some("a")),
        (
            "header >X-T \"a\"\nheader {\n-Server\nX-T \"b\"\n}\n",
            Some("a"),
        ),
        // Not deferred: the order of the file; a replacement finding no
        // value does nothing.
        ("header X-T \"a\"\nheader X-T \"b\"\n", Some("b")),
        ("header {\nX-T \"a\"\nX-T \"b\"\n}\n", Some("b")),
        (
            "header X-T \"a\"\nheader X-T \"a\" \"z\"\nheader X-T \"b\"\n",
            Some("b"),
        ),
        ("header X-T \"a\" \"z\"\nheader X-T \"a\"\n", Some("a")),
        // `?` sets a value only where there is none, when the response is
        // written; `+` adds a second one.
        ("header X-T \"a\"\nheader ?X-T \"b\"\n", Some("a")),
        ("header {\n?X-T \"d\"\n}\nheader X-T \"b\"\n", Some("b")),
        ("header ?X-T \"d\"\n", Some("d")),
        ("header X-T \"a\"\nheader +X-T \"b\"\n", None),
        ("header {\nX-T \"a\"\n?X-T \"d\"\n}\n", None),
        // A directive with a matcher runs first: a later one without
        // overrides it, wherever the file writes it.
        (
            "header X-T \"global\"\nheader /x X-T \"scoped\"\n",
            Some("global"),
        ),
        (
            "@all path *\nheader X-T \"global\"\nheader @all X-T \"named\"\n",
            Some("global"),
        ),
        (
            "@all path *\nheader X-T \"a\"\nheader @all X-T \"a\" \"z\"\n",
            Some("a"),
        ),
        // Unless it is deferred: it then applies last, on the responses it
        // matches only.
        (
            "@all path *\nheader X-T \"g\"\nheader @all {\n-Server\nX-T \"s\"\n}\n",
            None,
        ),
        (
            "header /x {\n-Server\nX-T \"s\"\n}\nheader {\n-Foo\nX-T \"g\"\n}\n",
            None,
        ),
    ]
    .into_iter()
    .filter(|(file, sent)| x(file).as_deref() != *sent)
    .map(|(file, sent)| format!("{file:?}: expected {sent:?}, read {:?}", x(file)))
    .collect::<Vec<_>>();
    // The case found in review: a deferred rewrite outlives a later set.
    let file = "header {\n-Server\nContent-Security-Policy \"script-src 'self'\" \"script-src 'self' 'unsafe-inline'\"\n}\n\
                header {\nContent-Security-Policy \"default-src 'self'; script-src 'self'\"\n}\n";
    let review = caddy_csp(file);
    assert!(
        wrong.is_empty() && review.is_none(),
        "\n{}\nreview case: {review:?}",
        wrong.join("\n")
    );
}

/// A `-Content-Security-Policy` anywhere — later in the file, inside the
/// same block, or under a matcher — takes the policy off the responses it
/// applies to: the file no longer sets one on every response.
#[test]
fn caddy_csp_is_none_once_a_header_deletes_it() {
    for file in [
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader -Content-Security-Policy\n",
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t\t-Content-Security-Policy\n\t}\n",
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader @api {\n\t\t-Content-Security-Policy\n\t}\n",
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader /login -Content-Security-Policy\n",
        // Header names are case-insensitive, and Caddy deletes by prefix or
        // suffix wildcard.
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader -content-security-policy\n",
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader -Content-Security-*\n",
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader -Content-*\n",
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader -*-Policy\n",
        "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t}\n\theader -*security*\n",
    ] {
        assert_eq!(caddy_csp(file).as_deref(), None, "{file}");
    }
    // Deleting another header, or one whose name only starts the same, is
    // not deleting this one.
    let file = "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t\t-Server\n\t\t-Content-Security-Policy-Report-Only\n\t}\n";
    assert_eq!(caddy_csp(file).as_deref(), Some("default-src 'self'"));
    let file = "\theader {\n\t\tContent-Security-Policy \"default-src 'self'\"\n\t\t-X-*\n\t\t-*Frame*\n\t}\n";
    assert_eq!(caddy_csp(file).as_deref(), Some("default-src 'self'"));
}

/// Two `header` directives setting the same field: Caddy sends the last one,
/// so that is the one the guard reads; and the name matches in any case.
#[test]
fn caddy_csp_reads_the_last_value_set_in_any_case() {
    let file = "\theader Content-Security-Policy \"first\"\n\
                \theader {\n\t\tcontent-security-policy \"second\"\n\t}\n";
    assert_eq!(caddy_csp(file).as_deref(), Some("second"));
}

#[test]
fn caddy_header_reads_bare_and_backquoted_values() {
    let file = "\theader {\n\
                \t\tX-Frame-Options DENY\n\
                \t\tReporting-Endpoints `csp=\"/api/csp-report\"`\n\
                \t}\n\
                \theader_up Referrer-Policy origin\n";
    assert_eq!(
        caddy_header(file, "X-Frame-Options").as_deref(),
        Some("DENY")
    );
    assert_eq!(
        caddy_header(file, "Reporting-Endpoints").as_deref(),
        Some("csp=\"/api/csp-report\"")
    );
    assert_eq!(caddy_header(file, "Referrer-Policy").as_deref(), None);
}

#[test]
fn directive_returns_the_named_source_list_only() {
    let policy = "default-src 'self'; script-src 'unsafe-hashes' 'sha256-a=' ;img-src 'self'";
    assert_eq!(
        directive(policy, "script-src"),
        Some(vec!["'unsafe-hashes'", "'sha256-a='"])
    );
    assert_eq!(directive(policy, "img-src"), Some(vec!["'self'"]));
    assert_eq!(directive(policy, "script"), None);
    assert_eq!(directive(policy, "style-src"), None);
}

#[test]
fn inline_scripts_finds_handlers_script_elements_and_javascript_urls() {
    let src = r##"format!(r#"<input onchange="{SUBMIT}"/>"#);
let raw = r#"<button onclick="alert(1)">"#;
// <button onclick="commented()">
#[cfg(test)]
mod declared_elsewhere;
let s = format!("<script>{LIVE}</script>");
let t = format!(r#"<script src="{href}"></script>"#);
let u = r#"<a href="javascript:alert(2)">"#;
let not_a_handler = r#"<a href="/x" data-on="x" aria-controls="y" title="online">"#;
let one = 1;
#[cfg(test)]
mod tests { const X: &str = r#"<b onclick="x()">"#; }
"##;
    assert_eq!(
        inline_scripts(src),
        vec![
            Inline::Handler("onchange=\"{SUBMIT}\"/>\"#);".into()),
            Inline::Handler("onclick=\"alert(1)\">\"#;".into()),
            Inline::Script("<script>{LIVE}</script>\");".into()),
            Inline::JsUrl("javascript:alert(2)\">\"#;".into()),
        ]
    );
}

/// What sits between the separator and the name in the Rust source rather
/// than in the HTML: a leading space inside a `javascript:` URL, which
/// browsers strip, and any escape of a non-raw literal — `\n`, `\t`, `\r`,
/// `\x..`, `\u{..}` — read as the character it compiles to.
#[test]
fn inline_scripts_sees_through_leading_spaces_and_rust_escapes() {
    let src = r##"let a = r#"<a href=" javascript:x()">"#;
let b = "<b\nonclick=\"x()\">";
let c = "<b\tonload=x()>";
let d = "<b\ronfocus='x()'>";
let e = "\none = 1";
let f = "<b\x20onblur=x()>";
let g = "<b\u{20}oninput=x()>";
let h = "<b\x0conkeyup=x()>";
let i = "<b \x6fnclick=x()>";
"##;
    assert_eq!(
        inline_scripts(src),
        vec![
            Inline::JsUrl("javascript:x()\">\"#;".into()),
            Inline::Handler("onclick=\"x()\">\";".into()),
            Inline::Handler("onload=x()>\";".into()),
            Inline::Handler("onfocus='x()'>\";".into()),
            Inline::Handler("onblur=x()>\";".into()),
            Inline::Handler("oninput=x()>\";".into()),
            Inline::Handler("onkeyup=x()>\";".into()),
            Inline::Handler("onclick=x()>\";".into()),
        ]
    );
}

/// Browsers drop every tab and line break from a URL, wherever it sits:
/// `java<TAB>script:` is `javascript:`.
#[test]
fn inline_scripts_sees_a_javascript_url_split_by_tabs_or_line_breaks() {
    let src = "let a = \"<a href='java\\tscript:x()'>\";\nlet b = \"<a href='java\tscr\\nipt:y()'>\";\nlet c = \"<a href='java script:z()'>\";\n";
    assert_eq!(
        inline_scripts(src),
        vec![
            Inline::JsUrl("javascript:x()'>\";".into()),
            Inline::JsUrl("javascript:y()'>\";".into()),
        ]
    );
}

/// Browsers strip every C0 control character and space that opens a URL,
/// not only tabs and line breaks: `href="\x01javascript:…"` runs. Nothing
/// else: after DEL or a no-break space, the URL is relative and runs nothing.
#[test]
fn inline_scripts_sees_a_control_character_before_a_javascript_url() {
    let src = r##"let a = "<a href=\"\x01javascript:x()\">";
let b = "<a href='\x1f \x0bjavascript:y()'>";
let c = "<a href='\x7fjavascript:z()'>";
let d = "<a href='\u{a0}javascript:w()'>";
"##;
    assert_eq!(
        inline_scripts(src),
        vec![
            Inline::JsUrl("javascript:x()\">\";".into()),
            Inline::JsUrl("javascript:y()'>\";".into()),
        ]
    );
}

/// The forms HTML accepts beyond the house style: an attribute at the start
/// of a line, single quotes, no quotes, upper-case names (HTML names are
/// case-insensitive, so `<SCRIPT>` and `ONCLICK` run all the same), an
/// attribute glued to the previous one's closing quote, and `/` as the
/// separator after the tag name.
#[test]
fn inline_scripts_finds_the_other_spellings_html_accepts() {
    let src = r##"let a = r#"<button
onclick="alert(1)">"#;
let b = r#"<button onclick='alert(2)'>"#;
let c = r#"<b ONCLICK=go()>"#;
let d = "<SCRIPT>{LIVE}</SCRIPT>";
let e = r#"<i title="z"onclick="x()">"#;
let f = r#"<svg/onload=alert(3)>"#;
let g = r#"<a HREF='JavaScript:void(0)'>"#;
let h = "<script
src=x>";
"##;
    assert_eq!(
        inline_scripts(src),
        vec![
            Inline::Handler("onclick=\"alert(1)\">\"#;".into()),
            Inline::Handler("onclick='alert(2)'>\"#;".into()),
            Inline::Handler("ONCLICK=go()>\"#;".into()),
            Inline::Script("<SCRIPT>{LIVE}</SCRIPT>\";".into()),
            Inline::Handler("onclick=\"x()\">\"#;".into()),
            Inline::Handler("onload=alert(3)>\"#;".into()),
            Inline::JsUrl("JavaScript:void(0)'>\"#;".into()),
            Inline::Script("<script".into()),
        ]
    );
}

#[test]
fn no_inline_script_in_the_markup() {
    let stray: Vec<String> = sources()
        .iter()
        .flat_map(|(path, text)| {
            inline_scripts(text)
                .into_iter()
                .map(move |found| format!("{path}: {found:?}"))
        })
        .collect();
    assert!(
        stray.is_empty(),
        "inline JS, which infra/Caddyfile's `script-src 'self'` refuses: \
         move it into a script under /assets (`assets::Script`), and give the \
         markup a data-* attribute for it to find:\n{}",
        stray.join("\n")
    );
}

/// A script nobody loads any more is still served, and still allowed by
/// `'self'`: dead code that keeps running rights.
#[test]
fn every_script_is_loaded_by_some_page() {
    assert_eq!(
        scripts_not_loaded(&sources()),
        vec![],
        "served but loaded by no page: drop them"
    );
}

/// Only a call loads a script: a doc comment naming it, a comment, a test
/// or the bare variant does not.
#[test]
fn scripts_not_loaded_counts_tag_calls_in_production_code_only() {
    let page = "/// Loads `Script::MessagerieLive.tag()` on the live view.\n\
                // html.push_str(&Script::Push.tag());\n\
                let s = Script::ResetPassword;\n\
                html.push_str(&crate::assets::Script::Enhance.tag());\n\
                #[cfg(test)]\n\
                mod tests {\n    fn t() { Script::Push.tag(); }\n}\n";
    assert_eq!(
        scripts_not_loaded(&[("src/page.rs".to_string(), page.to_string())]),
        vec![Script::ResetPassword, Script::MessagerieLive, Script::Push]
    );
    // A call commented out at the end of a line or inside a block comment
    // is no call either.
    let page = "html.push_str(&String::new()); // Script::Enhance.tag()\n\
                let s = String::new() /* Script::ResetPassword.tag() */;\n\
                let t = Script::MessagerieLive.tag(); /* note */\n\
                let v = Script::Push.tag();\n";
    assert_eq!(
        scripts_not_loaded(&[("src/page.rs".to_string(), page.to_string())]),
        vec![Script::Enhance, Script::ResetPassword]
    );
    // A block comment spanning lines, nested block comments (Rust's nest)
    // and a string literal, raw or not, hold no call either.
    let page = r###"/* a note
   html.push_str(&Script::Enhance.tag());
*/
/* outer /* inner */ Script::ResetPassword.tag() */
let s = "Script::MessagerieLive.tag()";
let r = r#"say "Script::Push.tag()" here"#;
"###;
    assert_eq!(
        scripts_not_loaded(&[("src/page.rs".to_string(), page.to_string())]),
        Script::ALL.to_vec()
    );
    // A quote in a char literal opens no string, a lifetime is no char
    // literal, and comment markers inside a string comment nothing out.
    let page = r###"let q = '"'; html.push_str(&Script::Enhance.tag());
fn f<'a>(x: &'a str) { Script::ResetPassword.tag(); }
let e = '\''; let u = "https://x"; Script::MessagerieLive.tag();
let b = b"/*"; Script::Push.tag(); let c = "*/";
"###;
    assert_eq!(
        scripts_not_loaded(&[("src/page.rs".to_string(), page.to_string())]),
        vec![]
    );
    // `assets.rs` defines `tag` and calls nothing.
    let assets = "fn f() { Script::Push.tag() }\n";
    assert_eq!(
        scripts_not_loaded(&[("src/assets.rs".to_string(), assets.to_string())]),
        Script::ALL.to_vec()
    );
}

#[test]
fn the_caddyfile_allows_script_files_from_the_site_and_nothing_inline() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(&policy, "script-src"), Some(vec!["'self'"]));
}

/// The reminder notifications' service worker (#306) is a script file,
/// `/sw.js`. Without `worker-src` a browser falls back on `script-src`;
/// written out so that tightening `script-src` one day cannot take the
/// worker down with it.
#[test]
fn the_caddyfile_lets_the_service_worker_register_and_nothing_else() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(&policy, "worker-src"), Some(vec!["'self'"]));
}

/// Styles: the sheet under /assets and no `<style>` element; the `style="…"`
/// attributes some routes write (an avatar's colour, a column's alignment)
/// through `style-src-attr`.
#[test]
fn the_caddyfile_allows_the_stylesheet_and_style_attributes_only() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(&policy, "style-src"), Some(vec!["'self'"]));
    assert_eq!(
        directive(&policy, "style-src-attr"),
        Some(vec!["'unsafe-inline'"])
    );
}

/// `fetch` and the Messagerie's WebSocket (under /api, same origin) go
/// through `connect-src`, which is left to fall back on `default-src
/// 'self'`: a `connect-src` of its own is a place to widen it unseen.
#[test]
fn the_caddyfile_leaves_connections_to_default_src() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(&policy, "connect-src"), None);
    assert_eq!(directive(&policy, "default-src"), Some(vec!["'self'"]));
}

/// Violations are reported to apps/api (`csp_report.rs`), on the site's own
/// origin: `report-to` for the browsers that implement the Reporting API,
/// `report-uri` for the others, both to the same path.
#[test]
fn the_caddyfile_reports_violations_to_the_api_and_nowhere_else() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(
        directive(&policy, "report-uri"),
        Some(vec!["/api/csp-report"])
    );
    assert_eq!(directive(&policy, "report-to"), Some(vec!["csp"]));
    assert_eq!(
        caddy_header(CADDYFILE, "Reporting-Endpoints").as_deref(),
        Some("csp=\"/api/csp-report\"")
    );
}

#[test]
fn the_caddyfile_sets_the_other_security_headers() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(&policy, "default-src"), Some(vec!["'self'"]));
    assert_eq!(directive(&policy, "object-src"), Some(vec!["'none'"]));
    assert_eq!(directive(&policy, "base-uri"), Some(vec!["'none'"]));
    assert_eq!(directive(&policy, "frame-ancestors"), Some(vec!["'none'"]));
    assert_eq!(directive(&policy, "form-action"), Some(vec!["'self'"]));
    let live: Vec<&str> = CADDYFILE
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    for header in [
        "Strict-Transport-Security \"max-age=",
        "X-Content-Type-Options nosniff",
        "X-Frame-Options DENY",
        "Referrer-Policy no-referrer",
    ] {
        assert!(
            live.iter().any(|l| l.starts_with(header)),
            "infra/Caddyfile does not set `{header}`"
        );
    }
}
