//! Üyenin fatura bilgisi — iç API (X-Internal-Token). `member_id` Next.js tarafında oturumdan
//! alınır; tarayıcıdan gelen değere güvenilmez.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    billing::{self, BillingProfile, BillingProfileInput},
    db::billing_repo,
    error::AppError,
    AppState,
};

#[derive(Serialize)]
pub struct ProfileResponse {
    /// Kayıt yoksa null: bireysel sayılır (faturaya hesap adı + e-posta yazılır).
    profile: Option<BillingProfile>,
}

/// GET /api/v1/billing-profile/{member_id}
pub async fn get_profile(
    State(state): State<AppState>,
    Path(member_id): Path<i32>,
) -> Result<Json<ProfileResponse>, AppError> {
    let profile = billing_repo::find_profile(&state.db, member_id).await?;
    Ok(Json(ProfileResponse { profile }))
}

#[derive(Deserialize)]
pub struct SaveProfileRequest {
    member_id: i32,
    #[serde(flatten)]
    input: BillingProfileInput,
}

/// PUT /api/v1/billing-profile — doğrular, kaydeder, kaydedileni döner.
/// Yalnızca sonraki faturaları etkiler; kesilmiş faturadaki alıcı kopyası değişmez.
pub async fn save_profile(
    State(state): State<AppState>,
    Json(req): Json<SaveProfileRequest>,
) -> Result<Json<ProfileResponse>, AppError> {
    let profile = billing::validate_profile(&req.input).map_err(AppError::BadRequest)?;
    if !billing_repo::upsert_profile(&state.db, req.member_id, &profile).await? {
        return Err(AppError::BadRequest("Üye bulunamadı".into()));
    }
    Ok(Json(ProfileResponse { profile: Some(profile) }))
}
