use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Hash doğrulama başarısız")]
    InvalidHash,
    #[error("Geçersiz istek: {0}")]
    BadRequest(String),
    #[error("PayTR hatası: {0}")]
    PaytrError(String),
    #[error("İç sunucu hatası")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::InvalidHash => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::PaytrError(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
            AppError::Internal(e) => {
                tracing::error!("Internal error: {:?}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Sunucu hatası".to_string())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
