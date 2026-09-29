//! nlink.tr e-posta şablonları: ortak, resmi ve e-posta istemcileriyle uyumlu yerleşim.
//!
//! - Tablo tabanlı, satır içi stil (Gmail/Outlook/Apple Mail uyumlu), açık tema.
//! - Marka işareti görsel değil HTML (mavi kare + "n") — görseller engellense de görünür.
//! - Her şablon HTML + düz metin sürümü üretir (multipart/alternative).
//! - Dışarıdan gelen tüm değerler `esc` ile kaçışlanır.

pub struct EmailContent {
    pub subject: String,
    pub html: String,
    pub text: String,
}

const BRAND: &str = "#2563EB";
const FONT: &str = "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// "Sayın Ad Soyad," — ad yoksa "Sayın Kullanıcımız,".
pub fn greeting(name: Option<&str>) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => format!("Sayın {},", esc(n)),
        None => "Sayın Kullanıcımız,".to_string(),
    }
}

/// Şablon içeriği. `paragraphs` ve `note` HTML içerebilir (değişkenler önceden `esc`lenmiş olmalı).
pub struct Layout<'a> {
    pub subject: &'a str,
    pub preheader: &'a str,
    pub eyebrow: &'a str,
    pub heading: &'a str,
    pub greeting: String,
    pub paragraphs: Vec<String>,
    /// Başlık + açıklama listesi (ör. özellikler).
    pub features: Vec<(&'a str, &'a str)>,
    /// Etiket → değer bilgi tablosu (değerler düz metin; burada kaçışlanır).
    pub details: Vec<(&'a str, String)>,
    pub cta: Option<(&'a str, String)>,
    pub note: Option<String>,
    pub site_url: &'a str,
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.replace("<br>", "\n").replace("<br/>", "\n").chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

pub fn render(l: &Layout) -> EmailContent {
    let year = chrono::Utc::now().format("%Y");
    let p_style = format!("margin:0 0 16px;font:400 15px/1.65 {FONT};color:#374151;");

    let paragraphs: String = l
        .paragraphs
        .iter()
        .map(|p| format!(r#"<p style="{p_style}">{p}</p>"#))
        .collect();

    let features = if l.features.is_empty() {
        String::new()
    } else {
        let rows: String = l
            .features
            .iter()
            .map(|(title, desc)| {
                format!(
                    r#"<tr><td valign="top" style="padding:0 12px 14px 0;width:8px;"><div style="width:8px;height:8px;margin-top:7px;border-radius:2px;background:{BRAND};"></div></td>
<td style="padding:0 0 14px;font:400 14px/1.6 {FONT};color:#4b5563;"><strong style="color:#111827;">{}</strong><br>{}</td></tr>"#,
                    esc(title),
                    esc(desc)
                )
            })
            .collect();
        format!(
            r#"<tr><td style="padding:4px 40px 4px;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0">{rows}</table></td></tr>"#
        )
    };

    let details = if l.details.is_empty() {
        String::new()
    } else {
        let last = l.details.len() - 1;
        let rows: String = l
            .details
            .iter()
            .enumerate()
            .map(|(i, (label, value))| {
                let border = if i == last { "" } else { "border-bottom:1px solid #e5e7eb;" };
                format!(
                    r#"<tr><td style="padding:12px 16px;{border}font:400 13px/1.5 {FONT};color:#6b7280;">{}</td>
<td align="right" style="padding:12px 16px;{border}font:600 14px/1.5 {FONT};color:#111827;">{}</td></tr>"#,
                    esc(label),
                    esc(value)
                )
            })
            .collect();
        format!(
            r#"<tr><td style="padding:8px 40px 8px;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="border:1px solid #e5e7eb;border-radius:8px;background:#f9fafb;border-collapse:separate;">{rows}</table></td></tr>"#
        )
    };

    let cta = match &l.cta {
        None => String::new(),
        Some((label, url)) => format!(
            r#"<tr><td style="padding:24px 40px 4px;">
<table role="presentation" cellpadding="0" cellspacing="0" border="0"><tr><td bgcolor="{BRAND}" style="border-radius:8px;background:{BRAND};">
<a href="{url}" target="_blank" style="display:inline-block;padding:13px 28px;font:600 15px/1.2 {FONT};color:#ffffff;text-decoration:none;border-radius:8px;">{label}</a>
</td></tr></table>
<p style="margin:16px 0 0;font:400 12px/1.6 {FONT};color:#6b7280;">Buton çalışmıyorsa aşağıdaki bağlantıyı tarayıcınızın adres çubuğuna yapıştırabilirsiniz:<br>
<a href="{url}" target="_blank" style="color:{BRAND};word-break:break-all;">{url}</a></p>
</td></tr>"#,
            url = esc(url),
            label = esc(label)
        ),
    };

    let note = match &l.note {
        None => String::new(),
        Some(n) => format!(
            r#"<tr><td style="padding:20px 40px 0;"><p style="margin:0;padding:14px 16px;background:#f9fafb;border-left:3px solid #d1d5db;font:400 13px/1.6 {FONT};color:#4b5563;">{n}</p></td></tr>"#
        ),
    };

    let site = esc(l.site_url);
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="tr">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light">
<meta name="supported-color-schemes" content="light">
<title>{subject}</title>
</head>
<body style="margin:0;padding:0;background:#f3f4f6;-webkit-text-size-adjust:100%;">
<div style="display:none;max-height:0;overflow:hidden;opacity:0;color:transparent;">{preheader}</div>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background:#f3f4f6;">
<tr><td align="center" style="padding:32px 16px;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="max-width:560px;">
<tr><td style="padding:0 4px 20px;">
<table role="presentation" cellpadding="0" cellspacing="0" border="0"><tr>
<td width="32" height="32" align="center" valign="middle" bgcolor="{BRAND}" style="width:32px;height:32px;background:{BRAND};border-radius:7px;color:#ffffff;font:700 18px/32px Arial,Helvetica,sans-serif;">n</td>
<td style="padding-left:10px;font:700 18px/1 {FONT};color:#111827;">nlink<span style="color:#6b7280;font-weight:400;">.tr</span></td>
</tr></table>
</td></tr>
<tr><td style="background:#ffffff;border:1px solid #e5e7eb;border-radius:12px;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0">
<tr><td style="height:4px;background:{BRAND};border-radius:12px 12px 0 0;font-size:0;line-height:0;">&nbsp;</td></tr>
<tr><td style="padding:32px 40px 4px;">
<p style="margin:0 0 8px;font:600 12px/1.4 {FONT};color:{BRAND};letter-spacing:0.08em;text-transform:uppercase;">{eyebrow}</p>
<h1 style="margin:0 0 20px;font:700 22px/1.35 {FONT};color:#111827;">{heading}</h1>
<p style="{p_style}">{greeting}</p>
{paragraphs}
</td></tr>
{features}{details}{cta}{note}
<tr><td style="padding:28px 40px 36px;font:400 15px/1.65 {FONT};color:#374151;">Saygılarımızla,<br><strong style="color:#111827;">nlink.tr Ekibi</strong></td></tr>
</table>
</td></tr>
<tr><td style="padding:24px 8px 0;text-align:center;font:400 12px/1.7 {FONT};color:#6b7280;">
Bu e-posta, nlink.tr hesabınızla ilgili bilgilendirme amacıyla gönderilmiştir.<br>
Sorularınız için <a href="mailto:bilgi@nlink.tr" style="color:{BRAND};text-decoration:none;">bilgi@nlink.tr</a> adresinden bize ulaşabilirsiniz.<br>
&copy; {year} nlink.tr &middot; <a href="{site}/tr/app/privacy-policy" style="color:#6b7280;">Gizlilik Politikası</a>
</td></tr>
</table>
</td></tr>
</table>
</body>
</html>"#,
        subject = esc(l.subject),
        preheader = esc(l.preheader),
        eyebrow = esc(l.eyebrow),
        heading = esc(l.heading),
        greeting = l.greeting,
    );

    // Düz metin sürümü
    let mut text = format!("{}\n\n{}\n\n", l.heading, strip_tags(&l.greeting));
    for p in &l.paragraphs {
        text.push_str(&strip_tags(p));
        text.push_str("\n\n");
    }
    for (title, desc) in &l.features {
        text.push_str(&format!("- {title}: {desc}\n"));
    }
    if !l.features.is_empty() {
        text.push('\n');
    }
    for (label, value) in &l.details {
        text.push_str(&format!("{label}: {value}\n"));
    }
    if !l.details.is_empty() {
        text.push('\n');
    }
    if let Some((label, url)) = &l.cta {
        text.push_str(&format!("{label}: {url}\n\n"));
    }
    if let Some(n) = &l.note {
        text.push_str(&strip_tags(n));
        text.push_str("\n\n");
    }
    text.push_str("Saygılarımızla,\nnlink.tr Ekibi\n\n--\nBu e-posta, nlink.tr hesabınızla ilgili bilgilendirme amacıyla gönderilmiştir.\nİletişim: bilgi@nlink.tr\n");

    EmailContent { subject: l.subject.to_string(), html, text }
}

// ─── Biçimlendirme ────────────────────────────────────────────────────────────

/// "101099.00" → "101.099,00 TL"; ayrıştırılamazsa olduğu gibi + " TL".
pub fn format_tl(amount: &str) -> String {
    let a = amount.trim();
    let (int_part, frac) = match a.split_once('.') {
        Some((i, f)) => (i, format!("{:0<2}", f).chars().take(2).collect::<String>()),
        None => (a, "00".to_string()),
    };
    if int_part.is_empty()
        || !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return format!("{a} TL");
    }
    let digits: Vec<char> = int_part.chars().collect();
    let mut grouped = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(*c);
    }
    format!("{grouped},{frac} TL")
}

/// UTC → Türkiye saati (UTC+3, yaz saati uygulaması yok), "dd.mm.yyyy".
pub fn format_date_tr(utc: chrono::NaiveDateTime) -> String {
    (utc + chrono::Duration::hours(3)).format("%d.%m.%Y").to_string()
}

pub fn plan_label(plan: &str) -> String {
    match plan {
        "silver" => "Silver".to_string(),
        "gold" => "Gold".to_string(),
        "enterprise" => "Enterprise".to_string(),
        other => {
            let mut c = other.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

pub fn cycle_label(billing_cycle: &str) -> &'static str {
    if billing_cycle == "yearly" { "yıllık" } else { "aylık" }
}

fn plans_url(site_url: &str) -> String {
    format!("{}/tr/app/plans", site_url.trim_end_matches('/'))
}

// ─── Şablonlar ────────────────────────────────────────────────────────────────

pub struct PaymentInfo<'a> {
    pub name: Option<&'a str>,
    pub plan: &'a str,
    pub amount: &'a str,
    /// "monthly" | "yearly"
    pub billing_cycle: &'a str,
    pub expires_at: chrono::NaiveDateTime,
    pub order_no: &'a str,
    pub site_url: &'a str,
}

/// İlk ödeme (abonelik başlatıldı) ya da otomatik yenileme onayı.
/// `renews`: abonelik bir sonraki dönem otomatik yenilenecek mi (iptal edilmediyse true).
pub fn payment_success(p: &PaymentInfo, is_first: bool, renews: bool) -> EmailContent {
    let plan = plan_label(p.plan);
    let (subject, eyebrow, heading, lead) = if is_first {
        (
            format!("Aboneliğiniz başlatıldı: {plan} Plan"),
            "Ödeme Onayı",
            "Ödemeniz alındı",
            format!(
                "<strong>{}</strong> plan aboneliğiniz için ödemeniz başarıyla alınmış ve aboneliğiniz başlatılmıştır.",
                esc(&plan)
            ),
        )
    } else {
        (
            format!("Aboneliğiniz yenilendi: {plan} Plan"),
            "Yenileme Onayı",
            "Aboneliğiniz yenilendi",
            format!(
                "<strong>{}</strong> plan aboneliğiniz kayıtlı kartınızdan tahsil edilen ödemeyle yenilenmiştir.",
                esc(&plan)
            ),
        )
    };
    let date = format_date_tr(p.expires_at);
    let mut details = vec![
        ("Plan", format!("{plan} ({})", cycle_label(p.billing_cycle))),
        ("Tutar", format_tl(p.amount)),
        ("Dönem bitişi", date.clone()),
    ];
    let note = if renews {
        details.push(("Sonraki ödeme", date.clone()));
        format!(
            "Aboneliğiniz her {} kayıtlı kartınızdan otomatik olarak yenilenir. Dilediğiniz zaman Planlar sayfasından \
             iptal edebilirsiniz; iptal durumunda mevcut dönemin sonuna kadar planınızı kullanmaya devam edersiniz.",
            if p.billing_cycle == "yearly" { "yıl" } else { "ay" }
        )
    } else {
        details.push(("Otomatik yenileme", "Kapalı".to_string()));
        "Aboneliğiniz iptal edilmiş olduğundan bu dönemin sonunda yenilenmeyecektir.".to_string()
    };
    details.push(("Sipariş no", p.order_no.to_string()));
    let preheader = format!("{plan} plan aboneliğiniz {date} tarihine kadar geçerlidir.");

    render(&Layout {
        subject: &subject,
        preheader: &preheader,
        eyebrow,
        heading,
        greeting: greeting(p.name),
        paragraphs: vec![lead],
        features: vec![],
        details,
        cta: Some(("Aboneliği Yönet", plans_url(p.site_url))),
        note: Some(note.to_string()),
        site_url: p.site_url,
    })
}

/// Yenileme ödemesi alınamadı.
pub fn payment_failed(
    name: Option<&str>,
    plan: &str,
    billing_cycle: &str,
    amount: &str,
    reason: Option<&str>,
    attempts_left: i32,
    site_url: &str,
) -> EmailContent {
    let plan_l = plan_label(plan);
    let follow_up = if attempts_left > 0 {
        format!(
            "Ödeme, önümüzdeki günlerde günde bir kez olmak üzere <strong>{attempts_left}</strong> kez daha otomatik \
             olarak denenecektir. Bu süre içinde planınızın özelliklerini kullanmaya devam edebilirsiniz."
        )
    } else {
        "Tüm deneme hakları kullanıldığından aboneliğiniz yenilenemeyecek ve kısa süre içinde hesabınız ücretsiz \
         plana geçecektir. Planınızı sürdürmek için Planlar sayfasından yeniden abone olabilirsiniz."
            .to_string()
    };
    let preheader = format!("{plan_l} plan aboneliğinizin yenileme ödemesi tahsil edilemedi.");
    render(&Layout {
        subject: "Abonelik ödemeniz alınamadı",
        preheader: &preheader,
        eyebrow: "Ödeme Bildirimi",
        heading: "Ödemeniz alınamadı",
        greeting: greeting(name),
        paragraphs: vec![
            format!(
                "<strong>{}</strong> plan aboneliğinizin yenileme ödemesi kayıtlı kartınızdan tahsil edilemedi.",
                esc(&plan_l)
            ),
            follow_up,
        ],
        features: vec![],
        details: vec![
            ("Plan", format!("{plan_l} ({})", cycle_label(billing_cycle))),
            ("Tutar", format_tl(amount)),
            (
                "Sebep",
                reason
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or("Banka tarafından onaylanmadı")
                    .to_string(),
            ),
        ],
        cta: Some(("Planlar Sayfasına Git", plans_url(site_url))),
        note: Some(
            "Kartınızın limitinin yeterli ve internet alışverişine açık olduğunu kontrol etmenizi öneririz. \
             Sorun devam ederse kartınızı veren bankayla iletişime geçebilirsiniz."
                .to_string(),
        ),
        site_url,
    })
}

/// Kayıtlı kart CVV istediği için otomatik yenileme yapılamıyor.
pub fn cvv_required(
    name: Option<&str>,
    plan: &str,
    billing_cycle: &str,
    expires_at: Option<chrono::NaiveDateTime>,
    site_url: &str,
) -> EmailContent {
    let plan_l = plan_label(plan);
    let mut details = vec![("Plan", format!("{plan_l} ({})", cycle_label(billing_cycle)))];
    if let Some(e) = expires_at {
        details.push(("Dönem bitişi", format_date_tr(e)));
    }
    render(&Layout {
        subject: "Abonelik yenilemesi için işleminiz gerekiyor",
        preheader: "Kayıtlı kartınız otomatik yenileme için güvenlik kodu doğrulaması gerektiriyor.",
        eyebrow: "İşlem Gerekli",
        heading: "Otomatik yenileme yapılamadı",
        greeting: greeting(name),
        paragraphs: vec![
            format!(
                "<strong>{}</strong> plan aboneliğinizin yenileme zamanı gelmiştir; ancak kayıtlı kartınız otomatik \
                 tahsilat için güvenlik kodu (CVV) doğrulaması gerektirdiğinden ödeme alınamamıştır.",
                esc(&plan_l)
            ),
            "Aboneliğinizin devam etmesi için dönem sonunda Planlar sayfasından farklı bir kartla yeniden abone \
             olabilir veya bilgi@nlink.tr adresinden bizimle iletişime geçebilirsiniz."
                .to_string(),
        ],
        features: vec![],
        details,
        cta: Some(("Planlar Sayfasına Git", plans_url(site_url))),
        note: None,
        site_url,
    })
}

/// Kullanıcı aboneliği iptal etti (dönem sonuna kadar erişim sürer).
pub fn subscription_cancelled(
    name: Option<&str>,
    plan: &str,
    billing_cycle: &str,
    expires_at: Option<chrono::NaiveDateTime>,
    site_url: &str,
) -> EmailContent {
    let plan_l = plan_label(plan);
    let date = expires_at.map(format_date_tr).unwrap_or_else(|| "dönem sonu".to_string());
    let preheader = format!("{plan_l} planınızı {date} tarihine kadar kullanmaya devam edebilirsiniz.");
    render(&Layout {
        subject: "Aboneliğiniz iptal edildi",
        preheader: &preheader,
        eyebrow: "Abonelik Bilgisi",
        heading: "İptal talebiniz alındı",
        greeting: greeting(name),
        paragraphs: vec![
            format!(
                "<strong>{}</strong> plan aboneliğinizin otomatik yenilemesi talebiniz doğrultusunda durdurulmuştur.",
                esc(&plan_l)
            ),
            format!(
                "<strong>{}</strong> tarihine kadar planınızın tüm özelliklerini kullanmaya devam edebilirsiniz. \
                 Bu tarihte hesabınız ücretsiz plana geçecek ve kayıtlı kartınız sistemimizden silinecektir.",
                esc(&date)
            ),
        ],
        features: vec![],
        details: vec![
            ("Plan", format!("{plan_l} ({})", cycle_label(billing_cycle))),
            ("Erişim bitişi", date.clone()),
            ("Otomatik yenileme", "Kapalı".to_string()),
        ],
        cta: Some(("Aboneliği Yönet", plans_url(site_url))),
        note: Some(
            "Fikrinizi değiştirirseniz dönem sonuna kadar Planlar sayfasından iptali geri alabilirsiniz.".to_string(),
        ),
        site_url,
    })
}

/// Yöneticiye: elle incelenmesi gereken ödeme (tahsil edilmiş olabilir; iade gerekebilir).
pub fn payment_review_alert(reason: &str, details: Vec<(&str, String)>, site_url: &str) -> EmailContent {
    let subject = format!("[nlink ödeme] İnceleme gerekiyor: {reason}");
    render(&Layout {
        subject: &subject,
        preheader: "Bir ödeme otomatik işlenmedi ve incelemeye alındı.",
        eyebrow: "Yönetici Uyarısı",
        heading: "Ödeme incelemeye alındı",
        greeting: "Merhaba,".to_string(),
        paragraphs: vec![
            format!(
                "Aşağıdaki ödeme PayTR'dan başarılı bildirildi ancak otomatik işlenmedi (neden: <strong>{}</strong>). \
                 Abonelik ve müşteri kaydı değiştirilmedi; tahsilat gerekiyorsa iade edilmelidir.",
                esc(reason)
            ),
        ],
        features: vec![],
        details,
        cta: None,
        note: Some("Ödeme kaydının durumu <code>review</code> olarak işaretlendi.".to_string()),
        site_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn formats_turkish_amounts() {
        assert_eq!(format_tl("299.00"), "299,00 TL");
        assert_eq!(format_tl("101099.00"), "101.099,00 TL");
        assert_eq!(format_tl("1449.5"), "1.449,50 TL");
        assert_eq!(format_tl("99900"), "99.900,00 TL");
        assert_eq!(format_tl("abc"), "abc TL");
    }

    #[test]
    fn dates_are_turkey_time() {
        // 30 Eylül 22:00 UTC = 1 Ekim 01:00 TR
        assert_eq!(format_date_tr(dt("2026-09-30 22:00:00")), "01.10.2026");
    }

    #[test]
    fn reason_is_escaped() {
        let m = payment_failed(None, "gold", "monthly", "899.00", Some("<b>x</b>"), 2, "https://nlink.tr");
        assert!(m.html.contains("&lt;b&gt;x&lt;/b&gt;"));
        assert!(!m.html.contains("<b>x"));
        // Düz metin HTML olarak yorumlanmaz; değer olduğu gibi kalır.
        assert!(m.text.contains("Sebep: <b>x</b>"));
    }

    /// `cargo test write_previews -- --ignored` → target/email-previews/*.html
    #[test]
    #[ignore]
    fn write_previews() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/email-previews");
        std::fs::create_dir_all(&dir).unwrap();
        let site = "https://nlink.tr";
        let info = PaymentInfo {
            name: Some("Ayşe Yılmaz"),
            plan: "gold",
            amount: "899.00",
            billing_cycle: "monthly",
            expires_at: dt("2026-11-01 01:13:12"),
            order_no: "u41t1790000000000",
            site_url: site,
        };
        let renewal = PaymentInfo { order_no: "r41t1790000000000", ..info };
        let info = PaymentInfo { order_no: "u41t1790000000000", ..renewal };
        for (file, m) in [
            ("odeme-onayi", payment_success(&info, true, true)),
            ("yenileme-onayi", payment_success(&renewal, false, true)),
            ("odeme-alinamadi", payment_failed(Some("Ayşe Yılmaz"), "gold", "yearly", "8630.40", Some("Yetersiz bakiye"), 2, site)),
            ("cvv-gerekli", cvv_required(Some("Ayşe Yılmaz"), "gold", "monthly", Some(dt("2026-11-01 01:13:12")), site)),
            ("iptal", subscription_cancelled(Some("Ayşe Yılmaz"), "gold", "monthly", Some(dt("2026-11-01 01:13:12")), site)),
        ] {
            std::fs::write(dir.join(format!("{file}.html")), &m.html).unwrap();
            std::fs::write(dir.join(format!("{file}.txt")), format!("Konu: {}\n\n{}", m.subject, m.text)).unwrap();
        }
    }
}
