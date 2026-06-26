use axum::{extract::State, response::IntoResponse, Form};
use chrono::Months;

use crate::{
    crypto::{generate_card_list_token, verify_callback_hash},
    db::{card_repo, customer_repo, payment_repo, subscription_repo},
    email,
    error::AppError,
    models::{
        callback::{CallbackPayload, PaymentStatus},
        card::CardItem,
    },
    paytr_client::PAYTR_CARD_LIST_ENDPOINT,
    AppState,
};

pub async fn payment_callback(
    State(state): State<AppState>,
    Form(payload): Form<CallbackPayload>,
) -> Result<impl IntoResponse, AppError> {
    // 1. Hash doğrulama
    if !verify_callback_hash(
        &payload.merchant_oid,
        &state.config.merchant_salt,
        &payload.status,
        &payload.total_amount,
        &state.config.merchant_key,
        &payload.hash,
    ) {
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Geçersiz callback hash");
        return Err(AppError::InvalidHash);
    }

    let status: PaymentStatus = payload
        .status
        .parse()
        .map_err(|e: String| AppError::BadRequest(e))?;

    match status {
        PaymentStatus::Success => handle_success(&state, &payload).await?,
        PaymentStatus::Failed => handle_failed(&state, &payload).await?,
        PaymentStatus::WaitCallback => {
            tracing::info!(merchant_oid = %payload.merchant_oid, "Ödeme bekleniyor");
        }
    }

    Ok("OK")
}

