//! The Content-Security-Policy guard (#141). Test-only, like
//! `design_journal`: it embeds `infra/Caddyfile`, which the shipped binary
//! has no use for.
//!
//! The policy is set by Caddy, not by this application (one `header` block
//! covers apps/web and apps/api at once), and Caddy's configuration is
//! static: no per-response nonce. So the few inline scripts apps/web still
//! emits are allowed **by hash** — a `<script>` element's text, or an event
//! handler attribute's value under `'unsafe-hashes'`. A hash names one exact
//! string: the day someone edits one of those scripts without updating the
//! Caddyfile, the browser silently refuses to run it, and nothing in the
//! application's own tests notices, because they never go through Caddy.
//!
//! This module is what notices. Every inline script is a named constant,
//! referenced from the markup as `{NAME}`, and listed below; the tests hold
//! three things together:
//!
//! - the markup: no inline handler or `<script>` in `src/` that is not one
//!   of the listed constants, interpolated whole;
//! - the list: every constant is still emitted somewhere;
//! - the Caddyfile: its `script-src` carries exactly the hashes of the
//!   listed constants — none missing (a broken page), none extra (an
//!   allowance nobody needs any more).

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::app::PW_TOGGLE;
use crate::routes::account::delete::CONFIRM_ACCOUNT_DELETE;
use crate::routes::admin::users::{CONFIRM_DEACTIVATE, CONFIRM_REFUSE_REACTIVATION};
use crate::routes::agenda::new::ALL_DAY_TOGGLE;
use crate::routes::auth::reset_password::FRAGMENT_SCRIPT;
use crate::routes::grocery_list::list::SUBMIT_ON_CHANGE;
use crate::routes::messagerie::thread::LIVE_SCRIPT;

const CADDYFILE: &str = include_str!("../../../infra/Caddyfile");

/// Values of inline event-handler attributes (`onclick="{NAME}"`…).
const HANDLERS: &[(&str, &str)] = &[
    ("PW_TOGGLE", PW_TOGGLE),
    ("ALL_DAY_TOGGLE", ALL_DAY_TOGGLE),
    ("SUBMIT_ON_CHANGE", SUBMIT_ON_CHANGE),
    ("CONFIRM_DEACTIVATE", CONFIRM_DEACTIVATE),
    ("CONFIRM_REFUSE_REACTIVATION", CONFIRM_REFUSE_REACTIVATION),
    ("CONFIRM_ACCOUNT_DELETE", CONFIRM_ACCOUNT_DELETE),
];

/// Texts of inline `<script>{NAME}</script>` elements.
const SCRIPTS: &[(&str, &str)] = &[
    ("FRAGMENT_SCRIPT", FRAGMENT_SCRIPT),
    ("LIVE_SCRIPT", LIVE_SCRIPT),
];

/// The CSP source expression that allows exactly `js`: SHA-256 of its UTF-8
/// bytes, standard base64 with padding, quoted (CSP Level 3, "hash-source").
fn csp_hash(js: &str) -> String {
    let digest = Sha256::digest(js.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// The value of the Caddyfile's `Content-Security-Policy` header: the
/// double-quoted string on the (uncommented) line whose field name is
/// exactly that — inside a `header { … }` block or as `header <name> "…"`.
/// `Content-Security-Policy-Report-Only` does not count: it blocks nothing.
fn caddy_csp(caddyfile: &str) -> Option<&str> {
    const NAME: &str = "Content-Security-Policy";
    caddyfile
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix("header").map_or(l, str::trim_start);
            let rest = rest.strip_prefix(NAME)?;
            if !rest.starts_with(char::is_whitespace) {
                return None;
            }
            let value = rest.trim_start().strip_prefix('"')?;
            Some(&value[..value.find('"')?])
        })
}

/// The source list of one directive, or `None` if the policy lacks it.
fn directive<'a>(policy: &'a str, name: &str) -> Option<Vec<&'a str>> {
    policy.split(';').find_map(|d| {
        let mut words = d.split_whitespace();
        (words.next()? == name).then(|| words.collect())
    })
}

/// One inline script found in the markup: which kind, and what stands where
/// the script text should be — `Ok(NAME)` for a `{NAME}` interpolation,
/// `Err(raw)` for anything else.
#[derive(Debug, PartialEq)]
enum Inline {
    Handler(Result<String, String>),
    Script(Result<String, String>),
}

/// `{NAME}` followed by `end`, at the start of `s`.
fn interpolated(s: &str, end: &str) -> Result<String, String> {
    let raw = || s.chars().take(60).collect::<String>();
    let inner = s.strip_prefix('{').ok_or_else(raw)?;
    let close = inner.find('}').ok_or_else(raw)?;
    let name = &inner[..close];
    let well_formed = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && inner[close + 1..]
            .get(..end.len())
            .is_some_and(|after| after.eq_ignore_ascii_case(end));
    if well_formed {
        Ok(name.to_string())
    } else {
        Err(raw())
    }
}

