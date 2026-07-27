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
}