async fn handle_success(state: &crate::AppData, payload: &CallbackPayload) -> Result<(), AppError> {
    // Ödeme kaydını çek
    // "Bulunamadı" durumunda OK döndür — PayTR'nin yeniden denemesi bu durumu düzeltemez.
    // Gerçek DB hatalarında ise Err döner, PayTR yeniden dener (geçici hata kurtarma).
    let Some(payment) = payment_repo::find_by_oid(&state.db, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?
    else {
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Callback geldi fakat ödeme kaydı bulunamadı — görmezden geliniyor");
        return Ok(());
    };

    // Çift callback koruması: payment success VE subscription aktifse tekrar işleme.
    // payment=success ama subscription=pending durumunda (örn. kart parse hatası sonrası)
    // abonelik aktivasyonuna devam et.
    if payment.status == "success" {
        let sub_already_active = match payment.subscription_id {
            None => true,
            Some(sub_id) => subscription_repo::find_by_id(&state.db, sub_id)
                .await
                .map_err(anyhow::Error::from)?
                .map(|s| s.status != "pending")
                .unwrap_or(true),
        };
        if sub_already_active {
            tracing::warn!(merchant_oid = %payload.merchant_oid, "Tekrar callback, zaten işlendi");
            return Ok(());
        }
        tracing::warn!(
            merchant_oid = %payload.merchant_oid,
            "Ödeme başarılı ama abonelik hâlâ pending, aktivasyon yeniden deneniyor"
        );
    }

    let member_id = payment.member_id;

    // 4. Yeni utoken geldiyse (ilk kart saklama) — kart listesini çek ve DB'ye yaz
    let active_utoken: Option<String> = if let Some(ref utoken) = payload.utoken {
        card_repo::upsert_user_token(&state.db, member_id, utoken)
            .await
            .map_err(anyhow::Error::from)?;

        let cards = fetch_paytr_cards(state, utoken).await?;
        card_repo::sync_cards(&state.db, utoken, &cards)
            .await
            .map_err(anyhow::Error::from)?;

        tracing::info!(member_id, utoken = %utoken, cards = cards.len(), "Kart listesi senkronize edildi");
        Some(utoken.clone())
    } else {
        // Mevcut utoken'ı bul (renewal için)
        card_repo::get_user_token(&state.db, member_id)
            .await
            .map_err(anyhow::Error::from)?
            .map(|t| t.utoken)
    };

    // 5. Abonelik güncelle
    let Some(subscription_id) = payment.subscription_id else {
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Subscription ID yok, sadece ödeme kaydedildi");
        return Ok(());
    };

    let sub = subscription_repo::find_by_id(&state.db, subscription_id)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::BadRequest("Abonelik kaydı bulunamadı".to_string()))?;

    let now = chrono::Utc::now().naive_utc();

    let is_first_payment = sub.status == "pending";

    // Upgrade: mevcut aboneliğin kalan süresi yeni plana aktarılır.
    // previous_expires_at metadata'da varsa period_start olarak kullanılır.
    let upgrade_start = if is_first_payment {
        sub.metadata.as_ref()
            .and_then(|m| m.get("previous_expires_at")?.as_str())
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
            .map(|exp| exp.max(now))
    } else {
        None
    };

    let period_start = if is_first_payment {
        upgrade_start.unwrap_or(now)
    } else {
        sub.expires_at.unwrap_or(now).max(now)
    };

    let (expires_at, next_payment_date) = billing_dates(&sub.billing_cycle, period_start);

    if is_first_payment {
        // İlk ödeme: pending → active
        let utoken = active_utoken.as_deref().unwrap_or("");
        let ctoken = if let Some(ref ut) = active_utoken {
            card_repo::get_default_card(&state.db, ut)
                .await
                .map_err(anyhow::Error::from)?
                .map(|c| c.ctoken)
                .unwrap_or_default()
        } else {
            payment.ctoken.clone().unwrap_or_default()
        };

        subscription_repo::activate(&state.db, subscription_id, utoken, &ctoken, now, expires_at, next_payment_date)
            .await
            .map_err(anyhow::Error::from)?;

        // Upgrade: yeni abonelik aktifleşince önceki aktif aboneliği iptal et.
        subscription_repo::cancel_active_except(&state.db, member_id, subscription_id)
            .await
            .map_err(anyhow::Error::from)?;
    } else {
        // Yenileme: scheduled_plan varsa plan değişikliği ile yenile (downgrade).
        let effective_plan = sub.scheduled_plan.clone().unwrap_or_else(|| sub.plan.clone());
        if sub.scheduled_plan.is_some() {
            subscription_repo::renew_with_plan(&state.db, subscription_id, &effective_plan, expires_at, next_payment_date)
                .await
                .map_err(anyhow::Error::from)?;
        } else {
            subscription_repo::renew(&state.db, subscription_id, expires_at, next_payment_date)
                .await
                .map_err(anyhow::Error::from)?;
        }
    }

    // 6. customers tablosunu güncelle
    // Yenilemede effective_plan kullanılır (downgrade yenileme için).
    let effective_plan_for_customer = if is_first_payment {
        sub.plan.clone()
    } else {
        sub.scheduled_plan.clone().unwrap_or_else(|| sub.plan.clone())
    };

    let custom_plan = if effective_plan_for_customer == "enterprise" {
        sub.metadata.as_ref().and_then(|m| {
            let extra_links = m.get("extra_links")?.as_i64()?;
            let extra_clicks = m.get("extra_clicks")?.as_i64()?;
            Some(serde_json::json!({
                "links_limit": 10000 + extra_links * 1000,
                "clicks_limit": 100000 + extra_clicks * 10000,
            }).to_string())
        })
    } else {
        None
    };

    customer_repo::set_subscription_active(
        &state.db,
        member_id,
        &effective_plan_for_customer,
        subscription_id,
        expires_at,
        next_payment_date,
        custom_plan,
    )
    .await
    .map_err(anyhow::Error::from)?;

    // Atomik başarı işareti: eş zamanlı iki callback gelirse yalnızca biri
    // günceller (TOCTOU koruması). İkincisi false alır ama iş zaten yapıldı.
    payment_repo::set_success(&state.db, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?;

    // Scheduler denemelerini sıfırla
    let _ = subscription_repo::reset_renewal_attempts(&state.db, subscription_id).await;

    tracing::info!(
        member_id,
        merchant_oid = %payload.merchant_oid,
        plan = %sub.plan,
        expires_at = %expires_at,
        "Abonelik güncellendi"
    );

    // Email bildirimi — hata olursa callback'i etkilemez
    if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
        let to = sub.user_email.as_deref().unwrap_or("");
        if !to.is_empty() {
            let expires_str = expires_at.format("%d.%m.%Y").to_string();
            let is_first = sub.status == "pending";
            let (subject, html) = email::tpl_payment_success(
                &sub.plan,
                &expires_str,
                is_first,
                &email_cfg.site_url,
            );
            email::send(mailer, email_cfg, to, subject, html).await;
        }
    }

    Ok(())
}

