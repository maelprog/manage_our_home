//! The icon set (#522): inline SVG, drawn in the text colour.
//!
//! **Inline, not a sprite or an `<img>`.** The CSP admits images and styles
//! from the site only, and DESIGN.md → Contraintes non négociables refuses
//! any outside dependency; an inline `<svg>` asks nothing of either. It is
//! also the only form whose stroke follows `currentColor`, so an icon takes
//! the colour of the button, link or badge it sits in, in both themes and
//! under forced colours, with no rule of its own per context.
//!
//! **What the markup carries, and what the sheet does.** Each icon is
//! `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">` and its
//! shapes. The stroke (`currentColor`, width 2, round caps and joins), the
//! absence of fill and the size are one `.icon` rule in `style.css`: the
//! sheet is downloaded once per visitor and per deploy, a document on every
//! page view (DESIGN.md → Livraison du CSS), and an attribute written into
//! every icon would be paid on every one. No `style` attribute either way.
//!
//! **Always `aria-hidden`.** An icon is never the accessible name of what
//! carries it. Next to a visible label it would repeat it; alone in a control
//! the name belongs on the control. [`icon_link`] is the intended way to
//! write such a link, a convention held in review: nothing stops a caller
//! from wrapping [`icon`] in an `<a>` by hand.
//!
//! **Drawings: Lucide** (<https://lucide.dev>, lucide-static 1.63.0, ISC
//! licence; the icons Lucide inherits from Feather also under MIT). The
//! shapes are copied as Lucide publishes them, minus the attributes the
//! `.icon` rule carries. Both licences are reproduced in `docs/credits.md`.
//! The Lucide name of each is beside its arm in [`icon`].

use crate::app::html_escape;

