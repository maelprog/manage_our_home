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
//! tests would notice, because they never go through Caddy (nor does CI's
//! e2e job). This module is what notices. Its tests hold together:
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
///
/// A `header` that carries a matcher (`header @api { … }`,
/// `header /login Name "…"`) does not count: it sets the header on the
/// responses it matches only. Neither does `<name>-Report-Only` for
/// `Content-Security-Policy`, which reports and blocks nothing — the field
/// name has to be exactly `name`.
///
/// What it does **not** see — a reading of lines, not Caddy's parser: a
/// `header` nested in a `handle`, `route` or snippet (scoped by that block,
/// not by a matcher of its own), and a value split across lines.
fn caddy_header<'a>(caddyfile: &'a str, name: &str) -> Option<&'a str> {
    // Inside a `header … {` block: whether that block carries a matcher.
    let mut block: Option<bool> = None;
    for line in caddyfile.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if let Some(scoped) = block {
            if line.starts_with('}') {
                block = None;
            } else if !scoped {
                if let Some(value) = field_value(line, name) {
                    return Some(value);
                }
            }
            continue;
        }
        let Some(rest) = line
            .strip_prefix("header")
            .filter(|r| r.starts_with(char::is_whitespace))
        else {
            continue;
        };
        let rest = rest.trim_start();
        let scoped = rest.starts_with(['@', '/', '*']);
        let rest = if scoped {
            rest.split_once(char::is_whitespace)
                .map_or("", |(_, r)| r.trim_start())
        } else {
            rest
        };
        if rest.starts_with('{') {
            block = Some(scoped);
        } else if !scoped {
            if let Some(value) = field_value(rest, name) {
                return Some(value);
            }
        }
    }
    None
}

/// The value of `<name> <value>` at the start of `line`: `"…"`, `` `…` ``
/// or the rest of the line.
fn field_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(name)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let value = rest.trim();
    for quote in ['"', '`'] {
        if let Some(quoted) = value.strip_prefix(quote) {
            return Some(&quoted[..quoted.find(quote)?]);
        }
    }
    Some(value)
}

