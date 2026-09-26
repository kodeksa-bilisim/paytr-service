use axum::{extract::State, response::IntoResponse, Form};
use chrono::Months;

use crate::{
    crypto::{generate_card_list_token, verify_callback_hash},
    db::{card_repo, customer_repo, payment_repo, subscription_repo},
    email, email_templates,
    error::AppError,
    models::{
        callback::{CallbackPayload, PaymentStatus},
        card::CardItem,
    },
    paytr_client,
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

/// Kayıtlı TL tutarını kuruşa çevirir ("149.00" → 14900). Eski (noktasız) kayıtlar
/// kuruş kabul edilir.
fn amount_to_kurus(amount: &str) -> Option<i64> {
    let a = amount.trim();
    match a.split_once('.') {
        Some((tl, kr)) if !kr.is_empty() && kr.len() <= 2 => {
            let tl: i64 = tl.parse().ok()?;
            let kr: i64 = format!("{:0<2}", kr).parse().ok()?;
            Some(tl * 100 + kr)
        }
        Some(_) => None,
        None => a.parse().ok(),
    }
}

async fn handle_success(state: &crate::AppData, payload: &CallbackPayload) -> Result<(), AppError> {
    // "Bulunamadı" durumunda OK döndür — PayTR'nin yeniden denemesi bu durumu düzeltemez.
    // Gerçek DB hatalarında Err döner, PayTR yeniden dener (geçici hata kurtarma).
    let Some(payment) = payment_repo::find_by_oid(&state.db, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?
    else {
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Callback geldi fakat ödeme kaydı bulunamadı — görmezden geliniyor");
        return Ok(());
    };
    if payment.status == "success" {
        tracing::info!(merchant_oid = %payload.merchant_oid, "Tekrar callback, zaten işlendi");
        return Ok(());
    }

    // Savunma derinliği: tahsil edilen tutar (kuruş, taksit farkı dahil) beklenenden az olamaz.
    match (amount_to_kurus(&payment.amount), payload.total_amount.trim().parse::<i64>()) {
        (Some(expected), Ok(paid)) if paid < expected => {
            tracing::error!(
                merchant_oid = %payload.merchant_oid, expected, paid,
                "Tahsil edilen tutar beklenenden düşük — aktivasyon yapılmadı, inceleme gerekli"
            );
            payment_repo::set_review(&state.db, &payload.merchant_oid, "amount_mismatch")
                .await
                .map_err(anyhow::Error::from)?;
            return Ok(());
        }
        (Some(_), Ok(_)) => {}
        _ => tracing::warn!(
            merchant_oid = %payload.merchant_oid,
            amount = %payment.amount, total_amount = %payload.total_amount,
            "Tutar karşılaştırılamadı"
        ),
    }

    let member_id = payment.member_id;

    // Kart senkronizasyonu (PayTR HTTP) transaction dışında ve hata olsa da devam eder —
    // para çekilmişken kart listesi alınamadı diye abonelik aktifleşmemesin.
    let utoken: Option<String> = match payload.utoken.as_deref().filter(|u| !u.is_empty()) {
        Some(ut) => {
            if let Err(e) = card_repo::upsert_user_token(&state.db, member_id, ut).await {
                tracing::error!(member_id, error = %e, "utoken kaydedilemedi");
            }
            match fetch_paytr_cards(state, ut).await {
                Ok(cards) => match card_repo::sync_cards(&state.db, ut, &cards).await {
                    Ok(()) => tracing::info!(member_id, cards = cards.len(), "Kart listesi senkronize edildi"),
                    Err(e) => tracing::error!(member_id, error = %e, "Kart listesi DB'ye yazılamadı"),
                },
                Err(e) => tracing::error!(
                    member_id, error = ?e,
                    "PayTR kart listesi alınamadı — abonelik yine de aktifleştiriliyor"
                ),
            }
            Some(ut.to_string())
        }
        None => card_repo::get_user_token(&state.db, member_id)
            .await
            .map_err(anyhow::Error::from)?
            .map(|t| t.utoken),
    };
    let default_ctoken = match &utoken {
        Some(ut) => card_repo::get_default_card(&state.db, ut)
            .await
            .map_err(anyhow::Error::from)?
            .map(|c| c.ctoken),
        None => None,
    }
    .or_else(|| payment.ctoken.clone());

    // Abonelik + müşteri + ödeme durumu tek transaction'da; ödeme satırı kilitli.
    // Ara adımda hata olursa hiçbiri yazılmaz, PayTR yeniden dener (eskiden abonelik
    // aktifleşip ödeme pending kalıyor, retry'da "yenileme" sanılıp ek ay veriliyordu).
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;

    let Some(payment) = payment_repo::find_by_oid_for_update(&mut *tx, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?
    else {
        return Ok(());
    };
    if payment.status == "success" {
        // Eşzamanlı çift callback: diğeri bizden önce işledi.
        tracing::info!(merchant_oid = %payload.merchant_oid, "Eşzamanlı tekrar callback, zaten işlendi");
        return Ok(());
    }

    let Some(subscription_id) = payment.subscription_id else {
        payment_repo::set_success(&mut *tx, &payload.merchant_oid)
            .await
            .map_err(anyhow::Error::from)?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Subscription ID yok, sadece ödeme kaydedildi");
        return Ok(());
    };

    let sub = subscription_repo::find_by_id_for_update(&mut *tx, subscription_id)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::BadRequest("Abonelik kaydı bulunamadı".to_string()))?;

    let now = chrono::Utc::now().naive_utc();

    // Hiç aktif olmamış abonelik = ilk ödeme (kullanıcı yeni ödeme başlatınca
    // `cancel_pending` ile iptal edilmiş eski pending abonelik de dahil).
    let is_first_payment = sub.started_at.is_none();

    // Upgrade: mevcut aboneliğin kalan süresi yeni plana aktarılır.
    let upgrade_start = if is_first_payment {
        sub.metadata.as_ref()
            .and_then(|m| m.get("previous_expires_at")?.as_str())
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
            .map(|exp| exp.max(now))
    } else {
        None
    };

    // Yenileme dönemi eski bitişe sabitlenir (grace süresinde geç tahsil edilse de kayma ve
    // bedava gün olmaz). Abonelik grace sonrası expire olmuşsa yeni dönem şimdiden başlar.
    let period_start = if is_first_payment {
        upgrade_start.unwrap_or(now)
    } else if sub.status == "expired" {
        now
    } else {
        sub.expires_at.unwrap_or(now)
    };

    let (expires_at, next_payment_date) = billing_dates(&sub.billing_cycle, period_start);

    // Yenilemede planlanmış downgrade uygulanır.
    let effective_plan = if is_first_payment {
        sub.plan.clone()
    } else {
        sub.scheduled_plan.clone().unwrap_or_else(|| sub.plan.clone())
    };

    if is_first_payment {
        subscription_repo::activate(
            &mut *tx,
            subscription_id,
            utoken.as_deref().unwrap_or(""),
            default_ctoken.as_deref().unwrap_or(""),
            now,
            expires_at,
            next_payment_date,
        )
        .await
        .map_err(anyhow::Error::from)?;

        // Upgrade: önceki aboneliği `replaced` yap, diğer pending'leri iptal et.
        subscription_repo::replace_others(&mut *tx, member_id, subscription_id)
            .await
            .map_err(anyhow::Error::from)?;
    } else {
        subscription_repo::renew(
            &mut *tx,
            subscription_id,
            sub.scheduled_plan.as_deref(),
            expires_at,
            next_payment_date,
        )
        .await
        .map_err(anyhow::Error::from)?;
    }

    let custom_plan = if effective_plan == "enterprise" {
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

    // Kullanıcı yenileme tahsil edildikten sonra iptal ettiyse iptal durumu korunur.
    let customer_status = if !is_first_payment && sub.status == "cancelled" { "cancelled" } else { "active" };

    customer_repo::set_subscription_active(
        &mut *tx,
        member_id,
        &effective_plan,
        subscription_id,
        expires_at,
        next_payment_date,
        custom_plan,
        customer_status,
    )
    .await
    .map_err(anyhow::Error::from)?;

    payment_repo::set_success(&mut *tx, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?;

    tx.commit().await.map_err(anyhow::Error::from)?;

    tracing::info!(
        member_id,
        merchant_oid = %payload.merchant_oid,
        plan = %effective_plan,
        first = is_first_payment,
        expires_at = %expires_at,
        "Abonelik güncellendi"
    );

    // Email bildirimi — hata olursa callback'i etkilemez
    if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
        let to = sub.user_email.as_deref().unwrap_or("");
        if !to.is_empty() {
            let name = customer_repo::find_name(&state.db, member_id).await;
            let content = email_templates::payment_success(
                &email_templates::PaymentInfo {
                    name: name.as_deref(),
                    plan: &effective_plan,
                    amount: &payment.amount,
                    expires_at,
                    order_no: &payload.merchant_oid,
                    site_url: &email_cfg.site_url,
                },
                is_first_payment,
                customer_status == "active",
            );
            email::send(mailer, email_cfg, to, content).await;
        }
    }

    Ok(())
}

async fn handle_failed(state: &crate::AppData, payload: &CallbackPayload) -> Result<(), AppError> {
    // Yalnızca pending ödeme etkilenir: PayTR'ın tekrar bildirimleri ya da scheduler'ın
    // sync yanıtında zaten işaretlediği ödeme ikinci kez sayılmaz.
    let Some(p) = payment_repo::set_failed(
        &state.db,
        &payload.merchant_oid,
        payload.failed_reason_code.as_deref(),
        payload.failed_reason_msg.as_deref(),
    )
    .await
    .map_err(anyhow::Error::from)?
    else {
        tracing::info!(merchant_oid = %payload.merchant_oid, "Başarısız bildirimi: ödeme zaten işlenmiş");
        return Ok(());
    };

    tracing::warn!(
        merchant_oid = %payload.merchant_oid,
        code = ?payload.failed_reason_code,
        msg  = ?payload.failed_reason_msg,
        "Ödeme başarısız"
    );

    // İlk ödeme (3DS) başarısızlıkları yenileme hakkından düşmez; yalnızca yenilemeler.
    if p.is_3d {
        return Ok(());
    }

    customer_repo::increment_failed_attempts(&state.db, p.member_id)
        .await
        .map_err(anyhow::Error::from)?;
    if let Some(sub_id) = p.subscription_id {
        subscription_repo::increment_renewal_attempts(&state.db, sub_id)
            .await
            .map_err(anyhow::Error::from)?;
    }

    // Başarısız yenileme email bildirimi
    if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
        if let Some(sub_id) = p.subscription_id {
            if let Ok(Some(sub)) = subscription_repo::find_by_id(&state.db, sub_id).await {
                let to = sub.user_email.as_deref().unwrap_or("");
                if !to.is_empty() {
                    let remaining = (state.config.max_failed_attempts - sub.renewal_attempts).max(0);
                    let name = customer_repo::find_name(&state.db, p.member_id).await;
                    let content = email_templates::payment_failed(
                        name.as_deref(),
                        &sub.plan,
                        &p.amount,
                        payload.failed_reason_msg.as_deref(),
                        remaining,
                        &email_cfg.site_url,
                    );
                    email::send(mailer, email_cfg, to, content).await;
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
        .post(paytr_client::card_list_endpoint())
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
/// Yalnızca aylık abonelik satılıyor (init'te doğrulanır); "yearly" eski/elle girilmiş
/// kayıtlar için korunur.
fn billing_dates(
    billing_cycle: &str,
    from: chrono::NaiveDateTime,
) -> (chrono::NaiveDateTime, chrono::NaiveDateTime) {
    let expires_at = if billing_cycle == "yearly" {
        from + Months::new(12)
    } else {
        from + Months::new(1)
    };
    // Sonraki ödeme = bitiş anı; ödeme alınamazsa grace süresince günlük tekrar denenir.
    (expires_at, expires_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_amounts_to_kurus() {
        assert_eq!(amount_to_kurus("149.00"), Some(14_900));
        assert_eq!(amount_to_kurus("101099.00"), Some(10_109_900));
        assert_eq!(amount_to_kurus("12.5"), Some(1_250));
        assert_eq!(amount_to_kurus("99900"), Some(99_900)); // eski kayıt: kuruş
        assert_eq!(amount_to_kurus("1.234"), None);
        assert_eq!(amount_to_kurus("abc"), None);
    }

    #[test]
    fn monthly_billing_adds_one_month() {
        let from = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap().and_hms_opt(10, 0, 0).unwrap();
        let (exp, next) = billing_dates("monthly", from);
        assert_eq!(exp.to_string(), "2026-02-28 10:00:00");
        assert_eq!(exp, next);
    }
}
