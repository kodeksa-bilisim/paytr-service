//! PayTR uç noktaları. `PAYTR_BASE_URL` (varsayılan https://www.paytr.com) testlerde mock
//! sunucuya yönlendirmek için değiştirilebilir.

fn base() -> String {
    std::env::var("PAYTR_BASE_URL")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://www.paytr.com".to_string())
}

pub fn payment_endpoint() -> String {
    format!("{}/odeme", base())
}

pub fn card_list_endpoint() -> String {
    format!("{}/odeme/capi/list", base())
}

pub fn status_query_endpoint() -> String {
    format!("{}/odeme/durum-sorgu", base())
}

pub fn card_delete_endpoint() -> String {
    format!("{}/odeme/capi/delete", base())
}

/// PayTR'ın ret mesajı mağaza tarafındaki bir sorunu mu anlatıyor (yetki, token, hash)?
/// Bunlar müşterinin kartıyla ilgisizdir: deneme hakkından düşülmez, müşteriye "ödemeniz
/// alınamadı" gönderilmez, yöneticiye bildirilir. Mesajlar ASCII Türkçe gelir
/// ("Bu islem icin magazanin yetkisi yok (sync_mode)").
pub fn is_merchant_side_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    // "yetkisi yok" tek başına yetmez: banka reddi de "kartin internet islem yetkisi yok" diyebilir.
    ["magaza", "mağaza", "merchant", "paytr_token", "hash"]
        .iter()
        .any(|k| m.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_merchant_side_errors() {
        assert!(is_merchant_side_error("Bu islem icin magazanin yetkisi yok (sync_mode)"));
        assert!(is_merchant_side_error("paytr_token gecersiz"));
        assert!(is_merchant_side_error("Mağaza bulunamadı"));
        assert!(!is_merchant_side_error("Yetersiz bakiye"));
        assert!(!is_merchant_side_error("Kart limiti yetersiz"));
        assert!(!is_merchant_side_error("Islem onaylanmadi"));
        assert!(!is_merchant_side_error("Kartin internet islem yetkisi yok"));
    }
}
