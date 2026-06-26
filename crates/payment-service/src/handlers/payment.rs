use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use crate::{
    crypto::generate_payment_token,
    db::{customer_repo, payment_repo, subscription_repo},
    error::AppError,
    models::payment::{
        BasketItem, CancelScheduleRequest, EnterpriseInitRequest, InitPaymentRequest,
        InitPaymentResponse, PaytrFormParams, ScheduleDowngradeRequest, ScheduleDowngradeResponse,
        StoredCardPaymentRequest, StoredCardPaymentResponse,
    },
    paytr_client::PAYTR_PAYMENT_ENDPOINT,
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

/// Plan adına karşılık gelen TL tutarı döner.
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

pub async fn init_payment(
    State(state): State<AppState>,
    Json(req): Json<InitPaymentRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Plan ve tutarı doğrula
    validate_plan_amount(&req.plan, &req.payment_amount)?;
    validate_redirect_url(&req.merchant_ok_url, "merchant_ok_url")?;
    validate_redirect_url(&req.merchant_fail_url, "merchant_fail_url")?;

    // member_id gönderilmemişse email'den bul
    let member_id = match req.member_id {
        Some(id) if id > 0 => id,
        _ => customer_repo::find_member_id_by_email(&state.db, &req.email)
            .await
            .map_err(anyhow::Error::from)?
            .ok_or_else(|| AppError::BadRequest(format!("Kullanıcı bulunamadı: {}", req.email)))?,
    };

    let user_basket = encode_basket(&req.user_basket)
        .map_err(|e| AppError::BadRequest(format!("Sepet hatası: {}", e)))?;

    // Upgrade ise mevcut aktif aboneliğin bitiş tarihini metadata'ya kaydet (kalan süre aktarılır).
    let active_sub = subscription_repo::find_active(&state.db, member_id)
        .await
        .map_err(anyhow::Error::from)?;

    let metadata = active_sub.as_ref().and_then(|sub| {
        let is_upgrade = plan_rank(&req.plan) > plan_rank(&sub.plan);
        if is_upgrade {
            sub.expires_at.map(|exp| {
                serde_json::json!({ "previous_expires_at": exp.format("%Y-%m-%dT%H:%M:%S").to_string() })
            })
        } else {
            None
        }
    });

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
            paytr_endpoint: PAYTR_PAYMENT_ENDPOINT.to_string(),
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
                utoken: req.utoken,
                card_type: req.card_type,
                debug_on: req.debug_on,
            },
        }),
    ))
}

/// Kayıtlı kart ile abonelik yenileme (Non-3D, server-to-server, sync_mode=1).
pub async fn stored_card_payment(
    State(state): State<AppState>,
    Json(req): Json<StoredCardPaymentRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_plan_amount(&req.plan, &req.payment_amount)?;

    // subscription_id'nin bu üyeye ait olduğunu doğrula
    let sub = subscription_repo::find_by_id(&state.db, req.subscription_id)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::BadRequest("Abonelik bulunamadı".to_string()))?;
    if sub.member_id != req.member_id {
        return Err(AppError::BadRequest("Abonelik bu kullanıcıya ait değil".to_string()));
    }

    if req.require_cvv == 1 && req.cvv.is_none() {
        return Err(AppError::BadRequest("Bu kart için CVV zorunludur".to_string()));
    }

    let user_basket = encode_basket(&req.user_basket)
        .map_err(|e| AppError::BadRequest(format!("Sepet hatası: {}", e)))?;

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
        "1", // Non-3D: subscription ödemesi
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    // Pending ödeme kaydı oluştur
    let payment = payment_repo::create(
        &state.db,
        payment_repo::NewPayment {
            member_id: req.member_id,
            subscription_id: Some(req.subscription_id),
            merchant_oid: &req.merchant_oid,
            amount: &req.payment_amount,
            currency: &req.currency,
            payment_type: &req.payment_type,
            installment_count: req.installment_count as i32,
            is_3d: false,
            test_mode: state.config.test_mode == 1,
            utoken: Some(&req.utoken),
            ctoken: Some(&req.ctoken),
        },
    )
    .await
    .map_err(anyhow::Error::from)?;

    // PayTR'a doğrudan POST (sync_mode=1)
    let ok_url = format!("{}/api/v1/payments/ok", state.config.base_url);
    let fail_url = format!("{}/api/v1/payments/fail", state.config.base_url);

    let mut form = vec![
        ("merchant_id",       state.config.merchant_id.as_str()),
        ("paytr_token",       paytr_token.as_str()),
        ("user_ip",           req.user_ip.as_str()),
        ("merchant_oid",      req.merchant_oid.as_str()),
        ("email",             req.email.as_str()),
        ("payment_type",      req.payment_type.as_str()),
        ("payment_amount",    req.payment_amount.as_str()),
        ("installment_count", installment_str.as_str()),
        ("currency",          req.currency.as_str()),
        ("test_mode",         test_mode_str.as_str()),
        ("non_3d",            "1"),
        ("utoken",            req.utoken.as_str()),
        ("ctoken",            req.ctoken.as_str()),
        ("require_cvv",       if req.require_cvv == 1 { "1" } else { "0" }),
        ("user_name",         req.user_name.as_str()),
        ("user_address",      req.user_address.as_str()),
        ("user_phone",        req.user_phone.as_str()),
        ("user_basket",       user_basket.as_str()),
        ("merchant_ok_url",   ok_url.as_str()),
        ("merchant_fail_url", fail_url.as_str()),
        ("lang",              req.client_lang.as_str()),
        ("no_installment",    "1"),
        ("max_installment",   "0"),
        ("sync_mode",         "1"),
    ];

    let cvv_val;
    if let Some(ref cvv) = req.cvv {
        cvv_val = cvv.clone();
        form.push(("cvv", cvv_val.as_str()));
    }
    let card_type_val;
    if let Some(ref ct) = req.card_type {
        card_type_val = ct.clone();
        form.push(("card_type", card_type_val.as_str()));
    }
    let debug_on_val;
    if let Some(d) = req.debug_on {
        debug_on_val = d.to_string();
        form.push(("debug_on", debug_on_val.as_str()));
    }

    let sync_resp = state
        .http
        .post(PAYTR_PAYMENT_ENDPOINT)
        .form(&form)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR bağlantı hatası: {}", e))?
        .json::<PaytrSyncResponse>()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR sync yanıt hatası: {}", e))?;

    tracing::info!(
        member_id = req.member_id,
        merchant_oid = %req.merchant_oid,
        paytr_status = %sync_resp.status,
        "Kayıtlı kart ödemesi sync sonucu"
    );

    Ok((
        StatusCode::OK,
        Json(StoredCardPaymentResponse {
            payment_id: payment.id,
            subscription_id: req.subscription_id,
            merchant_oid: req.merchant_oid,
            paytr_status: sync_resp.status,
            paytr_message: sync_resp.msg,
        }),
    ))
}

