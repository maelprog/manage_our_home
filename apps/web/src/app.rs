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
            r#"<form method="post" action="/groups/switch" class="switcher" style="flex-direction:row;gap:0.4rem;align-items:center;margin:0;">
<label style="flex-direction:row;gap:0.4rem;align-items:center;margin:0;">Famille active
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
        r#"<header style="margin-bottom:1.5rem;">
<div class="muted" style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;">
<nav style="display:flex;gap:0.75rem;">
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
<span style="display:flex;gap:0.75rem;align-items:center;">
<span>{name}</span>
<a href="/account">Mon compte</a>
<form method="post" action="/logout" style="margin:0;">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
</span>
</div>
<div class="muted" style="margin-top:0.6rem;">{switcher}</div>
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

    const CSS: &str = include_str!("style.css");

    /// The stylesheet with its `/* … */` comments removed, so prose about a
    /// token is never mistaken for a declaration of one.
    fn css() -> String {
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
    fn every_token_painting_behind_text_is_overridden_in_the_dark_theme() {
        // A token left out of the dark block keeps its light value there,
        // which is exactly the #65 failure mode: `--fg` flips to near-white
        // while the surface behind it stays near-white.
        let (_, after_root) = block_after(&css(), ":root", 0);
        // `block_after` stops at the first `}`, which inside the media query
        // is the end of its nested `:root` — so the media body it hands back
        // *is* that `:root`, minus its closing brace.
        let (media, _) = block_after(&css(), "@media (prefers-color-scheme: dark)", after_root);
        let (dark, _) = block_after(&media, ":root", 0);
        let dark = declared_tokens(&dark);

        for token in [
            "--fg",
            "--bg",
            "--muted",
            "--border",
            "--accent-bg",
            "--chip-bg",
        ] {
            assert!(
                dark.contains(token),
                "`{token}` is not redefined under `prefers-color-scheme: dark`"
            );
        }
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
}
