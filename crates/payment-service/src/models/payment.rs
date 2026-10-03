use serde::{Deserialize, Serialize};

/// Yeni kart ile ödeme + kart saklama (ilk abonelik ödemesi, 3D Secure).
/// Next.js sunucusu `member_id`/`email`'i oturumdan doldurur; tutar ve plan burada
/// yeniden doğrulanır (server action'lar tarayıcıdan rastgele argümanla çağrılabilir).
#[derive(Debug, Deserialize)]
pub struct InitPaymentRequest {
    /// qurlbackend customers.member_id (zorunlu).
    pub member_id: i32,
    pub plan: String,          // silver, gold
    pub billing_cycle: String, // "monthly" | "yearly"
    // --- PayTR alanları ---
    pub user_ip: String,
    pub merchant_oid: String,
    pub email: String,
    /// TL cinsinden tutar, iki ondalık: "349.00"
    pub payment_amount: String,
    #[serde(default = "default_payment_type")]
    pub payment_type: String,
    #[serde(default)]
    pub installment_count: u8,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub user_name: String,
    pub user_address: String,
    /// Müşterinin telefonu; boşsa PayTR'a `PAYTR_FALLBACK_PHONE` gider.
    #[serde(default)]
    pub user_phone: String,
    pub user_basket: Vec<BasketItem>,
    pub merchant_ok_url: String,
    pub merchant_fail_url: String,
    pub card_type: Option<String>,
    #[serde(default = "default_lang")]
    pub client_lang: String,
    pub debug_on: Option<u8>,
    /// İndirim kodu (isteğe bağlı).
    #[serde(default)]
    pub coupon_code: Option<String>,
}

/// Enterprise plan için fiyat hesaplayıcı isteği — fiyat backend'de hesaplanır.
#[derive(Debug, Deserialize)]
pub struct EnterpriseInitRequest {
    pub member_id: i32,
    pub email: String,
    pub users: i32,
    pub extra_links: i32,
    pub extra_clicks: i32,
    /// "monthly" | "yearly"; göndermeyen eski istemciler için aylık.
    #[serde(default = "default_billing_cycle")]
    pub billing_cycle: String,
    pub user_name: String,
    pub user_ip: String,
    pub merchant_oid: String,
    pub merchant_ok_url: String,
    pub merchant_fail_url: String,
    #[serde(default = "default_lang")]
    pub client_lang: String,
    pub card_type: Option<String>,
    pub debug_on: Option<u8>,
    #[serde(default)]
    pub coupon_code: Option<String>,
}

/// Downgrade planlaması — ödeme alınmaz, dönem sonunda plan değişir.
#[derive(Debug, Deserialize)]
pub struct ScheduleDowngradeRequest {
    pub member_id: i32,
    pub new_plan: String, // "silver" | "standard"
}

/// Planlanmış downgrade'i iptal eder.
#[derive(Debug, Deserialize)]
pub struct CancelScheduleRequest {
    pub member_id: i32,
}

/// Yükseltme tutarı sorgusu (ödeme başlatmadan önce kullanıcıya gösterilir).
#[derive(Debug, Deserialize)]
pub struct UpgradeQuoteRequest {
    pub member_id: i32,
    /// "silver" | "gold" | "enterprise"
    pub plan: String,
    pub billing_cycle: String,
    /// Yalnızca enterprise için.
    pub users: Option<i32>,
    pub extra_links: Option<i32>,
    pub extra_clicks: Option<i32>,
    #[serde(default)]
    pub coupon_code: Option<String>,
}

/// Tutarlar TL ("724.50"). Geçerli abonelik yoksa kredi 0 ve `from_*` alanları boş.
/// `charge_amount` = liste − kalan süre kredisi − indirim − kredi bakiyesi (tahsil edilecek).
#[derive(Debug, Serialize)]
pub struct UpgradeQuoteResponse {
    pub list_amount: String,
    pub credit_amount: String,
    pub charge_amount: String,
    pub from_plan: Option<String>,
    pub from_expires_at: Option<String>,
    /// İndirim (kupon ya da referans) tutarı.
    pub discount_amount: String,
    /// "coupon" | "referral"
    pub discount_source: Option<String>,
    pub coupon_code: Option<String>,
    /// Kaç ödemede geçerli: null = süresiz, 1 = yalnızca bu ödeme.
    pub discount_cycles: Option<i64>,
    /// Kredi bakiyesinden düşülen.
    pub balance_used: String,
    /// Sonraki yenilemede çekilecek tutar.
    pub renewal_amount: String,
}

/// Downgrade planlama yanıtı.
#[derive(Debug, Serialize)]
pub struct ScheduleDowngradeResponse {
    pub scheduled: bool,
    pub effective_date: Option<String>, // ISO date string
}

fn default_billing_cycle() -> String { "monthly".to_string() }
fn default_payment_type() -> String { "card".to_string() }
fn default_currency() -> String { "TL".to_string() }
fn default_lang() -> String { "tr".to_string() }

#[derive(Debug, Serialize, Deserialize)]
pub struct BasketItem {
    pub name: String,
    pub price: String,
    pub quantity: u32,
}

/// init_payment yanıtı: frontend bu parametrelerle + kart bilgilerini PayTR'a POST eder.
#[derive(Debug, Serialize)]
pub struct InitPaymentResponse {
    pub payment_id: i32,
    pub subscription_id: i32,
    pub paytr_endpoint: String,
    /// Planın dönem fiyatı (TL). Tahsil edilen tutar `form_params.payment_amount`'tır:
    /// yükseltmede liste fiyatından `credit_amount` düşülmüş hâli.
    pub list_amount: String,
    pub credit_amount: String,
    /// İndirim + kredi bakiyesinden düşülen toplam.
    pub discount_amount: String,
    pub form_params: PaytrFormParams,
}

/// PayTR'a gönderilecek form parametreleri (kart verisi hariç).
#[derive(Debug, Serialize)]
pub struct PaytrFormParams {
    pub merchant_id: String,
    pub paytr_token: String,
    pub user_ip: String,
    pub merchant_oid: String,
    pub email: String,
    pub payment_type: String,
    pub payment_amount: String,
    pub installment_count: u8,
    pub no_installment: u8,
    pub max_installment: u8,
    pub currency: String,
    pub test_mode: u8,
    pub non_3d: u8,
    pub store_card: u8,
    pub user_name: String,
    pub user_address: String,
    pub user_phone: String,
    pub user_basket: String,
    pub merchant_ok_url: String,
    pub merchant_fail_url: String,
    pub lang: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_on: Option<u8>,
}
