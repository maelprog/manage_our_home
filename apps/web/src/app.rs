//! Page shell + small render helpers shared by `src/routes/auth/*`.
//!
//! Each route builds its page *content* with Leptos's `view!` macro,
//! rendered server-side to a string via `leptos::ssr::render_to_string`
//! (`apps/web` is server-rendered only — no client-side hydration/WASM
//! bundle, see `Cargo.toml`'s doc comment for why). `shell` then wraps
//! that fragment in the surrounding `<html>`/`<head>`/`<body>` document,
//! plain-string templated since it never needs reactivity.

use manage_our_home_shared::dto::auth::MeResponse;
use manage_our_home_shared::dto::groups::GroupSummary;
use manage_our_home_shared::validation::auth::{MAX_PASSWORD_LEN, MIN_PASSWORD_LEN};
use uuid::Uuid;

pub fn shell(title: &str, body_html: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="fr">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{title} — Manage our home</title>
<style>{css}</style>
</head>
<body>
<main class="container">
{body_html}
</main>
</body>
</html>"#,
        title = html_escape(title),
        css = include_str!("style.css"),
        body_html = body_html,
    )
}

/// Full authenticated header for pages that carry the family context:
/// a name + "Mon compte" (the RGPD self-service hub, front epic F10) + logout
/// row plus a nav (Accueil / Groupes)
/// and issue #17's active-family switcher — a plain `<form>` posting to
/// `POST /groups/switch` (persists the choice in the `active_group_id`
/// cookie, see `crate::family`) then bouncing back to `redirect_to`.
/// With no groups yet, the switcher gives way to a "create a family" link.
pub fn app_header(
    me: &MeResponse,
    groups: &[GroupSummary],
    active: Option<&GroupSummary>,
    redirect_to: &str,
) -> String {
    let switcher = if groups.is_empty() {
        r#"<a href="/groups/new">Créer une famille</a>"#.to_string()
    } else {
        let options: String = groups
            .iter()
            .map(|g| {
                let selected = if active.is_some_and(|a| a.group_id == g.group_id) {
                    " selected"
                } else {
                    ""
                };
                format!(
                    r#"<option value="{id}"{selected}>{name}</option>"#,
                    id = g.group_id,
                    name = html_escape(&g.name),
                )
            })
            .collect();
        format!(
            r#"<form method="post" action="/groups/switch" class="actions">
<label class="field inline">Famille active
<select name="group_id">{options}</select>
</label>
<input type="hidden" name="redirect_to" value="{redirect_to}"/>
<button type="submit" class="secondary">Changer</button>
</form>"#,
            options = options,
            redirect_to = html_escape(redirect_to),
        )
    };
    // The superadmin support screens (front epic F9) are only linked for the
    // single technical superadmin — everyone else never sees the door.
    let admin_link = if me.is_superadmin {
        r#"<a href="/admin/users">Admin</a>"#
    } else {
        ""
    };
    format!(
        r#"<header>
<div class="muted page-header">
<nav class="actions">
<a href="/">Accueil</a>
<a href="/agenda">Agenda</a>
<a href="/stocks">Stocks</a>
<a href="/recipes">Recettes</a>
<a href="/grocery-list">Liste de courses</a>
<a href="/budget">Budget</a>
<a href="/messagerie">Messagerie</a>
<a href="/groups">Groupes</a>
{admin_link}
</nav>
<span class="actions">
<span>{name}</span>
<a href="/account">Mon compte</a>
<form method="post" action="/logout">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
</span>
</div>
<div class="muted">{switcher}</div>
</header>"#,
        name = html_escape(&me.display_name),
        switcher = switcher,
        admin_link = admin_link,
    )
}

