use anyhow::Result;
use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

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

#[derive(Clone)]
pub struct EmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl EmailSender {
    pub fn new(transport: AsyncSmtpTransport<Tokio1Executor>, from: Mailbox) -> Self {
        Self { transport, from }
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<()> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(to.parse()?)
            .subject(subject)
            .body(body)?;
        // Never log recipient/body content — PII (AC #3 analog for email).
        self.transport.send(message).await?;
        Ok(())
    }

    pub fn verification_email_body(link: &str) -> String {
        format!("Cliquez sur ce lien pour vérifier votre email (valide 24h) : {link}")
    }

    pub fn password_reset_body(link: &str) -> String {
        format!("Cliquez sur ce lien pour réinitialiser votre mot de passe (valide 24h) : {link}")
    }

    pub fn invitation_body(link: &str, group_name: &str) -> String {
        format!("Vous avez été invité(e) à rejoindre le groupe « {group_name} » : {link}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