/// The policy `infra/Caddyfile` enforces on every response.
fn caddy_csp(caddyfile: &str) -> Option<&str> {
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
/// - `javascript:` right after an `=` and an optional quote.
///
/// What it does **not** see — a textual scan, not an HTML parser: spaces
/// around a handler's `=`, markup assembled from pieces (`"on" + "click"`,
/// a `<script` split across two literals), entity-encoded URLs
/// (`javascript&colon;`), and Leptos `view!` attributes, which SSR does not
/// emit as handlers anyway. One that slipped through would run nowhere
/// behind Caddy, and show up as a broken page, not as a hole in the policy.
fn inline_scripts(source: &str) -> Vec<Inline> {
    let excerpt = |s: &str| s.chars().take(60).collect::<String>();
    let mut found = Vec::new();
    for line in production_code(source).lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
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
        for (at, _) in lower.match_indices("javascript:") {
            let before = lower[..at].trim_end_matches(['"', '\'']);
            if before.trim_end().ends_with('=') {
                on_line.push((at, Inline::JsUrl(excerpt(&line[at..]))));
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
        caddy_csp(file),
        Some("default-src 'self'; script-src 'self'")
    );
    assert_eq!(caddy_csp("header X-Frame-Options DENY\n"), None);
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

/// A matcher scopes a `header` to the responses it matches: a policy set
/// that way protects one path and leaves every other page without one.
#[test]
fn caddy_csp_ignores_a_header_scoped_by_a_matcher() {
    for file in [
        "\theader @api {\n\t\tContent-Security-Policy \"default-src 'none'\"\n\t}\n",
        "\theader /login {\n\t\tContent-Security-Policy \"default-src 'none'\"\n\t}\n",
        "\theader * {\n\t\tContent-Security-Policy \"default-src 'none'\"\n\t}\n",
        "\theader @api Content-Security-Policy \"default-src 'none'\"\n",
        "\theader /login Content-Security-Policy \"default-src 'none'\"\n",
    ] {
        assert_eq!(caddy_csp(file), None, "{file}");
    }
    // The block after a scoped one is read again, and a line outside any
    // `header` is not a header.
    let file = "\theader @api {\n\t\tContent-Security-Policy \"scoped\"\n\t}\n\
                \tContent-Security-Policy \"loose\"\n\
                \theader {\n\t\tContent-Security-Policy \"global\"\n\t}\n";
    assert_eq!(caddy_csp(file), Some("global"));
}

#[test]
fn caddy_header_reads_bare_and_backquoted_values() {
    let file = "\theader {\n\
                \t\tX-Frame-Options DENY\n\
                \t\tReporting-Endpoints `csp=\"/api/csp-report\"`\n\
                \t}\n\
                \theader_up Referrer-Policy origin\n";
    assert_eq!(caddy_header(file, "X-Frame-Options"), Some("DENY"));
    assert_eq!(
        caddy_header(file, "Reporting-Endpoints"),
        Some("csp=\"/api/csp-report\"")
    );
    assert_eq!(caddy_header(file, "Referrer-Policy"), None);
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
    let production: Vec<String> = sources()
        .into_iter()
        .filter(|(path, _)| !path.ends_with("/assets.rs"))
        .map(|(_, text)| production_code(&text).to_string())
        .collect();
    for script in Script::ALL {
        let name = format!("Script::{script:?}");
        assert!(
            production.iter().any(|text| text.contains(&name)),
            "{name} is served but no page loads it: drop it"
        );
    }
}

#[test]
fn the_caddyfile_allows_script_files_from_the_site_and_nothing_inline() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(policy, "script-src"), Some(vec!["'self'"]));
}

/// The reminder notifications' service worker (#306) is a script file,
/// `/sw.js`. Without `worker-src` a browser falls back on `script-src`;
/// written out so that tightening `script-src` one day cannot take the
/// worker down with it.
#[test]
fn the_caddyfile_lets_the_service_worker_register_and_nothing_else() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(policy, "worker-src"), Some(vec!["'self'"]));
}

/// Styles: the sheet under /assets and no `<style>` element; the `style="…"`
/// attributes some routes write (an avatar's colour, a column's alignment)
/// through `style-src-attr`.
#[test]
fn the_caddyfile_allows_the_stylesheet_and_style_attributes_only() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(policy, "style-src"), Some(vec!["'self'"]));
    assert_eq!(
        directive(policy, "style-src-attr"),
        Some(vec!["'unsafe-inline'"])
    );
}

/// `fetch` and the Messagerie's WebSocket (under /api, same origin) go
/// through `connect-src`, which is left to fall back on `default-src
/// 'self'`: a `connect-src` of its own is a place to widen it unseen.
#[test]
fn the_caddyfile_leaves_connections_to_default_src() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(directive(policy, "connect-src"), None);
    assert_eq!(directive(policy, "default-src"), Some(vec!["'self'"]));
}

/// Violations are reported to apps/api (`csp_report.rs`), on the site's own
/// origin: `report-to` for the browsers that implement the Reporting API,
/// `report-uri` for the others, both to the same path.
#[test]
fn the_caddyfile_reports_violations_to_the_api_and_nowhere_else() {
    let policy = caddy_csp(CADDYFILE).expect("infra/Caddyfile sets a Content-Security-Policy");
    assert_eq!(
        directive(policy, "report-uri"),
        Some(vec!["/api/csp-report"])
    );
    assert_eq!(directive(policy, "report-to"), Some(vec!["csp"]));
    assert_eq!(
        caddy_header(CADDYFILE, "Reporting-Endpoints"),
        Some("csp=\"/api/csp-report\"")
    );
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
