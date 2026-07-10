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
/// `authenticated_header`'s name+logout row plus a nav (Accueil / Groupes)
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
    format!(
        r#"<header style="margin-bottom:1.5rem;">
<div class="muted" style="display:flex;justify-content:space-between;align-items:center;gap:0.75rem;">
<nav style="display:flex;gap:0.75rem;">
<a href="/">Accueil</a>
<a href="/groups">Groupes</a>
</nav>
<span style="display:flex;gap:0.75rem;align-items:center;">
<span>{name}</span>
<form method="post" action="/logout" style="margin:0;">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
</span>
</div>
<div class="muted" style="margin-top:0.6rem;">{switcher}</div>
</header>"#,
        name = html_escape(&me.display_name),
        switcher = switcher,
    )
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
