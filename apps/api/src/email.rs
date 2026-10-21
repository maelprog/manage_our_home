use std::sync::{Arc, Mutex};

use anyhow::Result;
use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use manage_our_home_shared::validation::rgpd::sanitize_email_line;

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

/// An email as [`EmailSender::send`] was asked to send it, before any
/// transfer encoding: what an [`Outbox`] keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentEmail {
    pub to: String,
    pub subject: String,
    pub body: String,
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

    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<()> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(to.parse()?)
            .subject(subject)
            .body(body.clone())?;
        // Never log recipient/body content — PII (AC #3 analog for email).
        match &self.transport {
            Transport::Smtp(transport) => {
                transport.send(message).await?;
            }
            Transport::Memory(outbox) => outbox.push(SentEmail {
                to: to.to_owned(),
                subject: subject.to_owned(),
                body,
            }),
        }
        Ok(())
    }

    pub fn verification_email_body(link: &str) -> String {
        format!("Cliquez sur ce lien pour vérifier votre email (valide 24h) : {link}")
    }

    pub fn password_reset_body(link: &str) -> String {
        format!("Cliquez sur ce lien pour réinitialiser votre mot de passe (valide 1 h) : {link}")
    }

    /// Sent when somebody registers an address that already belongs to a
    /// verified account (#460): the registration answers as for a new
    /// address, and only the holder of the mailbox learns that the account
    /// exists. The display name was typed by a person, so it goes through
    /// `sanitize_email_line` like every name an email carries: it cannot
    /// open a paragraph of its own. The link is alone on its line.
    pub fn account_exists_body(display_name: &str, forgot_password_link: &str) -> String {
        let display_name = sanitize_email_line(display_name);
        format!(
            "Bonjour {display_name},

Quelqu'un, peut-être vous, a demandé à créer un compte Manage Our Home
avec cette adresse email. Vous en avez déjà un : aucun nouveau compte n'a
été créé et le vôtre n'a pas changé.

Si vous avez oublié votre mot de passe, vous pouvez en choisir un nouveau :
{forgot_password_link}

Si vous n'êtes pas à l'origine de cette demande, vous pouvez ignorer cet
email."
        )
    }

    /// Unlike the three above, this one reaches somebody who has no account and
    /// never asked for anything: art. 14 RGPD, and the notice it requires,
    /// live in `manage_our_home_shared::validation::rgpd` next to the RGPD
    /// documents' placeholders, which the body reuses (#134).
    pub fn invitation_body(
        link: &str,
        group_name: &str,
        inviter_display_name: &str,
        privacy_policy_url: &str,
    ) -> String {
        manage_our_home_shared::validation::rgpd::invitation_email_body(
            group_name,
            inviter_display_name,
            link,
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

    #[tokio::test]
    async fn in_memory_keeps_each_email_as_given_in_order() {
        let (email, outbox) = sender();
        email
            .send("a@example.test", "Premier", "lien : https://x/é?t=1".into())
            .await
            .unwrap();
        email
            .send("b@example.test", "Second", "corps".into())
            .await
            .unwrap();
        assert_eq!(
            outbox.emails(),
            vec![
                SentEmail {
                    to: "a@example.test".into(),
                    subject: "Premier".into(),
                    body: "lien : https://x/é?t=1".into(),
                },
                SentEmail {
                    to: "b@example.test".into(),
                    subject: "Second".into(),
                    body: "corps".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn in_memory_refuses_what_smtp_would_refuse_and_keeps_nothing() {
        let (email, outbox) = sender();
        assert!(email
            .send("not an address", "Sujet", "corps".into())
            .await
            .is_err());
        assert!(outbox.emails().is_empty());
    }

    #[tokio::test]
    async fn clones_share_one_outbox() {
        let (email, outbox) = sender();
        email
            .clone()
            .send("a@example.test", "Sujet", "corps".into())
            .await
            .unwrap();
        assert_eq!(outbox.clone().emails().len(), 1);
    }

    #[test]
    fn account_exists_body_greets_the_holder_and_links_the_forgotten_password_page() {
        let body =
            EmailSender::account_exists_body("Alice", "https://maison.example/forgot-password");
        assert!(body.starts_with("Bonjour Alice,\n"), "{body}");
        assert!(
            body.contains("\nhttps://maison.example/forgot-password\n"),
            "the link sits alone on its line: {body}"
        );
    }

    #[test]
    fn account_exists_body_flattens_the_display_name() {
        let body = EmailSender::account_exists_body(
            "Alice\n\nCliquez ici : https://evil.example\u{202E}",
            "https://maison.example/forgot-password",
        );
        let first = body.lines().next().unwrap();
        assert_eq!(first, "Bonjour Alice Cliquez ici : https://evil.example,");
        assert!(!body.contains('\u{202E}'));
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
