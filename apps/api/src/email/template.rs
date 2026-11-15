//! The one layout every templated email shares (#831): a text part and an
//! HTML part built from the same [`Template`], so that both say the same
//! thing and carry the same link.
//!
//! The HTML part follows the mock-up validated on 2026-11-13: header with
//! the brand symbol and "MHome", a title, paragraphs, a button-link with the
//! same link spelled out under it, fine print, and a footer linking the
//! privacy policy. Light colours of `DESIGN.md` §Couleur only (mail clients
//! apply a dark theme unpredictably), system fonts, styles inline (clients
//! drop `<style>` blocks), and nothing loaded from anywhere: no remote
//! image, font or stylesheet, which would hand the reader's IP address to
//! whoever serves it. The symbol travels in the message itself, as the
//! inline part [`LOGO_CID`].
//!
//! Every string is escaped here, so a caller hands over plain text. What a
//! person typed (a display name…) still goes through `sanitize_email_line`
//! first, at the caller: escaping keeps it from becoming markup, not from
//! breaking the line or reordering it with bidi controls.

/// `Content-ID` of the brand symbol, the PNG the HTML part shows as
/// `cid:` + this. Sent as an inline part of the message: a remote image
/// would tell its host when and where the email was opened.
pub const LOGO_CID: &str = "logo@mhome";

/// The brand symbol (`apps/web/src/brand/icon-192.png`, drawn from
/// `favicon.svg` by `apps/web/src/brand/tools`), shown at 26 px. An SVG
/// inline in the HTML is dropped or mangled by most mail clients.
pub const LOGO_PNG: &[u8] = include_bytes!("../../../web/src/brand/icon-192.png");

/// What one email says, as plain text. Rendered by [`render`].
pub struct Template<'a> {
    pub title: &'a str,
    pub paragraphs: &'a [&'a str],
    /// The button's label and the link it goes to.
    pub action: (&'a str, &'a str),
    /// Smaller, muted lines after the button: validity, "not you?".
    pub fine_print: &'a [&'a str],
    pub privacy_policy_url: &'a str,
}

/// The two parts of an email: `text/plain` and `text/html`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailContent {
    pub text: String,
    pub html: String,
}

/// `s` with the five characters that mean something in HTML, in text and in
/// a double- or single-quoted attribute alike, replaced by their entities.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// "1 heure", "24 heures".
pub fn hours(n: i64) -> String {
    if n == 1 {
        "1 heure".to_owned()
    } else {
        format!("{n} heures")
    }
}

// DESIGN.md §Couleur, light theme: --bg, --surface, --fg, --muted, --border,
// --line (the separators inside the card) and --accent.
const BG: &str = "#FBFAF7";
const SURFACE: &str = "#FFFFFF";
const FG: &str = "#1C1A17";
const MUTED: &str = "#6B655C";
const BORDER: &str = "#E3DFD7";
const LINE: &str = "#EFEBE4";
const ACCENT: &str = "#14706E";
// Fraunces and Source Sans 3 would have to be downloaded: the headings fall
// back on Georgia, the text on the reader's system font.
const HEAD_FONT: &str = "Georgia,'Times New Roman',serif";
const BODY_FONT: &str =
    "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
// Long names and links wrap instead of widening the card.
const WRAP: &str = "overflow-wrap:anywhere;word-break:break-word;";

/// Both parts of `t`.
pub fn render(t: &Template) -> EmailContent {
    EmailContent {
        text: render_text(t),
        html: render_html(t),
    }
}

fn render_text(t: &Template) -> String {
    let (label, link) = t.action;
    let mut out = format!("{}\n\n", t.title);
    for p in t.paragraphs {
        out.push_str(&format!("{p}\n\n"));
    }
    out.push_str(&format!("{label} :\n{link}\n\n"));
    for p in t.fine_print {
        out.push_str(&format!("{p}\n\n"));
    }
    out.push_str(&format!(
        "--\nManage Our Home\nPolitique de confidentialité : {}\n",
        t.privacy_policy_url
    ));
    out
}

