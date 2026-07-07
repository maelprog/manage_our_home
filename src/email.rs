use anyhow::Result;
use lettre::message::Mailbox;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

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
