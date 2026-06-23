use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use crate::{
    db::{customer_repo, subscription_repo},
    email,
    error::AppError,
    AppState,
};

#[derive(Deserialize)]
pub struct CancelRequest {
    pub member_id: i32,
    pub subscription_id: i32,
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