fn render_html(t: &Template) -> String {
    let title = escape_html(t.title);
    let label = escape_html(t.action.0);
    let link = escape_html(t.action.1);
    let privacy = escape_html(t.privacy_policy_url);

    let mut body = format!(
        "<h1 style=\"margin:0 0 14px;font-family:{HEAD_FONT};font-weight:600;font-size:23px;\
         line-height:1.25;color:{FG};text-wrap:balance;{WRAP}\">{title}</h1>"
    );
    for p in t.paragraphs {
        body.push_str(&format!(
            "<p style=\"margin:0 0 14px;font-size:16px;line-height:1.55;{WRAP}\">{}</p>",
            escape_html(p)
        ));
    }
    // The button is a table cell, the one background every client paints;
    // the link is spelled out under it for the clients that hide buttons.
    body.push_str(&format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         style=\"margin:6px 0 12px;border-collapse:separate;\"><tr>\
         <td style=\"background:{ACCENT};border-radius:12px;\">\
         <a href=\"{link}\" style=\"display:inline-block;padding:12px 22px;\
         font-family:{BODY_FONT};font-size:16px;font-weight:600;line-height:1.5;\
         color:{SURFACE};text-decoration:none;border-radius:12px;\">{label}</a>\
         </td></tr></table>\
         <p style=\"margin:0 0 14px;font-size:13px;line-height:1.5;word-break:break-all;\">\
         <a href=\"{link}\" style=\"color:{ACCENT};\">{link}</a></p>"
    ));
    for p in t.fine_print {
        body.push_str(&format!(
            "<p style=\"margin:0 0 14px;font-size:14px;line-height:1.55;color:{MUTED};{WRAP}\">\
             {}</p>",
            escape_html(p)
        ));
    }

    format!(
        "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light\">\
         <meta name=\"supported-color-schemes\" content=\"light\">\
         <title>{title}</title></head>\
         <body style=\"margin:0;padding:0;background:{BG};\">\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" \
         border=\"0\" style=\"background:{BG};\"><tr>\
         <td align=\"center\" style=\"padding:28px 16px;\">\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" \
         border=\"0\" style=\"max-width:560px;background:{SURFACE};border:1px solid {BORDER};\
         border-radius:16px;border-collapse:separate;\">\
         <tr><td style=\"padding:18px 28px;border-bottom:1px solid {LINE};\
         font-family:{HEAD_FONT};font-weight:600;font-size:17px;line-height:26px;color:{FG};\">\
         <img src=\"cid:{LOGO_CID}\" width=\"26\" height=\"26\" alt=\"\" \
         style=\"display:inline-block;vertical-align:middle;border:0;margin-right:10px;\">\
         <span style=\"vertical-align:middle;\">MHome</span></td></tr>\
         <tr><td style=\"padding:28px;font-family:{BODY_FONT};color:{FG};text-align:left;\">\
         {body}</td></tr>\
         <tr><td style=\"padding:18px 28px 22px;border-top:1px solid {LINE};\
         font-family:{BODY_FONT};font-size:13px;line-height:1.5;color:{MUTED};\
         text-align:left;\">\
         MHome — gestion du foyer en famille.<br>\
         Cet e-mail a été envoyé automatiquement, merci de ne pas y répondre.<br>\
         <a href=\"{privacy}\" style=\"color:{MUTED};text-decoration:underline;\">\
         Politique de confidentialité</a></td></tr>\
         </table></td></tr></table></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "https://maison.example/verify-email?token=abc_-123";
    const PRIVACY: &str = "https://maison.example/privacy-policy";

    fn sample<'a>(title: &'a str, paragraphs: &'a [&'a str]) -> Template<'a> {
        Template {
            title,
            paragraphs,
            action: ("Vérifier mon adresse", LINK),
            fine_print: &["Ce lien est valable 24 heures."],
            privacy_policy_url: PRIVACY,
        }
    }

    #[test]
    fn escape_html_replaces_the_characters_markup_reads() {
        assert_eq!(
            escape_html(r#"<script>"a" & 'b'</script>"#),
            "&lt;script&gt;&quot;a&quot; &amp; &#39;b&#39;&lt;/script&gt;"
        );
        assert_eq!(escape_html("Élodie"), "Élodie");
    }

    #[test]
    fn hours_agrees_in_number() {
        assert_eq!(hours(1), "1 heure");
        assert_eq!(hours(24), "24 heures");
    }

    #[test]
    fn text_part_reads_title_paragraphs_link_fine_print_then_signature() {
        let c = render(&sample("Bienvenue !", &["Premier.", "Second."]));
        assert_eq!(
            c.text,
            "Bienvenue !\n\
             \n\
             Premier.\n\
             \n\
             Second.\n\
             \n\
             Vérifier mon adresse :\n\
             https://maison.example/verify-email?token=abc_-123\n\
             \n\
             Ce lien est valable 24 heures.\n\
             \n\
             --\n\
             Manage Our Home\n\
             Politique de confidentialité : https://maison.example/privacy-policy\n"
        );
    }

    #[test]
    fn html_part_has_the_layout_of_the_mock_up() {
        let c = render(&sample("Bienvenue !", &["Premier.", "Second."]));
        let h = &c.html;
        assert!(h.starts_with("<!doctype html>"), "{h}");
        assert!(h.contains("<html lang=\"fr\">"), "{h}");
        assert!(h.contains(&format!("src=\"cid:{LOGO_CID}\"")), "{h}");
        assert!(h.contains(">MHome<"), "{h}");
        assert!(h.contains(">Bienvenue !</h1>"), "{h}");
        assert!(h.contains(">Premier.</p>"), "{h}");
        assert!(h.contains(">Second.</p>"), "{h}");
        assert!(h.contains(">Vérifier mon adresse</a>"), "{h}");
        assert!(h.contains(">Ce lien est valable 24 heures.</p>"), "{h}");
        assert!(h.contains(&format!("href=\"{PRIVACY}\"")), "{h}");
        assert!(h.contains(">Politique de confidentialité</a>"), "{h}");
        // In order: title, paragraphs, button, fine print, footer.
        let at = |needle: &str| h.find(needle).unwrap_or_else(|| panic!("{needle}"));
        assert!(at("Bienvenue") < at("Premier."));
        assert!(at("Premier.") < at("Second."));
        assert!(at("Second.") < at("Vérifier mon adresse"));
        assert!(at("Vérifier mon adresse") < at("Ce lien est valable"));
        assert!(at("Ce lien est valable") < at("Politique de confidentialité"));
        // Light theme only, colours of DESIGN.md §Couleur.
        for colour in [
            "#FBFAF7", "#FFFFFF", "#1C1A17", "#6B655C", "#E3DFD7", "#14706E",
        ] {
            assert!(h.contains(colour), "{colour}");
        }
    }

    #[test]
    fn the_link_is_the_same_in_both_parts_and_spelled_out_under_the_button() {
        let c = render(&sample("T", &[]));
        assert!(c.text.contains(&format!("\n{LINK}\n")), "{}", c.text);
        assert!(c.html.contains(&format!("href=\"{LINK}\"")), "{}", c.html);
        assert!(c.html.contains(&format!(">{LINK}</")), "{}", c.html);
    }

    #[test]
    fn html_part_escapes_every_string_and_text_part_keeps_it_as_given() {
        let typed = r#"<script>alert("x")</script> & Cie"#;
        let c = render(&Template {
            title: typed,
            paragraphs: &[typed],
            action: (typed, "https://maison.example/a?b=1&c=\"2\""),
            fine_print: &[typed],
            privacy_policy_url: PRIVACY,
        });
        assert!(!c.html.contains("<script"), "{}", c.html);
        assert!(!c.html.contains("\"x\""), "{}", c.html);
        assert_eq!(
            c.html
                .matches("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; Cie")
                .count(),
            5, // <title>, <h1>, paragraph, button, fine print
            "{}",
            c.html
        );
        assert!(
            c.html
                .contains("href=\"https://maison.example/a?b=1&amp;c=&quot;2&quot;\""),
            "{}",
            c.html
        );
        assert_eq!(c.text.matches(typed).count(), 4, "{}", c.text);
    }

    #[test]
    fn html_part_loads_nothing_and_runs_nothing() {
        let h = render(&sample("T", &["P"])).html.to_ascii_lowercase();
        for banned in [
            "<script",
            "<link",
            "<style",
            "url(",
            "@import",
            "<iframe",
            "<object",
            "<form",
            "javascript:",
        ] {
            assert!(!h.contains(banned), "{banned}: {h}");
        }
        // No event handler attribute (` onclick=`…).
        for after in h.split(" on").skip(1) {
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            assert!(!after[name.len()..].starts_with('='), "on{name}=: {h}");
        }
        // The only image is the inline symbol.
        assert_eq!(h.matches("<img").count(), 1, "{h}");
        assert_eq!(h.matches("src=").count(), 1, "{h}");
        assert!(h.contains("src=\"cid:"), "{h}");
    }
}
