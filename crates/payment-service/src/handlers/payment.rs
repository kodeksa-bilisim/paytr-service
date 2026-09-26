use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use crate::{
    crypto::generate_payment_token,
    db::{customer_repo, payment_repo, subscription_repo},
    error::AppError,
    models::payment::{
        BasketItem, CancelScheduleRequest, EnterpriseInitRequest, InitPaymentRequest,
        InitPaymentResponse, PaytrFormParams, ScheduleDowngradeRequest, ScheduleDowngradeResponse,
    },
    paytr_client,
    AppState,
};

/// Planın sıralaması: düşük = düşük plan. Upgrade/downgrade tespiti için.
fn plan_rank(plan: &str) -> u8 {
    match plan.to_lowercase().as_str() {
        "silver"     => 1,
        "gold"       => 2,
        "enterprise" => 3,
        _            => 0, // standard / free
    }
}

/// Plan adına karşılık gelen aylık TL tutarı.
fn plan_amount_tl(plan: &str) -> Option<&'static str> {
    match plan.to_lowercase().as_str() {
        "silver" => Some("149.00"),
        "gold"   => Some("299.00"),
        _        => None,
    }
}

/// Redirect URL'nin güvenli olduğunu doğrular: yalnızca https:// kabul edilir.
fn validate_redirect_url(url: &str, field: &str) -> Result<(), AppError> {
    if !url.starts_with("https://") {
        return Err(AppError::BadRequest(format!(
            "{field} yalnızca https:// ile başlayan URL olabilir"
        )));
    }
    Ok(())
}

/// Plan ile tutar uyumunu doğrular — frontend manipülasyonunu önler.
fn validate_plan_amount(plan: &str, amount: &str) -> Result<(), AppError> {
    let expected = match plan {
        "silver" => "149.00",
        "gold"   => "299.00",
        _ => return Err(AppError::BadRequest(format!("Geçersiz plan: {}", plan))),
    };
    if amount != expected {
        return Err(AppError::BadRequest(format!(
            "Tutar plan ile uyuşmuyor (beklenen: {} TL)",
            expected
        )));
    }
    Ok(())
}

/// Ödeme koşulları: fiyat tablosu yalnızca aylık/TL/tek çekim için tanımlı. Aksi halde
/// ör. "yearly" ile aylık fiyata 12 ay alınabiliyordu.
fn validate_payment_terms(
    billing_cycle: &str,
    currency: &str,
    payment_type: &str,
    installment_count: u8,
) -> Result<(), AppError> {
    if billing_cycle != "monthly" {
        return Err(AppError::BadRequest("Yalnızca aylık abonelik destekleniyor".to_string()));
    }
    if currency != "TL" {
        return Err(AppError::BadRequest("Yalnızca TL destekleniyor".to_string()));
    }
    if payment_type != "card" {
        return Err(AppError::BadRequest("Yalnızca kart ile ödeme destekleniyor".to_string()));
    }
    if installment_count != 0 {
        return Err(AppError::BadRequest("Taksit desteklenmiyor".to_string()));
    }
    Ok(())
}

const ENTERPRISE_MAX_USERS: i32 = 100;
const ENTERPRISE_MAX_EXTRA: i32 = 10_000;

/// Enterprise aylık fiyatı (kuruş). Negatif/aşırı değerler reddedilir — aksi halde
/// `extra_links=-9` gibi değerlerle fiyat düşürülebiliyordu.
fn enterprise_price_kurus(users: i32, extra_links: i32, extra_clicks: i32) -> Result<i64, AppError> {
    if !(1..=ENTERPRISE_MAX_USERS).contains(&users) {
        return Err(AppError::BadRequest(format!(
            "Kullanıcı sayısı 1–{} arasında olmalı",
            ENTERPRISE_MAX_USERS
        )));
    }
    if !(0..=ENTERPRISE_MAX_EXTRA).contains(&extra_links) || !(0..=ENTERPRISE_MAX_EXTRA).contains(&extra_clicks) {
        return Err(AppError::BadRequest(format!(
            "Ek link/tıklama paketi 0–{} arasında olmalı",
            ENTERPRISE_MAX_EXTRA
        )));
    }
    Ok(99_900
        + (users as i64 - 1) * 15_000
        + extra_links as i64 * 10_000
        + extra_clicks as i64 * 5_000)
}