/// Declares [`Icon`] and, for the tests, `Icon::ALL` from one list of
/// variant names: a variant cannot be added to the enum without being added
/// to `ALL`, so every test that walks `ALL` covers every icon.
macro_rules! icon_set {
    (
        $(#[$meta:meta])*
        pub enum Icon { $($variant:ident),* $(,)? }
    ) => {
        $(#[$meta])*
        pub enum Icon { $($variant),* }

        #[cfg(test)]
        impl Icon {
            /// Every variant, in declaration order, written by `icon_set!`
            /// from the list that declares the enum.
            pub(crate) const ALL: &'static [Icon] = &[$(Icon::$variant),*];
        }
    };
}

icon_set! {
    /// One icon of the set: what the mock-ups of the 2026-10-21 request draw.
    ///
    /// Named for the role it plays in the application, not for its drawing:
    /// `Budget` stays `Budget` if its picture changes.
    // The set ships ahead of the screens that use it (#524 to #539 take the
    // mock-ups over one by one): until they land, most variants are only
    // constructed by the tests.
    #[allow(dead_code)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Icon {
        Home,
        Calendar,
        Groceries,
        Stocks,
        Recipes,
        Budget,
        Messages,
        Bell,
        Search,
        Scan,
        Previous,
        Next,
        Plus,
        Minus,
        Send,
        Clock,
        Members,
        Link,
        Palette,
        Check,
        Paperclip,
        Settings,
        Sparkles,
        Close,
        Camera,
        Trash,
    }
}

/// An icon's `<svg>` element around its shapes, assembled at compile time:
/// the opening tag is written once, here, for the whole set.
macro_rules! svg {
    ($shapes:literal) => {
        concat!(
            r#"<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">"#,
            $shapes,
            "</svg>"
        )
    };
}

/// The whole `<svg>` element of `icon`. Each arm names its Lucide source.
pub fn icon(icon: Icon) -> &'static str {
    match icon {
        // house
        Icon::Home => svg!(
            r#"<path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/><path d="M3 10a2 2 0 0 1 .709-1.528l7-6a2 2 0 0 1 2.582 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
        ),
        // calendar
        Icon::Calendar => svg!(
            r#"<path d="M8 2v3"/><path d="M16 2v3"/><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18"/>"#
        ),
        // shopping-cart
        Icon::Groceries => svg!(
            r#"<path d="m2.05 2.05 1.099-.028a1 1 0 0 1 1.008.815l2.69 14.347A1 1 0 0 0 7.83 18H18"/><path d="M4.563 5h16.435a1 1 0 0 1 .981 1.204l-1.026 6.226A2 2 0 0 1 18.962 14H6.25"/><circle cx="18" cy="20" r="2"/><circle cx="8" cy="20" r="2"/>"#
        ),
        // package
        Icon::Stocks => svg!(
            r#"<path d="M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z"/><path d="M12 22V12"/><polyline points="3.29 7 12 12 20.71 7"/><path d="m7.5 4.27 9 5.15"/>"#
        ),
        // chef-hat
        Icon::Recipes => svg!(
            r#"<path d="M17 21a1 1 0 0 0 1-1v-5.35c0-.457.316-.844.727-1.041a4 4 0 0 0-2.134-7.589 5 5 0 0 0-9.186 0 4 4 0 0 0-2.134 7.588c.411.198.727.585.727 1.041V20a1 1 0 0 0 1 1Z"/><path d="M6 17h12"/>"#
        ),
        // credit-card
        Icon::Budget => svg!(
            r#"<rect width="20" height="14" x="2" y="5" rx="2"/><line x1="2" x2="22" y1="10" y2="10"/><path d="M6 14h2"/>"#
        ),
        // message-circle
        Icon::Messages => svg!(
            r#"<path d="M2.992 16.342a2 2 0 0 1 .094 1.167l-1.065 3.29a1 1 0 0 0 1.236 1.168l3.413-.998a2 2 0 0 1 1.099.092 10 10 0 1 0-4.777-4.719"/>"#
        ),
        // bell
        Icon::Bell => svg!(
            r#"<path d="M10.268 21a2 2 0 0 0 3.464 0"/><path d="M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"/>"#
        ),
        // search
        Icon::Search => svg!(r#"<path d="m21 21-4.34-4.34"/><circle cx="11" cy="11" r="8"/>"#),
        // scan-barcode
        Icon::Scan => svg!(
            r#"<path d="M3 7V5a2 2 0 0 1 2-2h2"/><path d="M17 3h2a2 2 0 0 1 2 2v2"/><path d="M21 17v2a2 2 0 0 1-2 2h-2"/><path d="M7 21H5a2 2 0 0 1-2-2v-2"/><path d="M8 7v10"/><path d="M12 7v10"/><path d="M17 7v10"/>"#
        ),
        // chevron-left
        Icon::Previous => svg!(r#"<path d="m15 18-6-6 6-6"/>"#),
        // chevron-right
        Icon::Next => svg!(r#"<path d="m9 18 6-6-6-6"/>"#),
        // plus
        Icon::Plus => svg!(r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#),
        // minus
        Icon::Minus => svg!(r#"<path d="M5 12h14"/>"#),
        // send
        Icon::Send => svg!(
            r#"<path d="M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z"/><path d="m21.854 2.147-10.94 10.939"/>"#
        ),
        // clock
        Icon::Clock => svg!(r#"<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>"#),
        // users
        Icon::Members => svg!(
            r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><path d="M16 3.128a4 4 0 0 1 0 7.744"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><circle cx="9" cy="7" r="4"/>"#
        ),
        // link
        Icon::Link => svg!(
            r#"<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>"#
        ),
        // palette: the four dots are filled, in the text colour too.
        Icon::Palette => svg!(
            r#"<path d="M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8z"/><circle cx="13.5" cy="6.5" r=".5" fill="currentColor"/><circle cx="17.5" cy="10.5" r=".5" fill="currentColor"/><circle cx="6.5" cy="12.5" r=".5" fill="currentColor"/><circle cx="8.5" cy="7.5" r=".5" fill="currentColor"/>"#
        ),
        // check
        Icon::Check => svg!(r#"<path d="M20 6 9 17l-5-5"/>"#),
        // paperclip
        Icon::Paperclip => svg!(
            r#"<path d="m16 6-8.414 8.586a2 2 0 0 0 2.829 2.829l8.414-8.586a4 4 0 1 0-5.657-5.657l-8.379 8.551a6 6 0 1 0 8.485 8.485l8.379-8.551"/>"#
        ),
        // sliders-vertical: what the mock-ups draw for settings, not a gear.
        Icon::Settings => svg!(
            r#"<path d="M10 8h4"/><path d="M12 21v-9"/><path d="M12 8V3"/><path d="M17 16h4"/><path d="M19 12V3"/><path d="M19 21v-5"/><path d="M3 14h4"/><path d="M5 10V3"/><path d="M5 21v-7"/>"#
        ),
        // sparkles
        Icon::Sparkles => svg!(
            r#"<path d="M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594z"/><path d="M20 2v4"/><path d="M22 4h-4"/><circle cx="4" cy="20" r="2"/>"#
        ),
        // x
        Icon::Close => svg!(r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#),
        // camera
        Icon::Camera => svg!(
            r#"<path d="M13.997 4a2 2 0 0 1 1.76 1.05l.486.9A2 2 0 0 0 18.003 7H20a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h1.997a2 2 0 0 0 1.759-1.048l.489-.904A2 2 0 0 1 10.004 4z"/><circle cx="12" cy="13" r="3"/>"#
        ),
        // trash (Lucide's former trash-2)
        Icon::Trash => svg!(
            r#"<path d="M10 11v6"/><path d="M14 11v6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M3 6h18"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"#
        ),
    }
}

/// A link whose only content is an icon, named by `label`.
///
/// The label is a parameter, so a call site has to say something: an icon
/// alone gives a control no accessible name, and a screen reader would
/// announce the bare `href`. An empty `label` is not refused and gives the
/// link an empty name. `label` is escaped here; `class` and `href` are written
/// as given, and must already be safe in a double-quoted attribute.
pub fn icon_link(class: &str, href: &str, label: &str, icon: Icon) -> String {
    format!(
        r#"<a class="{class}" href="{href}" aria-label="{label}">{svg}</a>"#,
        label = html_escape(label),
        svg = self::icon(icon),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_one_hidden_svg_element() {
        let open = r#"<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">"#;
        for &i in Icon::ALL {
            let svg = icon(i);
            let shapes = svg
                .strip_prefix(open)
                .and_then(|s| s.strip_suffix("</svg>"))
                .unwrap_or_else(|| panic!("{i:?}: {svg}"));
            assert!(shapes.starts_with('<'), "{i:?} draws nothing: {svg}");
            assert!(!shapes.contains("<svg"), "{i:?}: {svg}");
        }
    }

    #[test]
    fn no_icon_carries_a_style() {
        // Neither a `style` attribute nor a `<style>` element: the stroke is
        // the sheet's `.icon` rule, and the CSP's `style-src 'self'` refuses
        // the element outright.
        for &i in Icon::ALL {
            assert!(!icon(i).contains("style"), "{i:?}: {}", icon(i));
        }
    }

    #[test]
    fn no_icon_paints_its_own_colour() {
        // Every colour is the text's. A fill or stroke written as a hex value
        // would stay black on the dark theme.
        for &i in Icon::ALL {
            let svg = icon(i);
            for attr in ["fill=\"", "stroke=\""] {
                for value in svg.split(attr).skip(1) {
                    assert!(value.starts_with("currentColor\""), "{i:?}: {svg}");
                }
            }
        }
    }

    #[test]
    fn no_two_icons_draw_the_same_thing() {
        for (n, a) in Icon::ALL.iter().enumerate() {
            for b in &Icon::ALL[n + 1..] {
                assert_ne!(icon(*a), icon(*b), "{a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn an_icon_link_is_named_by_its_label_and_shows_the_icon_only() {
        let html = icon_link(
            "btn secondary",
            "/agenda?date=2026-07-01",
            "Mois précédent",
            Icon::Previous,
        );
        assert_eq!(
            html,
            format!(
                r#"<a class="btn secondary" href="/agenda?date=2026-07-01" aria-label="Mois précédent">{}</a>"#,
                icon(Icon::Previous)
            )
        );
    }

    #[test]
    fn an_icon_link_escapes_its_label() {
        let html = icon_link("btn", "/", r#"Tom & "Jerry" <3"#, Icon::Close);
        assert!(
            html.contains(r#"aria-label="Tom &amp; &quot;Jerry&quot; &lt;3""#),
            "{html}"
        );
    }
}
