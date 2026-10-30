//! The theme a visitor picks on `/account/appearance` (#585): Automatique,
//! Clair or Sombre.
//!
//! The choice lives in a cookie of apps/web's own, read on every request
//! and turned into `<html data-theme="light|dark">` by `app::document`.
//! Automatique is the absence of the cookie: the page carries no attribute
//! and the sheet follows the device (`prefers-color-scheme`), as it did
//! before the choice existed. A value this module does not know — an old
//! name, a hand-edited cookie — reads as Automatique, never as an error.
//!
//! The cookie is a preference of the interface, like the active family
//! (`family.rs`): exempt from consent (CNIL), `HttpOnly` since no script
//! reads it, `SameSite=Lax`. And `__Host-` prefixed like the return path
//! (`return_to.rs`, #576): the browser keeps it only when it is `Secure`,
//! `Path=/` and has no `Domain`, so no sibling host can force a theme. The
//! same price as there: over plain HTTP, only `localhost` keeps it in
//! Chromium and Firefox, and no origin at all in WebKit.
//!
//! The dark values of the sheet are written once, under
//! `:root[data-theme="dark"]`; [`with_device_theme`] derives, once per
//! process, the `prefers-color-scheme: dark` block that applies them when
//! no choice is made. See DESIGN.md → Couleur → Thème choisi.

use axum::extract::Request;
use axum::http::header::COOKIE;
use axum::middleware::Next;
use axum::response::Response;

pub const THEME_COOKIE: &str = "__Host-theme";

/// A year, the active family's lifetime (`family.rs`).
pub const THEME_MAX_AGE_SECS: u32 = 365 * 24 * 60 * 60;

/// The three choices of the appearance page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    /// No cookie, no attribute: the device decides.
    Auto,
    Light,
    Dark,
}

impl Appearance {
    /// The three, in the order the page lists them.
    pub const ALL: [Appearance; 3] = [Appearance::Auto, Appearance::Light, Appearance::Dark];

    /// The value of the radio button, and of the cookie for the two that
    /// set one.
    pub fn value(self) -> &'static str {
        match self {
            Appearance::Auto => "auto",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    /// The name the page and the profile give it.
    pub fn label(self) -> &'static str {
        match self {
            Appearance::Auto => "Automatique",
            Appearance::Light => "Clair",
            Appearance::Dark => "Sombre",
        }
    }

    /// The choice a submitted form names, `None` for any other value.
    pub fn from_value(value: &str) -> Option<Appearance> {
        Appearance::ALL.into_iter().find(|a| a.value() == value)
    }

    /// The `data-theme` value `<html>` carries, `None` for Automatique.
    pub fn data_theme(self) -> Option<&'static str> {
        (self != Appearance::Auto).then(|| self.value())
    }

    /// The choice a request's `Cookie` header carries: Automatique without
    /// the cookie or with a value it does not know. The first cookie of the
    /// name is the one read, even when its value is unknown, as
    /// `return_to.rs` (`return_cookie`) does; `family.rs` instead skips a
    /// value it cannot parse and reads the next cookie of the name.
    pub fn from_cookie_header(cookie_header: Option<&str>) -> Appearance {
        let Some(header) = cookie_header else {
            return Appearance::Auto;
        };
        header
            .split(';')
            .find_map(|pair| {
                let (name, value) = pair.trim().split_once('=')?;
                (name == THEME_COOKIE).then_some(value)
            })
            .and_then(Appearance::from_value)
            .filter(|a| a.data_theme().is_some())
            .unwrap_or(Appearance::Auto)
    }

    /// The `Set-Cookie` value recording this choice: the cookie for Clair
    /// and Sombre, its removal for Automatique — with the same attributes
    /// either way, or the browser drops a `__Host-` cookie.
    pub fn set_cookie(self) -> String {
        let (value, max_age) = match self.data_theme() {
            Some(theme) => (theme, THEME_MAX_AGE_SECS),
            None => ("", 0),
        };
        format!("{THEME_COOKIE}={value}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Lax")
    }
}

