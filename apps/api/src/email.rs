use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use manage_our_home_shared::validation::rgpd::sanitize_email_line;

pub mod template;

pub use template::EmailContent;
use template::{hours, Template};

/// How `main.rs` should build the SMTP transport, decided from the
/// `SMTP_ALLOW_INSECURE` / `SMTP_PORT` env vars.
#[derive(Debug, PartialEq, Eq)]
pub enum SmtpMode {
    /// TLS relay with credentials — the only mode for real deployments.
    Relay,
    /// Plaintext, unauthenticated SMTP for local mail catchers (Mailpit).
    Insecure { port: u16 },
}

/// Mailpit's default SMTP port, used when `SMTP_PORT` is unset/empty in
/// insecure mode.
const DEFAULT_INSECURE_PORT: u16 = 1025;

/// Insecure mode requires the literal `"true"` (same strictness as
/// `SECURE_COOKIES` in `main.rs`) so a stray value can never silently
/// downgrade a real deployment to plaintext. `port` only applies to
/// insecure mode; docker-compose passes `""` when the var is unset in
/// `.env`, which counts as absent.
pub fn smtp_mode(allow_insecure: Option<&str>, port: Option<&str>) -> Result<SmtpMode> {
    if allow_insecure != Some("true") {
        return Ok(SmtpMode::Relay);
    }
    let port = match port {
        None | Some("") => DEFAULT_INSECURE_PORT,
        Some(p) => p
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid SMTP_PORT '{p}': {e}"))?,
    };
    Ok(SmtpMode::Insecure { port })
}

/// An email as [`EmailSender::send_html`] was asked to send it, before any
/// transfer encoding: what an [`Outbox`] keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentEmail {
    pub to: String,
    pub subject: String,
    /// The `text/plain` part.
    pub body: String,
    /// The `text/html` part. Every email has one since #832.
    pub html: Option<String>,
}

/// The emails an [`EmailSender::in_memory`] sender was given, in order.
/// Lets the flow tests read the link a handler actually mailed (#364)
/// rather than forge a token of their own.
#[derive(Clone, Default)]
pub struct Outbox(Arc<Mutex<Vec<SentEmail>>>);

impl Outbox {
    pub fn emails(&self) -> Vec<SentEmail> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn push(&self, email: SentEmail) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(email);
    }
}

/// The name to greet, as a person typed it: flattened by
/// `sanitize_email_line` like every name an email carries, so that it cannot
/// open a paragraph of its own or reorder the line around it. `None` when
/// nothing is left of it. The HTML part escapes it on top of that.
fn greeted(display_name: &str) -> Option<String> {
    let name = sanitize_email_line(display_name);
    (!name.is_empty()).then_some(name)
}

/// The route that gave an account a new password, which the notice of
/// [`EmailSender::password_changed_email`] names (#833): each closes a
/// different set of sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordChange {
    /// `reset_password`, by a forgotten-password link: every session closed.
    Reset,
    /// `change_password`, from the settings: every session but the caller's.
    Changed,
    /// `set_password`, on an account that had none: no session closed.
    Added,
}

impl PasswordChange {
    /// Names nobody and carries no date: the subject is the line a message
    /// list shows first and a mail provider indexes.
    pub fn subject(self) -> &'static str {
        match self {
            Self::Reset | Self::Changed => "Votre mot de passe a été modifié",
            Self::Added => "Un mot de passe a été ajouté à votre compte",
        }
    }
}

#[derive(Clone)]
enum Transport {
    Smtp(AsyncSmtpTransport<Tokio1Executor>),
    Memory(Outbox),
}

#[derive(Clone)]
pub struct EmailSender {
    transport: Transport,
    from: Mailbox,
}

impl EmailSender {
    pub fn new(transport: AsyncSmtpTransport<Tokio1Executor>, from: Mailbox) -> Self {
        Self {
            transport: Transport::Smtp(transport),
            from,
        }
    }

    /// A sender that keeps every email in the returned [`Outbox`] instead of
    /// handing it to a relay. For tests only: `main.rs` never builds one.
    pub fn in_memory(from: Mailbox) -> (Self, Outbox) {
        let outbox = Outbox::default();
        (
            Self {
                transport: Transport::Memory(outbox.clone()),
                from,
            },
            outbox,
        )
    }

