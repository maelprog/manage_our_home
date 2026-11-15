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

pub use manage_our_home_shared::validation::rgpd::{EmailCopy, EmailNotice, EmailNoticeSection};

/// What one email says, as plain text. Rendered by [`render`].
pub struct Template<'a> {
    pub title: &'a str,
    pub paragraphs: &'a [&'a str],
    /// The button's label and the link it goes to.
    pub action: (&'a str, &'a str),
    /// Smaller, muted lines after the button: validity, "not you?".
    pub fine_print: &'a [&'a str],
    /// A boxed passage after the fine print: the information an email owes
    /// its reader (art. 14 RGPD for the invitation, #832).
    pub notice: Option<&'a EmailNotice>,
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

/// Both parts of what `manage_our_home_shared` wrote (the emails whose text
/// it holds next to the RGPD documents, #832).
pub fn render_copy(copy: &EmailCopy, privacy_policy_url: &str) -> EmailContent {
    let paragraphs: Vec<&str> = copy.paragraphs.iter().map(String::as_str).collect();
    let fine_print: Vec<&str> = copy.fine_print.iter().map(String::as_str).collect();
    render(&Template {
        title: &copy.title,
        paragraphs: &paragraphs,
        action: (&copy.action.0, &copy.action.1),
        fine_print: &fine_print,
        notice: copy.notice.as_ref(),
        privacy_policy_url,
    })
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
    if let Some(notice) = t.notice {
        out.push_str(&format!("-- {} --\n\n", notice.heading));
        for section in &notice.sections {
            if let Some(heading) = &section.heading {
                out.push_str(&format!("-- {heading} --\n\n"));
            }
            for p in &section.paragraphs {
                out.push_str(&format!("{p}\n\n"));
            }
        }
    }
    // "-- " with its space: the signature separator of RFC 3676 §4.3.
    out.push_str(&format!(
        "-- \nManage Our Home\nPolitique de confidentialité : {}\n",
        t.privacy_policy_url
    ));
    out
}

/// `s` escaped, its hard wraps (written for the text part) joined into one
/// line.
fn html_line(s: &str) -> String {
    escape_html(&s.replace('\n', " "))
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
            html_line(p)
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
            html_line(p)
        ));
    }
    if let Some(notice) = t.notice {
        body.push_str(&render_notice_html(notice));
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

/// The `.rgpd` box of the mock-up: a bordered cell (a `<div>` border is
/// dropped by some clients, a table cell's is not), muted 14 px text, the
/// heading and sub-headings in bold in the text colour.
fn render_notice_html(notice: &EmailNotice) -> String {
    let strong = format!("font-weight:600;color:{FG};");
    let mut inner = format!(
        "<p style=\"margin:0 0 10px;{strong}\">{}</p>",
        html_line(&notice.heading)
    );
    for section in &notice.sections {
        if let Some(heading) = &section.heading {
            inner.push_str(&format!(
                "<p style=\"margin:14px 0 6px;{strong}\">{}</p>",
                html_line(heading)
            ));
        }
        for p in &section.paragraphs {
            inner.push_str(&format!(
                "<p style=\"margin:0 0 10px;\">{}</p>",
                html_line(p)
            ));
        }
    }
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" \
         border=\"0\" style=\"margin:6px 0 16px;border-collapse:separate;\"><tr>\
         <td style=\"border:1px solid {BORDER};border-radius:12px;padding:12px 14px;\
         font-size:14px;line-height:1.55;color:{MUTED};text-align:left;{WRAP}\">\
         {inner}</td></tr></table>"
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
            notice: None,
            privacy_policy_url: PRIVACY,
        }
    }

    fn notice() -> EmailNotice {
        EmailNotice {
            heading: "Vos données (art. 14 RGPD)".into(),
            sections: vec![
                EmailNoticeSection {
                    heading: None,
                    paragraphs: vec!["Sans titre,\nsur deux lignes.".into()],
                },
                EmailNoticeSection {
                    heading: Some("Ce que vous pouvez faire".into()),
                    paragraphs: vec!["Premier <b>.".into(), "Second.".into()],
                },
            ],
        }
    }

    /// RFC 3676 §4.3: the signature separator is "-- " with its trailing
    /// space, alone on its line, so that clients can fold or strip what
    /// follows.
    #[test]
    fn text_part_signature_separator_keeps_its_trailing_space() {
        let c = render(&sample("T", &[]));
        assert!(
            c.text.contains("\n\n-- \nManage Our Home\n"),
            "{:?}",
            c.text
        );
    }

    #[test]
    fn text_part_shows_the_notice_after_the_fine_print_as_written() {
        let n = notice();
        let c = render(&Template {
            notice: Some(&n),
            ..sample("T", &["P."])
        });
        assert_eq!(
            c.text,
            "T\n\
             \n\
             P.\n\
             \n\
             Vérifier mon adresse :\n\
             https://maison.example/verify-email?token=abc_-123\n\
             \n\
             Ce lien est valable 24 heures.\n\
             \n\
             -- Vos données (art. 14 RGPD) --\n\
             \n\
             Sans titre,\n\
             sur deux lignes.\n\
             \n\
             -- Ce que vous pouvez faire --\n\
             \n\
             Premier <b>.\n\
             \n\
             Second.\n\
             \n\
             -- \n\
             Manage Our Home\n\
             Politique de confidentialité : https://maison.example/privacy-policy\n"
        );
    }

    /// The box of the mock-up (`.rgpd`): bordered, muted, its heading and
    /// sub-headings in bold in the text colour. The hard wraps of the text
    /// part are joined into one line of HTML.
    #[test]
    fn html_part_boxes_the_notice_after_the_fine_print() {
        let n = notice();
        let h = render(&Template {
            notice: Some(&n),
            ..sample("T", &["P."])
        })
        .html;
        assert!(h.contains(">Vos données (art. 14 RGPD)</"), "{h}");
        assert!(h.contains(">Ce que vous pouvez faire</"), "{h}");
        assert!(h.contains(">Sans titre, sur deux lignes.</p>"), "{h}");
        assert!(h.contains(">Premier &lt;b&gt;.</p>"), "{h}");
        assert!(!h.contains("<b>."), "{h}");
        assert!(
            h.contains(&format!("border:1px solid {BORDER};border-radius:12px")),
            "{h}"
        );
        let at = |needle: &str| h.find(needle).unwrap_or_else(|| panic!("{needle}"));
        assert!(at("Ce lien est valable") < at("Vos données"));
        assert!(at("Vos données") < at("Sans titre"));
        assert!(at("Sans titre") < at("Ce que vous pouvez faire"));
        assert!(at("Second.") < at("Politique de confidentialité</a>"));
        // No notice, no box.
        let plain = render(&sample("T", &["P."])).html;
        assert!(
            !plain.contains("border-radius:12px;padding:12px 14px"),
            "{plain}"
        );
    }

    /// A paragraph or fine print written with hard wraps reads as one line
    /// of HTML too.
    #[test]
    fn html_part_joins_the_lines_of_a_paragraph() {
        let h = render(&Template {
            fine_print: &["Une\nseule ligne."],
            ..sample("T", &["Deux\nlignes."])
        })
        .html;
        assert!(h.contains(">Deux lignes.</p>"), "{h}");
        assert!(h.contains(">Une seule ligne.</p>"), "{h}");
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
             -- \n\
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
            notice: None,
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
