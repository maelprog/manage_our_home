//! Page shell + small render helpers shared by `src/routes/auth/*`.
//!
//! Each route builds its page *content* with Leptos's `view!` macro,
//! rendered server-side to a string via `leptos::ssr::render_to_string`
//! (`apps/web` is server-rendered only — no client-side hydration/WASM
//! bundle, see `Cargo.toml`'s doc comment for why). `shell` then wraps
//! that fragment in the surrounding `<html>`/`<head>`/`<body>` document,
//! plain-string templated since it never needs reactivity.

use manage_our_home_shared::dto::auth::MeResponse;

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

/// Very small header shown on authenticated pages (home, and anywhere
/// else once other front epics land): display name + a logout form.
/// `POST /logout` is a route on apps/web itself (not apps/api directly)
/// so it can also clear/redirect server-side in one step.
pub fn authenticated_header(me: &MeResponse) -> String {
    format!(
        r#"<div class="muted" style="display:flex;justify-content:space-between;align-items:center;margin-bottom:1.5rem;">
<span>{name}</span>
<form method="post" action="/logout" style="margin:0;">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
</div>"#,
        name = html_escape(&me.display_name),
    )
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
