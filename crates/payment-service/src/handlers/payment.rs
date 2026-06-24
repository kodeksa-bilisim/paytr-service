use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;

use crate::{
    crypto::generate_payment_token,
    db::{customer_repo, payment_repo, subscription_repo},
    error::AppError,
    models::payment::{
        BasketItem, InitPaymentRequest, InitPaymentResponse, PaytrFormParams,
        StoredCardPaymentRequest, StoredCardPaymentResponse,
    },
    paytr_client::PAYTR_PAYMENT_ENDPOINT,
    AppState,
};

/// Plan ile tutar uyumunu doğrular — frontend manipülasyonunu önler.
fn validate_plan_amount(plan: &str, amount: &str) -> Result<(), AppError> {
    let expected = match plan {
        "silver" => "14900",
        "gold"   => "29900",
        _ => return Err(AppError::BadRequest(format!("Geçersiz plan: {}", plan))),
    };
    if amount != expected {
        return Err(AppError::BadRequest(format!(
            "Tutar plan ile uyuşmuyor (beklenen: {} kuruş)",
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
                payment_amount: req.payment_amount,
                installment_count: req.installment_count,
                no_installment: 1,
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
                client_lang: req.client_lang,
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
        ("client_lang",       req.client_lang.as_str()),
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


/// PayTR sepet formatı: JSON.stringify([["Ürün Adı", "Fiyat", Adet], ...])
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
    Ok(serde_json::to_string(&raw)?)
}
