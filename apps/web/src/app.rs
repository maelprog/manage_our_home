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

/// The content width a page asks for — DESIGN.md → Layout names three, and
/// `shell` takes one instead of imposing the single 28rem column that used
/// to wrap the month grid, the admin tables and the messagerie alike
/// (audit §3.1: on a workstation the app was a ribbon in an empty screen).
///
/// The choice is made at the call site rather than looked up from the path:
/// a route knows what it renders, and a central table would be one more
/// thing to keep in step with the router.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    /// 28 rem — authentication, create/edit forms, confirmations and the
    /// short error pages. The auth screens are the one part of the app the
    /// old single column served correctly; they keep exactly this.
    Form,
    /// 65 rem — a detail page, a recipe, the account hub, the privacy
    /// policy. Long text at 28 rem is *too narrow*, not too wide.
    Read,
    /// The whole column — the agenda grid, the admin tables, the messagerie
    /// and the list screens, i.e. everything the narrow column broke.
    Full,
}

impl Width {
    /// The modifier `<main class="content…">` wears. `Full` adds nothing:
    /// filling the column is what `.content` already does, so the widest
    /// case costs no class and no rule.
    fn class(self) -> &'static str {
        match self {
            Width::Form => " w-form",
            Width::Read => " w-read",
            Width::Full => "",
        }
    }
}

/// A page with no navigation: the authentication screens, the public
/// privacy policy served to a signed-out visitor, and the short error
/// pages a route renders when it has no family context to build a header
/// from.
///
/// The width comes first so that adding it to the 73 existing call sites was
/// a mechanical edit rather than 73 chances to move a comma.
pub fn shell(width: Width, title: &str, body_html: &str) -> String {
    document(
        title,
        &format!(
            r#"<main class="content{width}">
{body_html}
</main>"#,
            width = width.class(),
            body_html = body_html,
        ),
    )
}

/// A page of the application proper: the navigation and the content are
/// **siblings**, because the navigation is a grid column beside the content
/// above 861 px and a bottom tab bar below it (#70).
///
/// Until this issue every route pasted `app_header`'s output at the top of
/// its own body, so `<header>` rendered *inside* `<main>` — which no grid
/// can lay out side by side, and which mislabelled the landmark besides.
pub fn shell_with_header(width: Width, title: &str, header_html: &str, body_html: &str) -> String {
    document(
        title,
        &format!(
            r#"<div class="app">
{header_html}
<main class="content{width}">
{body_html}
</main>
</div>"#,
            header_html = header_html,
            width = width.class(),
            body_html = body_html,
        ),
    )
}

/// The `<html>`/`<head>`/`<body>` skeleton both shells share.
///
/// The stylesheet used to be `include_str!`'d into this template, one copy
/// per response. Since #89 it is a `<link>` to `crate::assets`, which
/// serves the same `include_str!`'d constant under a URL made of its own
/// digest — so a browser that has the sheet never asks again, and a
/// browser that is handed new HTML can never resolve the link to old CSS.
/// See DESIGN.md → Livraison du CSS for the trade this makes (one blocking
/// round trip on a cold cache, against ~6.6 kB of gzip on every page view)
/// and for the budget that still bounds the sheet.
fn document(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="fr">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{title} — Manage our home</title>
<link rel="stylesheet" href="{css}"/>
</head>
<body>
{body}
</body>
</html>"#,
        title = html_escape(title),
        css = crate::assets::stylesheet_href(),
        body = body,
    )
}

/// The first path segment of a URL path, query and fragment dropped —
/// `"agenda"` for `/agenda`, `/agenda/new` and `/agenda?month=2026-07`
/// alike, and `""` for `/`.
fn first_segment(path: &str) -> &str {
    path.split(['?', '#'])
        .next()
        .unwrap_or(path)
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
}

/// Whether a nav link leads to the section `current_path` is in — the one
/// thing `aria-current="page"` needs to know (#69).
///
/// Matching is on the **first path segment**, not a prefix. A prefix test
/// would light two tabs at once in both directions: `/` prefixes every path
/// in the app, and `/groups` prefixes nothing but `/grocery-list` is the kind
/// of near-miss it invites. The segment also keeps the Admin link — which
/// points at `/admin/users`, one of two admin screens — lit on the other one,
/// and keeps a section lit on the pages below it (`/agenda/new` is still the
/// agenda).
pub fn nav_link_is_current(href: &str, current_path: &str) -> bool {
    first_segment(href) == first_segment(current_path)
}