/// What precedes a file's first inline `#[cfg(test)] mod … {` block.
pub(crate) fn production_code(source: &str) -> &str {
    let tests = source.match_indices("#[cfg(test)]").find(|(at, attr)| {
        let item = source[at + attr.len()..].trim_start();
        item.starts_with("mod ") && item.lines().next().is_some_and(|l| l.contains('{'))
    });
    tests.map_or(source, |(at, _)| &source[..at])
}

/// Every inline event handler (`on<event>="…"` or `='…'`, after whitespace
/// or at the start of a line) and `<script` in one source file's production
/// code (`production_code`), comment lines left out. Names are matched
/// case-insensitively, as HTML reads them.
///
/// What it does **not** see — a textual scan, not an HTML parser: an
/// unquoted handler (`onclick=f()`), spaces around the `=`, and markup
/// assembled from pieces (`"on" + "click"`, a `<script` split across two
/// literals, or Leptos `view!` attributes, which SSR does not emit as
/// handlers anyway). None of those appear in `src/` today; one that did
/// would run nowhere behind Caddy, and show up as a broken page, not as a
/// hole in the policy.
fn inline_scripts(source: &str) -> Vec<Inline> {
    let production = production_code(source);
    let mut found = Vec::new();
    for line in production.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        // Same byte offsets as `line`: ASCII lowering keeps every length.
        let lower = line.to_ascii_lowercase();
        for (at, _) in lower.match_indices('=') {
            let Some(quote) = lower[at + 1..]
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
            else {
                continue;
            };
            let before = &lower[..at];
            let word_start = before
                .rfind(|c: char| !c.is_ascii_lowercase())
                .map_or(0, |i| i + 1);
            let word = &before[word_start..];
            let delimited = word_start == 0 || before[..word_start].ends_with(char::is_whitespace);
            if word.len() > 2 && word.starts_with("on") && delimited {
                let end = quote.to_string();
                found.push(Inline::Handler(interpolated(&line[at + 2..], &end)));
            }
        }
        for (at, _) in lower.match_indices("<script") {
            let rest = &line[at + "<script".len()..];
            found.push(Inline::Script(match rest.strip_prefix('>') {
                Some(body) => interpolated(body, "</script>"),
                None => Err(rest.chars().take(60).collect()),
            }));
        }
    }
    found
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
fn csp_hash_matches_a_known_answer() {
    // `printf '%s' 'this.form.submit()' | openssl dgst -sha256 -binary | base64`
    assert_eq!(
        csp_hash("this.form.submit()"),
        "'sha256-osjxnKEPL/pQJbFk1dKsF7PYFmTyMWGmVSiL9inhxJY='"
    );
    assert_eq!(
        csp_hash(""),
        "'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='"
    );
}