#[derive(Deserialize)]
struct PaytrSyncResponse {
    status: String,
    msg: Option<String>,
}


pub async fn init_enterprise_payment(
    State(state): State<AppState>,
    Json(req): Json<EnterpriseInitRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_redirect_url(&req.merchant_ok_url, "merchant_ok_url")?;
    validate_redirect_url(&req.merchant_fail_url, "merchant_fail_url")?;

    let member_id = match req.member_id {
        Some(id) if id > 0 => id,
        _ => customer_repo::find_member_id_by_email(&state.db, &req.email)
            .await
            .map_err(anyhow::Error::from)?
            .ok_or_else(|| AppError::BadRequest(format!("Kullanıcı bulunamadı: {}", req.email)))?,
    };

    let extra_users = (req.users - 1).max(0) as i64;
    let total_kurus: i64 = 99900
        + extra_users * 15000
        + (req.extra_links as i64) * 10000
        + (req.extra_clicks as i64) * 5000;
    let payment_amount = format!("{:.2}", total_kurus as f64 / 100.0); // TL cinsinden

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
            paytr_endpoint: PAYTR_PAYMENT_ENDPOINT.to_string(),
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
                user_address: String::new(),
                user_phone: String::new(),
                user_basket,
                merchant_ok_url: req.merchant_ok_url,
                merchant_fail_url: req.merchant_fail_url,
                lang: req.client_lang,
                utoken: req.utoken,
                card_type: req.card_type,
                debug_on: req.debug_on,
            },
        }),
    ))
}

/// Downgrade planlama: ödeme alınmaz, mevcut plan dönem sonuna kadar devam eder.
pub async fn schedule_downgrade(
    State(state): State<AppState>,
    Json(req): Json<ScheduleDowngradeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let new_rank = plan_rank(&req.new_plan);
    let new_amount = plan_amount_tl(&req.new_plan)
        .ok_or_else(|| AppError::BadRequest(format!("Geçersiz plan: {}", req.new_plan)))?;

    let active_sub = subscription_repo::find_active(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::BadRequest("Aktif abonelik bulunamadı".to_string()))?;

    if plan_rank(&active_sub.plan) <= new_rank {
        return Err(AppError::BadRequest(
            "Bu işlem yalnızca daha düşük bir plana geçiş için geçerlidir".to_string(),
        ));
    }

    subscription_repo::set_scheduled_downgrade(&state.db, active_sub.id, &req.new_plan, new_amount)
        .await
        .map_err(anyhow::Error::from)?;

    customer_repo::set_scheduled_plan(&state.db, req.member_id, &req.new_plan)
        .await
        .map_err(anyhow::Error::from)?;

    let effective_date = active_sub
        .expires_at
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string());

    tracing::info!(
        member_id = req.member_id,
        current_plan = %active_sub.plan,
        new_plan = %req.new_plan,
        effective_date = ?effective_date,
        "Downgrade planlandı"
    );

    Ok((StatusCode::OK, Json(ScheduleDowngradeResponse { scheduled: true, effective_date })))
}

/// Planlanmış downgrade'i iptal eder; mevcut plan dönem sonunda yenilenir.
pub async fn cancel_scheduled_downgrade(
    State(state): State<AppState>,
    Json(req): Json<CancelScheduleRequest>,
) -> Result<impl IntoResponse, AppError> {
    let active_sub = subscription_repo::find_active(&state.db, req.member_id)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::BadRequest("Aktif abonelik bulunamadı".to_string()))?;

    subscription_repo::cancel_scheduled(&state.db, active_sub.id)
        .await
        .map_err(anyhow::Error::from)?;

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
