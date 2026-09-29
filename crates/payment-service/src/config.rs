use anyhow::Context;

/// SMTP ile email göndermek için opsiyonel ayarlar.
/// Tüm alanlar yoksa email devre dışı kalır.
#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_address: String,
    pub from_name: String,
    pub site_url: String,
}

#[derive(Clone)]
pub struct Config {
    pub merchant_id: String,
    pub merchant_key: String,
    pub merchant_salt: String,
    pub host: String,
    pub port: u16,
    pub test_mode: u8,
    pub database_url: String,
    /// Servisin dışarıdan erişilebilir base URL'i (ok/fail redirect ve PayTR callback için)
    pub base_url: String,
    /// Scheduler çalışma aralığı (saniye). Varsayılan: 3600 (1 saat).
    pub scheduler_interval_secs: u64,
    /// Bu kadar başarısız ödeme denemesinden sonra abonelik yenilenmez. Varsayılan: 3.
    pub max_failed_attempts: i32,
    /// Ödeme alınamayan aktif abonelik, bitişten bu kadar gün sonra expire edilir (yenileme
    /// denemeleri günde bir yapılır). Varsayılan: 4.
    pub grace_days: i32,
    /// Next.js → servis çağrılarında `X-Internal-Token` olarak beklenen gizli değer (zorunlu).
    pub internal_api_token: String,
    /// Email ayarları — tüm SMTP değişkenleri tanımlıysa Some, değilse None.
    pub email: Option<EmailConfig>,
    /// İnceleme gerektiren ödemeler (tutar uyuşmazlığı, geçersiz aboneliğe tahsilat) bu adrese
    /// bildirilir (`ALERT_EMAIL`). Tanımlı değilse yalnızca loglanır.
    pub alert_email: Option<String>,
    /// PayTR `user_phone` zorunlu; müşterinin telefonu yoksa gönderilecek şirket iletişim
    /// numarası (`PAYTR_FALLBACK_PHONE`, varsayılan: sitede yayımlanan WhatsApp hattı).
    pub fallback_phone: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("merchant_id", &self.merchant_id)
            .field("merchant_key", &"[REDACTED]")
            .field("merchant_salt", &"[REDACTED]")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("test_mode", &self.test_mode)
            .field("database_url", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("grace_days", &self.grace_days)
            .field("internal_api_token", &"[REDACTED]")
            .finish()
    }
}

/// Tüm SMTP env değişkenleri tanımlıysa EmailConfig döner, eksik varsa None.
fn build_email_config() -> Option<EmailConfig> {
    let host = std::env::var("SMTP_HOST").ok()?;
    let username = std::env::var("SMTP_USERNAME").ok()?;
    let password = std::env::var("SMTP_PASSWORD").ok()?;
    let from = std::env::var("EMAIL_FROM").ok()?;
    Some(EmailConfig {
        smtp_host: host,
        smtp_port: std::env::var("SMTP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(587),
        smtp_username: username,
        smtp_password: password,
        from_address: from,
        from_name: std::env::var("EMAIL_FROM_NAME").unwrap_or_else(|_| "nlink.tr".to_string()),
        site_url: std::env::var("SITE_URL").unwrap_or_else(|_| "https://nlink.tr".to_string()),
    })
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            merchant_id: std::env::var("MERCHANT_ID").context("MERCHANT_ID eksik")?,
            merchant_key: std::env::var("MERCHANT_KEY").context("MERCHANT_KEY eksik")?,
            merchant_salt: std::env::var("MERCHANT_SALT").context("MERCHANT_SALT eksik")?,
            host: std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "3001".to_string())
                .parse()
                .context("PORT geçerli bir sayı olmalıdır")?,
            test_mode: std::env::var("TEST_MODE")
                .unwrap_or_else(|_| "0".to_string())
                .parse()
                .context("TEST_MODE 0 veya 1 olmalıdır")?,
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL eksik")?,
            base_url: std::env::var("BASE_URL")
                .unwrap_or_else(|_| "http://localhost:3001".to_string()),
            scheduler_interval_secs: std::env::var("SCHEDULER_INTERVAL_SECS")
                .unwrap_or_else(|_| "3600".to_string())
                .parse()
                .context("SCHEDULER_INTERVAL_SECS geçerli bir sayı olmalıdır")?,
            max_failed_attempts: std::env::var("MAX_FAILED_ATTEMPTS")
                .unwrap_or_else(|_| "3".to_string())
                .parse()
                .context("MAX_FAILED_ATTEMPTS geçerli bir sayı olmalıdır")?,
            grace_days: std::env::var("GRACE_DAYS")
                .unwrap_or_else(|_| "4".to_string())
                .parse()
                .context("GRACE_DAYS geçerli bir sayı olmalıdır")?,
            internal_api_token: {
                let t = std::env::var("INTERNAL_API_TOKEN").context("INTERNAL_API_TOKEN eksik")?;
                anyhow::ensure!(t.len() >= 32, "INTERNAL_API_TOKEN en az 32 karakter olmalıdır");
                t
            },
            email: build_email_config(),
            alert_email: std::env::var("ALERT_EMAIL").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            fallback_phone: std::env::var("PAYTR_FALLBACK_PHONE")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "5305861333".to_string()),
        })
    }
}