pub async fn init_payment(
    State(state): State<AppState>,
    Json(req): Json<InitPaymentRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_plan_amount(&req.plan, &req.payment_amount)?;
    validate_payment_terms(&req.billing_cycle, &req.currency, &req.payment_type, req.installment_count)?;
    validate_redirect_url(&req.merchant_ok_url, "merchant_ok_url")?;
    validate_redirect_url(&req.merchant_fail_url, "merchant_fail_url")?;
    if req.member_id <= 0 {
        return Err(AppError::BadRequest("Geçersiz member_id".to_string()));
    }
    let member_id = req.member_id;

    let user_basket = encode_basket(&req.user_basket)
        .map_err(|e| AppError::BadRequest(format!("Sepet hatası: {}", e)))?;

    // Upgrade ise mevcut aktif aboneliğin bitiş tarihini metadata'ya kaydet (kalan süre aktarılır).
    // Aynı ya da daha düşük plan yeni ödemeyle alınamaz (kalan süre sessizce yanardı);
    // düşük plana geçiş schedule-downgrade ile yapılır.
    let active_sub = subscription_repo::find_active(&state.db, member_id)
        .await
        .map_err(anyhow::Error::from)?;

    let metadata = match active_sub.as_ref() {
        Some(sub) if plan_rank(&req.plan) <= plan_rank(&sub.plan) => {
            return Err(AppError::BadRequest(
                "Bu plan ya da daha üstü zaten aktif. Plan düşürmek için plan değişikliğini kullanın.".to_string(),
            ));
        }
        Some(sub) => sub.expires_at.map(|exp| {
            serde_json::json!({ "previous_expires_at": exp.format("%Y-%m-%dT%H:%M:%S").to_string() })
        }),
        None => None,
    };

    // Mevcut pending aboneliği temizle (tekrar tıklama / modal yeniden açma)
    subscription_repo::cancel_pending(&state.db, member_id)
        .await
        .map_err(anyhow::Error::from)?;

    // Pending abonelik oluştur
    let subscription = subscription_repo::create(
        &state.db,
        member_id,
        &req.plan,
        &req.billing_cycle,
        &req.payment_amount,
        &req.currency,
        &req.user_phone,
        &req.email,
        metadata,
    )
    .await
    .map_err(anyhow::Error::from)?;

    let installment_str = req.installment_count.to_string();
    let test_mode_str = state.config.test_mode.to_string();

    let paytr_token = generate_payment_token(
        &state.config.merchant_id,
        &req.user_ip,
        &req.merchant_oid,
        &req.email,
        &req.payment_amount,
        &req.payment_type,
        &installment_str,
        &req.currency,
        &test_mode_str,
        "0", // 3D Secure: non_3d=0
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    // Pending ödeme kaydı oluştur
    let payment = payment_repo::create(
        &state.db,
        payment_repo::NewPayment {
            member_id,
            subscription_id: Some(subscription.id),
            merchant_oid: &req.merchant_oid,
            amount: &req.payment_amount,
            currency: &req.currency,
            payment_type: &req.payment_type,
            installment_count: req.installment_count as i32,
            is_3d: true,
            test_mode: state.config.test_mode == 1,
            utoken: None,
            ctoken: None,
        },
    )
    .await
    .map_err(anyhow::Error::from)?;

    tracing::info!(
        member_id,
        merchant_oid = %req.merchant_oid,
        plan = %req.plan,
        "İlk ödeme başlatıldı (3DS)"
    );

    Ok((
        StatusCode::OK,
        Json(InitPaymentResponse {
            payment_id: payment.id,
            subscription_id: subscription.id,
            paytr_endpoint: paytr_client::payment_endpoint(),
            form_params: PaytrFormParams {
                merchant_id: state.config.merchant_id.clone(),
                paytr_token,
                user_ip: req.user_ip,
                merchant_oid: req.merchant_oid,
                email: req.email,
                payment_type: req.payment_type,
                payment_amount: req.payment_amount.clone(),
                installment_count: req.installment_count,
                no_installment: 1,
                max_installment: 0,
                currency: req.currency,
                test_mode: state.config.test_mode,
                non_3d: 0,
                store_card: 1, // Her zaman kartı sakla
                user_name: req.user_name,
                user_address: req.user_address,
                user_phone: req.user_phone,
                user_basket,
                merchant_ok_url: req.merchant_ok_url,
                merchant_fail_url: req.merchant_fail_url,
                lang: req.client_lang,
                card_type: req.card_type,
                debug_on: req.debug_on,
            },
        }),
    ))
}

pub async fn init_enterprise_payment(
    State(state): State<AppState>,
    Json(req): Json<EnterpriseInitRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_redirect_url(&req.merchant_ok_url, "merchant_ok_url")?;
    validate_redirect_url(&req.merchant_fail_url, "merchant_fail_url")?;
    if req.member_id <= 0 {
        return Err(AppError::BadRequest("Geçersiz member_id".to_string()));
    }
    let member_id = req.member_id;

    let total_kurus = enterprise_price_kurus(req.users, req.extra_links, req.extra_clicks)?;
    let payment_amount = format!("{}.{:02}", total_kurus / 100, total_kurus % 100); // TL cinsinden

    // Enterprise zaten aktifse yeni ödeme alınmaz (kalan süre yanardı).
    if let Some(sub) = subscription_repo::find_active(&state.db, member_id)
        .await
        .map_err(anyhow::Error::from)?
    {
        if plan_rank(&sub.plan) >= plan_rank("enterprise") {
            return Err(AppError::BadRequest("Enterprise aboneliğiniz zaten aktif.".to_string()));
        }
    }

    let basket_label = format!(
        "Enterprise Plan ({} kullanıcı, {}k link/ay, {}k tıklama/ay)",
        req.users,
        10 + req.extra_links,
        100 + req.extra_clicks * 10,
    );
    let basket_items = vec![BasketItem {
        name: basket_label,
        price: payment_amount.clone(),
        quantity: 1,
    }];
    let user_basket = encode_basket(&basket_items)
        .map_err(|e| AppError::BadRequest(format!("Sepet hatası: {}", e)))?;

    let metadata = serde_json::json!({
        "users": req.users,
        "extra_links": req.extra_links,
        "extra_clicks": req.extra_clicks,
    });

    subscription_repo::cancel_pending(&state.db, member_id)
        .await
        .map_err(anyhow::Error::from)?;

    let subscription = subscription_repo::create(
        &state.db,
        member_id,
        "enterprise",
        "monthly",
        &payment_amount,
        "TL",
        "",
        &req.email,
        Some(metadata),
    )
    .await
    .map_err(anyhow::Error::from)?;

    let installment_str = "0".to_string();
    let test_mode_str = state.config.test_mode.to_string();

    let paytr_token = generate_payment_token(
        &state.config.merchant_id,
        &req.user_ip,
        &req.merchant_oid,
        &req.email,
        &payment_amount,
        "card",
        &installment_str,
        "TL",
        &test_mode_str,
        "0",
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    let payment = payment_repo::create(
        &state.db,
        payment_repo::NewPayment {
            member_id,
            subscription_id: Some(subscription.id),
            merchant_oid: &req.merchant_oid,
            amount: &payment_amount,
            currency: "TL",
            payment_type: "card",
            installment_count: 0,
            is_3d: true,
            test_mode: state.config.test_mode == 1,
            utoken: None,
            ctoken: None,
        },
    )
    .await
    .map_err(anyhow::Error::from)?;

    tracing::info!(
        member_id,
        merchant_oid = %req.merchant_oid,
        amount = %payment_amount,
        "Enterprise ödeme başlatıldı"
    );

    Ok((
        StatusCode::OK,
        Json(InitPaymentResponse {
            payment_id: payment.id,
            subscription_id: subscription.id,
            paytr_endpoint: paytr_client::payment_endpoint(),
            form_params: PaytrFormParams {
                merchant_id: state.config.merchant_id.clone(),
                paytr_token,
                user_ip: req.user_ip,
                merchant_oid: req.merchant_oid,
                email: req.email,
                payment_type: "card".to_string(),
                payment_amount,
                installment_count: 0,
                no_installment: 1,
                max_installment: 0,
                currency: "TL".to_string(),
                test_mode: state.config.test_mode,
                non_3d: 0,
                store_card: 1,
                user_name: req.user_name,
                user_address: "Online".to_string(),
                user_phone: "5305861333".to_string(),
                user_basket,
                merchant_ok_url: req.merchant_ok_url,
                merchant_fail_url: req.merchant_fail_url,
                lang: req.client_lang,
                card_type: req.card_type,
                debug_on: req.debug_on,
            },
        }),
    ))
}

/// Downgrade planlama: ödeme alınmaz, mevcut plan dönem sonuna kadar devam eder.
/// "standard" (ücretsiz) hedefi, dönem sonunda sona eren bir iptaldir.
pub async fn schedule_downgrade(
    State(state): State<AppState>,
    Json(req): Json<ScheduleDowngradeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let new_plan = req.new_plan.to_lowercase();
    let new_rank = plan_rank(&new_plan);

    let active_sub = subscription_repo::find_active(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::BadRequest("Aktif abonelik bulunamadı".to_string()))?;

    if plan_rank(&active_sub.plan) <= new_rank {
        return Err(AppError::BadRequest(
            "Bu işlem yalnızca daha düşük bir plana geçiş için geçerlidir".to_string(),
        ));
    }

    if new_plan == "standard" {
        // Ücretsiz plana geçiş: otomatik yenileme durur, erişim expires_at'e kadar sürer.
        let cancelled = subscription_repo::cancel_by_member(&state.db, active_sub.id, req.member_id)
            .await
            .map_err(anyhow::Error::from)?;
        if !cancelled {
            return Err(AppError::BadRequest("Aktif abonelik bulunamadı".to_string()));
        }
        customer_repo::set_subscription_cancelled(&state.db, req.member_id)
            .await
            .map_err(anyhow::Error::from)?;
    } else {
        let new_amount = plan_amount_tl(&new_plan)
            .ok_or_else(|| AppError::BadRequest(format!("Geçersiz plan: {}", req.new_plan)))?;
        subscription_repo::set_scheduled_downgrade(&state.db, active_sub.id, &new_plan, new_amount)
            .await
            .map_err(anyhow::Error::from)?;
    }

    customer_repo::set_scheduled_plan(&state.db, req.member_id, &new_plan)
        .await
        .map_err(anyhow::Error::from)?;

    let effective_date = active_sub
        .expires_at
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string());

    tracing::info!(
        member_id = req.member_id,
        current_plan = %active_sub.plan,
        new_plan = %new_plan,
        effective_date = ?effective_date,
        "Downgrade planlandı"
    );

    Ok((StatusCode::OK, Json(ScheduleDowngradeResponse { scheduled: true, effective_date })))
}

/// Planlanmış downgrade'i iptal eder; mevcut plan dönem sonunda yenilenir.
/// Ücretsiz plana geçiş planlandıysa (abonelik iptal edilmişti) iptal geri alınır.
pub async fn cancel_scheduled_downgrade(
    State(state): State<AppState>,
    Json(req): Json<CancelScheduleRequest>,
) -> Result<impl IntoResponse, AppError> {
    match subscription_repo::find_active(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?
    {
        Some(active_sub) => {
            subscription_repo::cancel_scheduled(&state.db, active_sub.id)
                .await
                .map_err(anyhow::Error::from)?;
        }
        None => {
            let reactivated = subscription_repo::reactivate_by_member(&state.db, req.member_id)
                .await
                .map_err(anyhow::Error::from)?;
            if !reactivated {
                return Err(AppError::BadRequest("Aktif abonelik bulunamadı".to_string()));
            }
            customer_repo::set_subscription_reactivated(&state.db, req.member_id)
                .await
                .map_err(anyhow::Error::from)?;
        }
    }

    customer_repo::clear_scheduled_plan(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?;

    tracing::info!(member_id = req.member_id, "Planlanmış downgrade iptal edildi");

    Ok((StatusCode::OK, Json(serde_json::json!({ "cancelled": true }))))
}

/// PayTR sepet formatı: htmlEntities(JSON.stringify([["Ürün Adı", "Fiyat", Adet], ...]))
fn encode_basket(items: &[BasketItem]) -> anyhow::Result<String> {
    let raw: Vec<[serde_json::Value; 3]> = items
        .iter()
        .map(|item| {
            [
                serde_json::Value::String(item.name.clone()),
                serde_json::Value::String(item.price.clone()),
                serde_json::Value::Number(item.quantity.into()),
            ]
        })
        .collect();
    let json = serde_json::to_string(&raw)?;
    Ok(json
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_amounts_are_fixed() {
        assert!(validate_plan_amount("silver", "149.00").is_ok());
        assert!(validate_plan_amount("gold", "299.00").is_ok());
        assert!(validate_plan_amount("gold", "149.00").is_err());
        assert!(validate_plan_amount("enterprise", "1.00").is_err());
        assert!(validate_plan_amount("Gold", "299.00").is_err());
    }

    #[test]
    fn only_monthly_tl_card_single_payment() {
        assert!(validate_payment_terms("monthly", "TL", "card", 0).is_ok());
        assert!(validate_payment_terms("yearly", "TL", "card", 0).is_err());
        assert!(validate_payment_terms("monthly", "USD", "card", 0).is_err());
        assert!(validate_payment_terms("monthly", "TL", "eft", 0).is_err());
        assert!(validate_payment_terms("monthly", "TL", "card", 3).is_err());
    }

    #[test]
    fn enterprise_price_rejects_negative_and_huge() {
        assert_eq!(enterprise_price_kurus(1, 0, 0).unwrap(), 99_900);
        assert!(enterprise_price_kurus(1, -9, -1).is_err());
        assert!(enterprise_price_kurus(0, 0, 0).is_err());
        assert!(enterprise_price_kurus(-5, 0, 0).is_err());
        assert!(enterprise_price_kurus(101, 0, 0).is_err());
        assert!(enterprise_price_kurus(1, 10_001, 0).is_err());
        assert!(enterprise_price_kurus(1, 0, i32::MAX).is_err());
    }

    #[test]
    fn enterprise_amount_matches_historic_payment() {
        // 30 Haziran'daki gerçek ödeme: users=2, extra_links=333, extra_clicks=1333 → 101099.00 TL
        let k = enterprise_price_kurus(2, 333, 1333).unwrap();
        assert_eq!(format!("{}.{:02}", k / 100, k % 100), "101099.00");
    }

    #[test]
    fn plan_ranks() {
        assert!(plan_rank("gold") > plan_rank("silver"));
        assert!(plan_rank("enterprise") > plan_rank("gold"));
        assert_eq!(plan_rank("standard"), 0);
    }
}
