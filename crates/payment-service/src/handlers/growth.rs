//! Büyüme uçları (iç API, X-Internal-Token): ücretsiz deneme, referans, kupon yönetimi.
//! `member_id` Next.js'te oturumdan alınır; tarayıcıdan gelen değere güvenilmez.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{Duration, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    db::{customer_repo, growth_repo, subscription_repo},
    error::AppError,
    pricing::{format_tl, normalize_coupon_code, plan_amount_kurus, REFERRAL_DISCOUNT_PCT},
    AppState,
};

const TRIAL_PLAN: &str = "gold";
const TRIAL_DAYS: i64 = 7;

/// GET /api/v1/members/{member_id}/growth — deneme hakkı/durumu, referans kodu ve istatistik,
/// kredi bakiyesi, davetle gelip ilk ödemesini bekleyen üyenin indirimi.
pub async fn summary(State(state): State<AppState>, Path(member_id): Path<i32>) -> Result<Json<Value>, AppError> {
    let trial: Option<(chrono::NaiveDateTime, String)> = sqlx::query_as(
        "SELECT expires_at, status FROM paytr_subscriptions
         WHERE member_id = $1 AND COALESCE((metadata->>'trial')::boolean, false)
         ORDER BY id DESC LIMIT 1",
    )
    .bind(member_id)
    .fetch_optional(&state.db)
    .await
    .map_err(anyhow::Error::from)?;
    let code = growth_repo::referral_code(&state.db, member_id).await?;
    let stats = growth_repo::referral_stats(&state.db, member_id).await?;
    let credit = growth_repo::credit_balance(&state.db, member_id).await?;
    let referred = growth_repo::has_unpaid_referral(&state.db, member_id).await?;
    Ok(Json(json!({
        "trial": {
            "eligible": subscription_repo::trial_eligible(&state.db, member_id).await?,
            "plan": TRIAL_PLAN,
            "days": TRIAL_DAYS,
            "active": trial.as_ref().is_some_and(|(e, s)| s == "active" && *e > Utc::now().naive_utc()),
            "expires_at": trial.map(|(e, _)| e.format("%Y-%m-%dT%H:%M:%S").to_string()),
        },
        "referral": { "code": code, "signed_up": stats.signed_up, "paid": stats.paid, "rewarded": stats.rewarded },
        "credit": format_tl(credit.max(0)),
        "referral_discount_percent": referred.then_some(REFERRAL_DISCOUNT_PCT),
    })))
}

#[derive(Deserialize)]
pub struct TrialRequest {
    member_id: i32,
    email: String,
}

/// POST /api/v1/trials/start — kartsız 7 gün Gold; hiç aboneliği/denemesi olmamış üyeye ve posta
/// kutusuna bir kez. E-posta doğrulaması Next.js'te de, burada da kontrol edilir.
pub async fn start_trial(State(state): State<AppState>, Json(req): Json<TrialRequest>) -> Result<Json<Value>, AppError> {
    if req.member_id <= 0 {
        return Err(AppError::BadRequest("Geçersiz member_id".into()));
    }
    let list = plan_amount_kurus(TRIAL_PLAN, "monthly").ok_or_else(|| anyhow::anyhow!("deneme planı fiyatı yok"))?;
    let now = Utc::now().naive_utc();
    let expires_at = now + Duration::days(TRIAL_DAYS);

    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    subscription_repo::lock_member(&mut *tx, req.member_id).await?;
    // Doğrulanmamış e-posta (yalnızca e-postayla kayıtta `false`; OAuth/eski hesap NULL) deneme açamaz.
    let verified: Option<bool> = sqlx::query_scalar("SELECT email_verified IS NOT FALSE FROM customers WHERE member_id = $1")
        .bind(req.member_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    match verified {
        None => return Err(AppError::BadRequest("Üye bulunamadı.".into())),
        Some(false) => return Err(AppError::BadRequest("Ücretsiz deneme için önce e-posta adresinizi doğrulayın.".into())),
        Some(true) => {}
    }
    let eligible: bool = sqlx::query_scalar(&format!("SELECT {}", subscription_repo::trial_eligible_sql()))
        .bind(req.member_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    if !eligible {
        return Err(AppError::BadRequest("Ücretsiz deneme yalnızca daha önce abonelik ya da deneme kullanmamış hesaplarda geçerlidir.".into()));
    }
    let sub_id = subscription_repo::create_trial(&mut *tx, req.member_id, TRIAL_PLAN, &format_tl(list), &req.email, now, expires_at).await?;
    customer_repo::set_trial(&mut *tx, req.member_id, TRIAL_PLAN, sub_id, expires_at).await?;
    tx.commit().await.map_err(anyhow::Error::from)?;

    tracing::info!(member_id = req.member_id, subscription_id = sub_id, "Ücretsiz deneme başladı");
    Ok(Json(json!({ "plan": TRIAL_PLAN, "expires_at": expires_at.format("%Y-%m-%dT%H:%M:%S").to_string() })))
}

#[derive(Deserialize)]
pub struct ClaimRequest {
    member_id: i32,
    code: String,
}

/// POST /api/v1/referrals/claim — davet bağlantısıyla gelen üye (çerezdeki kod). Hiç ödemesi
/// olmamış ve daha önce davet edilmemiş üye için; geçersiz kod sessizce yok sayılır.
pub async fn claim_referral(State(state): State<AppState>, Json(req): Json<ClaimRequest>) -> Result<Json<Value>, AppError> {
    let code = req.code.trim().to_uppercase();
    if code.is_empty() || code.len() > 16 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(Json(json!({ "claimed": false })));
    }
    let Some(referrer) = growth_repo::referrer_by_code(&state.db, &code).await? else {
        return Ok(Json(json!({ "claimed": false })));
    };
    let claimed = growth_repo::claim_referral(&state.db, req.member_id, referrer).await?;
    if claimed {
        tracing::info!(referred = req.member_id, referrer, "Referans kaydedildi");
    }
    Ok(Json(json!({ "claimed": claimed })))
}

// ── Kupon yönetimi (yönetici; yetki Next.js'te) ───────────────────────────────

/// GET /api/v1/admin/coupons
pub async fn list_coupons(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "items": growth_repo::list_coupons(&state.db).await? })))
}