#[test]
fn caddy_csp_reads_the_quoted_value_and_skips_comments() {
    let file = "# header Content-Security-Policy \"commented\"\n\
                \theader {\n\
                \t\tContent-Security-Policy \"default-src 'self'; script-src 'sha256-x='\"\n\
                \t}\n";
    assert_eq!(
        caddy_csp(file),
        Some("default-src 'self'; script-src 'sha256-x='")
    );
    assert_eq!(caddy_csp("header X-Frame-Options DENY\n"), None);
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
fn inline_scripts_finds_handlers_and_script_elements() {
    let src = r##"format!(r#"<input onchange="{SUBMIT}"/>"#);
let raw = r#"<button onclick="alert(1)">"#;
// <button onclick="commented()">
#[cfg(test)]
mod declared_elsewhere;
let s = format!("<script>{LIVE}</script>");
let t = "<script src=x></script>";
let not_a_handler = r#"<a href="/x" data-on="x" aria-controls="y">"#;
#[cfg(test)]
mod tests { const X: &str = r#"<b onclick="x()">"#; }
"##;
    assert_eq!(
        inline_scripts(src),
        vec![
            Inline::Handler(Ok("SUBMIT".into())),
            Inline::Handler(Err("alert(1)\">\"#;".into())),
            Inline::Script(Ok("LIVE".into())),
            Inline::Script(Err(" src=x></script>\";".into())),
        ]
    );
}

/// The forms HTML accepts beyond the house style: an attribute at the start
/// of a line, single quotes, and upper-case names (HTML names are
/// case-insensitive, so `<SCRIPT>` and `ONCLICK` run all the same).
#[test]
fn inline_scripts_finds_the_other_spellings_html_accepts() {
    let src = r##"let a = r#"<button
onclick="alert(1)">"#;
let b = r#"<button onclick='alert(2)'>"#;
let c = r#"<b ONCLICK="{PW_TOGGLE}">"#;
let d = "<SCRIPT>{LIVE}</SCRIPT>";
let e = "<Script>alert(3)</Script>";
let f = r#"<i onchange='{SUBMIT}'>"#;
"##;
    assert_eq!(
        inline_scripts(src),
        vec![
            Inline::Handler(Err("alert(1)\">\"#;".into())),
            Inline::Handler(Err("alert(2)'>\"#;".into())),
            Inline::Handler(Ok("PW_TOGGLE".into())),
            Inline::Script(Ok("LIVE".into())),
            Inline::Script(Err("alert(3)</Script>\";".into())),
            Inline::Handler(Ok("SUBMIT".into())),
        ]
    );
}

#[test]
fn caddy_csp_reads_the_enforced_header_only() {
    // Report-only reports and blocks nothing: it must not satisfy the guard.
    let report_only = "\theader {\n\
                       \t\tContent-Security-Policy-Report-Only \"default-src 'self'\"\n\
                       \t}\n";
    assert_eq!(caddy_csp(report_only), None);
    let one_line = "\theader Content-Security-Policy \"default-src 'none'\"\n";
    assert_eq!(caddy_csp(one_line), Some("default-src 'none'"));
    let other = "\theader X-Content-Security-Policy \"default-src 'none'\"\n";
    assert_eq!(caddy_csp(other), None);
}

#[test]
fn every_inline_script_in_the_markup_is_a_registered_constant() {
    let mut used: Vec<String> = Vec::new();
    let mut stray = Vec::new();
    for (path, text) in sources() {
        for found in inline_scripts(&text) {
            match found {
                Inline::Handler(Ok(n)) if HANDLERS.iter().any(|(k, _)| *k == n) => used.push(n),
                Inline::Script(Ok(n)) if SCRIPTS.iter().any(|(k, _)| *k == n) => used.push(n),
                other => stray.push(format!("{path}: {other:?}")),
            }
        }
    }
    assert!(
        stray.is_empty(),
        "inline JS that the Caddyfile's CSP cannot allow: move it into a \
         constant, list it in csp.rs and add its hash to infra/Caddyfile:\n{}",
        stray.join("\n")
    );
    for (name, _) in HANDLERS.iter().chain(SCRIPTS) {
        assert!(
            used.iter().any(|u| u == name),
            "{name} is listed but no longer emitted: drop it here and its hash from infra/Caddyfile"
        );
    }
}

#[test]
fn registered_scripts_are_hashed_as_the_browser_reads_them() {
    // A handler's hash is taken over the attribute value *after* HTML
    // decoding; with no `"` or `&` in it, that value is the constant itself.
    for (name, js) in HANDLERS {
        assert!(
            !js.contains('"') && !js.contains('&'),
            "{name} would be HTML-decoded before hashing"
        );
    }
    // A `<script>` element's text runs to the first `</script`.
    for (name, js) in SCRIPTS {
        assert!(!js.to_ascii_lowercase().contains("</script"), "{name}");
    }
}

#[test]
fn the_caddyfile_csp_allows_exactly_the_registered_scripts() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    let script_src = directive(policy, "script-src").expect("the policy has a script-src");
    let mut listed: Vec<String> = script_src
        .iter()
        .filter(|s| s.starts_with("'sha256-"))
        .map(|s| s.to_string())
        .collect();
    let mut expected: Vec<String> = HANDLERS
        .iter()
        .chain(SCRIPTS)
        .map(|(_, js)| csp_hash(js))
        .collect();
    listed.sort();
    expected.sort();
    let names: Vec<String> = HANDLERS
        .iter()
        .chain(SCRIPTS)
        .map(|(n, js)| format!("{n}: {}", csp_hash(js)))
        .collect();
    assert_eq!(
        listed,
        expected,
        "script-src must list exactly these hashes:\n{}",
        names.join("\n")
    );
    // Hashes of event-handler attributes only count under 'unsafe-hashes';
    // anything looser would make the hashes pointless.
    assert!(script_src.contains(&"'unsafe-hashes'"));
    for loose in ["'unsafe-inline'", "'unsafe-eval'", "*", "data:", "'self'"] {
        assert!(!script_src.contains(&loose), "script-src allows {loose}");
    }
}

#[test]
fn the_caddyfile_sets_the_other_security_headers() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(policy, "default-src"), Some(vec!["'self'"]));
    assert_eq!(directive(policy, "object-src"), Some(vec!["'none'"]));
    assert_eq!(directive(policy, "base-uri"), Some(vec!["'none'"]));
    assert_eq!(directive(policy, "frame-ancestors"), Some(vec!["'none'"]));
    assert_eq!(directive(policy, "form-action"), Some(vec!["'self'"]));
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