tokio::task_local! {
    /// The choice of the request being served, set by [`scope`].
    static CURRENT: Appearance;
}

/// The choice of the request being served — Automatique outside [`scope`],
/// as in a unit test rendering a page directly.
pub fn current() -> Appearance {
    CURRENT.try_with(|a| *a).unwrap_or(Appearance::Auto)
}

/// Middleware: reads the cookie and serves the request inside its choice,
/// so `app::document` can write `data-theme` without every route passing
/// it down. Every `Cookie` header is read: HTTP/2 may split them.
pub async fn scope(request: Request, next: Next) -> Response {
    let cookies: Vec<&str> = request
        .headers()
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    let appearance = Appearance::from_cookie_header(Some(&cookies.join("; ")));
    CURRENT.scope(appearance, next.run(request)).await
}

/// The selector the sheet writes its dark values under — the one place
/// they exist in the source.
pub const DARK_SELECTOR: &str = r#":root[data-theme="dark"]"#;

/// The selector of the derived block: the device asks for dark and the
/// visitor has not asked for light.
pub const DEVICE_DARK_SELECTOR: &str = r#":root:not([data-theme="light"])"#;

/// The media query of the derived block.
pub const DEVICE_DARK_MEDIA: &str = "@media (prefers-color-scheme: dark)";

