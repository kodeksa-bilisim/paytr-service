pub mod admin;
pub mod billing;
pub mod callback;
pub mod growth;
pub mod member;
pub mod payment;
pub mod subscription;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

use crate::AppState;

/// Servis sağlığı + scheduler'ın son başarılı çalışması (izleme, çalışmanın durduğunu
/// fark etmek için: `scheduler_last_ok_age_secs` aralığın birkaç katını geçmemeli).
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let last = state.scheduler_last_ok.load(std::sync::atomic::Ordering::Relaxed);
    let age = (last > 0).then(|| chrono::Utc::now().timestamp() - last);
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "service": "payment-service",
            "scheduler_interval_secs": state.config.scheduler_interval_secs,
            "scheduler_last_ok_age_secs": age,
        })),
    )
}

/// PayTR'ın sync_mode olmayan akışta yönlendirdiği ok/fail sayfaları.
/// Gerçek sonuç callback üzerinden gelir; bu endpoint'ler sadece birer placeholder.
pub async fn payment_ok() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn payment_fail() -> impl IntoResponse {
    StatusCode::OK
}