async fn handle_failed(state: &crate::AppData, payload: &CallbackPayload) -> Result<(), AppError> {
    payment_repo::set_failed(
        &state.db,
        &payload.merchant_oid,
        payload.failed_reason_code.as_deref(),
        payload.failed_reason_msg.as_deref(),
    )
    .await
    .map_err(anyhow::Error::from)?;

    let payment = payment_repo::find_by_oid(&state.db, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?;

    if let Some(ref p) = payment {
        customer_repo::increment_failed_attempts(&state.db, p.member_id)
            .await
            .map_err(anyhow::Error::from)?;
    }

    tracing::warn!(
        merchant_oid = %payload.merchant_oid,
        code = ?payload.failed_reason_code,
        msg  = ?payload.failed_reason_msg,
        "Ödeme başarısız"
    );

    // Başarısız ödeme email bildirimi
    if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
        if let Some(p) = payment {
            // Kullanıcı emailini abonelik üzerinden bul
            if let Some(sub_id) = p.subscription_id {
                if let Ok(Some(sub)) = subscription_repo::find_by_id(&state.db, sub_id).await {
                    let to = sub.user_email.as_deref().unwrap_or("");
                    if !to.is_empty() {
                        let failed_so_far: i32 = sqlx::query_scalar(
                            "SELECT COALESCE(failed_payment_attempts,0) FROM customers WHERE member_id=$1",
                        )
                        .bind(p.member_id)
                        .fetch_one(&state.db)
                        .await
                        .unwrap_or(0);
                        let remaining = (state.config.max_failed_attempts - failed_so_far).max(0);

                        let (subject, html) = email::tpl_payment_failed(
                            &sub.plan,
                            payload.failed_reason_msg.as_deref(),
                            remaining.max(0),
                            &email_cfg.site_url,
                        );
                        email::send(mailer, email_cfg, to, subject, html).await;
                    }
                }
            }
        }
    }

    Ok(())
}

/// PayTR'dan güncel kart listesini çeker.
async fn fetch_paytr_cards(state: &crate::AppData, utoken: &str) -> Result<Vec<CardItem>, AppError> {
    let paytr_token = generate_card_list_token(
        utoken,
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    let body: serde_json::Value = state
        .http
        .post(PAYTR_CARD_LIST_ENDPOINT)
        .form(&[
            ("merchant_id", state.config.merchant_id.as_str()),
            ("utoken",      utoken),
            ("paytr_token", paytr_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR kart listesi hatası: {}", e))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR kart listesi parse hatası: {}", e))?;

    // Hata durumunda PayTR {"status":"error","err_msg":"..."} objesi döner.
    // Başarıda doğrudan kart array'i döner: [{...}, ...].
    if body.get("status").and_then(|s| s.as_str()) == Some("error") {
        let msg = body.get("err_msg").and_then(|v| v.as_str()).unwrap_or("Bilinmeyen hata");
        return Err(AppError::PaytrError(msg.to_string()));
    }

    let cards: Vec<CardItem> = serde_json::from_value(body)
        .map_err(|e| anyhow::anyhow!("Kart parse hatası: {}", e))?;

    Ok(cards)
}

/// Fatura döngüsüne göre bitiş ve sonraki ödeme tarihlerini hesaplar.
fn billing_dates(
    billing_cycle: &str,
    from: chrono::NaiveDateTime,
) -> (chrono::NaiveDateTime, chrono::NaiveDateTime) {
    let expires_at = if billing_cycle == "yearly" {
        from + Months::new(12)
    } else {
        from + Months::new(1)
    };
    // Sonraki ödeme = bitiş günü (aynı gün yenilenir)
    (expires_at, expires_at)
}
