use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

use crate::config::EmailConfig;

pub type Mailer = AsyncSmtpTransport<Tokio1Executor>;

pub fn build_mailer(cfg: &EmailConfig) -> anyhow::Result<Mailer> {
    let creds = Credentials::new(cfg.smtp_username.clone(), cfg.smtp_password.clone());
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.smtp_host)?
        .port(cfg.smtp_port)
        .credentials(creds)
        .build();
    Ok(mailer)
}

/// Emaili gönderir. Hata olursa loglar, panic etmez.
pub async fn send(mailer: &Mailer, cfg: &EmailConfig, to: &str, subject: &str, html: String) {
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

    let msg = match Message::builder()
        .from(from)
        .to(to_box)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(html)
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

// ─── Şablonlar ────────────────────────────────────────────────────────────────

pub fn tpl_payment_success(
    plan: &str,
    expires_at: &str,
    is_first: bool,
    site_url: &str,
) -> (&'static str, String) {
    let plan_label = plan_display(plan);
    let (subject, heading, body) = if is_first {
        (
            "Aboneliğiniz aktif edildi ✓",
            format!("{} planı aktif edildi", plan_label),
            format!(
                "Ödemeniz başarıyla alındı. <strong>{}</strong> planı <strong>{}</strong> tarihine kadar geçerlidir.",
                plan_label, expires_at
            ),
        )
    } else {
        (
            "Aboneliğiniz yenilendi ✓",
            format!("{} planı yenilendi", plan_label),
            format!(
                "Aboneliğiniz otomatik olarak yenilendi. Yeni bitiş tarihi: <strong>{}</strong>",
                expires_at
            ),
        )
    };

    let html = base_template(
        &heading,
        &body,
        Some(("Planlara Git", &format!("{}/tr/app/plans", site_url))),
    );
    (subject, html)
}

pub fn tpl_payment_failed(
    plan: &str,
    reason: Option<&str>,
    attempts_left: i32,
    site_url: &str,
) -> (&'static str, String) {
    let plan_label = plan_display(plan);
    let reason_text = reason.unwrap_or("Banka tarafından reddedildi");
    let body = if attempts_left > 0 {
        format!(
            "{}. planına ait ödemeniz başarısız oldu.<br><br>Sebep: <em>{}</em><br><br>Ödemeniz <strong>{} kez</strong> daha denlenecek. Kart bilgilerinizi güncellemek için planlar sayfasını ziyaret edebilirsiniz.",
            plan_label, reason_text, attempts_left
        )
    } else {
        format!(
            "{}. planına ait ödemeniz <strong>birden fazla kez</strong> başarısız oldu.<br><br>Sebep: <em>{}</em><br><br>Aboneliğiniz yenilenemedi. Tekrar abone olmak için planlar sayfasını ziyaret edin.",
            plan_label, reason_text
        )
    };

    let html = base_template(
        "Ödeme başarısız",
        &body,
        Some(("Ödeme Yöntemini Güncelle", &format!("{}/tr/app/plans", site_url))),
    );
    ("Ödemeniz başarısız oldu", html)
}

pub fn tpl_cvv_required(plan: &str, site_url: &str) -> (&'static str, String) {
    let plan_label = plan_display(plan);
    let body = format!(
        "<strong>{}</strong> planı için aboneliğiniz yenileme zamanı geldi, ancak kayıtlı kartınız güvenlik nedeniyle otomatik ödeme için uygun değil (CVV doğrulaması gerekiyor).<br><br>Aboneliğinizin devam etmesi için lütfen planlar sayfasından manuel ödeme yapın.",
        plan_label
    );
    let html = base_template(
        "Abonelik yenileme için işlem gerekiyor",
        &body,
        Some(("Planlar Sayfasına Git", &format!("{}/tr/app/plans", site_url))),
    );
    ("Abonelik yenileme için işlem gerekiyor", html)
}

pub fn tpl_subscription_cancelled(
    plan: &str,
    expires_at: &str,
    site_url: &str,
) -> (&'static str, String) {
    let plan_label = plan_display(plan);
    let body = format!(
        "<strong>{}</strong> planı aboneliğiniz iptal edildi.<br><br>Aboneliğiniz <strong>{}</strong> tarihine kadar aktif kalmaya devam edecek, bu tarihten sonra standart plana geçiş yapılacaktır.",
        plan_label, expires_at
    );
    let html = base_template(
        "Aboneliğiniz iptal edildi",
        &body,
        Some(("Planlar Sayfasına Git", &format!("{}/tr/app/plans", site_url))),
    );
    ("Aboneliğiniz iptal edildi", html)
}

fn plan_display(plan: &str) -> &str {
    match plan {
        "gold"   => "Gold",
        "silver" => "Silver",
        other    => other,
    }
}

fn base_template(heading: &str, body: &str, cta: Option<(&str, &str)>) -> String {
    let cta_html = if let Some((label, url)) = cta {
        format!(
            r#"<p style="text-align:center;margin-top:28px">
               <a href="{}" style="background:#7c3aed;color:#fff;padding:12px 28px;border-radius:8px;text-decoration:none;font-weight:600;display:inline-block">{}</a>
               </p>"#,
            url, label
        )
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="tr">
<head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body style="margin:0;padding:0;background:#0f0f0f;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif">
  <table width="100%" cellpadding="0" cellspacing="0" style="padding:40px 20px">
    <tr><td align="center">
      <table width="560" cellpadding="0" cellspacing="0" style="background:#1a1a1a;border-radius:12px;border:1px solid #2a2a2a;overflow:hidden">
        <tr>
          <td style="background:#7c3aed;padding:24px 32px">
            <p style="margin:0;font-size:22px;font-weight:700;color:#fff">nlink.tr</p>
          </td>
        </tr>
        <tr>
          <td style="padding:32px">
            <h1 style="margin:0 0 16px;font-size:20px;font-weight:700;color:#f0f0f0">{heading}</h1>
            <p style="margin:0;font-size:15px;line-height:1.6;color:#a0a0a0">{body}</p>
            {cta}
            <hr style="border:none;border-top:1px solid #2a2a2a;margin:28px 0">
            <p style="margin:0;font-size:12px;color:#555">Bu e-postayı nlink.tr üzerindeki hesabınız nedeniyle aldınız. Sorularınız için <a href="mailto:destek@nlink.tr" style="color:#7c3aed">destek@nlink.tr</a> adresine yazabilirsiniz.</p>
          </td>
        </tr>
      </table>
    </td></tr>
  </table>
</body>
</html>"#,
        heading = heading,
        body = body,
        cta = cta_html,
    )
}