/// Password `<label>` block shared by login/register/reset-password
/// (embed via `<div inner_html=...></div>` like `app_header`).
/// The show/hide toggle is progressive enhancement: with JS disabled the
/// button does nothing and the form still submits normally. `with_rules`
/// states the password rules upfront and adds the matching `minlength`,
/// so browsers reject a too-short password *before* the form round-trips
/// (a server-rendered error page can't refill the password field without
/// echoing the password back into the HTML, so avoiding the round trip is
/// what keeps the field from being wiped).
pub fn password_field(label: &str, name: &str, autocomplete: &str, with_rules: bool) -> String {
    let (rules_attr, hint) = if with_rules {
        (
            format!(r#" minlength="{MIN_PASSWORD_LEN}" maxlength="{MAX_PASSWORD_LEN}""#),
            format!(
                r#"<span class="muted">Au moins {MIN_PASSWORD_LEN} caractères — une phrase de passe (plusieurs mots) est idéale. Pas de règle de majuscules ou de chiffres.</span>"#
            ),
        )
    } else {
        (String::new(), String::new())
    };
    format!(
        r#"<label>
{label}
<div class="pw-wrap">
<input type="password" name="{name}" autocomplete="{autocomplete}" required{rules_attr} />
<button type="button" class="pw-toggle" aria-label="Afficher le mot de passe" onclick="var i=this.parentNode.querySelector('input');var s=i.type==='password';i.type=s?'text':'password';this.setAttribute('aria-label',s?'Masquer le mot de passe':'Afficher le mot de passe');this.classList.toggle('shown',s)"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1 12s4-7 11-7 11 7 11 7-4 7-11 7-11-7-11-7z"/><circle cx="12" cy="12" r="3"/></svg></button>
</div>
{hint}
</label>"#,
        label = html_escape(label),
        name = html_escape(name),
        autocomplete = html_escape(autocomplete),
    )
}

/// French form message for a `validate_password` error code, shared by
/// register and reset-password so the two pages can't drift apart.
pub fn password_error_message(code: &str) -> String {
    match code {
        "password_too_short" => {
            format!("Le mot de passe doit contenir au moins {MIN_PASSWORD_LEN} caractères.")
        }
        "password_too_long" => {
            format!("Le mot de passe ne peut pas dépasser {MAX_PASSWORD_LEN} caractères.")
        }
        "password_too_common" => {
            "Ce mot de passe est trop courant, choisissez-en un autre.".to_string()
        }
        _ => "Mot de passe invalide.".to_string(),
    }
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The member colour ramp of DESIGN.md → Couleur → Couleurs par membre, as
/// the token names the stylesheet defines. Eight muted hues, one per member,
/// so a family's members are told apart at a glance in both themes.
const MEMBER_RAMP: [&str; 8] = [
    "--m1", "--m2", "--m3", "--m4", "--m5", "--m6", "--m7", "--m8",
];

/// The ramp token for a member, picked by a stable hash of the user id.
///
/// Deriving it from the id is what keeps `GroupMember` out of a migration:
/// there is no colour column and none is needed. The hash has to be stable
/// across processes and releases — a member whose colour changed between two
/// page loads would be worse than no colour at all — so it is a plain sum of
/// the id's bytes rather than anything from `std::hash`, whose `SipHash`
/// keys are randomised per process.
pub fn member_colour(user_id: Uuid) -> &'static str {
    let sum: u32 = user_id.as_bytes().iter().map(|b| u32::from(*b)).sum();
    MEMBER_RAMP[sum as usize % MEMBER_RAMP.len()]
}

/// The initial shown in a member's `.avatar`, uppercased.
///
/// Colour is never the only carrier of the information (WCAG 1.4.1): the
/// avatar always pairs the hue with this letter, and the member's full name
/// is next to it. An unusable name (empty, or punctuation only) falls back
/// to `?` rather than rendering an empty box.
pub fn member_initial(display_name: &str) -> String {
    display_name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string())
}