#[derive(Deserialize)]
pub struct NewCouponRequest {
    code: String,
    /// "percent" | "fixed"
    kind: String,
    /// percent: 1–100; fixed: TL ("150" ya da "150.00")
    value: String,
    #[serde(default)]
    plans: Option<Vec<String>>,
    #[serde(default)]
    billing_cycles: Option<Vec<String>>,
    #[serde(default)]
    duration_cycles: Option<i32>,
    #[serde(default)]
    max_redemptions: Option<i32>,
    /// "YYYY-MM-DD" (gün sonuna kadar, Türkiye saati)
    #[serde(default)]
    valid_until: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

/// POST /api/v1/admin/coupons
pub async fn create_coupon(State(state): State<AppState>, Json(req): Json<NewCouponRequest>) -> Result<Json<Value>, AppError> {
    let bad = |m: &str| AppError::BadRequest(m.to_string());
    let code = normalize_coupon_code(&req.code).ok_or_else(|| bad("Kod 3–40 karakter olmalı: harf, rakam, - ve _"))?;
    let value: i32 = match req.kind.as_str() {
        "percent" => req.value.trim().parse().ok().filter(|v| (1..=100).contains(v)).ok_or_else(|| bad("Yüzde 1–100 arasında olmalı"))?,
        "fixed" => crate::pricing::tl_to_kurus(req.value.trim())
            .filter(|k| *k > 0 && *k < i32::MAX as i64)
            .map(|k| k as i32)
            .ok_or_else(|| bad("Geçerli bir TL tutarı girin"))?,
        _ => return Err(bad("Tür 'percent' ya da 'fixed' olmalı")),
    };
    let clean = |v: Option<Vec<String>>, allowed: &[&str]| -> Result<Option<Vec<String>>, AppError> {
        match v.filter(|x| !x.is_empty()) {
            None => Ok(None),
            Some(list) if list.iter().all(|x| allowed.contains(&x.as_str())) => Ok(Some(list)),
            Some(_) => Err(bad("Geçersiz plan ya da dönem")),
        }
    };
    let plans = clean(req.plans, &["silver", "gold", "enterprise"])?;
    let billing_cycles = clean(req.billing_cycles, &["monthly", "yearly"])?;
    if req.duration_cycles.is_some_and(|d| d < 1) || req.max_redemptions.is_some_and(|m| m < 1) {
        return Err(bad("Süre ve kullanım sınırı en az 1 olmalı"));
    }
    let valid_until = match req.valid_until.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => {
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| bad("Tarih YYYY-AA-GG olmalı"))?;
            // Türkiye saatiyle gün sonu = 20:59:59 UTC.
            Some(d.and_hms_opt(20, 59, 59).expect("geçerli saat"))
        }
    };
    let created = growth_repo::create_coupon(
        &state.db,
        growth_repo::NewCoupon {
            code: &code,
            kind: &req.kind,
            value,
            plans,
            billing_cycles,
            duration_cycles: req.duration_cycles,
            max_redemptions: req.max_redemptions,
            valid_until,
            note: req.note.map(|n| n.chars().take(200).collect()),
        },
    )
    .await?;
    if !created {
        return Err(bad("Bu kod zaten var"));
    }
    tracing::info!(code = %code, "Kupon oluşturuldu");
    Ok(Json(json!({ "code": code })))
}

#[derive(Deserialize)]
pub struct ActiveRequest {
    active: bool,
}

/// POST /api/v1/admin/coupons/{code}/active
pub async fn set_coupon_active(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Json(req): Json<ActiveRequest>,
) -> Result<Json<Value>, AppError> {
    let code = normalize_coupon_code(&code).ok_or_else(|| AppError::BadRequest("Geçersiz kod".into()))?;
    if !growth_repo::set_coupon_active(&state.db, &code, req.active).await? {
        return Err(AppError::BadRequest("Kupon bulunamadı".into()));
    }
    Ok(Json(json!({ "code": code, "active": req.active })))
}