/// One `<a class="navlink">`, carrying `aria-current="page"` when it leads to
/// the page being rendered. `href` and `label` are literals from the nav
/// table below, so only the path needs escaping.
fn navlink(href: &str, label: &str, current_path: &str) -> String {
    let current = if nav_link_is_current(href, current_path) {
        r#" aria-current="page""#
    } else {
        ""
    };
    format!(r#"<a class="navlink" href="{href}"{current}>{label}</a>"#)
}

/// The main navigation, in order. `/admin/users` is appended for the single
/// technical superadmin only — everyone else never sees the door.
const NAV: [(&str, &str); 8] = [
    ("/", "Accueil"),
    ("/agenda", "Agenda"),
    ("/stocks", "Stocks"),
    ("/recipes", "Recettes"),
    ("/grocery-list", "Liste de courses"),
    ("/budget", "Budget"),
    ("/messagerie", "Messagerie"),
    ("/groups", "Groupes"),
];

/// The application's navigation, rendered once and laid out twice (#70): a
/// persistent sidebar from 861 px up, the same links as a bottom tab bar
/// below. One markup for both, so `aria-current="page"` lands on exactly
/// one element whatever the viewport — two navs would light two tabs.
///
/// It carries the nav proper, then issue #17's active-family switcher — a
/// plain `<form>` posting to `POST /groups/switch` (persists the choice in
/// the `active_group_id` cookie, see `crate::family`) then bouncing back to
/// `redirect_to` — then the account block: the caller's name, "Mon compte"
/// (the RGPD self-service hub, front epic F10) and logout. With no groups
/// yet, the switcher gives way to a "create a family" link.
///
/// The switcher moved out of a full-width strip under the nav and into the
/// sidebar, which is DESIGN.md → Layout's structure and what makes the
/// active family visible on every page rather than a line above the title.
///
/// `redirect_to` doubles as the current path, which is what decides the nav
/// link that carries `aria-current="page"` (#69): every caller already passes
/// the path of the page it is rendering, because that is where the family
/// switcher has to come back to. A caller that ever passes something else
/// would mislead the nav, not the switcher — the failure is cosmetic and
/// visible on the page itself.
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
    let links: String = NAV
        .iter()
        .copied()
        .chain(me.is_superadmin.then_some(("/admin/users", "Admin")))
        .map(|(href, label)| navlink(href, label, redirect_to))
        .collect();
    format!(
        r#"<header>
<nav class="tabs">{links}</nav>
<div class="muted">{switcher}</div>
<div class="actions">
<span class="muted">{name}</span>
{account}
<form method="post" action="/logout">
<button type="submit" class="secondary">Se déconnecter</button>
</form>
</div>
</header>"#,
        name = html_escape(&me.display_name),
        switcher = switcher,
        links = links,
        // "Mon compte" is a destination like any other, so it goes through
        // the same helper rather than restating its conditional (#69's
        // leftover nit, in this diff anyway because the link moves).
        account = navlink("/account", "Mon compte", redirect_to),
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
    const CSS: &str = crate::assets::STYLESHEET;
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

    // -- navigation, current page (#69) --------------------------------
    //
    // The eight nav links rendered identically whatever page was open, so
    // nothing said where you were. `aria-current="page"` is the answer for
    // assistive tech and the hook the stylesheet tints; the only piece of
    // logic it needs is "does this link lead to the page I am on", and that
    // is the function below.

    #[test]
    fn a_nav_link_is_current_on_its_own_page() {
        assert!(nav_link_is_current("/", "/"));
        assert!(nav_link_is_current("/agenda", "/agenda"));
        assert!(nav_link_is_current("/grocery-list", "/grocery-list"));
    }

    #[test]
    fn a_nav_link_is_current_on_the_pages_below_it() {
        // A section link stays lit while you are inside the section: the
        // agenda tab is still where `/agenda/new` lives.
        for path in ["/agenda/new", "/agenda/imports", "/agenda/imports/new"] {
            assert!(nav_link_is_current("/agenda", path), "{path}");
        }
        assert!(nav_link_is_current("/groups", "/groups/new"));
        // The Admin link points at one of the two admin screens, and has to
        // stay lit on the other.
        assert!(nav_link_is_current("/admin/users", "/admin/groups"));
    }

    #[test]
    fn only_one_nav_link_is_current_on_any_page() {
        // Two lit tabs are worse than none. `/` matching by prefix would
        // light Accueil on every page of the app, and a plain `starts_with`
        // would light Groupes on `/grocery-list`.
        // Not named `NAV`: `e2e/scripts/lib/measure-core.ts` reads the real
        // table out of this file and refuses a second one, on purpose.
        const HREFS: [&str; 9] = [
            "/",
            "/agenda",
            "/stocks",
            "/recipes",
            "/grocery-list",
            "/budget",
            "/messagerie",
            "/groups",
            "/admin/users",
        ];
        for path in [
            "/",
            "/agenda",
            "/agenda/new",
            "/stocks/new",
            "/recipes",
            "/grocery-list",
            "/budget/new",
            "/messagerie",
            "/groups/new",
            "/admin/groups",
        ] {
            let lit: Vec<&str> = HREFS
                .into_iter()
                .filter(|href| nav_link_is_current(href, path))
                .collect();
            assert_eq!(lit.len(), 1, "on `{path}`, lit links: {lit:?}");
        }
    }

    #[test]
    fn a_page_outside_the_nav_lights_nothing() {
        // `/privacy-policy` and the account screens are reachable from the
        // header but are not nav tabs.
        for path in ["/privacy-policy", "/account", "/account/export"] {
            for href in ["/", "/agenda", "/groups", "/admin/users"] {
                assert!(!nav_link_is_current(href, path), "{href} on {path}");
            }
        }
    }

    #[test]
    fn the_query_string_is_not_part_of_the_page() {
        // Every mutating action bounces back with `?notice=…`/`?error=…`
        // (PRG), so the current path routinely arrives with one attached.
        assert!(nav_link_is_current("/", "/?notice=x"));
        assert!(nav_link_is_current(
            "/groups",
            "/groups?notice=group_created"
        ));
        assert!(nav_link_is_current(
            "/agenda",
            "/agenda?month=2026-07#day-3"
        ));
        assert!(!nav_link_is_current("/agenda", "/?month=2026-07"));
    }

    #[test]
    fn a_trailing_slash_does_not_change_the_page() {
        assert!(nav_link_is_current("/agenda", "/agenda/"));
        assert!(nav_link_is_current("/", "/"));
    }

    /// A session, for the two header tests below.
    fn me(is_superadmin: bool) -> MeResponse {
        MeResponse {
            user_id: Uuid::nil(),
            email: "alice@example.test".to_string(),
            display_name: "Alice".to_string(),
            email_verified: true,
            is_superadmin,
            has_password: true,
            deletion_requested_at: None,
        }
    }

    #[test]
    fn the_header_marks_exactly_one_link_as_the_current_page() {
        let header = app_header(&me(true), &[], None, "/agenda/new");
        assert_eq!(
            header.matches(r#"aria-current="page""#).count(),
            1,
            "header: {header}"
        );
        assert!(header.contains(r#"<a class="navlink" href="/agenda" aria-current="page">"#));
        // Every nav link wears the component class, current or not — that is
        // what takes the navigation out of `.muted`: eight tabs, the Admin
        // one a superadmin gets, and "Mon compte" in the account block.
        assert_eq!(header.matches(r#"class="navlink""#).count(), 10);
    }

    #[test]
    fn the_account_link_is_current_on_the_account_screens() {
        // "Mon compte" is a destination like any other; on /account/export
        // the header has to say so too.
        let header = app_header(&me(false), &[], None, "/account/export");
        assert!(header.contains(r#"href="/account" aria-current="page""#));
        assert_eq!(header.matches(r#"aria-current="page""#).count(), 1);
    }

    #[test]
    fn the_header_navigation_is_not_secondary_content() {
        // The nav sat in a `.muted` block: grey, 0.875rem, ranked below the
        // content it leads to (#69).
        let header = app_header(&me(false), &[], None, "/");
        let nav = &header[..header.find("</nav>").expect("a nav in the header")];
        assert!(
            !nav.contains("muted"),
            "the main navigation is still styled as secondary content: {nav}"
        );
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
                let value = if let Some(after) = rest.strip_prefix('"') {
                    &after[..after.find('"').unwrap_or(after.len())]
                } else {
                    &rest[..end]
                };
                out.push(value.to_string());
            }
        }
        out
    }

    /// A ceiling, not a target: it exists so the 173 do not creep back one
    /// route at a time. What is left is what no class can carry — a member's
    /// computed hue, a control's one-off width, a table column's alignment —
    /// plus the messagerie bits #72 reworks.
    ///
    /// At module level rather than inside the test below because
    /// `budget_report` prints it too, and a report that restated the number
    /// beside the guard would drift from it (#95).
    const INLINE_STYLE_CEILING: usize = 6;

    #[test]
    fn inline_styles_are_bounded_to_a_justified_residue() {
        const CEILING: usize = INLINE_STYLE_CEILING;
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
        //
        // **This is a sample, not the whole list, and nothing keeps it in
        // step.** DESIGN.md → Composants and its Classes de soutien name 39
        // selectors; the 20 below are the ones successive issues thought to
        // add. All 39 are defined today, so the gap is latent rather than
        // broken — but a component named by the document and absent from
        // both this array and any `class="…"` is caught by nothing. Closing
        // it properly means deriving the list from the document instead of
        // retyping it, which is an issue of its own.
        for class in [
            ".page-header",
            ".list-row",
            // #72, and all three of what it adds to DESIGN.md → Composants:
            // a message you wrote yourself, the disclosure trigger that
            // replaced a `<summary>` wearing `btn secondary sm`, and the
            // open disclosure that gives the edit form the row's width.
            // The last two are not classes — the list is "what
            // DESIGN.md → Composants names", and it names selectors.
            //
            // `.actions > details[open]` is here because nothing else can
            // catch it: it appears in no `class="…"`, so deleting the rule
            // left the whole suite green while the edit textarea silently
            // collapsed back to its ~20-column default. That is the hole
            // the inline `min-width: 16rem` used to plug.
            ".list-row.mine",
            "summary {",
            ".actions > details[open]",
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
            ".navlink",
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

    // -- interaction states (#69) --------------------------------------
    //
    // Nothing in a build or a browser reports a missing hover, and a focus
    // ring is invisible precisely to the people who never press Tab. The
    // e2e suite (`e2e/tests/interaction.spec.ts`) walks the app with the
    // keyboard in both themes; these guard the rules the sheet has to carry
    // for that walk to be possible at all.

    /// Every `transition:` value the stylesheet declares, whitespace
    /// collapsed so a multi-line value reads as one string.
    fn transition_values() -> Vec<String> {
        let css = css();
        // Read forward to the end of the declaration rather than splitting
        // the sheet on `;`: a selector list carries colons of its own
        // (`a:hover`, `input:focus-visible`), so `split_once(':')` on a chunk
        // would answer with the selector, not the property.
        let needle = format!("transition{}", ':');
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(at) = css[from..].find(&needle) {
            let start = from + at + needle.len();
            let end = css[start..]
                .find([';', '}'])
                .map_or(css.len(), |i| start + i);
            out.push(
                css[start..end]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            from = end;
        }
        out
    }

    #[test]
    fn every_interactive_surface_answers_the_pointer() {
        // Audit §3.3: not one `:hover` in the sheet, so on a PC nothing said
        // it could be clicked.
        let css = css();
        for selector in [
            "button:hover",
            ".btn:hover",
            ".list-row:hover",
            ".chip:hover",
            ".navlink:hover",
            "a:hover",
        ] {
            assert!(css.contains(selector), "no `{selector}` rule in style.css");
        }
    }

    #[test]
    fn focus_is_stated_on_clickables_and_on_fields() {
        let (clickable, _) = block_after(&css(), "a:focus-visible", 0);
        assert!(clickable.contains(&format!("outline: 2px solid {}", var_of("--accent"))));
        assert!(clickable.contains("outline-offset: 2px"));
        let (fields, _) = block_after(&css(), "input:focus-visible", 0);
        assert!(fields.contains(&format!(
            "box-shadow: 0 0 0 3px {}",
            var_of("--accent-soft")
        )));
    }

    #[test]
    fn no_focus_outline_is_removed_without_a_replacement() {
        // `outline: none` is how a keyboard user loses the caret. The fields
        // do neutralise the browser's own ring, but with a *transparent*
        // outline — forced-colors mode drops `box-shadow` and repaints
        // outlines in a system colour, so `none` there would leave nothing.
        let css = css();
        for spelling in ["outline: none", "outline:none", "outline: 0"] {
            assert!(
                !css.contains(spelling),
                "`{spelling}` in style.css removes a focus indicator"
            );
        }
    }

    #[test]
    fn only_the_three_allowed_properties_are_transitioned() {
        // DESIGN.md → Interaction et motion. Anything else — a width, a
        // transform, `all` — animates layout, which is what makes an
        // interface feel slower rather than more responsive.
        const ALLOWED: [&str; 3] = ["background", "border-color", "box-shadow"];
        let values = transition_values();
        assert!(
            !values.is_empty(),
            "the sheet declares no transition at all"
        );
        for value in &values {
            for part in value.split(',') {
                let property = part.split_whitespace().next().unwrap_or("");
                assert!(
                    ALLOWED.contains(&property),
                    "`transition: {value}` animates `{property}`"
                );
            }
        }
    }

    #[test]
    fn every_transition_is_timed_by_the_motion_tokens() {
        // Both halves have to come from the tokens, because the reduced-motion
        // override works by zeroing `--dur` — a hardcoded `150ms` would keep
        // running for a visitor who asked for no motion.
        for value in transition_values() {
            for part in value.split(',') {
                assert!(
                    part.contains(&var_of("--dur")) && part.contains(&var_of("--ease")),
                    "`{part}` does not take its timing from --dur/--ease"
                );
            }
        }
    }

    #[test]
    fn reduced_motion_neutralises_every_transition() {
        let (block, _) = block_after(&css(), "@media (prefers-reduced-motion: reduce)", 0);
        let (root, _) = block_after(&block, ":root", 0);
        assert!(
            root.contains("--dur: 0s"),
            "reduced motion has to zero the duration token: {root}"
        );
    }

    #[test]
    fn the_current_nav_link_is_told_apart_by_more_than_its_colour() {
        // WCAG 1.4.1: the active tab is the ground *and* the weight, so it
        // survives a colour-blind reading and a monochrome screenshot.
        let (current, _) = block_after(&css(), r#".navlink[aria-current="page"]"#, 0);
        assert!(current.contains(&format!("background: {}", var_of("--accent-soft"))));
        assert!(current.contains("font-weight: 600"));
    }

    // -- responsive layout (#70) ---------------------------------------
    //
    // `shell` used to wrap every page in one 28rem column, and the sheet
    // carried exactly one media query — the dark theme. The app fitted a
    // phone by being 448px wide, not by adapting, and a workstation got the
    // month grid and the admin tables in that same ribbon (audit §3.1).
    // These guards hold the three halves of the fix a machine can see: the
    // width a page asks for is a class the sheet defines, the breakpoint and
    // the sidebar exist, and the tab bar is actually touchable.

    #[test]
    fn each_content_width_wears_a_class_the_stylesheet_defines() {
        // A width that names a class nobody wrote is a page rendered at the
        // wrong width, silently — CSS does not report an unknown class.
        let css = css();
        assert!(
            stylesheet_defines_class(&css, "content"),
            "`.content` is the column every page sits in"
        );
        for width in [Width::Form, Width::Read] {
            let class = width.class().trim();
            assert!(
                !class.is_empty(),
                "{width:?} is a bounded width and needs a class"
            );
            assert!(
                stylesheet_defines_class(&css, class),
                "{width:?} asks for `.{class}`, which style.css does not define"
            );
        }
        // Full width is `.content`'s own behaviour, so it adds nothing.
        assert_eq!(Width::Full.class(), "");
    }

    #[test]
    fn the_three_widths_are_told_apart() {
        // Two widths collapsing onto one class would put the auth forms and
        // the agenda in the same column again, which is the bug being fixed.
        let classes: BTreeSet<&str> = [Width::Form, Width::Read, Width::Full]
            .into_iter()
            .map(Width::class)
            .collect();
        assert_eq!(classes.len(), 3, "classes: {classes:?}");
    }

    #[test]
    fn each_bounded_width_takes_its_value_from_the_layout_token() {
        // DESIGN.md → Layout fixes 28rem and 65rem as tokens; a class that
        // restated the number would drift from the document.
        for (width, token) in [(Width::Form, "--w-form"), (Width::Read, "--w-read")] {
            let (rule, _) = block_after(&css(), &format!(".{}", width.class().trim()), 0);
            assert!(
                rule.contains(&format!("max-width: {}", var_of(token))),
                "{width:?} should bound itself with `{token}`: {rule}"
            );
        }
    }

    #[test]
    fn the_page_wears_the_width_it_asked_for() {
        for (width, expected) in [
            (Width::Form, r#"<main class="content w-form">"#),
            (Width::Read, r#"<main class="content w-read">"#),
            (Width::Full, r#"<main class="content">"#),
        ] {
            let html = shell(width, "Titre", "<h1>x</h1>");
            assert!(html.contains(expected), "{width:?}: {html}");
        }
    }

    #[test]
    fn the_navigation_is_a_sibling_of_the_content_not_a_child() {
        // The sidebar is a grid column next to the content, so the header
        // cannot live inside `<main>` the way it did when every route
        // pasted it at the top of its own body — and `<header>` inside
        // `<main>` was a mislabelled landmark besides.
        let html = shell_with_header(Width::Full, "Titre", "<header>nav</header>", "<h1>x</h1>");
        let header_end = html.find("</header>").expect("a header");
        let main_start = html.find("<main").expect("a main");
        assert!(header_end < main_start, "{html}");
        assert!(html.contains(r#"<div class="app">"#), "{html}");
        // A page with no session renders no navigation at all, so it needs
        // no grid either.
        let bare = shell(Width::Form, "Connexion", "<h1>x</h1>");
        assert!(!bare.contains("<header"), "{bare}");
        assert!(!bare.contains(r#"class="app""#), "{bare}");
    }

    #[test]
    fn the_sidebar_appears_at_the_breakpoint_the_design_system_names() {
        // 861px is DESIGN.md → Layout's threshold, and `--w-sidebar` its
        // width. Below it the same links are a bottom bar; there is one DOM
        // for both, so `aria-current="page"` is on exactly one element
        // whatever the viewport.
        let css = css();
        let at = css
            .find("@media (min-width: 861px)")
            .expect("no 861px breakpoint in style.css");
        let (wide, _) = block_after(&css, ".app", at);
        assert!(
            wide.contains("display: grid")
                && wide.contains("grid-template-columns")
                && wide.contains(&var_of("--w-sidebar")),
            "the wide layout should lay the sidebar out as a grid column \
             sized by --w-sidebar: {wide}"
        );
        // Below the breakpoint there is nothing to lay out side by side, so
        // the shell carries no rule at all — the header and the content are
        // two blocks, and the tab bar is out of flow.
        assert!(
            css[..at].find(".app").is_none(),
            "the narrow layout should need no `.app` rule"
        );
    }

    #[test]
    fn the_tab_bar_is_pinned_to_the_bottom_on_a_phone() {
        let css = css();
        let (tabs, _) = block_after(&css, "\n.tabs {", 0);
        assert!(tabs.contains("position: fixed"), "{tabs}");
        // Anchored to the bottom edge, not just taken out of flow.
        assert!(tabs.contains("inset: auto 0 0 0"), "{tabs}");
        // …and released again once there is room for a sidebar.
        let at = css
            .find("@media (min-width: 861px)")
            .expect("no 861px breakpoint in style.css");
        let (wide, _) = block_after(&css, ".tabs", at);
        assert!(
            wide.contains("position: static"),
            "the tab bar stays fixed on a wide screen: {wide}"
        );
    }

    #[test]
    fn a_tab_is_at_least_the_forty_four_pixels_a_finger_needs() {
        // DESIGN.md → Espacement: ≥ 44px on every interactive element. The
        // height is read back through the spacing token rather than trusted
        // as a literal, because the token is what the rule names.
        let css = css();
        let (tab, _) = block_after(&css, ".tabs .navlink", 0);
        let token = tab
            .split(';')
            .filter_map(|d| d.split_once(':'))
            .find(|(name, _)| name.trim() == "min-height")
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_else(|| panic!("`.tabs .navlink` states no min-height: {tab}"));
        let token = token
            .trim_start_matches("var(")
            .trim_end_matches(')')
            .to_string();
        let (root, _) = block_after(&css, ":root", 0);
        let value = root
            .split(';')
            .filter_map(|d| d.split_once(':'))
            .find(|(name, _)| name.trim() == token)
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_else(|| panic!("`{token}` is not a spacing token"));
        let px: f32 = value
            .trim_end_matches("px")
            .parse()
            .unwrap_or_else(|_| panic!("`{token}: {value}` is not a pixel length"));
        assert!(px >= 44.0, "a tab is {px}px tall, DESIGN.md asks for ≥ 44");
    }

    #[test]
    fn the_authentication_pages_keep_the_form_width() {
        // The one part of the app the old single column served correctly.
        // Widening a login form to 65rem would be a regression dressed up as
        // the fix, so the rule is checked against the sources rather than
        // trusted to review.
        let mut offenders = Vec::new();
        for (path, body) in rust_sources() {
            if !path.contains("routes/auth") {
                continue;
            }
            for call in shell_widths(&body) {
                if call != "Form" {
                    offenders.push(format!("{path}: Width::{call}"));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "an authentication page asks for something other than the form \
             width: {offenders:?}"
        );
    }

    #[test]
    fn every_screen_of_the_app_states_a_width() {
        // The checklist item is "give each route its width", and the type
        // makes it unskippable — but a route can still be added tomorrow, so
        // the count is asserted rather than assumed: every `shell` call in
        // the routes hands over one of the three.
        let calls: usize = rust_sources()
            .iter()
            .filter(|(path, _)| path.contains("routes"))
            .map(|(_, body)| shell_widths(body).len())
            .sum();
        assert!(
            calls >= 70,
            "only {calls} route screens name a width; the app has ~73 of them"
        );
    }

    /// The `Width::…` variant handed to every `shell`/`shell_with_header`
    /// call in a source, in order.
    fn shell_widths(src: &str) -> Vec<String> {
        // Assembled like the other scanners in this module: this file is one
        // of the sources the scan reads.
        let needle = format!("Width{}", "::");
        let mut out = Vec::new();
        for line in src.lines().filter(|l| !l.trim_start().starts_with("//")) {
            let mut rest = line;
            while let Some(at) = rest.find(&needle) {
                rest = &rest[at + needle.len()..];
                let end = rest
                    .find(|c: char| !c.is_alphanumeric())
                    .unwrap_or(rest.len());
                out.push(rest[..end].to_string());
            }
        }
        out
    }

    // -- delivery budget (#83, re-derived by #89) ------------------------
    //
    // Inlining was a bet: a sheet that travels inside the document costs a
    // copy per page view and buys a first paint with no blocking round
    // trip. The bet only paid while the document *and* the sheet fit in
    // the first congestion window together, and #83 wrote down the size at
    // which it stopped being true. #89 is the moment it did — the sheet is
    // now a content-addressed response of its own (`crate::assets`), and
    // the document no longer carries it.
    //
    // **Both guards are still here, and neither has been weakened in
    // silence.** What changed is what they are guards *for*; DESIGN.md's
    // decision journal carries the same statement in prose:
    //
    // * `SHEET_CEILING` weighed the sheet's *share* of a window it shared
    //   with the document. It no longer shares one, so that derivation is
    //   void and the number moves — from 10 KiB to 11 KiB, bracketed
    //   between one round trip above and the ordering floor below (see its
    //   comment for both). That is a real relaxation, declared as one. Its
    //   old remedy ("move the sheet to /assets") is spent: there is no
    //   third place to put it.
    // * `DECLARATIONS_CEILING` is unchanged, value and motive both. It
    //   weighs the CSS alone and its remedy is editorial: a rule that
    //   repeats another one goes.
    //
    // The one thing the pair no longer does is push back on prose. It
    // never should have: charging comments to the visitor was a property
    // of inlining, and inlining is what just went away. The ordering of
    // the two guards survives that, but doing less than it used to — it
    // now only decides which question a red test asks first, which is
    // argued where it is calibrated rather than claimed twice.

    /// A string's size on the wire, as gzip — one of the two encodings
    /// Caddy is configured to produce (`encode zstd gzip`,
    /// infra/Caddyfile) and the one every client accepts.
    ///
    /// What it does **not** see. Stated plainly, because a guard that is
    /// believed to cover more than it does is worse than none:
    ///
    /// * It weighs the sheet, not the pages. Since #89 those are two
    ///   separate responses, so this says nothing at all about whether a
    ///   given page fits DESIGN.md's whole-response budget — measured on
    ///   2026-08-03 with a month of real household data, the eight nav
    ///   routes run 684–8 133 gzipped bytes, and that number tracks how
    ///   much data a family has, which no unit test can know.
    /// * It was measured against *empty* pages once, and that reading
    ///   under-stated the document by a factor of two to three. Anyone
    ///   re-measuring the whole-response budget has to seed the stack
    ///   first (`npm run seed`, then `npm run measure`).
    /// * This is not an upper bound over both encodings. flate2's default
    ///   level (6) sits one notch above Caddy's gzip default (5) — 7 199
    ///   against 7 211 bytes on today's sheet — and Caddy's zstd, tuned
    ///   for speed,
    ///   measured *larger* than its own gzip on every main route (8 159 vs
    ///   7 949 bytes on `/`, +2.6 %). The three figures agree to within
    ///   ~3 %, which is far inside the ceilings' margin, but the number
    ///   below is an estimate of the wire, not a ceiling on it.
    /// * A sheet is compressed here in isolation. Until #89 that
    ///   under-stated nothing and over-stated a little — inside a document
    ///   the two shared one window and cost less than the sum of the
    ///   parts. Since #89 the sheet *is* its own response, so this is now
    ///   the right unit of measure for it, and the only thing missing is
    ///   the response headers.
    fn gzipped(bytes: &[u8]) -> usize {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("in-memory write");
        encoder.finish().expect("in-memory flush").len()
    }

    /// 11 KiB (11 264 bytes). **This was 10 KiB, and moving it is the one
    /// deliberate relaxation in #89** — written out here so nobody has to
    /// reconstruct it from a diff.
    ///
    /// Why the old value cannot simply stay. It was not a judgement about
    /// stylesheets, it was arithmetic: 14 336 minus the heaviest document
    /// (3 248 gzipped bytes on `/agenda`, `npm run seed`'s corpus) = 11 088,
    /// rounded down to 10 240. That subtraction has no meaning once the
    /// document travels without the sheet. And the old comment's "this
    /// ceiling is not raised — passing it means moving the sheet to
    /// /assets" is *spent*, not overruled: that move is what #89 is. A
    /// ceiling whose only remedy has been used has to be re-derived or
    /// dropped, and dropping it throws away the one pressure DESIGN.md
    /// credits with making #66 and #68 happen.
    ///
    /// The new value is **bracketed by two constraints**, not one. Both
    /// figures below are flate2 level 6 — the encoder `gzipped` uses, so
    /// they are the numbers this guard actually sees.
    ///
    /// * **Upper bound — one round trip.** The sheet is render-blocking on
    ///   a cold cache, so it should arrive in a single one: IW10, 10
    ///   segments of a 1 460-byte MSS ≈ 14 600 bytes, less headers and TLS
    ///   framing ⇒ 14 336. That bound is *conservative twice over*: the
    ///   sheet measures 10 131 bytes here (10 398 through Caddy), and it
    ///   does not even get a fresh IW10 — it is fetched on the connection
    ///   that just carried the document, one RTT in, with `cwnd` already
    ///   grown by slow start.
    /// * **Lower bound — the ordering of the two guards.**
    ///   `DECLARATIONS_CEILING` only trips first while this one stays above
    ///   `3 072 / (declarations / sheet)` = 3 072 / (2 995 / 10 131) =
    ///   **10 391.5 bytes**. Below that the pair inverts. This is the bound
    ///   that actually forces the move: 10 240 now sits *under* the floor.
    ///
    /// 11 264 sits inside that bracket with **1 133 bytes** of sheet growth
    /// and **872 bytes** of inversion margin. 14 336 would also have been
    /// defensible on the upper bound alone — it is the most permissive
    /// number that is, which is why it is not the one chosen. A ceiling
    /// 4 353 bytes above today's sheet would not fire for several issues.
    ///
    /// # Raised from 11 264 to 13 312 by #72, and why
    ///
    /// Everything above this heading is #89's reasoning, kept as the record
    /// of how 11 264 was picked. It is superseded, on a user arbitration,
    /// for a reason #89 could not see from where it stood.
    ///
    /// **The gap between 11 264 and the physical bound was discipline, not
    /// performance.** #89's own note above concedes it in passing: 14 336
    /// "would also have been defensible on the upper bound alone", and the
    /// upper bound itself is called "conservative twice over". What was
    /// actually holding the number down was the wish for a ceiling that
    /// fires *soon* — which is a policy about how the team is made to feel
    /// pressure, not a fact about what a browser can fetch. #72 hit it, and
    /// what it hit was the margin, not the round trip.
    ///
    /// **The derivation, redone from the physical side, and it no longer
    /// lands on 14 336** — and not because a term was missing from #89's.
    /// Read #89's own bound above: it goes from 14 600 "less headers and TLS
    /// framing" to 14 336, so it subtracted both already, and it was written
    /// *after* the move to /assets — it was not blind to the sheet being a
    /// response of its own. What it did not do is price them. 14 600 −
    /// 14 336 leaves **264 bytes** for the two together, an allowance the
    /// round number carries implicitly. What #72 changes is the size of that
    /// allowance, **264 → 346**, by measuring one of the two terms instead
    /// of leaving it inside a rounding:
    ///
    /// * **IW10** — 10 segments of a 1 460-byte MSS = **14 600 bytes** of
    ///   TCP payload in the first round trip.
    /// * **− response headers.** Measured, not guessed: `apps/web` answers
    ///   this route with **170 bytes** of HTTP/1.1 headers (`content-type`,
    ///   `cache-control`, `content-length`, `date`). Through Caddy the
    ///   deployed response also carries `server`, `content-encoding` and
    ///   `vary`, so budget **280**. (On HTTP/2, HPACK squeezes these to a
    ///   fraction; 280 is the pessimistic reading, which is the one a
    ///   ceiling wants.)
    /// * **− TLS record framing** — TLS 1.3 AEAD costs ~22 bytes a record
    ///   (5 header + 16 tag + 1 content type); allow 3 records ⇒ **66**.
    /// * ⇒ **14 254 bytes** for the compressed body.
    ///
    /// **14 KiB (14 336) does not fit. 13 KiB (13 312) is the largest KiB
    /// boundary that does**, and rounding down to a KiB boundary is what
    /// this document has done at every step. So the answer to "raise it to
    /// the physical bound" is 13 312, not 14 336 — and what separates
    /// 14 336 from 14 254 is exactly the 82 bytes the allowance grew by
    /// (346 − 264). The allowance itself is the thing `gzipped`'s own
    /// doc-note flags: it weighs a body and never the headers around it, so
    /// they come off the window here or nowhere.
    ///
    /// **And 13 312 is not that bound.** It sits **942 bytes** under the
    /// 14 254 derived just above. Those 942 are the price of landing on a
    /// KiB boundary — not a reserve anyone sized, and not a second margin
    /// on top of the conservatism already inside 14 254. Say so rather than
    /// letting "derived from its physical bound" stand for both numbers:
    /// what is not available is anything *above* 14 254; between 13 312 and
    /// 14 254 there is only an argument to make about round numbers.
    ///
    /// **What this abandons, stated rather than glossed.** DESIGN.md
    /// credits the budget with the "permanent pressure toward sobriety"
    /// that made #66 and #68 happen. That is an assertion of the document,
    /// and it has never been tested against the obvious alternative — that
    /// an audit naming 173 duplicated inline styles would have produced
    /// those two issues with or without a byte ceiling. It is not nothing,
    /// and it is not established. What is certain is that the pressure it
    /// bought was, by #72, being spent on paragraphs rather than on rules
    /// (see `DECLARATIONS_CEILING`), which is the opposite of the intent.
    ///
    /// **What it keeps.** The guard, its remedy, and its meaning: the sheet
    /// is render-blocking on a cold cache and must still arrive in one
    /// round trip. Exceeding *this* number is a real failure with no
    /// architectural escape left — the remedy would be splitting the sheet
    /// (critical CSS inline, the rest deferred), which is an issue of its
    /// own and not a line edited on the way past.
    ///
    /// **And it strengthens the pair rather than weakening it**, which is
    /// the asymmetry nobody had named: the ordering constraint is a *lower*
    /// bound on this ceiling, so raising it moves the pair further from
    /// inversion. With `DECLARATIONS_CEILING` at 3 136 the floor is
    /// 3 136 × 10 926 / 3 071 = 11 157.3, so the inversion margin goes from
    /// **107 bytes to 2 154.7** — the declarations guard keeps asking the
    /// cheap question first, now by a wide margin instead of a hair.
    ///
    /// As of #72 the sheet is **10 926 bytes**, so this leaves **2 386
    /// bytes** of growth for #73 and #74.
    ///
    /// **And it does not move again without an arbitration.** Room under
    /// the bound is not a self-service. What is unavailable is anything
    /// *above* 14 254; between 13 312 and 14 254 there is only an argument
    /// to make about round numbers — but *making* that argument is not a
    /// line edited on the way past. Both of #72's raises, and #89's before
    /// them, were arbitrated by the user before the value changed. So the
    /// rule here is `DECLARATIONS_CEILING`'s — redo the arithmetic against
    /// the sheet of the day and say so in the PR body — plus one step:
    /// **ask first**.
    const SHEET_CEILING: usize = 13 * 1024;

    /// 3 KiB of declarations, comments stripped. **Unchanged by #89** —
    /// same number, same reason — and it is now the only one of the two
    /// with any bite.
    ///
    /// Calibrated to trip before `SHEET_CEILING` does. **What that
    /// ordering is worth has changed, and pretending otherwise would be
    /// the weakest part of #89** — so, plainly:
    ///
    /// * Its original purpose is **gone**. It existed so the pressure
    ///   never landed on the comments, because inlining charged every
    ///   comment to every page view. The sheet is cached now; a comment
    ///   costs one download per deploy. There is no per-page-view prose
    ///   tax left to protect anyone from.
    /// * What it still does is **narrower, and real**: it decides which
    ///   question the first failing test asks. `DECLARATIONS_CEILING` asks
    ///   "is there a rule here that restates another one?" — local,
    ///   answerable, cheap to fix. `SHEET_CEILING` asks "does the delivery
    ///   strategy still work?", whose only remaining answer is splitting
    ///   the sheet, a whole issue. Keeping the cheap question first is
    ///   worth keeping. And deleting comments is *still* the tempting
    ///   shortcut in front of a red size test, even though it now buys
    ///   almost nothing; the ordering keeps that shortcut off the first
    ///   path.
    ///
    /// The ordering has a floor: declarations are 28.1 % of the compressed
    /// sheet (3 071 of 10 926, flate2 level 6, after #72), so this ceiling
    /// is reached first only while `SHEET_CEILING` stays above
    /// `DECLARATIONS_CEILING / 0.281`. Read the other way round — which is
    /// the form #72 needed — this ceiling may rise to at most
    /// `SHEET_CEILING × 0.281` = 11 264 × 3 071 / 10 926 = **3 166.0 bytes**
    /// before the pair inverts.
    ///
    /// After #71 that floor sat at **10 239.66 against a ceiling of
    /// 10 240** — 0.34 bytes of margin, *as flate2 measures it*. That sign
    /// is not a property of the world: an independent reading with system
    /// zlib put the same pair at 9 978 / 2 992, a floor of 10 244.8, i.e.
    /// **already inverted by ~4.8 bytes**. Which way it tips depends on
    /// the gzip implementation; what both readings agree on is that the
    /// margin was inside 5 bytes of the ceiling, i.e. gone. #89 does not
    /// fix that by trimming anything — `SHEET_CEILING` moves to 11 264,
    /// which puts the floor **872 bytes** under it. The pair works again
    /// because the sheet got a window of its own, not because anyone
    /// deleted a comment.
    ///
    /// What it has cost so far: #69 spent a third of the room that was
    /// left (2 298 → 2 670 bytes) for the hover, focus and nav-current
    /// rules; #70 nearly two thirds of the rest (2 670 → 2 921) for the
    /// responsive shell; #71 74 more (→ 2 995) for the agenda's single
    /// table; #72 76 more (→ 3 071) for the messagerie's four rules. #89
    /// adds none — it moves the sheet without touching a rule.
    ///
    /// # Raised from 3 072 to 3 136 by #72, and why
    ///
    /// This is the first raise, and it is an arbitration, not a drift. The
    /// old value did not become inconvenient; **it became pathological**,
    /// and the probe is one line:
    ///
    /// At 3 071 against a ceiling of 3 072, appending an ordinary
    /// three-line comment block to `style.css` — no rule, no selector, pure
    /// prose — measures **3 072**. It lands exactly on the ceiling, and the
    /// next comment breaks the build. `css_without_comments` removes the
    /// text between `/*` and `*/` but keeps the newline and the indentation
    /// around it, so prose is not free here even though everything that
    /// governs this file says it is: the message this very test prints
    /// ("Comments are not counted here, so this is about the CSS"), the
    /// bullet above ("There is no per-page-view prose tax left to protect
    /// anyone from"), and the note at the head of `style.css` since #89
    /// ("Write the comment"). A guard whose remedy is "delete a redundant
    /// rule" that in practice fires on a paragraph is telling the next
    /// author to do the one thing #89 exists to stop.
    ///
    /// The derivation of 3 136, in the same form as the bound above and on
    /// the same measurements (flate2 level 6, this branch):
    ///
    /// * **Upper bound — the ordering must survive.** The pair inverts at
    ///   `SHEET_CEILING × declarations / sheet`, which was 11 264 × 3 071 /
    ///   10 926 = **3 166.0** when this value was chosen. At or above that,
    ///   `SHEET_CEILING` fires first and the expensive question ("does the
    ///   delivery strategy still work?") becomes the one a red build asks.
    ///   That is the whole thing this ceiling exists to prevent, so 3 166
    ///   was a wall, not a target. (`SHEET_CEILING` moved to 13 312 later
    ///   in the same issue, which lifts that wall to 3 741.6 — 3 136 is
    ///   further inside the bracket than it was, never nearer the edge.
    ///   The value is left where the arbitration put it: it was chosen for
    ///   the pathology below, not for the bracket.)
    /// * **Round down to the 64-byte step below it: 3 136** (3 KiB + 64 B).
    ///   The 30 bytes given up buy the band the ratio needs. Re-measured
    ///   on *today's* sheet rather than carried over from #89 — whose "~5
    ///   bytes apart" was about the sheet of the day — flate2 and system
    ///   zlib disagree by **35 bytes on the sheet and 13 on the
    ///   declarations**; 30 covers the 13 that matter here, and the bound
    ///   recomputed under zlib (3 189.6) still sits above 3 136. The ratio
    ///   also moves as the sheet grows. Sitting *on* 3 166 would rebuild
    ///   the 0.34-byte margin #89 spent a whole issue undoing.
    /// * **Lower bound — sanity.** 3 136 − 3 071 = **65 bytes** of real
    ///   headroom, against the ~1 byte a comment block costs. The tripwire
    ///   is gone; the guard is a guard again.
    ///
    /// `SHEET_CEILING` moved to **13 312** later in this same issue, on its
    /// own arbitration and for reasons that have nothing to do with this
    /// one (see its comment). Together they leave #73 and #74 **65 bytes of
    /// declarations and 2 386 bytes of sheet**.
    ///
    /// **What a paragraph really costs, measured in situ.** Three earlier
    /// readings in this file were wrong and are worth naming — two on the
    /// method, the third on the sample it was read from. Take the method
    /// first: appending the *same* comment block over and over measures
    /// ~135 bytes for the first and 5–20 for each repeat — but that is
    /// gzip finding a copy of text it has already seen, not the cost of
    /// writing something. The project briefing warns about exactly this,
    /// and DESIGN.md already dismisses an earlier figure for the same
    /// fault.
    ///
    /// The number that means anything is measured **by deleting real
    /// comment blocks from this stylesheet, one at a time, and weighing
    /// what comes back** — not by appending filler.
    ///
    /// **That number is no longer restated here.** It was, as a range read
    /// off a ten-block sample, and the range was then taken for the
    /// sheet's: the real spread across blocks is wide enough that a small
    /// sample's upper bound **badly understates the worst case**, which is
    /// exactly the mistake a reader budgeting "at worst so many bytes a
    /// paragraph" would make. A number nobody copies cannot go stale
    /// (#95).
    ///
    /// To get it for today's sheet: drop one comment block from
    /// `style.css` with its whole lines, reweigh, take the difference,
    /// repeat block by block — and reweigh with **the guard's own encoder
    /// and no other**: flate2 level 6, `Compression::default()`, which is
    /// `gzipped` above and the thing that decides whether this test
    /// passes. Node's zlib (`npm run measure`) answers a few bytes away
    /// with nobody being wrong; it is a different encoder, and **naming
    /// which one you read is part of the number**. On varied prose the
    /// sheet grows in step: 10 926 → 11 076 (1 block) → 11 212 (2) →
    /// 11 450 (4) → 11 911 (8) → 13 139 (20).
    ///
    /// So **2 386 bytes is roughly twenty more paragraphs**, and that is
    /// the honest figure for #73 and #74 to plan against.
    ///
    /// **Declarations do *not* plateau** — an earlier draft of this comment
    /// claimed they did, off the same degenerate corpus. On varied prose
    /// they creep: 3 071 → 3 072 (1–4 blocks) → 3 074 (8) → 3 074 (20) →
    /// **3 075 (50)**. `css_without_comments` keeps each block's newline
    /// and indentation, and those accumulate; there is no ceiling to the
    /// creep, only a very shallow slope. The practical conclusion survives
    /// — at ~0.08 bytes a block, reaching 3 136 from 3 071 would take some
    /// **800 paragraphs**, and `SHEET_CEILING` rings around twenty — but it
    /// survives *because the sheet guard fires first*, not because this one
    /// is immune. Do not write "never" here.
    ///
    /// **Which of the two fires first depends on the shape of the growth,
    /// and the honest answer is: usually the sheet.** Comparing *levels*
    /// (declarations are 28.1 % of the sheet) says this guard leads — but
    /// no batch has ever grown at the cumulative ratio. Marginal ratio per
    /// batch — measured over `git show <merge>:apps/web/src/style.css` at
    /// each design merge (`3d1ed71` #66, `330bbf2` #67, `e5785ac` #68,
    /// `30fbd01` #69, `bc438e4` #70, `edc0527` #71, `9f08e97` #89), gzip
    /// level 6 with the comment-stripping of `css_without_comments`
    /// reimplemented over each revision:
    ///
    /// | batch | Δ sheet | Δ declarations | marginal |
    /// |---|---|---|---|
    /// | #67 | +358 | +123 | 34.4 % |
    /// | #68 | +2 918 | +626 | **21.5 %** |
    /// | #69 | +1 440 | +369 | 25.6 % |
    /// | #70 | +935 | +252 | 27.0 % |
    /// | #71 | +411 | +72 | 17.5 % |
    /// | #89 | +146 | 0 | 0 % |
    /// | **#72** | +791 | +76 | **9.6 %** |
    ///
    /// (Those are system-zlib figures, which run ~11 bytes under flate2 on
    /// the sheet and ~3 on the declarations; the ratios are unaffected.)
    /// An earlier draft of this comment put #68 at 22.8 %. That figure
    /// takes #66 as the base and so charges #67's growth to #68:
    /// (123 + 626) / (358 + 2 918) = 22.9 %. Against its actual base it is
    /// **21.5 %**.
    ///
    /// Only #67 — 358 bytes of `@font-face` and almost no prose — ever beat
    /// the cumulative 28.1 %. At one paragraph per rule the ordering barely
    /// holds; at two it inverts; at three — the shape of #72 itself — it is
    /// `SHEET_CEILING` that rings. Write for the sheet.
    ///
    /// A ceiling, not a target. What it forbids is drifting past it
    /// unnoticed while #73–#74 each add "just a few rules"; raising it
    /// again means redoing the arithmetic above against the sheet of the
    /// day and saying so in the PR body, as #72 did.
    const DECLARATIONS_CEILING: usize = 3 * 1024 + 64;

    /// A byte count with its thousands separated, as DESIGN.md writes
    /// numbers — `10 926`, not `10926`.
    ///
    /// A narrow no-break space (U+202F) rather than a plain one: the report
    /// is read in a terminal and a plain space lets a line break fall in the
    /// middle of a figure, which is how "10 926" becomes two numbers.
    fn thousands(n: usize) -> String {
        let digits = n.to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3);
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push('\u{202f}');
            }
            out.push(c);
        }
        out
    }

    /// A permille-precision percentage, French-style — `17,9`.
    fn percent(part: f64, whole: f64) -> String {
        format!("{:.1}", 100.0 * part / whole).replace('.', ",")
    }

    /// A one-decimal byte figure, French-style — `2 154,7`, `-2 154,7`.
    ///
    /// Rounds *once*, on the scaled value, then splits. Rounding the whole
    /// part and the fraction separately loses the carry (`11 157,96` prints
    /// `11 157,10`), and clamping the whole part loses the sign — which
    /// matters here, because the one figure that can go negative is the
    /// inversion margin, whose sign is the message.
    fn tenths(n: f64) -> String {
        let scaled = (n * 10.0).round();
        let sign = if scaled < 0.0 { "-" } else { "" };
        let abs = scaled.abs() as usize;
        format!("{sign}{},{}", thousands(abs / 10), abs % 10)
    }

    #[test]
    fn thousands_groups_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(65), "65");
        assert_eq!(thousands(1_024), "1\u{202f}024");
        assert_eq!(thousands(10_926), "10\u{202f}926");
        assert_eq!(thousands(1_000_000), "1\u{202f}000\u{202f}000");
    }

    #[test]
    fn percent_and_tenths_use_the_french_decimal_comma() {
        assert_eq!(percent(2_386.0, 13_312.0), "17,9");
        assert_eq!(tenths(2_154.7), "2\u{202f}154,7");
        assert_eq!(tenths(11_157.3), "11\u{202f}157,3");
    }

    /// Rounding a tenth up has to carry into the units, and past them.
    ///
    /// Written because the first version of `tenths` truncated the whole part
    /// and rounded the fraction *separately*, so anything from x.95 up
    /// printed a tenth of "10": `11 157,10`. The two figures this function
    /// exists for are read as budget headroom — a tenth that reads as ten is
    /// the kind of thing nobody re-checks.
    #[test]
    fn tenths_carries_into_the_whole_part() {
        assert_eq!(tenths(11_157.96), "11\u{202f}158,0");
        assert_eq!(tenths(999.97), "1\u{202f}000,0");
        assert_eq!(tenths(9.95), "10,0");
        assert_eq!(tenths(0.04), "0,0");
    }

    /// A negative inversion margin is the whole point of printing one: it
    /// says the two guards have swapped order. The first version clamped the
    /// whole part at zero and dropped the sign, so `-2 154,7` printed `0,7`.
    #[test]
    fn tenths_keeps_the_sign_of_a_negative_margin() {
        assert_eq!(tenths(-2_154.7), "-2\u{202f}154,7");
        assert_eq!(tenths(-0.4), "-0,4");
        assert_eq!(tenths(0.0), "0,0");
    }

    /// What is left under a ceiling — or, past it, by how much.
    ///
    /// One function for both sides because the first version had them apart:
    /// the byte count was a `saturating_sub` and the percentage on the same
    /// line was not, so the report panicked with a subtract overflow exactly
    /// when a guard went red. A report that is the single source of the
    /// budget cannot be the one thing that stops working the day the budget
    /// is blown.
    fn headroom(value: usize, ceiling: usize) -> String {
        if value <= ceiling {
            let left = ceiling - value;
            format!(
                "{} o  ({} % du plafond)",
                thousands(left),
                percent(left as f64, ceiling as f64)
            )
        } else {
            format!("DÉPASSÉ de {} o", thousands(value - ceiling))
        }
    }

    /// A ceiling written the way its constant is written — `13 × 1024`,
    /// `3 × 1024 + 64` — derived rather than copied beside it. A literal
    /// there would go on saying `13 × 1024` after the constant moved, which
    /// is the exact defect this whole report exists to remove.
    fn kib_expression(n: usize) -> String {
        match (n / 1024, n % 1024) {
            (kib, 0) => format!("{kib} × 1024"),
            (kib, rest) => format!("{kib} × 1024 + {rest}"),
        }
    }

    #[test]
    fn headroom_reports_both_sides_of_a_ceiling() {
        assert_eq!(
            headroom(10_926, 13_312),
            "2\u{202f}386 o  (17,9 % du plafond)"
        );
        assert_eq!(headroom(13_312, 13_312), "0 o  (0,0 % du plafond)");
    }

    /// The case that used to panic: over budget.
    #[test]
    fn headroom_does_not_panic_past_the_ceiling() {
        assert_eq!(headroom(13_400, 13_312), "DÉPASSÉ de 88 o");
        assert_eq!(headroom(30_000, 3_136), "DÉPASSÉ de 26\u{202f}864 o");
    }

    #[test]
    fn a_ceiling_prints_the_arithmetic_of_its_own_constant() {
        assert_eq!(kib_expression(13 * 1024), "13 × 1024");
        assert_eq!(kib_expression(3 * 1024 + 64), "3 × 1024 + 64");
    }

    /// **The one command that prints the budget** (#95).
    ///
    /// ```text
    /// cargo test -p manage_our_home_web budget_report -- --nocapture
    /// ```
    ///
    /// DESIGN.md used to restate these figures in prose, and they went stale
    /// at every PR: the verification of #72 spent five rounds on them and
    /// found the defect in the prose every single time, never in the code.
    /// So the document stopped carrying them and points here instead.
    ///
    /// **Why a test and not an `xtask` or a small binary.** The report has to
    /// be measured by *the same encoder as the guards*, not by one that
    /// merely claims to be the same — three verification findings came out of
    /// confusing flate2 level 6 with the system zlib and with Node's. A test
    /// in this module calls the very `gzipped` the two assertions call and
    /// reads the very constants they compare against, so the two cannot drift.
    /// The alternatives cannot: `flate2` is deliberately a dev-dependency
    /// (see apps/web/Cargo.toml — nothing in the shipped binary compresses
    /// anything), and this crate has no `lib` target, so an `xtask` or a
    /// second `[[bin]]` would have to re-declare both the encoder and its
    /// level, which is exactly the ambiguity this report exists to close.
    ///
    /// It asserts nothing on purpose — the assertions are the two tests
    /// below. This one only reads what they read out loud.
    #[test]
    fn budget_report() {
        let sheet_raw = crate::assets::STYLESHEET.len();
        let sheet = gzipped(crate::assets::STYLESHEET.as_bytes());
        let declarations_source = css();
        let declarations_raw = declarations_source.len();
        let declarations = gzipped(declarations_source.as_bytes());

        // The share of the compressed sheet the declarations occupy, and the
        // two readings of the ordering constraint it produces. The pair of
        // guards inverts — the sheet ceiling firing before the declarations
        // one — when `SHEET_CEILING` drops below this floor.
        let share = declarations as f64 / sheet as f64;
        let inversion_floor = DECLARATIONS_CEILING as f64 / share;
        let inversion_margin = SHEET_CEILING as f64 - inversion_floor;
        let declarations_at_inversion = SHEET_CEILING as f64 * share;

        let inline_styles: usize = rust_sources()
            .iter()
            .map(|(_, body)| inline_styles(body).len())
            .sum();

        // One `String` per line rather than one long literal: a `\` line
        // continuation in a Rust string eats the following line's indentation,
        // which is exactly the indentation this report is made of.
        let rule = "─".repeat(66);
        let lines: Vec<String> = vec![
            String::new(),
            format!("┌{rule}"),
            "│ DESIGN.md · état du budget de la feuille".to_string(),
            format!("└{rule}"),
            String::new(),
            "Encodeur : flate2 niveau 6 (`Compression::default()`), celui de".to_string(),
            "`gzipped` dans apps/web/src/app.rs — le seul qui décide si les deux".to_string(),
            "garde-fous passent. Le zlib système et celui de Node".to_string(),
            "(`npm run measure`) rendent quelques octets de moins sur la même".to_string(),
            "feuille : un chiffre de budget sans le nom de son encodeur ne veut".to_string(),
            "rien dire.".to_string(),
            String::new(),
            "Feuille — apps/web/src/style.css, telle que le binaire la sert".to_string(),
            format!(
                "  brut ...................................... {} o",
                thousands(sheet_raw)
            ),
            format!(
                "  compressée ................................ {} o",
                thousands(sheet)
            ),
            format!(
                "  SHEET_CEILING ............................. {} o  ({})",
                thousands(SHEET_CEILING),
                kib_expression(SHEET_CEILING)
            ),
            format!(
                "  reste ..................................... {}",
                headroom(sheet, SHEET_CEILING)
            ),
            String::new(),
            "Déclarations — la même feuille, commentaires retirés".to_string(),
            format!(
                "  brut ...................................... {} o",
                thousands(declarations_raw)
            ),
            format!(
                "  compressées ............................... {} o",
                thousands(declarations)
            ),
            format!(
                "  DECLARATIONS_CEILING ...................... {} o  ({})",
                thousands(DECLARATIONS_CEILING),
                kib_expression(DECLARATIONS_CEILING)
            ),
            format!(
                "  reste ..................................... {}",
                headroom(declarations, DECLARATIONS_CEILING)
            ),
            String::new(),
            "Part des commentaires — ce que la prose pèse dans la feuille".to_string(),
            format!(
                "  du brut ................................... {} %",
                percent((sheet_raw - declarations_raw) as f64, sheet_raw as f64)
            ),
            format!(
                "  du compressé .............................. {} %",
                percent((sheet - declarations) as f64, sheet as f64)
            ),
            String::new(),
            "Ordre des deux garde-fous — lequel mord en premier".to_string(),
            format!(
                "  part des déclarations dans la feuille ..... {} %",
                percent(declarations as f64, sheet as f64)
            ),
            format!(
                "  plancher d'inversion (SHEET_CEILING mini) . {} o",
                tenths(inversion_floor)
            ),
            format!(
                "  marge d'inversion ......................... {} o",
                tenths(inversion_margin)
            ),
            format!(
                "  DECLARATIONS_CEILING d'inversion .......... {} o",
                tenths(declarations_at_inversion)
            ),
            format!(
                "  → {} mord en premier.",
                if inversion_margin > 0.0 {
                    "DECLARATIONS_CEILING"
                } else {
                    "SHEET_CEILING"
                }
            ),
            String::new(),
            "Styles inline résiduels dans les routes".to_string(),
            format!("  comptés ................................... {inline_styles}"),
            format!("  plafond ................................... {INLINE_STYLE_CEILING}"),
            String::new(),
            "Ce que ce rapport ne sait pas".to_string(),
            "  Le budget de réponse (14 KiB, seuil 1 de DESIGN.md) dépend du".to_string(),
            "  volume de données d'un foyer : aucun test unitaire ne peut le".to_string(),
            "  connaître, et son encodeur est celui de Caddy, pas celui-ci. Il".to_string(),
            "  se mesure sur une stack semée — `npm run seed` puis".to_string(),
            "  `npm run measure` dans e2e/ — et une stack vide sous-estime le".to_string(),
            "  document d'un facteur deux à trois.".to_string(),
            String::new(),
            format!("└{rule}"),
            String::new(),
        ];
        println!("{}", lines.join("\n"));
    }

    #[test]
    fn the_compressed_stylesheet_still_arrives_in_one_round_trip() {
        let sheet = gzipped(crate::assets::STYLESHEET.as_bytes());
        assert!(
            sheet <= SHEET_CEILING,
            "the stylesheet compresses to {sheet} bytes; the budget is \
             {SHEET_CEILING} (DESIGN.md → Livraison du CSS). The sheet is \
             render-blocking on a cold cache, so past this it needs a second \
             round trip before anything paints. There is no /assets to move \
             it to any more — that move already happened (#89) — so the \
             remedy would be splitting the sheet, which is an issue of its \
             own. Do not answer this by deleting comments: the sheet is \
             cached now, the prose is paid once per deploy, not per page view."
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
