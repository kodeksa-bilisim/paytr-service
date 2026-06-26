use serde::{Deserialize, Serialize};

/// Yeni kart ile ödeme + kart saklama (ilk abonelik ödemesi, 3D Secure).
#[derive(Debug, Deserialize)]
pub struct InitPaymentRequest {
    /// qurlbackend customers.member_id — opsiyonel, yoksa email üzerinden bulunur.
    pub member_id: Option<i32>,
    pub plan: String,          // gold, silver, standard
    pub billing_cycle: String, // monthly, yearly
    // --- PayTR alanları ---
    pub user_ip: String,
    pub merchant_oid: String,
    pub email: String,
    /// Kuruş cinsinden tutar (örn: 99.90 TL → "9990")
    pub payment_amount: String,
    #[serde(default = "default_payment_type")]
    pub payment_type: String,
    #[serde(default)]
    pub installment_count: u8,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub user_name: String,
    pub user_address: String,
    pub user_phone: String,
    pub user_basket: Vec<BasketItem>,
    pub merchant_ok_url: String,
    pub merchant_fail_url: String,
    /// Mevcut kullanıcının utoken'ı varsa gönderilir (ikinci kart eklerken)
    pub utoken: Option<String>,
    pub card_type: Option<String>,
    #[serde(default = "default_lang")]
    pub client_lang: String,
    pub debug_on: Option<u8>,
}

/// Kayıtlı kart ile abonelik ödemesi (Non-3D, server-to-server).
#[derive(Debug, Deserialize)]
pub struct StoredCardPaymentRequest {
    pub member_id: i32,
    pub subscription_id: i32,
    pub plan: String, // gold, silver — tutar doğrulaması için
    // --- PayTR alanları ---
    pub user_ip: String,
    pub merchant_oid: String,
    pub email: String,
    pub payment_amount: String,
    #[serde(default = "default_payment_type")]
    pub payment_type: String,
    #[serde(default)]
    pub installment_count: u8,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub user_name: String,
    pub user_address: String,
    pub user_phone: String,
    pub user_basket: Vec<BasketItem>,
    pub utoken: String,
    pub ctoken: String,
    pub require_cvv: u8,
    /// require_cvv = 1 ise zorunlu
    pub cvv: Option<String>,
    pub card_type: Option<String>,
    #[serde(default = "default_lang")]
    pub client_lang: String,
    pub debug_on: Option<u8>,
}

/// Enterprise plan için fiyat hesaplayıcı isteği — fiyat backend'de hesaplanır.
#[derive(Debug, Deserialize)]
pub struct EnterpriseInitRequest {
    pub member_id: Option<i32>,
    pub email: String,
    pub users: i32,
    pub extra_links: i32,
    pub extra_clicks: i32,
    pub user_name: String,
    pub user_ip: String,
    pub merchant_oid: String,
    pub merchant_ok_url: String,
    pub merchant_fail_url: String,
    #[serde(default = "default_lang")]
    pub client_lang: String,
    pub utoken: Option<String>,
    pub card_type: Option<String>,
    pub debug_on: Option<u8>,
}

/// Downgrade planlaması — ödeme alınmaz, dönem sonunda plan değişir.
#[derive(Debug, Deserialize)]
pub struct ScheduleDowngradeRequest {
    pub member_id: i32,
    pub email: String,
    pub new_plan: String, // "silver" | "standard"
}

/// Planlanmış downgrade'i iptal eder.
#[derive(Debug, Deserialize)]
pub struct CancelScheduleRequest {
    pub member_id: i32,
}

/// Downgrade planlama yanıtı.
#[derive(Debug, Serialize)]
pub struct ScheduleDowngradeResponse {
    pub scheduled: bool,
    pub effective_date: Option<String>, // ISO date string
}

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
    pub form_params: PaytrFormParams,
}

/// Kayıtlı kart ödemesinin anlık sonucu (sync_mode=1).
#[derive(Debug, Serialize)]
pub struct StoredCardPaymentResponse {
    pub payment_id: i32,
    pub subscription_id: i32,
    pub merchant_oid: String,
    /// PayTR sync yanıtı: success | failed | wait_callback
    pub paytr_status: String,
    pub paytr_message: Option<String>,
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
    pub utoken: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_on: Option<u8>,
}