/// The stylesheet with its `/* … */` comments removed, so prose about a
/// token is never mistaken for a declaration of one.
///
/// Test-only, but at module level rather than inside `mod tests`: the
/// `/assets` route tests in `crate::assets` read the font file names off
/// the stylesheet through `stylesheet_font_files` below, so that what the
/// route is asked to serve and what the browser is told to fetch come from
/// one place.
#[cfg(test)]
pub(crate) fn css_without_comments() -> String {
    const CSS: &str = include_str!("style.css");
    let mut out = String::with_capacity(CSS.len());
    let mut rest = CSS;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        rest = match rest[at + 2..].find("*/") {
            Some(end) => &rest[at + 2 + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Every `url(…)` the stylesheet loads, unquoted.
#[cfg(test)]
pub(crate) fn stylesheet_urls() -> Vec<String> {
    let css = css_without_comments();
    let mut out = Vec::new();
    let mut rest = css.as_str();
    while let Some(at) = rest.find("url(") {
        rest = &rest[at + "url(".len()..];
        let inner = &rest[..rest.find(')').unwrap_or(rest.len())];
        out.push(inner.trim().trim_matches(['"', '\'']).to_string());
    }
    out
}

/// The file names under `/assets/fonts/` the stylesheet asks the browser
/// to fetch — the other half of the contract `crate::assets` serves.
#[cfg(test)]
pub(crate) fn stylesheet_font_files() -> Vec<String> {
    const PREFIX: &str = "/assets/fonts/";
    stylesheet_urls()
        .iter()
        .filter_map(|url| url.strip_prefix(PREFIX))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    // -- password_field ------------------------------------------------

    #[test]
    fn with_rules_shows_hint_and_minlength_upfront() {
        let html = password_field("Mot de passe", "password", "new-password", true);
        assert!(html.contains(r#"name="password""#));
        assert!(html.contains(r#"type="password""#));
        assert!(html.contains(r#"minlength="12""#));
        assert!(html.contains(r#"maxlength="128""#));
        assert!(html.contains("Au moins 12 caractères"));
        assert!(html.contains("phrase de passe"));
        assert!(html.contains(r#"autocomplete="new-password""#));
    }

    #[test]
    fn without_rules_has_no_hint_or_minlength() {
        let html = password_field("Mot de passe", "password", "current-password", false);
        assert!(!html.contains("minlength"));
        assert!(!html.contains("maxlength"));
        assert!(!html.contains("Au moins"));
        assert!(html.contains(r#"autocomplete="current-password""#));
    }

    #[test]
    fn always_renders_a_visibility_toggle() {
        for with_rules in [true, false] {
            let html = password_field("Mot de passe", "password", "new-password", with_rules);
            assert!(html.contains("pw-toggle"));
            assert!(html.contains("Afficher le mot de passe"));
        }
    }

    // -- password_error_message ----------------------------------------

    #[test]
    fn maps_each_password_error_code_to_a_french_message() {
        assert_eq!(
            password_error_message("password_too_short"),
            "Le mot de passe doit contenir au moins 12 caractères."
        );
        assert_eq!(
            password_error_message("password_too_long"),
            "Le mot de passe ne peut pas dépasser 128 caractères."
        );
        assert_eq!(
            password_error_message("password_too_common"),
            "Ce mot de passe est trop courant, choisissez-en un autre."
        );
    }

    #[test]
    fn unknown_password_error_code_gets_a_generic_message() {
        assert_eq!(
            password_error_message("something_else"),
            "Mot de passe invalide."
        );
    }

    // -- member colours and initials (#68) -----------------------------

    /// Ids differing by one byte, which is all a v4 id is to a byte hash.
    fn sample_ids() -> Vec<Uuid> {
        (0u8..64)
            .map(|i| Uuid::from_bytes([i, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2, 3, 4, 5]))
            .collect()
    }

    #[test]
    fn member_colour_only_ever_names_a_ramp_token() {
        for id in sample_ids() {
            assert!(
                MEMBER_RAMP.contains(&member_colour(id)),
                "{id} got {}, which is not in the ramp",
                member_colour(id)
            );
        }
    }

    #[test]
    fn member_colour_is_stable_for_the_same_id() {
        // A colour that changed between two page loads would be worse than
        // no colour: the member's identity is what it encodes.
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").expect("valid uuid");
        assert_eq!(member_colour(id), member_colour(id));
        assert_eq!(member_colour(id), "--m5");
    }

    #[test]
    fn member_colour_uses_the_whole_ramp() {
        // A hash that collapses onto two hues makes a four-person family
        // look like a two-person one.
        let used: BTreeSet<&str> = sample_ids().into_iter().map(member_colour).collect();
        assert_eq!(
            used.len(),
            MEMBER_RAMP.len(),
            "hues actually used: {used:?}"
        );
    }

    #[test]
    fn every_ramp_token_is_defined_by_the_stylesheet() {
        let (root, _) = block_after(&css(), ":root", 0);
        let defined = declared_tokens(&root);
        for token in MEMBER_RAMP {
            assert!(
                defined.contains(token),
                "the ramp names `{token}`, which `:root` does not define"
            );
        }
    }

    #[test]
    fn member_initial_is_the_first_letter_uppercased() {
        assert_eq!(member_initial("alice"), "A");
        assert_eq!(member_initial("Bob Martin"), "B");
        // French display names are the common case, not an edge one.
        assert_eq!(member_initial("Édouard"), "É");
        assert_eq!(member_initial("  camille"), "C");
        assert_eq!(member_initial("3615 Papa"), "3");
    }

    #[test]
    fn member_initial_falls_back_when_there_is_no_letter() {
        // Better a `?` than an empty avatar, which reads as a rendering bug.
        assert_eq!(member_initial(""), "?");
        assert_eq!(member_initial("   "), "?");
        assert_eq!(member_initial("!?"), "?");
    }

    #[test]
    fn escapes_label_and_name() {
        let html = password_field("a<b>", "x\"y", "new-password", false);
        assert!(html.contains("a&lt;b&gt;"));
        assert!(html.contains("x&quot;y"));
        assert!(!html.contains("a<b>"));
    }

    // -- style.css tokens (#65) ----------------------------------------
    //
    // The dark theme broke because two tokens were referenced with a light
    // `var(--x, #hex)` fallback and never defined anywhere: the fallback
    // always won, so light mode looked correct and dark mode silently
    // painted near-white behind near-white text. Nothing in the build or
    // the browser reports that. These tests are the guard for both halves
    // of it — the undefined token, and the fallback that hid it.

    use super::css_without_comments as css;

    /// The body of the first `<selector> {` … `}` block starting at or
    /// after `from`, plus the offset just past it.
    fn block_after(css: &str, selector: &str, from: usize) -> (String, usize) {
        let head = from
            + css[from..]
                .find(selector)
                .unwrap_or_else(|| panic!("`{selector}` not found in style.css"));
        let open = head
            + css[head..]
                .find('{')
                .unwrap_or_else(|| panic!("`{selector}` has no opening brace"));
        // No closing brace means the caller handed us a slice that was
        // already cut at one — the nested `:root` of the dark media query.
        let close = css[open..].find('}').map_or(css.len(), |i| open + i);
        (css[open + 1..close].to_string(), close + 1)
    }

    /// The custom properties *declared* in a block — `--name:` on the left
    /// of a colon, ignoring `var(--name)` references on the right.
    fn declared_tokens(block: &str) -> BTreeSet<String> {
        block
            .split(';')
            .filter_map(|decl| decl.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("--"))
            .map(str::to_string)
            .collect()
    }

    /// The tokens a block declares to a *literal* colour — `#hex`, `rgb(…)`,
    /// `hsl(…)`. Those are the ones each theme has to restate. A token
    /// holding a length, a font size, or a reference to another token
    /// resolves the same way whatever the theme, so it needs no dark entry.
    fn literal_colour_tokens(block: &str) -> BTreeSet<String> {
        block
            .split(';')
            .filter_map(|decl| decl.split_once(':'))
            .map(|(name, value)| (name.trim(), value.trim()))
            .filter(|(name, value)| {
                name.starts_with("--")
                    && (value.starts_with('#')
                        || value.starts_with("rgb")
                        || value.starts_with("hsl"))
            })
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// `var(--name)`, assembled instead of written out: the reference scan
    /// below reads this file too, and a literal `var(--…)` inside a test
    /// expectation would register as a reference to satisfy.
    fn var_of(token: &str) -> String {
        format!("var({token})")
    }

    /// Every custom-property reference in `src`, paired with whether it
    /// carries a fallback value.
    ///
    /// Whole-line comments are skipped: this module documents the very
    /// pattern it forbids, and prose about a rule is not a breach of it.
    fn var_references(src: &str) -> Vec<(String, bool)> {
        let mut refs = Vec::new();
        for line in src.lines().filter(|l| !l.trim_start().starts_with("//")) {
            let mut rest = line;
            while let Some(at) = rest.find("var(") {
                rest = &rest[at + "var(".len()..];
                let inner = &rest[..rest.find(')').unwrap_or(rest.len())];
                if !inner.starts_with("--") {
                    continue;
                }
                let (name, has_fallback) = match inner.split_once(',') {
                    Some((name, _)) => (name.trim(), true),
                    None => (inner.trim(), false),
                };
                refs.push((name.to_string(), has_fallback));
            }
        }
        refs
    }

    /// `(path, contents)` for every `.rs` file under `src/`, so the scan
    /// covers the inline `style="…"` attributes in the routes and not just
    /// the stylesheet.
    fn rust_sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("readable source dir") {
                let path = entry.expect("readable dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let body = std::fs::read_to_string(&path).expect("readable source file");
                    out.push((path.display().to_string(), body));
                }
            }
        }
        let mut out = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut out,
        );
        assert!(!out.is_empty(), "found no Rust sources to scan");
        out
    }

    /// `(path, contents)` for the stylesheet *and* every Rust source, which
    /// is what a rule about colour has to cover: 173 of the declarations that
    /// paint something live in inline `style="…"` attributes in the routes,
    /// not in the sheet.
    fn styled_sources() -> Vec<(String, String)> {
        let mut out = rust_sources();
        out.push(("style.css".to_string(), css()));
        out
    }

    /// Every run of declarations that paints a solid `--accent` or `--error`
    /// surface, cut at the end of the group it belongs to: the closing quote
    /// of a route's inline `style="…"` attribute, or the closing brace of a
    /// CSS rule. Whitespace after each `:` is dropped so one needle matches
    /// both syntaxes. `--accent-bg` and `--error-soft` do not match: the
    /// closing paren is part of the needle, and a tint is not a solid.
    fn solid_tinted_surfaces(src: &str) -> Vec<String> {
        let flat = src.replace(": ", ":");
        let mut out = Vec::new();
        for token in ["--accent", "--error"] {
            let needle = format!("background:var({token})");
            let mut from = 0;
            while let Some(at) = flat[from..].find(&needle) {
                let start = from + at;
                let end = flat[start..]
                    .find(['}', '"'])
                    .map_or(flat.len(), |i| start + i);
                out.push(flat[start..end].to_string());
                from = start + needle.len();
            }
        }
        out
    }

    #[test]
    fn every_token_referenced_anywhere_is_defined_in_root() {
        let (root, _) = block_after(&css(), ":root", 0);
        let defined = declared_tokens(&root);

        let mut missing = Vec::new();
        for (path, body) in rust_sources() {
            for (name, _) in var_references(&body) {
                if !defined.contains(&name) {
                    missing.push(format!("{name} ({path})"));
                }
            }
        }
        for (name, _) in var_references(&css()) {
            if !defined.contains(&name) {
                missing.push(format!("{name} (style.css)"));
            }
        }
        assert!(
            missing.is_empty(),
            "CSS custom properties used but never defined in `:root`: {missing:?}"
        );
    }

    #[test]
    fn no_var_reference_carries_a_hardcoded_fallback() {
        // DESIGN.md → Couleur → Règles bans the hardcoded fallback. It is
        // not a safety net, it is a way of shipping a missing token that
        // only misbehaves in the theme nobody screenshotted.
        let mut offenders = Vec::new();
        for (path, body) in rust_sources() {
            for (name, has_fallback) in var_references(&body) {
                if has_fallback {
                    offenders.push(format!("{name} ({path})"));
                }
            }
        }
        // The stylesheet is where the CSS is written first, so it is the most
        // likely home for the pattern — and it is exactly where #65 hid.
        for (name, has_fallback) in var_references(&css()) {
            if has_fallback {
                offenders.push(format!("{name} (style.css)"));
            }
        }
        assert!(
            offenders.is_empty(),
            "a hardcoded var() fallback is forbidden by DESIGN.md \
             (Couleur → Règles); offenders: {offenders:?}"
        );
    }

    #[test]
    fn every_colour_token_is_restated_in_the_dark_theme() {
        // A colour token left out of the dark block keeps its light value
        // there. #65 was that fault *behind* the text: `--fg` flipped to
        // near-white while `--accent-bg` stayed near-white. #74 is the same
        // fault in *front* of it: `--accent` was declared once, in light, and
        // serves as link colour, so every link sat at 2.71:1 in dark.
        // Naming the tokens by hand is what let the second one through — the
        // first list covered only what paints behind `--fg` — so the list is
        // now derived from the stylesheet: every literal colour, whichever
        // side of the text it lands on.
        let (root, after_root) = block_after(&css(), ":root", 0);
        // `block_after` stops at the first `}`, which inside the media query
        // is the end of its nested `:root` — so the media body it hands back
        // *is* that `:root`, minus its closing brace.
        let (media, _) = block_after(&css(), "@media (prefers-color-scheme: dark)", after_root);
        let (dark, _) = block_after(&media, ":root", 0);
        let dark = declared_tokens(&dark);

        let light = literal_colour_tokens(&root);
        assert!(
            light.contains("--accent"),
            "the light theme should declare `--accent` as a literal colour"
        );
        let missing: Vec<&String> = light.difference(&dark).collect();
        assert!(
            missing.is_empty(),
            "colour tokens never restated under `prefers-color-scheme: dark`, \
             so they keep their light value in the dark theme: {missing:?}"
        );
    }

    #[test]
    fn each_notice_variant_carries_its_own_semantic_colour() {
        // `.notice.success` borrowed the accent, so a confirmation was
        // indistinguishable from a neutral information, and there was no
        // warning variant at all (#66).
        for variant in ["success", "warning", "error"] {
            let (rule, _) = block_after(&css(), &format!(".notice.{variant}"), 0);
            assert!(
                rule.contains(&format!("color: {}", var_of(&format!("--{variant}")))),
                "`.notice.{variant}` should take its text colour from `--{variant}`"
            );
            assert!(
                rule.contains(&format!(
                    "background: {}",
                    var_of(&format!("--{variant}-soft"))
                )),
                "`.notice.{variant}` should sit on `--{variant}-soft`, not a hardcoded tint"
            );
        }
    }

    #[test]
    fn solid_accent_and_danger_surfaces_take_their_text_from_a_token() {
        // `color: #fff` survives the theme switch, the surface under it does
        // not: on the dark `--accent` white text measures 2.50:1 and on the
        // dark `--error` 2.70:1. `--accent-fg` is the token that flips with
        // the theme (#74).
        //
        // The scan covers the routes and not just the sheet, because the two
        // "Stock bas" badges are inline `style="…"` attributes and that is
        // exactly where this slipped through: while `--error` had no dark
        // value at all, white on it was safe in both themes, so giving the
        // token its dark value turned two passing badges into 2.70:1 ones.
        // A guard that only knew about the sheet's two rules described the
        // class of bug and checked two instances of it.
        let needle = format!("color:{}", var_of("--accent-fg"));
        let mut offenders = Vec::new();
        for (path, body) in styled_sources() {
            for surface in solid_tinted_surfaces(&body) {
                if !surface.contains(&needle) {
                    offenders.push(format!("{path}: {surface}"));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a solid --accent or --error surface has to name its text colour \
             with `--accent-fg`, which flips with the theme: {offenders:?}"
        );
    }

    // -- component classes (#68) ---------------------------------------
    //
    // The audit counted 173 inline `style="…"` attributes, the same handful
    // of patterns retyped in 20 route files: changing what a list row looks
    // like meant editing seven of them, and reading the stylesheet told you
    // nothing about the cards, badges or calendar the app is made of. The
    // guards below keep the patterns in the sheet: one bounds what may stay
    // inline, the other names the declarations that are a component's job.

    /// Every inline `style` attribute in a route, one string per attribute.
    ///
    /// Both spellings are counted: the `style="…"` of the `format!` routes
    /// and the `style=expr` of the `view!` ones — a computed attribute is
    /// still an inline style, and counting only the quoted form would let
    /// the debt back in through the macro.
    ///
    /// Comment lines are skipped: this module documents the pattern it
    /// bounds, and prose about a rule is not a breach of it.
    fn inline_styles(src: &str) -> Vec<String> {
        // Assembled rather than written out, like `var_of` above: this file
        // is one of the sources the scan reads, and a literal needle in the
        // scanner would register as an inline style of its own.
        let needle = format!("style{}", '=');
        let mut out = Vec::new();
        for line in src.lines().filter(|l| !l.trim_start().starts_with("//")) {
            let mut rest = line;
            while let Some(at) = rest.find(&needle) {
                rest = &rest[at + needle.len()..];
                let end = rest.find(['"', '>', ' ']).unwrap_or(rest.len());
                let value = if rest.starts_with('"') {
                    &rest[1..rest[1..].find('"').map_or(rest.len(), |i| i + 1)]
                } else {
                    &rest[..end]
                };
                out.push(value.to_string());
            }
        }
        out
    }

    #[test]
    fn inline_styles_are_bounded_to_a_justified_residue() {
        // A ceiling, not a target: it exists so the 173 do not creep back
        // one route at a time. What is left is what no class can carry —
        // a member's computed hue, a control's one-off width, a table
        // column's alignment — plus the messagerie bits #72 reworks.
        const CEILING: usize = 6;
        let counted: Vec<(String, String)> = rust_sources()
            .iter()
            .flat_map(|(path, body)| {
                inline_styles(body)
                    .into_iter()
                    .map(move |s| (path.clone(), s))
            })
            .collect();
        assert!(
            counted.len() <= CEILING,
            "{} inline style attributes, ceiling is {CEILING}. Each recurring \
             pattern belongs in style.css as a class (DESIGN.md → Composants); \
             raising this number needs a reason in the PR: {counted:#?}",
            counted.len(),
        );
    }

    /// Every class name a source *asks for*: the `class="…"` attributes of
    /// the routes (escaped or not — `class=\"done\"` inside a Rust string is
    /// the same attribute) and the `className = "…"` the messagerie's JS
    /// fallback path assigns.
    ///
    /// Two things it deliberately does not see, so that the test below is
    /// not read as more than it is: a name reaching the attribute through a
    /// `format!` placeholder (`class="chip{done}"` is counted as `chip`, and
    /// whatever `{done}` expands to is invisible here), and
    /// `classList.toggle("…")`.
    fn referenced_classes(src: &str) -> Vec<String> {
        // Assembled, like `var_of` and `inline_styles` above: this file is
        // one of the sources the scan reads.
        let attr = format!("class{}\"", '=');
        let js = format!("className {} \"", '=');
        // Comment lines are dropped, as in `inline_styles`: this module
        // quotes the very attribute it scans for, and prose about a class is
        // not a reference to one.
        let unescaped: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
            .replace("\\\"", "\"");
        let mut out = Vec::new();
        for needle in [attr, js] {
            let mut rest = unescaped.as_str();
            while let Some(at) = rest.find(&needle) {
                rest = &rest[at + needle.len()..];
                let value = &rest[..rest.find('"').unwrap_or(rest.len())];
                for name in value.split_whitespace() {
                    // A `format!` placeholder starts the dynamic part of the
                    // attribute; what precedes it is a name all the same.
                    let name = name.split('{').next().unwrap_or(name);
                    if !name.is_empty() {
                        out.push(name.to_string());
                    }
                }
            }
        }
        out
    }

    /// Whether the stylesheet carries a selector for `.name`, the name ending
    /// where the identifier does — so `.field-error` cannot stand in for
    /// `.field`.
    fn stylesheet_defines_class(css: &str, name: &str) -> bool {
        let needle = format!(".{name}");
        let mut from = 0;
        while let Some(at) = css[from..].find(&needle) {
            from = from + at + needle.len();
            match css[from..].chars().next() {
                None => return true,
                Some(c) if !c.is_alphanumeric() && c != '-' && c != '_' => return true,
                _ => {}
            }
        }
        false
    }

    #[test]
    fn every_class_a_source_references_exists_in_the_stylesheet() {
        // Renaming `.button` to `.btn` left one reference behind: the JS
        // recovery path of the messagerie built `className = "button
        // secondary"`, and because it is an `<a>`, the `button` element
        // selector did not catch it either — the "Recharger" link of a lost
        // connection rendered as bare text. Nothing failed, nothing warned:
        // a class that does not exist is not an error in CSS.
        let css = css();
        let mut missing = Vec::new();
        for (path, body) in rust_sources() {
            for name in referenced_classes(&body) {
                if !stylesheet_defines_class(&css, &name) {
                    missing.push(format!("{name} ({path})"));
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "classes asked for by a source and defined nowhere in style.css: \
             {missing:#?}"
        );
    }

    #[test]
    fn no_inline_style_hand_rolls_a_component() {
        // These are the declarations the audit found copied across routes.
        // A route that sets one of them is redefining a component instead
        // of wearing it, and the two copies drift: `1.05rem` in the account
        // subtitles against `1.1rem` in the agenda's, `3px` badges next to
        // `6px` buttons.
        const COMPONENT_PROPERTIES: [&str; 8] = [
            "font-size",
            "border",
            "border-radius",
            "background",
            "padding",
            "display",
            "justify-content",
            "list-style",
        ];
        let mut offenders = Vec::new();
        for (path, body) in rust_sources() {
            for style in inline_styles(&body) {
                for property in COMPONENT_PROPERTIES {
                    if style.contains(&format!("{property}:")) {
                        offenders.push(format!("{path}: {property} in `{style}`"));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these belong to a class in style.css, not to a route: {offenders:#?}"
        );
    }

    #[test]
    fn the_stylesheet_defines_every_class_the_design_system_names() {
        // DESIGN.md → Composants is the list; the point of the list is that
        // reading the stylesheet should tell you what the app is made of.
        for class in [
            ".page-header",
            ".list-row",
            ".card",
            ".badge",
            ".badge.warn",
            ".avatar",
            ".chip",
            ".field",
            ".btn",
            ".btn.secondary",
            ".btn.danger",
            ".btn.sm",
            ".notice",
            ".notice.success",
            ".notice.warning",
            ".notice.error",
        ] {
            assert!(
                css().contains(class),
                "`{class}` is named by DESIGN.md → Composants but the \
                 stylesheet does not define it"
            );
        }
    }

    #[test]
    fn the_primary_button_wears_no_grey_border() {
        // Audit §4.4: the primary button painted itself with the accent
        // *and* took a 1px border from the neutral `--border`, so every
        // primary action was ringed in a warm grey that meant nothing.
        // `.danger` recoloured its border and the primary did not — the
        // oversight was there to be seen.
        let (button, _) = block_after(&css(), "button, .btn", 0);
        assert!(
            !button.contains(&var_of("--border")),
            "the primary button still takes its border from --border: {button}"
        );
    }

    #[test]
    fn buttons_inherit_the_page_font() {
        // Same trap as the fields below, and it stayed open when the
        // self-hosted fonts landed (#67): no form control inherits the
        // document font, so ~55 buttons rendered in the browser's UI face
        // next to text set in Source Sans 3.
        let (button, _) = block_after(&css(), "button, .btn", 0);
        assert!(
            button.contains("font-family: inherit"),
            "button rule: {button}"
        );
    }

    #[test]
    fn one_subtitle_size_comes_from_the_type_scale() {
        // 14 `h2`s carried an inline `font-size`, in two different values
        // for the same level. The scale (#66) is where the answer lives.
        for (heading, token) in [("h1", "--t-2xl"), ("h2", "--t-xl"), ("h3", "--t-lg")] {
            let (rule, _) = block_after(&css(), &format!("\n{heading} {{"), 0);
            assert!(
                rule.contains(&format!("font-size: {}", var_of(token))),
                "`{heading}` should size itself from `{token}`: {rule}"
            );
        }
    }

    // -- self-hosted fonts (#67) ---------------------------------------
    //
    // The failure these guard is silent by construction: `font-display:
    // swap` plus a fallback stack means a font that 404s, or one loaded
    // from a domain that is blocked or slow, renders the page in Georgia
    // and system-ui and reports nothing. The CDN variant is worse than
    // silent — it works, and quietly hands every visitor's IP to a third
    // party, which is a processing activity `docs/registre-traitements.md`
    // would have to declare.

    /// One string per `@font-face` block in the stylesheet.
    fn font_face_blocks() -> Vec<String> {
        let css = css();
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(at) = css[from..].find("@font-face") {
            let start = from + at;
            let (block, next) = block_after(&css, "@font-face", start);
            out.push(block);
            from = next;
        }
        out
    }

    #[test]
    fn the_stylesheet_declares_both_families_of_the_design_system() {
        let families: Vec<String> = font_face_blocks()
            .iter()
            .filter_map(|b| {
                b.split(';')
                    .filter_map(|d| d.split_once(':'))
                    .find(|(name, _)| name.trim() == "font-family")
                    .map(|(_, value)| value.trim().trim_matches('"').to_string())
            })
            .collect();
        assert_eq!(
            families,
            vec!["Source Sans 3".to_string(), "Fraunces".to_string()],
            "DESIGN.md → Typographie names exactly these two, body face first"
        );
    }

    #[test]
    fn the_stylesheet_fetches_nothing_from_a_third_party() {
        // No CDN, no `@import`, no absolute URL of any kind: every byte the
        // browser loads for a page has to come from apps/web itself.
        let css = css();
        assert!(
            !css.contains("@import"),
            "`@import` pulls in a stylesheet at render time — inline it instead"
        );
        for url in stylesheet_urls() {
            assert!(
                url.starts_with("/assets/"),
                "the stylesheet loads `{url}`, which is not served by apps/web; \
                 self-hosting the fonts is what keeps visitors' IPs off a third \
                 party's logs (DESIGN.md → Typographie → Chargement)"
            );
        }
        for scheme in ["http://", "https://", "//fonts."] {
            assert!(
                !css.contains(scheme),
                "the stylesheet names an external origin (`{scheme}`)"
            );
        }
    }

    #[test]
    fn every_font_face_swaps_instead_of_blocking_the_text() {
        for block in font_face_blocks() {
            assert!(
                block.contains("font-display: swap"),
                "without `swap` the browser hides the text for up to 3s while \
                 the font loads: {block}"
            );
        }
    }

    #[test]
    fn every_font_face_declares_the_weight_range_it_covers() {
        // Both files are *variable*. Left undeclared, the browser assumes a
        // single 400 and synthesises bold by smearing the outlines instead
        // of moving the `wght` axis.
        for block in font_face_blocks() {
            let range = block
                .split(';')
                .filter_map(|d| d.split_once(':'))
                .find(|(name, _)| name.trim() == "font-weight")
                .map(|(_, value)| value.trim().to_string())
                .unwrap_or_else(|| panic!("no `font-weight` range in: {block}"));
            assert!(
                range.split_whitespace().count() == 2,
                "`font-weight: {range}` is a single weight, not a variable \
                 font's range"
            );
        }
    }

    #[test]
    fn every_font_file_the_stylesheet_names_is_committed() {
        // A renamed or forgotten file is a 404 nobody sees. `crate::assets`
        // asserts the other half — that the route actually serves these.
        let files = stylesheet_font_files();
        assert_eq!(files.len(), 2, "one file per family: {files:?}");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
        for file in &files {
            assert!(
                dir.join(file).is_file(),
                "{} is referenced by style.css but not committed",
                dir.join(file).display()
            );
        }
        // The OFL requires the licence to travel with the files it covers.
        for licence in ["OFL-Fraunces.txt", "OFL-SourceSans3.txt"] {
            assert!(
                dir.join(licence).is_file(),
                "redistributing an OFL font without its licence file is a \
                 licence breach: {licence} missing"
            );
        }
    }

    #[test]
    fn the_body_and_heading_stacks_name_their_fallbacks() {
        // The stack is what renders during the swap, and for good on a
        // client that blocks font downloads. DESIGN.md fixes both.
        let (body, _) = block_after(&css(), "body", 0);
        assert!(
            body.contains(r#"font-family: "Source Sans 3", ui-sans-serif, system-ui, sans-serif"#),
            "body stack: {body}"
        );
        let (headings, _) = block_after(&css(), "h1, h2, h3", 0);
        assert!(
            headings.contains(r#"font-family: "Fraunces", Georgia, serif"#),
            "heading stack: {headings}"
        );
    }

    #[test]
    fn textarea_is_styled_alongside_the_other_form_fields() {
        // Left out of the field selector, the 14 `textarea`s fall back to
        // the browser's defaults — a white box with black text sitting next
        // to dark `input`s in the same form.
        let (fields, _) = block_after(&css(), "input, select, textarea", 0);
        assert!(fields.contains("background: var(--bg)"));
        assert!(fields.contains("color: var(--fg)"));
        // Browsers do not inherit the page font into a `textarea`.
        assert!(fields.contains("font-family: inherit"));
    }

    // -- delivery budget (#83) -----------------------------------------
    //
    // Inlining is a bet: a sheet that travels inside the document costs a
    // copy per page view and buys a first paint with no blocking round
    // trip. The bet only pays while the document *and* the sheet fit in
    // the first congestion window, so it has a size at which it stops
    // being true — and until #83 that size was written nowhere.
    // DESIGN.md → Livraison du CSS now names it; these two guards are the
    // half of it a machine can check.
    //
    // Two ceilings rather than one, because "how big is the sheet" has two
    // answers with two different remedies:
    //
    // * `SHEET_CEILING` weighs the bytes a visitor downloads, comments
    //   included. Its remedy is architectural — move the sheet to
    //   `/assets`, never delete documentation. Charging prose to the user
    //   is a property of inlining, so prose outgrowing the budget is
    //   inlining's failure, not the writer's.
    // * `DECLARATIONS_CEILING` weighs the CSS alone. Its remedy is
    //   editorial: a rule that repeats another one goes. It is calibrated
    //   to trip *first* (see its comment), so the pressure lands on the
    //   code before it can ever land on the comments.

    /// A string's size on the wire, as gzip — one of the two encodings
    /// Caddy is configured to produce (`encode zstd gzip`,
    /// infra/Caddyfile) and the one every client accepts.
    ///
    /// What it does **not** see. Stated plainly, because a guard that is
    /// believed to cover more than it does is worse than none:
    ///
    /// * It weighs the sheet, not the response. Measured against the
    ///   docker stack on 2026-07-30, Caddy shipped the eight main routes
    ///   at 7 948–8 354 gzipped bytes while the sheet alone weighed 7 199
    ///   here: the document adds ~750–1 150 bytes on top, and that number
    ///   tracks how much *data* a family has, which no unit test can know.
    ///   The whole-response half of DESIGN.md's budget is checked by
    ///   measuring a running stack, in the PR that suspects it moved.
    /// * This is not an upper bound over both encodings. flate2's default
    ///   level (6) sits one notch above Caddy's gzip default (5) — 7 199
    ///   against 7 211 bytes on today's sheet — and Caddy's zstd, tuned
    ///   for speed,
    ///   measured *larger* than its own gzip on every main route (8 158 vs
    ///   7 948 bytes on `/`, +2.6 %). The three figures agree to within
    ///   ~3 %, which is far inside the ceilings' margin, but the number
    ///   below is an estimate of the wire, not a ceiling on it.
    /// * A sheet is compressed here in isolation; inside a document it
    ///   shares one window with the markup, so the pair costs less than
    ///   the sum of the parts. That is slack in the safe direction.
    fn gzipped(bytes: &[u8]) -> usize {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("in-memory write");
        encoder.finish().expect("in-memory flush").len()
    }

    /// 10 KiB — the sheet's share of the ~14 KiB that fits in the first
    /// congestion window (IW10: 10 segments of a 1 460-byte MSS ≈ 14 600
    /// bytes, less response headers and TLS record framing). The other
    /// ~4 KiB is reserved for the document itself — three and a half times
    /// what it added on the heaviest of the eight main routes measured on
    /// 2026-07-30.
    ///
    /// **This one is not raised.** Passing it means the inlining bet has
    /// lost and the sheet moves to `/assets` (DESIGN.md → Livraison du CSS
    /// → Porte de sortie). A PR that answers it by deleting comments has
    /// answered the wrong question.
    const SHEET_CEILING: usize = 10 * 1024;

    /// 3 KiB of declarations, comments stripped.
    ///
    /// Calibrated to trip before `SHEET_CEILING` does: today the
    /// declarations are 32 % of the compressed sheet, so growth that keeps
    /// the project's comment-to-code ratio reaches 3 KiB of CSS while the
    /// whole sheet is still around 9.6 KiB. The design system therefore
    /// runs out of room before the delivery strategy does.
    ///
    /// A ceiling, not a target, and unlike `SHEET_CEILING` it *can* be
    /// raised — with a reason in the PR, like the inline-style ceiling
    /// above. What it forbids is drifting past it unnoticed while #69–#74
    /// each add "just a few rules".
    const DECLARATIONS_CEILING: usize = 3 * 1024;

    #[test]
    fn the_compressed_stylesheet_fits_its_share_of_the_first_round_trip() {
        let sheet = gzipped(include_str!("style.css").as_bytes());
        assert!(
            sheet <= SHEET_CEILING,
            "the stylesheet compresses to {sheet} bytes; the budget is \
             {SHEET_CEILING} (DESIGN.md → Livraison du CSS). Past it, inlining \
             no longer buys the round trip it costs: serve the sheet from \
             /assets instead. Do not answer this by deleting comments — that \
             is the perverse incentive the budget exists to make visible."
        );
    }

    #[test]
    fn the_compressed_declarations_stay_inside_the_design_system_budget() {
        let declarations = gzipped(css().as_bytes());
        assert!(
            declarations <= DECLARATIONS_CEILING,
            "the declarations compress to {declarations} bytes, ceiling is \
             {DECLARATIONS_CEILING}. Comments are not counted here, so this is \
             about the CSS: a rule that restates another one is what to remove. \
             Raising the ceiling needs a reason in the PR."
        );
    }
}
