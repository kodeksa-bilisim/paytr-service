use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use crate::{
    crypto::generate_payment_token,
    db::{customer_repo, payment_repo, subscription_repo},
    error::AppError,
    models::payment::{
        BasketItem, CancelScheduleRequest, EnterpriseInitRequest, InitPaymentRequest,
        InitPaymentResponse, PaytrFormParams, ScheduleDowngradeRequest, ScheduleDowngradeResponse,
        UpgradeQuoteRequest, UpgradeQuoteResponse,
    },
    paytr_client,
    pricing::{
        amount_to_kurus, enterprise_price_kurus, format_tl, plan_amount_kurus, plan_rank,
        unused_credit_kurus, MIN_CHARGE_KURUS,
    },
    AppState,
};

/// Redirect URL'nin güvenli olduğunu doğrular: yalnızca https:// kabul edilir.
fn validate_redirect_url(url: &str, field: &str) -> Result<(), AppError> {
    if !url.starts_with("https://") {
        return Err(AppError::BadRequest(format!(
            "{field} yalnızca https:// ile başlayan URL olabilir"
        )));
    }
    Ok(())
}

/// Plan ile tutar uyumunu doğrular — frontend manipülasyonunu önler. İstekteki tutar planın
/// liste fiyatıdır; yükseltmede tahsil edilen tutar sunucuda ayrıca hesaplanır.
fn validate_plan_amount(plan: &str, billing_cycle: &str, amount: &str) -> Result<i64, AppError> {
    let expected = plan_amount_kurus(plan, billing_cycle)
        .ok_or_else(|| AppError::BadRequest(format!("Geçersiz plan: {}", plan)))?;
    if amount != format_tl(expected) {
        return Err(AppError::BadRequest(format!(
            "Tutar plan ile uyuşmuyor (beklenen: {} TL)",
            format_tl(expected)
        )));
    }
    Ok(expected)
}

