use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use crate::{
    crypto::generate_card_delete_token,
    db::{card_repo, models::CardResponse},
    error::AppError,
    models::card::{CardDeleteRequest, CardListRequest, PaytrErrorResponse},
    paytr_client::PAYTR_CARD_DELETE_ENDPOINT,
    AppState,
};

/// DB'deki kayıtlı kartları döner (PayTR API çağrısı yapmaz).
/// utoken response'a dahil edilmez — istemciye sızdırılmamalı.
pub async fn list_cards(
    State(state): State<AppState>,
    Json(req): Json<CardListRequest>,
) -> Result<impl IntoResponse, AppError> {
    let cards: Vec<CardResponse> = card_repo::list_by_member(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?
        .into_iter()
        .map(CardResponse::from)
        .collect();

    Ok((StatusCode::OK, Json(cards)))
}

/// PayTR'dan kartı siler ve DB'yi günceller.
pub async fn delete_card(
    State(state): State<AppState>,
    Json(req): Json<CardDeleteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let paytr_token = generate_card_delete_token(
        &req.ctoken,
        &req.utoken,
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    let resp = state
        .http
        .post(PAYTR_CARD_DELETE_ENDPOINT)
        .form(&[
            ("merchant_id", state.config.merchant_id.as_str()),
            ("utoken",      req.utoken.as_str()),
            ("ctoken",      req.ctoken.as_str()),
            ("paytr_token", paytr_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR bağlantı hatası: {}", e))?
        .json::<PaytrErrorResponse>()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR yanıt hatası: {}", e))?;

    if resp.status == "error" {
        return Err(AppError::PaytrError(
            resp.err_msg.unwrap_or_else(|| "Kart silinemedi".to_string()),
        ));
    }

    card_repo::deactivate_card(&state.db, &req.ctoken, req.member_id)
        .await
        .map_err(anyhow::Error::from)?;

    tracing::info!(member_id = req.member_id, ctoken = %req.ctoken, "Kart silindi");

    Ok(StatusCode::NO_CONTENT)
}