/// The sheet as served: `source` with, right after its
/// `:root[data-theme="dark"]` block, the same declarations under
/// `prefers-color-scheme: dark` for a page that made no choice — comments
/// left out, the source carries them. Panics when the block is missing:
/// the input is the compiled-in sheet, and a test reads it.
pub fn with_device_theme(source: &str) -> String {
    let head = source
        .find(&format!("{DARK_SELECTOR} {{"))
        .unwrap_or_else(|| panic!("`{DARK_SELECTOR} {{` not found in the sheet"));
    let open = head + DARK_SELECTOR.len() + 1;
    let close = open
        + closing_brace(&source[open..])
            .unwrap_or_else(|| panic!("`{DARK_SELECTOR}` is never closed"));
    let declarations = without_comments(&source[open + 1..close]);
    let declarations: Vec<&str> = declarations
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut out = String::with_capacity(source.len() + close - open);
    out.push_str(&source[..=close]);
    out.push('\n');
    out.push_str(DEVICE_DARK_MEDIA);
    out.push_str(" {\n  ");
    out.push_str(DEVICE_DARK_SELECTOR);
    out.push_str(" {\n");
    for line in declarations {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("  }\n}");
    out.push_str(&source[close + 1..]);
    out
}

/// The offset of the first `}` of `css` outside a `/* … */` comment: a
/// brace written in the prose of a comment does not end the block.
fn closing_brace(css: &str) -> Option<usize> {
    let mut from = 0;
    loop {
        let brace = from + css[from..].find('}')?;
        match css[from..].find("/*").map(|at| from + at) {
            Some(comment) if comment < brace => {
                from = comment + 2 + css[comment + 2..].find("*/")? + 2;
            }
            _ => return Some(brace),
        }
    }
}

/// `css` with its `/* … */` comments taken out.
pub(crate) fn without_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_choices_round_trip_through_the_form() {
        for a in Appearance::ALL {
            assert_eq!(Appearance::from_value(a.value()), Some(a), "{a:?}");
        }
        assert_eq!(Appearance::Auto.value(), "auto");
        assert_eq!(Appearance::Light.value(), "light");
        assert_eq!(Appearance::Dark.value(), "dark");
        for unknown in ["", "Dark", "sombre", "system", "dark "] {
            assert_eq!(Appearance::from_value(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn the_choices_are_named_as_the_page_names_them() {
        assert_eq!(Appearance::Auto.label(), "Automatique");
        assert_eq!(Appearance::Light.label(), "Clair");
        assert_eq!(Appearance::Dark.label(), "Sombre");
    }

    #[test]
    fn only_a_forced_theme_writes_data_theme() {
        assert_eq!(Appearance::Auto.data_theme(), None);
        assert_eq!(Appearance::Light.data_theme(), Some("light"));
        assert_eq!(Appearance::Dark.data_theme(), Some("dark"));
    }

    #[test]
    fn light_and_dark_are_read_from_the_cookie() {
        assert_eq!(
            Appearance::from_cookie_header(Some("__Host-theme=light")),
            Appearance::Light
        );
        assert_eq!(
            Appearance::from_cookie_header(Some(
                "session=abc; __Host-theme=dark; active_group_id=x"
            )),
            Appearance::Dark
        );
    }

    #[test]
    fn no_cookie_is_automatique() {
        assert_eq!(Appearance::from_cookie_header(None), Appearance::Auto);
        assert_eq!(Appearance::from_cookie_header(Some("")), Appearance::Auto);
        assert_eq!(
            Appearance::from_cookie_header(Some("session=abc")),
            Appearance::Auto
        );
    }

    /// Never an error: whatever the cookie holds, the page renders, on the
    /// device's theme.
    #[test]
    fn an_unknown_value_is_automatique() {
        for value in [
            "",
            "auto",
            "Dark",
            "sombre",
            "dark-mode",
            "light\u{0}",
            "%64ark",
        ] {
            let header = format!("__Host-theme={value}");
            assert_eq!(
                Appearance::from_cookie_header(Some(&header)),
                Appearance::Auto,
                "{value:?}"
            );
        }
        // Neither the unprefixed name nor a name that only ends like it.
        for header in ["theme=dark", "x__Host-theme=dark", "__host-theme=dark"] {
            assert_eq!(
                Appearance::from_cookie_header(Some(header)),
                Appearance::Auto,
                "{header:?}"
            );
        }
    }

    /// Two cookies of the name: the first one counts, whatever the second
    /// holds, even when the first is unknown — the rule of `return_to.rs`
    /// (`return_cookie`), not of `family.rs`, which skips an unparsable one.
    #[test]
    fn a_duplicated_cookie_reads_the_first() {
        assert_eq!(
            Appearance::from_cookie_header(Some("__Host-theme=dark; __Host-theme=light")),
            Appearance::Dark
        );
        assert_eq!(
            Appearance::from_cookie_header(Some("__Host-theme=light; __Host-theme=dark")),
            Appearance::Light
        );
        assert_eq!(
            Appearance::from_cookie_header(Some("__Host-theme=bogus; __Host-theme=dark")),
            Appearance::Auto
        );
    }

    /// The `__Host-` prefix: the browser drops the cookie unless it is
    /// `Secure`, `Path=/` and has no `Domain` — the removal included.
    #[test]
    fn the_cookie_is_host_prefixed_secure_and_host_only() {
        assert_eq!(THEME_COOKIE, "__Host-theme");
        for a in Appearance::ALL {
            let set = a.set_cookie();
            let mut parts = set.split("; ");
            let first = parts.next().unwrap();
            let attributes: Vec<&str> = parts.collect();
            for wanted in ["Secure", "Path=/", "HttpOnly", "SameSite=Lax"] {
                assert!(attributes.contains(&wanted), "{wanted} missing: {set}");
            }
            assert!(
                !attributes
                    .iter()
                    .any(|a| a.to_ascii_lowercase().starts_with("domain")),
                "{set}"
            );
            match a.data_theme() {
                Some(theme) => {
                    assert_eq!(first, format!("__Host-theme={theme}"), "{set}");
                    assert!(attributes.contains(&"Max-Age=31536000"), "{set}");
                }
                None => {
                    assert_eq!(first, "__Host-theme=", "{set}");
                    assert!(attributes.contains(&"Max-Age=0"), "{set}");
                }
            }
        }
    }

    /// What a cookie set by the page reads back as.
    #[test]
    fn a_set_cookie_reads_back_as_its_choice() {
        for a in Appearance::ALL {
            let set = a.set_cookie();
            let pair = set.split("; ").next().unwrap();
            assert_eq!(Appearance::from_cookie_header(Some(pair)), a, "{set}");
        }
    }

    #[tokio::test]
    async fn the_current_choice_is_the_scoped_one_and_automatique_outside() {
        assert_eq!(current(), Appearance::Auto);
        let inside = CURRENT.scope(Appearance::Dark, async { current() }).await;
        assert_eq!(inside, Appearance::Dark);
    }

    const SOURCE: &str = r#":root {
  color-scheme: light dark;
  --bg: #fbfaf7;
}
:root[data-theme="light"] { color-scheme: light; }
:root[data-theme="dark"] {
  /* why the values are what they are */
  color-scheme: dark;
  --bg: #1a1917;
  --shadow: 0 0 0 1px var(--hover);
}
body { background: var(--bg); }
"#;

    #[test]
    fn the_device_block_follows_the_dark_block_with_its_declarations() {
        let sheet = with_device_theme(SOURCE);
        assert!(
            sheet.starts_with(&SOURCE[..SOURCE.find("body {").unwrap()]),
            "{sheet}"
        );
        assert!(
            sheet.ends_with("body { background: var(--bg); }\n"),
            "{sheet}"
        );
        let at = sheet
            .find(DEVICE_DARK_MEDIA)
            .expect("the derived media block");
        let derived = &sheet[at..sheet.find("body {").unwrap()];
        assert!(derived.contains(DEVICE_DARK_SELECTOR), "{derived}");
        for declaration in [
            "color-scheme: dark;",
            "--bg: #1a1917;",
            "--shadow: 0 0 0 1px var(--hover);",
        ] {
            assert!(derived.contains(declaration), "{declaration}: {derived}");
        }
        // The comment stays in the source, not in the copy.
        assert!(!derived.contains("/*"), "{derived}");
        // Opened and closed: the media query, then its rule.
        assert_eq!(derived.matches('{').count(), 2, "{derived}");
        assert_eq!(derived.matches('}').count(), 2, "{derived}");
    }

    #[test]
    fn the_dark_values_are_not_repeated_by_hand_anywhere_else() {
        let sheet = with_device_theme(SOURCE);
        assert_eq!(sheet.matches("--bg: #1a1917;").count(), 2, "{sheet}");
        assert_eq!(SOURCE.matches(DEVICE_DARK_MEDIA).count(), 0);
        assert_eq!(sheet.matches(DEVICE_DARK_MEDIA).count(), 1);
    }

    /// A `}` inside a comment of the dark block is prose, not its end: the
    /// declarations after it are copied too, and the comment is not.
    #[test]
    fn a_brace_in_a_comment_does_not_end_the_dark_block() {
        let source = r#":root { --bg: #fff; --fg: #000; }
:root[data-theme="dark"] {
  /* a closing } and an opening { in prose */
  --bg: #111;
  /* } */
  --fg: #eee;
}
body { color: var(--fg); }
"#;
        let sheet = with_device_theme(source);
        let at = sheet.find(DEVICE_DARK_MEDIA).expect("the derived block");
        let derived = &sheet[at..sheet.find("body {").unwrap()];
        for declaration in ["--bg: #111;", "--fg: #eee;"] {
            assert!(derived.contains(declaration), "{declaration}: {sheet}");
        }
        assert!(!derived.contains("/*"), "{derived}");
        assert!(
            sheet.ends_with("}\nbody { color: var(--fg); }\n"),
            "{sheet}"
        );
        assert_eq!(derived.matches('{').count(), 2, "{derived}");
        assert_eq!(derived.matches('}').count(), 2, "{derived}");
    }

    #[test]
    #[should_panic(expected = "data-theme")]
    fn a_source_without_the_dark_block_is_refused() {
        with_device_theme(":root { --bg: #fff; }");
    }
}
