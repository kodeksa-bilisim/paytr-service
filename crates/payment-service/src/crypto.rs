use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256_base64(data: &str, key: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC her uzunlukta anahtarı kabul eder");
    mac.update(data.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

/// Direkt API ödeme token'ı.
///
/// Formül: HMAC-SHA256(
///   merchant_id + user_ip + merchant_oid + email + payment_amount +
///   payment_type + installment_count + currency + test_mode + non_3d + merchant_salt,
///   merchant_key
/// ) → base64
pub fn generate_payment_token(
    merchant_id: &str,
    user_ip: &str,
    merchant_oid: &str,
    email: &str,
    payment_amount: &str,
    payment_type: &str,
    installment_count: &str,
    currency: &str,
    test_mode: &str,
    non_3d: &str,
    merchant_salt: &str,
    merchant_key: &str,
) -> String {
    tracing::debug!(merchant_oid, "PayTR token hesaplanıyor");
    let data = format!(
        "{}{}{}{}{}{}{}{}{}{}{}",
        merchant_id, user_ip, merchant_oid, email, payment_amount,
        payment_type, installment_count, currency, test_mode, non_3d,
        merchant_salt,
    );
    hmac_sha256_base64(&data, merchant_key)
}

/// Adım 2: Callback hash doğrulama.
///
/// Formül: HMAC-SHA256(
///   merchant_oid + merchant_salt + status + total_amount,
///   merchant_key
/// ) → base64
pub fn verify_callback_hash(
    merchant_oid: &str,
    merchant_salt: &str,
    status: &str,
    total_amount: &str,
    merchant_key: &str,
    received_hash: &str,
) -> bool {
    let data = format!("{}{}{}{}", merchant_oid, merchant_salt, status, total_amount);
    // HMAC verify_slice sabit zamanlı karşılaştırma yapar (timing saldırısını önler)
    let decoded = match STANDARD.decode(received_hash) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let mut mac = HmacSha256::new_from_slice(merchant_key.as_bytes())
        .expect("HMAC her uzunlukta anahtar kabul eder");
    mac.update(data.as_bytes());
    mac.verify_slice(&decoded).is_ok()
}

/// Kayıtlı kart listesi token'ı.
///
/// Formül: HMAC-SHA256(utoken + merchant_salt, merchant_key) → base64
pub fn generate_card_list_token(utoken: &str, merchant_salt: &str, merchant_key: &str) -> String {
    let data = format!("{}{}", utoken, merchant_salt);
    hmac_sha256_base64(&data, merchant_key)
}

/// Kayıtlı kart silme token'ı.
///
/// Formül: HMAC-SHA256(ctoken + utoken + merchant_salt, merchant_key) → base64
pub fn generate_card_delete_token(
    ctoken: &str,
    utoken: &str,
    merchant_salt: &str,
    merchant_key: &str,
) -> String {
    let data = format!("{}{}{}", ctoken, utoken, merchant_salt);
    hmac_sha256_base64(&data, merchant_key)
}
