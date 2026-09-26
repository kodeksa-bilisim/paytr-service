use lettre::{
    message::{Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

use crate::config::EmailConfig;
use crate::email_templates::EmailContent;

pub type Mailer = AsyncSmtpTransport<Tokio1Executor>;

pub fn build_mailer(cfg: &EmailConfig) -> anyhow::Result<Mailer> {
    let creds = Credentials::new(cfg.smtp_username.clone(), cfg.smtp_password.clone());
    // 465 = doğrudan TLS (SMTPS); diğerleri (Google Workspace: smtp.gmail.com:587) STARTTLS.
    // Eskiden her portta `relay` (doğrudan TLS) kullanılıyordu → 587'de bağlantı kurulamazdı.
    let builder = if cfg.smtp_port == 465 {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.smtp_host)?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_host)?
    };
    Ok(builder.port(cfg.smtp_port).credentials(creds).build())
}

/// Açılışta SMTP bağlantısını ve kimlik doğrulamayı dener (mail göndermez); sonucu loglar.
pub async fn check_connection(mailer: &Mailer) {
    match mailer.test_connection().await {
        Ok(true) => tracing::info!("SMTP bağlantısı ve kimlik doğrulama başarılı"),
        Ok(false) => tracing::error!("SMTP sunucusu bağlantıyı kabul etmedi — email bildirimleri çalışmayacak"),
        Err(e) => tracing::error!("SMTP bağlantı testi başarısız — email bildirimleri çalışmayacak: {}", e),
    }
}

/// E-postayı düz metin + HTML (multipart/alternative) olarak gönderir.
/// Hata olursa loglar, panic etmez; çağıranın akışını etkilemez.
pub async fn send(mailer: &Mailer, cfg: &EmailConfig, to: &str, content: EmailContent) {
    let from: Mailbox = match format!("{} <{}>", cfg.from_name, cfg.from_address).parse() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("Email from adresi geçersiz: {}", e);
            return;
        }
    };
    let to_box: Mailbox = match to.parse() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("Email to adresi geçersiz ({}): {}", to, e);
            return;
        }
    };

    let subject = content.subject.clone();
    let msg = match Message::builder()
        .from(from.clone())
        .reply_to(from)
        .to(to_box)
        .subject(content.subject)
        .multipart(MultiPart::alternative_plain_html(content.text, content.html))
    {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("Email oluşturulamadı: {}", e);
            return;
        }
    };

    if let Err(e) = mailer.send(msg).await {
        tracing::error!("Email gönderilemedi (to={}): {}", to, e);
    } else {
        tracing::info!("Email gönderildi: {} → {}", subject, to);
    }
}
