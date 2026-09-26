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

pub fn card_delete_endpoint() -> String {
    format!("{}/odeme/capi/delete", base())
}
