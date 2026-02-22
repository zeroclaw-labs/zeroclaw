use crate::config::SmtpConfig;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

/// Async SMTP email service for OTP delivery.
pub struct EmailService {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from_email: String,
    from_name: String,
}

impl EmailService {
    pub fn new(config: &SmtpConfig) -> anyhow::Result<Self> {
        let creds = Credentials::new(config.username.clone(), config.password.clone());

        let transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?
            .port(config.port)
            .credentials(creds)
            .build();

        Ok(Self {
            transport,
            from_email: config.from_email.clone(),
            from_name: config.from_name.clone(),
        })
    }

    /// Send OTP code to the given email address.
    pub async fn send_otp(&self, to_email: &str, code: &str) -> anyhow::Result<()> {
        let from = format!("{} <{}>", self.from_name, self.from_email)
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid from address: {}", e))?;

        let to = to_email
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid to address: {}", e))?;

        let body = format!(
            "Your ZeroClaw verification code is:\n\n  {}\n\nThis code expires in 10 minutes.\nIf you did not request this, please ignore this email.",
            code
        );

        let email = Message::builder()
            .from(from)
            .to(to)
            .subject(format!("ZeroClaw OTP: {}", code))
            .header(ContentType::TEXT_PLAIN)
            .body(body)?;

        self.transport
            .send(email)
            .await
            .map_err(|e| anyhow::anyhow!("SMTP send failed: {}", e))?;

        Ok(())
    }
}
