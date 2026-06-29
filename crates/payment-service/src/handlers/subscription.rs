use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use crate::{
    crypto::generate_card_delete_token,
    db::{card_repo, customer_repo, subscription_repo},
    email,
    error::AppError,
    models::card::PaytrErrorResponse,
    paytr_client::PAYTR_CARD_DELETE_ENDPOINT,
    AppState,
};

#[derive(Deserialize)]
pub struct CancelRequest {
    pub member_id: i32,
    pub subscription_id: i32,
}

#[derive(Deserialize)]
pub struct ReactivateRequest {
    pub member_id: i32,
}

pub async fn cancel_subscription(
    State(state): State<AppState>,
    Json(req): Json<CancelRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Aboneliği iptal et (member_id eşleşmesi zorunlu — yetki kontrolü)
    let cancelled = subscription_repo::cancel_by_member(&state.db, req.subscription_id, req.member_id)
        .await
        .map_err(anyhow::Error::from)?;

    if !cancelled {
        return Err(AppError::BadRequest(
            "Abonelik bulunamadı veya zaten iptal edilmiş".to_string(),
        ));
    }

    // customers tablosunu güncelle
    customer_repo::set_subscription_cancelled(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?;

    tracing::info!(
        member_id = req.member_id,
        subscription_id = req.subscription_id,
        "Abonelik iptal edildi"
    );

    // KVKK md.7: abonelik iptalinde ödeme rızası sona erer; kart verisi
    // tutmanın yasal dayanağı kalmaz. PayTR'dan ve DB'den hemen silinir.
    delete_member_cards(&state, req.member_id).await;

    // İptal email bildirimi
    if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
        if let Ok(Some(sub)) = subscription_repo::find_by_id(&state.db, req.subscription_id).await {
            let to = sub.user_email.as_deref().unwrap_or("");
            if !to.is_empty() {
                let expires_str = sub
                    .expires_at
                    .map(|d| d.format("%d.%m.%Y").to_string())
                    .unwrap_or_else(|| "—".to_string());

                let (subject, html) = email::tpl_subscription_cancelled(
                    &sub.plan,
                    &expires_str,
                    &email_cfg.site_url,
                );
                email::send(mailer, email_cfg, to, subject, html).await;
            }
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

pub async fn reactivate_subscription(
    State(state): State<AppState>,
    Json(req): Json<ReactivateRequest>,
) -> Result<impl IntoResponse, AppError> {
    let reactivated = subscription_repo::reactivate_by_member(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?;

    if !reactivated {
        return Err(AppError::BadRequest(
            "Geri alınabilecek iptal edilmiş abonelik bulunamadı".to_string(),
        ));
    }

    customer_repo::set_subscription_reactivated(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?;

    tracing::info!(member_id = req.member_id, "Abonelik iptali geri alındı");

    Ok(StatusCode::NO_CONTENT)
}

/// Üyenin tüm kayıtlı kartlarını PayTR'dan siler, ardından DB'yi temizler.
/// Hata olursa loglayıp devam eder — kart silme hatası iptal işlemini engellemez.
async fn delete_member_cards(state: &crate::AppData, member_id: i32) {
    let cards = match card_repo::list_by_member(&state.db, member_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(member_id, error = %e, "Kart listesi alınamadı, silme atlandı");
            return;
        }
    };

    let utoken = match card_repo::get_user_token(&state.db, member_id).await {
        Ok(Some(t)) => t.utoken,
        Ok(None) => {
            tracing::info!(member_id, "Kayıtlı utoken yok, kart silme adımı atlandı");
            return;
        }
        Err(e) => {
            tracing::error!(member_id, error = %e, "utoken alınamadı, silme atlandı");
            return;
        }
    };

    for card in &cards {
        let token = generate_card_delete_token(
            &card.ctoken,
            &utoken,
            &state.config.merchant_salt,
            &state.config.merchant_key,
        );

        let result = state
            .http
            .post(PAYTR_CARD_DELETE_ENDPOINT)
            .form(&[
                ("merchant_id", state.config.merchant_id.as_str()),
                ("utoken",      utoken.as_str()),
                ("ctoken",      card.ctoken.as_str()),
                ("paytr_token", token.as_str()),
            ])
            .send()
            .await;

        match result {
            Err(e) => {
                tracing::error!(
                    member_id, ctoken = %card.ctoken,
                    error = %e, "PayTR kart silme isteği başarısız"
                );
            }
            Ok(resp) => {
                match resp.json::<PaytrErrorResponse>().await {
                    Ok(body) if body.status == "error" => {
                        tracing::error!(
                            member_id, ctoken = %card.ctoken,
                            err_msg = ?body.err_msg, "PayTR kart silme reddedildi"
                        );
                    }
                    Ok(_) => {
                        tracing::info!(member_id, ctoken = %card.ctoken, "PayTR kart silindi");
                    }
                    Err(e) => {
                        tracing::error!(
                            member_id, ctoken = %card.ctoken,
                            error = %e, "PayTR silme yanıtı parse hatası"
                        );
                    }
                }
            }
        }
    }

    // PayTR adımı tamamlandıktan sonra DB'yi temizle
    if let Err(e) = card_repo::purge_member_cards(&state.db, member_id).await {
        tracing::error!(member_id, error = %e, "DB kart temizleme hatası");
    } else {
        tracing::info!(member_id, cards = cards.len(), "Kart verileri DB'den temizlendi");
    }
}