    /// A `multipart/alternative` email: the text part, then the HTML part
    /// with the brand symbol it shows (#831). Every email goes out this way
    /// since #832: none is left as a bare `text/plain` body, which went
    /// without a `Content-Type` and so without its charset.
    pub async fn send_html(&self, to: &str, subject: &str, content: EmailContent) -> Result<()> {
        let message = self.message(to, subject, &content)?;
        self.deliver(
            message,
            SentEmail {
                to: to.to_owned(),
                subject: subject.to_owned(),
                body: content.text,
                html: Some(content.html),
            },
        )
        .await
    }

    async fn deliver(&self, message: Message, kept: SentEmail) -> Result<()> {
        // Never log recipient/body content — PII (AC #3 analog for email).
        match &self.transport {
            Transport::Smtp(transport) => {
                transport.send(message).await?;
            }
            Transport::Memory(outbox) => outbox.push(kept),
        }
        Ok(())
    }

    /// The message as it goes to the relay; the in-memory sender builds it
    /// too, so it refuses what the relay would be refused.
    fn message(&self, to: &str, subject: &str, content: &EmailContent) -> Result<Message> {
        // Text first: a client shows the last alternative it can render.
        // The symbol rides with the HTML part, in a multipart/related the
        // HTML refers to by `cid:`.
        Ok(Message::builder()
            .from(self.from.clone())
            .to(to.parse()?)
            .subject(subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::plain(content.text.clone()))
                    .multipart(
                        MultiPart::related()
                            .singlepart(SinglePart::html(content.html.clone()))
                            .singlepart(
                                Attachment::new_inline(template::LOGO_CID.to_owned()).body(
                                    template::LOGO_PNG.to_vec(),
                                    ContentType::parse("image/png")?,
                                ),
                            ),
                    ),
            )?)
    }

    /// Mailed at registration, and again on each resend, to confirm the
    /// address. It greets nobody: the account's name was typed by whoever
    /// registered the address, before anybody proved to hold it, and the
    /// email must not carry it to the mailbox (arbitrated 2026-11-15).
    pub fn verification_email(link: &str, privacy_policy_url: &str) -> EmailContent {
        let title = "Bienvenue !";
        let validity = format!(
            "Ce lien est valable {} et ne sert qu'une fois.",
            hours(crate::auth::EMAIL_VERIFICATION_TTL_HOURS)
        );
        template::render(&Template {
            title,
            paragraphs: &[
                "Plus qu'une étape : confirmez que cette adresse est bien la vôtre \
                 pour activer votre compte Manage Our Home.",
            ],
            action: ("Vérifier mon adresse", link),
            fine_print: &[
                &validity,
                "Vous n'avez pas créé de compte ? Ignorez ce message.",
            ],
            notice: None,
            privacy_policy_url,
        })
    }

    /// Mailed when a signed-in account that had no password adds one: the
    /// password opens no session until the address is confirmed again. The
    /// name is the account's, shown to its signed-in holder.
    pub fn password_added_email(
        display_name: &str,
        link: &str,
        privacy_policy_url: &str,
    ) -> EmailContent {
        let title = match greeted(display_name) {
            Some(name) => format!("Bonjour {name}, confirmez votre adresse"),
            None => "Confirmez votre adresse".to_owned(),
        };
        let validity = format!(
            "Ce lien est valable {} et ne sert qu'une fois.",
            hours(crate::auth::EMAIL_VERIFICATION_TTL_HOURS)
        );
        template::render(&Template {
            title: &title,
            paragraphs: &[
                "Vous avez ajouté un mot de passe à votre compte Manage Our Home. Confirmez \
                 que cette adresse est bien la vôtre pour pouvoir vous connecter avec ce mot \
                 de passe.",
            ],
            action: ("Vérifier mon adresse", link),
            fine_print: &[
                &validity,
                "Vous n'êtes pas à l'origine de cet ajout ? Ne suivez pas ce lien : sans \
                 confirmation, ce mot de passe ne permet pas de se connecter.",
            ],
            notice: None,
            privacy_policy_url,
        })
    }

    /// Mailed by the forgotten-password form to a live account. An empty
    /// `display_name` greets nobody: the route passes one while the address
    /// is unverified (#831).
    pub fn password_reset_email(
        display_name: &str,
        link: &str,
        privacy_policy_url: &str,
    ) -> EmailContent {
        let title = match greeted(display_name) {
            Some(name) => format!("Bonjour {name}, choisissez un nouveau mot de passe"),
            None => "Choisissez un nouveau mot de passe".to_owned(),
        };
        // `reset_password` revokes every session of the account.
        let validity = format!(
            "Ce lien est valable {} et ne sert qu'une fois. Une fois le mot de passe changé, \
             vos autres sessions sont fermées.",
            hours(crate::auth::PASSWORD_RESET_TTL_HOURS)
        );
        template::render(&Template {
            title: &title,
            paragraphs: &["Vous avez demandé à réinitialiser le mot de passe de votre compte."],
            action: ("Choisir un nouveau mot de passe", link),
            fine_print: &[
                &validity,
                "Vous n'avez rien demandé ? Ignorez ce message : votre mot de passe actuel \
                 reste valable.",
            ],
            notice: None,
            privacy_policy_url,
        })
    }

    /// Sent when somebody registers an address that already belongs to a
    /// verified account (#460): the registration answers as for a new
    /// address, and only the holder of the mailbox learns that the account
    /// exists.
    pub fn account_exists_email(
        display_name: &str,
        forgot_password_link: &str,
        privacy_policy_url: &str,
    ) -> EmailContent {
        let title = match greeted(display_name) {
            Some(name) => format!("Bonjour {name}, vous avez déjà un compte"),
            None => "Vous avez déjà un compte".to_owned(),
        };
        template::render(&Template {
            title: &title,
            paragraphs: &[
                "Quelqu'un, peut-être vous, a demandé à créer un compte Manage Our Home avec \
                 cette adresse. Vous en avez déjà un : aucun nouveau compte n'a été créé et le \
                 vôtre n'a pas changé.",
                "Si vous avez oublié votre mot de passe, vous pouvez en choisir un nouveau.",
            ],
            action: ("Mot de passe oublié", forgot_password_link),
            fine_print: &["Vous n'êtes pas à l'origine de cette demande ? Ignorez ce message."],
            notice: None,
            privacy_policy_url,
        })
    }

    /// Mailed to the account's address once a password route has given it a
    /// new password (#833), so that a password changed by somebody else
    /// does not go unnoticed. Dated in Europe/Paris, the fixed v1 display
    /// timezone; its button leads to the forgotten-password page, whose
    /// reset closes every session. An empty `display_name` greets nobody.
    pub fn password_changed_email(
        display_name: &str,
        change: PasswordChange,
        at: DateTime<Utc>,
        forgot_password_link: &str,
        privacy_policy_url: &str,
    ) -> EmailContent {
        let what = match change {
            PasswordChange::Reset | PasswordChange::Changed => "votre mot de passe a été modifié",
            PasswordChange::Added => "un mot de passe a été ajouté à votre compte",
        };
        let title = match greeted(display_name) {
            Some(name) => format!("Bonjour {name}, {what}"),
            None => {
                let mut chars = what.chars();
                let first = chars.next().map(|c| c.to_uppercase().to_string());
                format!("{}{}", first.unwrap_or_default(), chars.as_str())
            }
        };
        let when = format!(
            "{} (heure de Paris)",
            at.with_timezone(&rrule::Tz::Europe__Paris)
                .format("le %d/%m/%Y à %H:%M")
        );
        let (done, sessions) = match change {
            PasswordChange::Reset => (
                format!(
                    "Le mot de passe de votre compte Manage Our Home a été réinitialisé {when}, \
                     par un lien « mot de passe oublié »."
                ),
                "Toutes les sessions ouvertes sur votre compte ont été fermées : reconnectez-vous \
                 avec le nouveau mot de passe sur chacun de vos appareils.",
            ),
            PasswordChange::Changed => (
                format!(
                    "Le mot de passe de votre compte Manage Our Home a été modifié {when}, \
                     depuis les réglages du compte."
                ),
                "Vos autres sessions ont été fermées : seule celle qui a servi à ce changement \
                 reste ouverte.",
            ),
            PasswordChange::Added => (
                format!(
                    "Un mot de passe a été ajouté à votre compte Manage Our Home {when}, depuis \
                     les réglages du compte."
                ),
                "Il ne permet de se connecter qu'une fois votre adresse confirmée par le lien \
                 envoyé dans un autre message. Vos sessions ouvertes n'ont pas été fermées.",
            ),
        };
        template::render(&Template {
            title: &title,
            paragraphs: &[&done, sessions],
            action: ("Réinitialiser mon mot de passe", forgot_password_link),
            fine_print: &[
                "Ce n'est pas vous ? Réinitialisez votre mot de passe sans attendre avec le lien \
                 ci-dessus : la réinitialisation ferme toutes les sessions ouvertes sur votre \
                 compte.",
                "C'est bien vous ? Vous n'avez rien à faire.",
            ],
            notice: None,
            privacy_policy_url,
        })
    }

    /// Unlike the four above, this one reaches somebody who has no account and
    /// never asked for anything: art. 14 RGPD, and the notice it requires,
    /// live in `manage_our_home_shared::validation::rgpd` next to the RGPD
    /// documents' placeholders, which the body reuses (#134). Laid out by
    /// the shared template since #832, the notice boxed after the button.
    pub fn invitation_email(
        link: &str,
        group_name: &str,
        inviter_display_name: &str,
        privacy_policy_url: &str,
    ) -> EmailContent {
        template::render_copy(
            &manage_our_home_shared::validation::rgpd::invitation_email_body(
                group_name,
                inviter_display_name,
                link,
            ),
            privacy_policy_url,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender() -> (EmailSender, Outbox) {
        EmailSender::in_memory("noreply@example.test".parse().unwrap())
    }

    fn content(text: &str) -> EmailContent {
        EmailContent {
            text: text.into(),
            html: format!("<p>{text}</p>"),
        }
    }

    #[tokio::test]
    async fn in_memory_keeps_each_email_as_given_in_order() {
        let (email, outbox) = sender();
        email
            .send_html(
                "a@example.test",
                "Premier",
                content("lien : https://x/é?t=1"),
            )
            .await
            .unwrap();
        email
            .send_html("b@example.test", "Second", content("corps"))
            .await
            .unwrap();
        assert_eq!(
            outbox.emails(),
            vec![
                SentEmail {
                    to: "a@example.test".into(),
                    subject: "Premier".into(),
                    body: "lien : https://x/é?t=1".into(),
                    html: Some("<p>lien : https://x/é?t=1</p>".into()),
                },
                SentEmail {
                    to: "b@example.test".into(),
                    subject: "Second".into(),
                    body: "corps".into(),
                    html: Some("<p>corps</p>".into()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn in_memory_keeps_both_parts_of_an_html_email() {
        let (email, outbox) = sender();
        let content = EmailContent {
            text: "texte".into(),
            html: "<p>html</p>".into(),
        };
        email
            .send_html("a@example.test", "Sujet", content)
            .await
            .unwrap();
        assert_eq!(
            outbox.emails(),
            vec![SentEmail {
                to: "a@example.test".into(),
                subject: "Sujet".into(),
                body: "texte".into(),
                html: Some("<p>html</p>".into()),
            }]
        );
    }

    /// Every `Content-Type` of the raw message, in order, lowercased.
    fn content_types(raw: &str) -> Vec<String> {
        raw.lines()
            .filter_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-type: ")
                    .map(str::to_owned)
            })
            .collect()
    }

    /// Every email, the four of #832 as the auth ones of #831: their text
    /// parts used to go out bare, with no `Content-Type` and so no charset.
    #[test]
    fn every_email_is_multipart_alternative_text_first_then_html_with_its_inline_symbol() {
        let at = chrono::Utc::now();
        for (subject, content) in [
            (
                "Vérifiez votre email",
                EmailSender::verification_email("https://m.example/v?token=a", PRIVACY),
            ),
            (
                "Invitation à rejoindre un groupe",
                EmailSender::invitation_email("https://m.example/i", "G", "Alice", PRIVACY),
            ),
            (
                "Rappel d'un événement à venir",
                crate::jobs::scheduled_notifications::reminder_email(
                    "Dîner",
                    at,
                    false,
                    "https://m.example/agenda",
                    PRIVACY,
                ),
            ),
            (
                "Votre compte Manage Our Home désactivé va être supprimé",
                template::render_copy(
                    &manage_our_home_shared::validation::rgpd::deactivation_notice_email_body(
                        "Alice",
                        at,
                        at,
                        "https://m.example/login",
                    ),
                    PRIVACY,
                ),
            ),
        ] {
            assert_multipart_with_symbol(subject, &content);
        }
    }

    fn assert_multipart_with_symbol(subject: &str, content: &EmailContent) {
        let (email, _) = sender();
        let message = email.message("a@example.test", subject, content).unwrap();
        let raw = String::from_utf8(message.formatted()).unwrap();
        let types = content_types(&raw);
        let kinds: Vec<&str> = types.iter().map(|t| t.split(';').next().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "multipart/alternative",
                "text/plain",
                "multipart/related",
                "text/html",
                "image/png",
            ],
            "{raw}"
        );
        assert!(types[1].contains("charset=utf-8"), "{raw}");
        assert!(types[3].contains("charset=utf-8"), "{raw}");
        let lower = raw.to_ascii_lowercase();
        assert!(
            lower.contains(&format!("content-id: <{}>", template::LOGO_CID)),
            "{raw}"
        );
        assert!(lower.contains("content-disposition: inline"), "{raw}");
    }

    const PRIVACY: &str = "https://maison.example/privacy-policy";

    /// Acceptance criterion of #832: every paragraph of the art. 14 notice
    /// is in both parts — as written in the text one, its lines joined and
    /// escaped in the HTML one.
    #[test]
    fn invitation_email_carries_the_whole_art_14_notice_in_both_parts() {
        let link = "https://maison.example/groups/invitations/t/accept";
        let copy = manage_our_home_shared::validation::rgpd::invitation_email_body(
            "Famille Dupont",
            "Alice Martin",
            link,
        );
        let c = EmailSender::invitation_email(link, "Famille Dupont", "Alice Martin", PRIVACY);
        let notice = copy.notice.as_ref().unwrap();
        let mut paragraphs = 0;
        for section in &notice.sections {
            for p in &section.paragraphs {
                assert!(c.text.contains(p.as_str()), "{p}\n---\n{}", c.text);
                let html = template::escape_html(&p.replace('\n', " "));
                assert!(c.html.contains(&html), "{html}\n---\n{}", c.html);
                paragraphs += 1;
            }
        }
        assert_eq!(paragraphs, 6);
        for needle in [
            "réclamation auprès de la CNIL",
            "[adresse de contact — à renseigner avant la mise en ligne]",
            "-- Ce que vous pouvez faire --",
        ] {
            assert!(c.text.contains(needle), "{needle}");
        }
        assert!(
            c.html.contains(">Vos données (art. 14 RGPD)</p>"),
            "{}",
            c.html
        );
        assert!(
            c.html.contains(">Ce que vous pouvez faire</p>"),
            "{}",
            c.html
        );
        assert!(c
            .text
            .starts_with("Vous êtes invité(e) à rejoindre « Famille Dupont »\n"));
        assert!(
            c.text.contains(&format!("Rejoindre le groupe :\n{link}\n")),
            "{}",
            c.text
        );
        assert!(c.html.contains(&format!("href=\"{link}\"")), "{}", c.html);
        assert!(c.text.contains(PRIVACY), "{}", c.text);
        assert!(
            c.html.contains(&format!("href=\"{PRIVACY}\"")),
            "{}",
            c.html
        );
    }

    /// The names of the group and of the inviter are flattened in both
    /// parts, then escaped in the HTML one.
    #[test]
    fn invitation_email_flattens_and_escapes_what_members_typed() {
        let c = EmailSender::invitation_email(
            "https://m.example/i",
            "<b>Famille</b>\n\n-- Ce que vous pouvez faire --",
            "Eve & \"Cie\"\u{202E}",
            PRIVACY,
        );
        assert_eq!(
            c.text
                .lines()
                .filter(|l| *l == "-- Ce que vous pouvez faire --")
                .count(),
            1,
            "{}",
            c.text
        );
        assert!(!c.text.contains('\u{202E}'), "{}", c.text);
        assert!(!c.html.contains("<b>"), "{}", c.html);
        assert!(c.html.contains("&lt;b&gt;Famille&lt;/b&gt;"), "{}", c.html);
        assert!(c.html.contains("Eve &amp; &quot;Cie&quot;"), "{}", c.html);
    }

    /// Arbitrated 2026-11-15: the name was typed by whoever registered the
    /// address, before anybody proved to hold it, so the verification email
    /// greets nobody.
    #[test]
    fn verification_email_greets_nobody_recalls_the_request_and_says_how_long_the_link_lasts() {
        let link = "https://maison.example/verify-email?token=abc";
        let c = EmailSender::verification_email(link, PRIVACY);
        assert!(c.text.starts_with("Bienvenue !\n"), "{}", c.text);
        assert!(c.text.contains("confirmez que cette adresse"), "{}", c.text);
        assert!(c.text.contains("valable 24 heures"), "{}", c.text);
        assert!(c.text.contains("ne sert qu'une fois"), "{}", c.text);
        assert!(c.text.contains("Ignorez ce message"), "{}", c.text);
        assert!(c.text.contains(&format!("\n{link}\n")), "{}", c.text);
        assert!(c.text.contains(PRIVACY), "{}", c.text);
        assert!(c.html.contains(&format!("href=\"{link}\"")), "{}", c.html);
        assert!(c.html.contains(">Bienvenue !</h1>"), "{}", c.html);
    }

    /// Arbitrated 2026-11-15: adding a password to an account that already
    /// exists is no account creation — no welcome, no "you did not create
    /// an account?".
    #[test]
    fn password_added_email_confirms_the_address_without_welcoming_anybody() {
        let link = "https://maison.example/verify-email?token=abc";
        let c = EmailSender::password_added_email("Alice", link, PRIVACY);
        assert!(
            c.text
                .starts_with("Bonjour Alice, confirmez votre adresse\n"),
            "{}",
            c.text
        );
        assert!(
            c.text.contains("Vous avez ajouté un mot de passe"),
            "{}",
            c.text
        );
        assert!(c.text.contains("valable 24 heures"), "{}", c.text);
        assert!(c.text.contains(&format!("\n{link}\n")), "{}", c.text);
        assert!(c.html.contains(&format!("href=\"{link}\"")), "{}", c.html);
        for welcome in ["Bienvenue", "activer votre compte", "créé de compte"] {
            assert!(!c.text.contains(welcome), "{welcome}: {}", c.text);
            assert!(!c.html.contains(welcome), "{welcome}: {}", c.html);
        }
        let blank = EmailSender::password_added_email(" \u{202E}", link, PRIVACY);
        assert!(
            blank.text.starts_with("Confirmez votre adresse\n"),
            "{}",
            blank.text
        );
    }

    #[test]
    fn password_reset_email_greets_recalls_the_request_and_says_how_long_the_link_lasts() {
        let link = "https://maison.example/reset-password#token=abc";
        let c = EmailSender::password_reset_email("Alice", link, PRIVACY);
        assert!(
            c.text
                .starts_with("Bonjour Alice, choisissez un nouveau mot de passe\n"),
            "{}",
            c.text
        );
        assert!(
            c.text.contains("Vous avez demandé à réinitialiser"),
            "{}",
            c.text
        );
        assert!(c.text.contains("valable 1 heure "), "{}", c.text);
        assert!(c.text.contains("Ignorez ce message"), "{}", c.text);
        assert!(c.text.contains(&format!("\n{link}\n")), "{}", c.text);
        assert!(c.html.contains(&format!("href=\"{link}\"")), "{}", c.html);
    }

    #[test]
    fn account_exists_email_greets_the_holder_and_links_the_forgotten_password_page() {
        let link = "https://maison.example/forgot-password";
        let c = EmailSender::account_exists_email("Alice", link, PRIVACY);
        assert!(
            c.text
                .starts_with("Bonjour Alice, vous avez déjà un compte\n"),
            "{}",
            c.text
        );
        assert!(c.text.contains("aucun nouveau compte"), "{}", c.text);
        assert!(c.text.contains("Ignorez ce message"), "{}", c.text);
        assert!(
            c.text.contains(&format!("\n{link}\n")),
            "the link sits alone on its line: {}",
            c.text
        );
        assert!(c.html.contains(&format!("href=\"{link}\"")), "{}", c.html);
    }

    /// What a person typed reaches both parts flattened by
    /// `sanitize_email_line`, then escaped in the HTML one only.
    #[test]
    fn auth_emails_flatten_the_display_name_and_escape_it_in_html_only() {
        let typed = "<script>\"x\" & Cie</script>\n\nCliquez ici\u{202E}";
        let flat = "<script>\"x\" & Cie</script> Cliquez ici";
        let escaped = "&lt;script&gt;&quot;x&quot; &amp; Cie&lt;/script&gt; Cliquez ici";
        for c in [
            EmailSender::password_added_email(typed, "https://m.example/v?token=a", PRIVACY),
            EmailSender::password_reset_email(typed, "https://m.example/r#token=a", PRIVACY),
            EmailSender::account_exists_email(typed, "https://m.example/f", PRIVACY),
            EmailSender::password_changed_email(
                typed,
                PasswordChange::Changed,
                november_afternoon(),
                "https://m.example/f",
                PRIVACY,
            ),
        ] {
            let first = c.text.lines().next().unwrap();
            assert!(first.contains(flat), "{first}");
            assert!(!c.text.contains('\u{202E}'), "{}", c.text);
            assert!(!c.html.contains('\u{202E}'), "{}", c.html);
            assert!(c.html.contains(escaped), "{}", c.html);
            assert!(!c.html.contains("<script"), "{}", c.html);
        }
    }

    const FORGOT: &str = "https://maison.example/forgot-password";

    /// 2026-11-16 14:30 UTC: 15:30 in Paris (winter time, UTC+1).
    fn november_afternoon() -> chrono::DateTime<chrono::Utc> {
        "2026-11-16T14:30:00Z".parse().unwrap()
    }

    /// #833: after a reset, the notice dates it, says every session was
    /// closed, and links the forgotten-password page for "not you?".
    #[test]
    fn password_changed_email_after_a_reset_dates_it_and_says_every_session_was_closed() {
        let c = EmailSender::password_changed_email(
            "Alice",
            PasswordChange::Reset,
            november_afternoon(),
            FORGOT,
            PRIVACY,
        );
        assert!(
            c.text
                .starts_with("Bonjour Alice, votre mot de passe a été modifié\n"),
            "{}",
            c.text
        );
        assert!(
            c.text
                .contains("réinitialisé le 16/11/2026 à 15:30 (heure de Paris)"),
            "{}",
            c.text
        );
        assert!(c.text.contains("Toutes les sessions"), "{}", c.text);
        assert!(c.text.contains("Ce n'est pas vous ?"), "{}", c.text);
        assert!(c.text.contains(&format!("\n{FORGOT}\n")), "{}", c.text);
        assert!(c.text.contains(PRIVACY), "{}", c.text);
        assert!(c.html.contains(&format!("href=\"{FORGOT}\"")), "{}", c.html);
        assert!(c.html.contains("15:30 (heure de Paris)"), "{}", c.html);
    }

    /// #833: a change from the settings keeps the session it came from.
    #[test]
    fn password_changed_email_after_a_change_says_the_other_sessions_were_closed() {
        let c = EmailSender::password_changed_email(
            "Alice",
            PasswordChange::Changed,
            november_afternoon(),
            FORGOT,
            PRIVACY,
        );
        assert!(
            c.text
                .starts_with("Bonjour Alice, votre mot de passe a été modifié\n"),
            "{}",
            c.text
        );
        assert!(
            c.text
                .contains("modifié le 16/11/2026 à 15:30 (heure de Paris)"),
            "{}",
            c.text
        );
        assert!(
            c.text.contains("Vos autres sessions ont été fermées"),
            "{}",
            c.text
        );
        assert!(!c
            .text
            .contains("Toutes les sessions ouvertes sur votre compte ont"));
        assert!(c.text.contains("Ce n'est pas vous ?"), "{}", c.text);
        assert!(c.text.contains(&format!("\n{FORGOT}\n")), "{}", c.text);
    }

    /// #833: adding a password closes no session; the notice says so
    /// rather than claim otherwise.
    #[test]
    fn password_changed_email_after_an_addition_claims_no_session_was_closed() {
        let c = EmailSender::password_changed_email(
            "Alice",
            PasswordChange::Added,
            november_afternoon(),
            FORGOT,
            PRIVACY,
        );
        assert!(
            c.text
                .starts_with("Bonjour Alice, un mot de passe a été ajouté à votre compte\n"),
            "{}",
            c.text
        );
        assert!(
            c.text.contains(
                "ajouté à votre compte Manage Our Home le 16/11/2026 à 15:30 (heure de Paris)"
            ),
            "{}",
            c.text
        );
        assert!(!c.text.contains("ont été fermées"), "{}", c.text);
        assert!(c.text.contains("n'ont pas été fermées"), "{}", c.text);
        assert!(c.text.contains("Ce n'est pas vous ?"), "{}", c.text);
        assert!(c.text.contains(&format!("\n{FORGOT}\n")), "{}", c.text);
    }

    /// The hour is Paris's, summer time included.
    #[test]
    fn password_changed_email_reads_the_hour_in_paris_in_summer_too() {
        let july: chrono::DateTime<chrono::Utc> = "2026-07-01T22:15:00Z".parse().unwrap();
        let c =
            EmailSender::password_changed_email("", PasswordChange::Changed, july, FORGOT, PRIVACY);
        assert!(
            c.text.contains("le 02/07/2026 à 00:15 (heure de Paris)"),
            "{}",
            c.text
        );
    }

    /// The subject is what a message list shows first: it names nobody and
    /// carries no date.
    #[test]
    fn password_changed_subjects_carry_no_data() {
        assert_eq!(
            PasswordChange::Reset.subject(),
            "Votre mot de passe a été modifié"
        );
        assert_eq!(
            PasswordChange::Changed.subject(),
            "Votre mot de passe a été modifié"
        );
        assert_eq!(
            PasswordChange::Added.subject(),
            "Un mot de passe a été ajouté à votre compte"
        );
    }

    /// No name to greet (nothing left once flattened): the title reads
    /// without one.
    #[test]
    fn auth_emails_without_a_name_greet_nobody() {
        let p = EmailSender::password_changed_email(
            " \n\u{202E}",
            PasswordChange::Reset,
            november_afternoon(),
            FORGOT,
            PRIVACY,
        );
        assert!(
            p.text.starts_with("Votre mot de passe a été modifié\n"),
            "{}",
            p.text
        );
        let a = EmailSender::password_changed_email(
            "",
            PasswordChange::Added,
            november_afternoon(),
            FORGOT,
            PRIVACY,
        );
        assert!(
            a.text
                .starts_with("Un mot de passe a été ajouté à votre compte\n"),
            "{}",
            a.text
        );
        let blank = " \n\u{202E}";
        let r = EmailSender::password_reset_email(blank, "https://m.example/r", PRIVACY);
        assert!(
            r.text.starts_with("Choisissez un nouveau mot de passe\n"),
            "{}",
            r.text
        );
        let e = EmailSender::account_exists_email(blank, "https://m.example/f", PRIVACY);
        assert!(
            e.text.starts_with("Vous avez déjà un compte\n"),
            "{}",
            e.text
        );
    }

    #[tokio::test]
    async fn in_memory_refuses_what_smtp_would_refuse_and_keeps_nothing() {
        let (email, outbox) = sender();
        assert!(email
            .send_html("not an address", "Sujet", content("corps"))
            .await
            .is_err());
        assert!(outbox.emails().is_empty());
    }

    #[tokio::test]
    async fn clones_share_one_outbox() {
        let (email, outbox) = sender();
        email
            .clone()
            .send_html("a@example.test", "Sujet", content("corps"))
            .await
            .unwrap();
        assert_eq!(outbox.clone().emails().len(), 1);
    }

    #[test]
    fn defaults_to_relay() {
        assert_eq!(smtp_mode(None, None).unwrap(), SmtpMode::Relay);
    }

    #[test]
    fn anything_but_literal_true_is_relay() {
        for v in ["false", "", "TRUE", "1", "yes"] {
            assert_eq!(smtp_mode(Some(v), None).unwrap(), SmtpMode::Relay);
        }
    }

    #[test]
    fn relay_ignores_port() {
        assert_eq!(smtp_mode(None, Some("2525")).unwrap(), SmtpMode::Relay);
        assert_eq!(
            smtp_mode(Some("false"), Some("garbage")).unwrap(),
            SmtpMode::Relay
        );
    }

    #[test]
    fn insecure_defaults_to_mailpit_port() {
        assert_eq!(
            smtp_mode(Some("true"), None).unwrap(),
            SmtpMode::Insecure { port: 1025 }
        );
    }

    #[test]
    fn insecure_treats_empty_port_as_unset() {
        // docker-compose passes "" when SMTP_PORT is absent from .env.
        assert_eq!(
            smtp_mode(Some("true"), Some("")).unwrap(),
            SmtpMode::Insecure { port: 1025 }
        );
    }

    #[test]
    fn insecure_custom_port() {
        assert_eq!(
            smtp_mode(Some("true"), Some("2525")).unwrap(),
            SmtpMode::Insecure { port: 2525 }
        );
    }

    #[test]
    fn insecure_invalid_port_errors() {
        assert!(smtp_mode(Some("true"), Some("not-a-port")).is_err());
        assert!(smtp_mode(Some("true"), Some("70000")).is_err());
    }
}
