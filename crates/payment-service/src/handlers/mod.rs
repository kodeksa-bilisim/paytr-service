pub mod callback;
pub mod card;
pub mod payment;
pub mod subscription;

use axum::{http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok", "service": "payment-service" })))
}

/// PayTR'ın sync_mode olmayan akışta yönlendirdiği ok/fail sayfaları.
/// Gerçek sonuç callback üzerinden gelir; bu endpoint'ler sadece birer placeholder.
pub async fn payment_ok() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn payment_fail() -> impl IntoResponse {
    StatusCode::OK
}