/// Ödeme koşulları: aylık/yıllık, TL, tek çekim. Tutar `validate_plan_amount` ile döneme
/// göre ayrıca doğrulanır (yıllık abonelik aylık fiyata alınamaz).
fn validate_payment_terms(
    billing_cycle: &str,
    currency: &str,
    payment_type: &str,
    installment_count: u8,
) -> Result<(), AppError> {
    if billing_cycle != "monthly" && billing_cycle != "yearly" {
        return Err(AppError::BadRequest("Geçersiz faturalandırma dönemi".to_string()));
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

/// Yeni satın alma / yükseltme için tahsil edilecek tutar.
struct Charge {
    /// Planın dönem liste fiyatı (yenilemelerde bu tutar çekilir).
    list_kurus: i64,
    /// Mevcut aboneliğin kullanılmamış kısmının değeri (yükseltmede düşülür).
    credit_kurus: i64,
    /// Bu ödemede tahsil edilen: liste − kredi.
    charge_kurus: i64,
    /// Yükseltilen abonelik (plan, bitiş).
    from: Option<(i32, String, Option<chrono::NaiveDateTime>)>,
}

impl Charge {
    /// Abonelik metadata'sına yazılan yükseltme bilgisi (callback yalnızca bu aboneliğin
    /// yerini almaya izin verir).
    fn metadata(&self) -> Option<serde_json::Value> {
        self.from.as_ref().map(|(id, _, _)| {
            serde_json::json!({
                "upgrade_from": id,
                "list_amount": format_tl(self.list_kurus),
                "credit_amount": format_tl(self.credit_kurus),
            })
        })
    }
}

/// Fark ücreti: üyenin geçerli aboneliği (aktif ya da süresi dolmamış iptal) varsa, yalnızca
/// daha üst bir plana geçilebilir ve eski dönemin kullanılmamış değeri yeni plan tutarından
/// düşülür; yeni dönem ödemeyle birlikte başlar. (Eskiden kalan süre olduğu gibi üst plana
/// aktarılıyordu: Silver yıllık + Gold aylık ile ~13 ay Gold alınabiliyordu.)
async fn compute_charge(state: &crate::AppData, member_id: i32, target_plan: &str, list_kurus: i64) -> Result<Charge, AppError> {
    let Some(cur) = subscription_repo::find_live(&state.db, member_id)
        .await
        .map_err(anyhow::Error::from)?
    else {
        return Ok(Charge { list_kurus, credit_kurus: 0, charge_kurus: list_kurus, from: None });
    };

    if plan_rank(target_plan) <= plan_rank(&cur.plan) {
        let msg = match (cur.status.as_str(), plan_rank(target_plan) == plan_rank(&cur.plan)) {
            ("cancelled", true) => "İptal ettiğiniz abonelik dönem sonuna kadar geçerli. Devam etmek için iptali geri alın.",
            ("cancelled", false) => "Mevcut aboneliğiniz dönem sonuna kadar geçerli; daha düşük bir plana dönem bitiminden sonra geçebilirsiniz.",
            _ => "Bu plan ya da daha üstü zaten aktif. Plan düşürmek için plan değişikliğini kullanın.",
        };
        return Err(AppError::BadRequest(msg.to_string()));
    }

    // Eski aboneliğin yenileme ödemesi sürüyorsa bekle: aksi halde hem yenileme hem yükseltme tahsil edilir.
    if payment_repo::has_pending(&state.db, cur.id).await.map_err(anyhow::Error::from)? {
        return Err(AppError::BadRequest(
            "Mevcut aboneliğinizin yenileme ödemesi işleniyor; lütfen birkaç dakika sonra tekrar deneyin.".to_string(),
        ));
    }

    let now = chrono::Utc::now().naive_utc();
    let credit_kurus = match (amount_to_kurus(&cur.amount), cur.expires_at) {
        (Some(paid), Some(exp)) => unused_credit_kurus(paid, &cur.billing_cycle, exp, now),
        _ => 0,
    };
    let charge_kurus = list_kurus - credit_kurus;
    if charge_kurus < MIN_CHARGE_KURUS {
        return Err(AppError::BadRequest(format!(
            "Mevcut aboneliğinizin kalan değeri ({} TL) seçilen planın tutarını karşılıyor; daha uzun bir dönem seçin.",
            format_tl(credit_kurus)
        )));
    }
    Ok(Charge { list_kurus, credit_kurus, charge_kurus, from: Some((cur.id, cur.plan, cur.expires_at)) })
}

/// Liste fiyatından farklı tahsilatta (yükseltme, indirim, kredi) sepet tek kalem: tahsil
/// edilen tutar (PayTR sepeti tutarla uyuşmalı).
fn adjusted_basket(label: &str, charge: &Charge, adj: &crate::growth::Adjusted) -> Vec<BasketItem> {
    let mut notes = Vec::new();
    if charge.from.is_some() {
        notes.push("yükseltme, kalan süre düşüldü");
    }
    if adj.discount_kurus > 0 {
        notes.push("indirimli");
    }
    if adj.credit_used_kurus > 0 {
        notes.push("bakiye kullanıldı");
    }
    vec![BasketItem { name: format!("{label} ({})", notes.join(", ")), price: format_tl(adj.final_kurus), quantity: 1 }]
}

/// Abonelik metadata'sı: yükseltme bilgisi + indirim/kredi.
fn subscription_metadata(base: Option<serde_json::Value>, adj: &crate::growth::Adjusted) -> Option<serde_json::Value> {
    let mut m = match base {
        Some(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    if let Some(extra) = adj.metadata() {
        m.extend(extra);
    }
    (!m.is_empty()).then_some(serde_json::Value::Object(m))
}

/// PayTR `debug_on` yalnızca test modunda iletilir (canlıda hata ayrıntısı kullanıcıya gösterilmesin).
fn debug_flag(state: &crate::AppData, requested: Option<u8>) -> Option<u8> {
    if state.config.test_mode == 1 { requested } else { None }
}

/// POST /api/v1/subscriptions/upgrade-quote — ödeme öncesi kullanıcıya gösterilecek tutar.
pub async fn upgrade_quote(
    State(state): State<AppState>,
    Json(req): Json<UpgradeQuoteRequest>,
) -> Result<impl IntoResponse, AppError> {
    if req.member_id <= 0 {
        return Err(AppError::BadRequest("Geçersiz member_id".to_string()));
    }
    let list_kurus = if req.plan == "enterprise" {
        enterprise_price_kurus(
            req.users.unwrap_or(1),
            req.extra_links.unwrap_or(0),
            req.extra_clicks.unwrap_or(0),
            &req.billing_cycle,
        )
        .map_err(AppError::BadRequest)?
    } else {
        plan_amount_kurus(&req.plan, &req.billing_cycle)
            .ok_or_else(|| AppError::BadRequest(format!("Geçersiz plan ya da dönem: {} / {}", req.plan, req.billing_cycle)))?
    };
    let c = compute_charge(&state, req.member_id, &req.plan, list_kurus).await?;
    let adj = crate::growth::adjust(&state, req.member_id, &req.plan, &req.billing_cycle, req.coupon_code.as_deref(), c.charge_kurus).await?;
    let renewal_kurus = list_kurus - adj.discount.as_ref().filter(|d| d.applies_to_renewal()).map_or(0, |d| d.amount_off(list_kurus));
    Ok(Json(UpgradeQuoteResponse {
        list_amount: format_tl(c.list_kurus),
        credit_amount: format_tl(c.credit_kurus),
        charge_amount: format_tl(adj.final_kurus),
        from_plan: c.from.as_ref().map(|(_, p, _)| p.clone()),
        from_expires_at: c.from.as_ref().and_then(|(_, _, e)| e.map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())),
        discount_amount: format_tl(adj.discount_kurus),
        discount_source: adj.discount.as_ref().map(|d| d.source.clone()),
        coupon_code: adj.discount.as_ref().and_then(|d| d.code.clone()),
        discount_cycles: adj.discount.as_ref().and_then(|d| d.cycles_left.map(|n| n + 1)),
        balance_used: format_tl(adj.credit_used_kurus),
        renewal_amount: format_tl(renewal_kurus),
    }))
}

pub async fn init_payment(
    State(state): State<AppState>,
    Json(req): Json<InitPaymentRequest>,
) -> Result<impl IntoResponse, AppError> {
    let list_kurus = validate_plan_amount(&req.plan, &req.billing_cycle, &req.payment_amount)?;
    validate_payment_terms(&req.billing_cycle, &req.currency, &req.payment_type, req.installment_count)?;
    validate_redirect_url(&req.merchant_ok_url, "merchant_ok_url")?;
    validate_redirect_url(&req.merchant_fail_url, "merchant_fail_url")?;
    if req.member_id <= 0 {
        return Err(AppError::BadRequest("Geçersiz member_id".to_string()));
    }
    let member_id = req.member_id;

    // Aynı ya da daha düşük plan yeni ödemeyle alınamaz (düşük plana geçiş schedule-downgrade
    // ile); üst plana geçişte eski dönemin kalan değeri düşülür.
    let charge = compute_charge(&state, member_id, &req.plan, list_kurus).await?;
    let adj = crate::growth::adjust(&state, member_id, &req.plan, &req.billing_cycle, req.coupon_code.as_deref(), charge.charge_kurus).await?;
    let charge_amount = format_tl(adj.final_kurus);

    let basket_items = if adj.final_kurus != charge.list_kurus {
        adjusted_basket(&format!("{} Plan", crate::email_templates::plan_label(&req.plan)), &charge, &adj)
    } else {
        req.user_basket
    };
    let user_basket = encode_basket(&basket_items)
        .map_err(|e| AppError::BadRequest(format!("Sepet hatası: {}", e)))?;

    // Mevcut pending aboneliği temizle (tekrar tıklama / modal yeniden açma)
    // Eski pending'in iptali ve yeni kayıtlar tek transaction'da, üye kilidi altında.
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    subscription_repo::lock_member(&mut *tx, member_id)
        .await
        .map_err(anyhow::Error::from)?;
    subscription_repo::cancel_pending(&mut *tx, member_id)
        .await
        .map_err(anyhow::Error::from)?;

    // Pending abonelik: tutarı liste fiyatı (yenilemelerde çekilen).
    let subscription = subscription_repo::create(
        &mut *tx,
        member_id,
        &req.plan,
        &req.billing_cycle,
        &req.payment_amount,
        &req.currency,
        &req.user_phone,
        &req.email,
        subscription_metadata(charge.metadata(), &adj),
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
        &charge_amount,
        &req.payment_type,
        &installment_str,
        &req.currency,
        &test_mode_str,
        "0", // 3D Secure: non_3d=0
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    // Pending ödeme kaydı: tahsil edilen tutar (callback'teki tutar kontrolü buna göre).
    let payment = payment_repo::create(
        &mut *tx,
        payment_repo::NewPayment {
            member_id,
            subscription_id: Some(subscription.id),
            merchant_oid: &req.merchant_oid,
            amount: &charge_amount,
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
    tx.commit().await.map_err(anyhow::Error::from)?;

    tracing::info!(
        member_id,
        merchant_oid = %req.merchant_oid,
        plan = %req.plan,
        charge = %charge_amount,
        credit = %format_tl(charge.credit_kurus),
        "İlk ödeme başlatıldı (3DS)"
    );

    Ok((
        StatusCode::OK,
        Json(InitPaymentResponse {
            payment_id: payment.id,
            subscription_id: subscription.id,
            paytr_endpoint: paytr_client::payment_endpoint(),
            list_amount: format_tl(charge.list_kurus),
            credit_amount: format_tl(charge.credit_kurus),
            discount_amount: format_tl(adj.discount_kurus + adj.credit_used_kurus),
            form_params: PaytrFormParams {
                merchant_id: state.config.merchant_id.clone(),
                paytr_token,
                user_ip: req.user_ip,
                merchant_oid: req.merchant_oid,
                email: req.email,
                payment_type: req.payment_type,
                payment_amount: charge_amount,
                installment_count: req.installment_count,
                no_installment: 1,
                max_installment: 0,
                currency: req.currency,
                test_mode: state.config.test_mode,
                non_3d: 0,
                store_card: 1, // Her zaman kartı sakla
                user_name: req.user_name,
                user_address: req.user_address,
                // PayTR telefonu zorunlu tutar; müşterinin numarası yoksa şirket iletişim hattı.
                user_phone: if req.user_phone.trim().is_empty() { state.config.fallback_phone.clone() } else { req.user_phone },
                user_basket,
                merchant_ok_url: req.merchant_ok_url,
                merchant_fail_url: req.merchant_fail_url,
                lang: req.client_lang,
                card_type: req.card_type,
                debug_on: debug_flag(&state, req.debug_on),
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

    let list_kurus = enterprise_price_kurus(req.users, req.extra_links, req.extra_clicks, &req.billing_cycle)
        .map_err(AppError::BadRequest)?;
    let list_amount = format_tl(list_kurus);

    // Enterprise zaten geçerliyse yeni ödeme alınmaz; alt plandan geçişte kalan değer düşülür.
    let charge = compute_charge(&state, member_id, "enterprise", list_kurus).await?;
    let adj = crate::growth::adjust(&state, member_id, "enterprise", &req.billing_cycle, req.coupon_code.as_deref(), charge.charge_kurus).await?;
    let charge_amount = format_tl(adj.final_kurus);

    let basket_label = format!(
        "Enterprise Plan{} ({} kullanıcı, {}k link/ay, {}k tıklama/ay)",
        if req.billing_cycle == "yearly" { " - Yıllık" } else { "" },
        req.users,
        10 + req.extra_links,
        100 + req.extra_clicks * 10,
    );
    let basket_items = if adj.final_kurus != charge.list_kurus {
        adjusted_basket(&basket_label, &charge, &adj)
    } else {
        vec![BasketItem { name: basket_label, price: charge_amount.clone(), quantity: 1 }]
    };
    let user_basket = encode_basket(&basket_items)
        .map_err(|e| AppError::BadRequest(format!("Sepet hatası: {}", e)))?;

    let mut metadata = serde_json::json!({
        "users": req.users,
        "extra_links": req.extra_links,
        "extra_clicks": req.extra_clicks,
    });
    if let (Some(serde_json::Value::Object(up)), Some(m)) = (charge.metadata(), metadata.as_object_mut()) {
        m.extend(up);
    }
    let metadata = subscription_metadata(Some(metadata), &adj).unwrap_or_default();

    // Eski pending'in iptali ve yeni kayıtlar tek transaction'da, üye kilidi altında.
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    subscription_repo::lock_member(&mut *tx, member_id)
        .await
        .map_err(anyhow::Error::from)?;
    subscription_repo::cancel_pending(&mut *tx, member_id)
        .await
        .map_err(anyhow::Error::from)?;

    let subscription = subscription_repo::create(
        &mut *tx,
        member_id,
        "enterprise",
        &req.billing_cycle,
        &list_amount,
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
        &charge_amount,
        "card",
        &installment_str,
        "TL",
        &test_mode_str,
        "0",
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    let payment = payment_repo::create(
        &mut *tx,
        payment_repo::NewPayment {
            member_id,
            subscription_id: Some(subscription.id),
            merchant_oid: &req.merchant_oid,
            amount: &charge_amount,
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
    tx.commit().await.map_err(anyhow::Error::from)?;

    tracing::info!(
        member_id,
        merchant_oid = %req.merchant_oid,
        amount = %charge_amount,
        credit = %format_tl(charge.credit_kurus),
        "Enterprise ödeme başlatıldı"
    );

    Ok((
        StatusCode::OK,
        Json(InitPaymentResponse {
            payment_id: payment.id,
            subscription_id: subscription.id,
            paytr_endpoint: paytr_client::payment_endpoint(),
            list_amount,
            credit_amount: format_tl(charge.credit_kurus),
            discount_amount: format_tl(adj.discount_kurus + adj.credit_used_kurus),
            form_params: PaytrFormParams {
                merchant_id: state.config.merchant_id.clone(),
                paytr_token,
                user_ip: req.user_ip,
                merchant_oid: req.merchant_oid,
                email: req.email,
                payment_type: "card".to_string(),
                payment_amount: charge_amount,
                installment_count: 0,
                no_installment: 1,
                max_installment: 0,
                currency: "TL".to_string(),
                test_mode: state.config.test_mode,
                non_3d: 0,
                store_card: 1,
                user_name: req.user_name,
                user_address: "Online".to_string(),
                user_phone: state.config.fallback_phone.clone(),
                user_basket,
                merchant_ok_url: req.merchant_ok_url,
                merchant_fail_url: req.merchant_fail_url,
                lang: req.client_lang,
                card_type: req.card_type,
                debug_on: debug_flag(&state, req.debug_on),
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
        // Yeni planın tutarı mevcut aboneliğin dönemine göre (yıllık abonelik yıllık yenilenir).
        let new_amount = plan_amount_kurus(&new_plan, &active_sub.billing_cycle)
            .map(format_tl)
            .ok_or_else(|| AppError::BadRequest(format!("Geçersiz plan: {}", req.new_plan)))?;
        subscription_repo::set_scheduled_downgrade(&state.db, active_sub.id, &new_plan, &new_amount)
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
        assert!(validate_plan_amount("silver", "monthly", "349.00").is_ok());
        assert!(validate_plan_amount("gold", "monthly", "899.00").is_ok());
        assert!(validate_plan_amount("gold", "monthly", "299.00").is_err());
        assert!(validate_plan_amount("enterprise", "monthly", "1.00").is_err());
        assert!(validate_plan_amount("Gold", "monthly", "899.00").is_err());
    }

    #[test]
    fn yearly_is_twelve_months_minus_twenty_percent() {
        assert!(validate_plan_amount("silver", "yearly", "3350.40").is_ok());
        assert!(validate_plan_amount("gold", "yearly", "8630.40").is_ok());
        // Yıllık abonelik aylık fiyata alınamaz
        assert!(validate_plan_amount("gold", "yearly", "899.00").is_err());
        assert!(validate_plan_amount("gold", "weekly", "899.00").is_err());
    }

    #[test]
    fn payment_terms() {
        assert!(validate_payment_terms("monthly", "TL", "card", 0).is_ok());
        assert!(validate_payment_terms("yearly", "TL", "card", 0).is_ok());
        assert!(validate_payment_terms("weekly", "TL", "card", 0).is_err());
        assert!(validate_payment_terms("monthly", "USD", "card", 0).is_err());
        assert!(validate_payment_terms("monthly", "TL", "eft", 0).is_err());
        assert!(validate_payment_terms("monthly", "TL", "card", 3).is_err());
    }

    #[test]
    fn upgrade_metadata_names_its_source() {
        let c = Charge { list_kurus: 89_900, credit_kurus: 17_450, charge_kurus: 72_450, from: Some((7, "silver".into(), None)) };
        let m = c.metadata().unwrap();
        assert_eq!(m["upgrade_from"], 7);
        assert_eq!(m["credit_amount"], "174.50");
        let adj = crate::growth::Adjusted { discount: None, discount_kurus: 0, credit_used_kurus: 0, final_kurus: c.charge_kurus };
        let basket = adjusted_basket("Gold Plan", &c, &adj);
        assert_eq!(basket[0].price, "724.50");
        assert_eq!(basket[0].name, "Gold Plan (yükseltme, kalan süre düşüldü)");
        let fresh = Charge { list_kurus: 89_900, credit_kurus: 0, charge_kurus: 89_900, from: None };
        assert!(fresh.metadata().is_none());
    }

    #[test]
    fn discount_goes_into_subscription_metadata() {
        let d = crate::growth::referral_discount();
        let adj = crate::growth::Adjusted { discount: Some(d.clone()), discount_kurus: 17_980, credit_used_kurus: 500, final_kurus: 71_420 };
        let m = subscription_metadata(Some(serde_json::json!({ "users": 3 })), &adj).unwrap();
        assert_eq!(m["users"], 3);
        assert_eq!(m["discount_kurus"], 17_980);
        assert_eq!(m["credit_used_kurus"], 500);
        assert_eq!(serde_json::from_value::<crate::pricing::Discount>(m["discount"].clone()).unwrap(), d);
        assert!(subscription_metadata(None, &crate::growth::Adjusted::default()).is_none());
    }
}
