//! Kupon, referans ve kredi kayıtları (0012_growth).

use anyhow::Result;
use chrono::NaiveDateTime;
use serde::Serialize;
use sqlx::{PgExecutor, PgPool};

// ── Kupon ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Coupon {
    pub code: String,
    pub kind: String,
    pub value: i32,
    pub plans: Option<Vec<String>>,
    pub billing_cycles: Option<Vec<String>>,
    pub duration_cycles: Option<i32>,
    pub max_redemptions: Option<i32>,
    pub redemptions: i32,
    pub valid_until: Option<NaiveDateTime>,
    pub active: bool,
    pub note: Option<String>,
    pub created_at: NaiveDateTime,
}

pub async fn find_coupon<'e>(ex: impl PgExecutor<'e>, code: &str) -> Result<Option<Coupon>> {
    Ok(sqlx::query_as::<_, Coupon>("SELECT * FROM coupons WHERE code = $1").bind(code).fetch_optional(ex).await?)
}

pub async fn member_redeemed(pool: &PgPool, code: &str, member_id: i32) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM coupon_redemptions WHERE code = $1 AND member_id = $2)")
        .bind(code)
        .bind(member_id)
        .fetch_one(pool)
        .await?)
}

/// İlk ödeme başarılı: kullanım yazılır ve sayaç artar (aynı ödeme/üye için bir kez).
pub async fn record_redemption<'c>(
    tx: &mut sqlx::Transaction<'c, sqlx::Postgres>,
    code: &str,
    member_id: i32,
    subscription_id: i32,
    merchant_oid: &str,
    discount_kurus: i64,
) -> Result<()> {
    let r = sqlx::query(
        "INSERT INTO coupon_redemptions (code, member_id, subscription_id, merchant_oid, discount_kurus)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
    )
    .bind(code)
    .bind(member_id)
    .bind(subscription_id)
    .bind(merchant_oid)
    .bind(discount_kurus)
    .execute(&mut **tx)
    .await?;
    if r.rows_affected() > 0 {
        sqlx::query("UPDATE coupons SET redemptions = redemptions + 1 WHERE code = $1").bind(code).execute(&mut **tx).await?;
    }
    Ok(())
}

pub async fn list_coupons(pool: &PgPool) -> Result<Vec<Coupon>> {
    Ok(sqlx::query_as::<_, Coupon>("SELECT * FROM coupons ORDER BY created_at DESC LIMIT 500").fetch_all(pool).await?)
}

pub struct NewCoupon<'a> {
    pub code: &'a str,
    pub kind: &'a str,
    pub value: i32,
    pub plans: Option<Vec<String>>,
    pub billing_cycles: Option<Vec<String>>,
    pub duration_cycles: Option<i32>,
    pub max_redemptions: Option<i32>,
    pub valid_until: Option<NaiveDateTime>,
    pub note: Option<String>,
}

/// Yeni kupon; kod zaten varsa false.
pub async fn create_coupon(pool: &PgPool, c: NewCoupon<'_>) -> Result<bool> {
    let r = sqlx::query(
        "INSERT INTO coupons (code, kind, value, plans, billing_cycles, duration_cycles, max_redemptions, valid_until, note)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (code) DO NOTHING",
    )
    .bind(c.code)
    .bind(c.kind)
    .bind(c.value)
    .bind(c.plans)
    .bind(c.billing_cycles)
    .bind(c.duration_cycles)
    .bind(c.max_redemptions)
    .bind(c.valid_until)
    .bind(c.note)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn set_coupon_active(pool: &PgPool, code: &str, active: bool) -> Result<bool> {
    let r = sqlx::query("UPDATE coupons SET active = $2 WHERE code = $1").bind(code).bind(active).execute(pool).await?;
    Ok(r.rows_affected() > 0)
}

// ── Referans ─────────────────────────────────────────────────────────────────

/// Üyenin referans kodu; yoksa üretilir (8 karakter, karışmayan harf/rakam).
pub async fn referral_code(pool: &PgPool, member_id: i32) -> Result<String> {
    if let Some(c) = sqlx::query_scalar::<_, String>("SELECT code FROM referral_codes WHERE member_id = $1")
        .bind(member_id)
        .fetch_optional(pool)
        .await?
    {
        return Ok(c);
    }
    for _ in 0..5 {
        let code: Option<String> = sqlx::query_scalar(
            r#"
            INSERT INTO referral_codes (member_id, code)
            SELECT $1, string_agg(substr('ABCDEFGHJKLMNPQRSTUVWXYZ23456789', 1 + floor(random() * 32)::int, 1), '')
            FROM generate_series(1, 8)
            ON CONFLICT DO NOTHING
            RETURNING code
            "#,
        )
        .bind(member_id)
        .fetch_optional(pool)
        .await?;
        if let Some(c) = code {
            return Ok(c);
        }
        // Üye satırı eşzamanlı oluştuysa onu döndür; kod çakıştıysa yeniden dene.
        if let Some(c) = sqlx::query_scalar::<_, String>("SELECT code FROM referral_codes WHERE member_id = $1")
            .bind(member_id)
            .fetch_optional(pool)
            .await?
        {
            return Ok(c);
        }
    }
    anyhow::bail!("referans kodu üretilemedi")
}

pub async fn referrer_by_code(pool: &PgPool, code: &str) -> Result<Option<i32>> {
    Ok(sqlx::query_scalar("SELECT member_id FROM referral_codes WHERE code = $1")
        .bind(code.trim().to_uppercase())
        .fetch_optional(pool)
        .await?)
}

/// Davet ilişkisini kaydeder. Yalnızca hiç ücretli ödemesi olmamış ve daha önce davet edilmemiş
/// üye için; kendini davet edemez. Kaydedildiyse true.
pub async fn claim_referral(pool: &PgPool, referred_id: i32, referrer_id: i32) -> Result<bool> {
    if referred_id == referrer_id {
        return Ok(false);
    }
    let r = sqlx::query(
        r#"
        INSERT INTO referrals (referred_id, referrer_id)
        SELECT $1, $2
        WHERE EXISTS (SELECT 1 FROM customers WHERE member_id = $1)
          AND EXISTS (SELECT 1 FROM customers WHERE member_id = $2)
          AND NOT EXISTS (SELECT 1 FROM paytr_payments WHERE member_id = $1 AND status = 'success')
        ON CONFLICT (referred_id) DO NOTHING
        "#,
    )
    .bind(referred_id)
    .bind(referrer_id)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Üye davetle geldi ve henüz ilk ödemesini yapmadı mı? (ilk ödeme indirimi)
pub async fn has_unpaid_referral(pool: &PgPool, member_id: i32) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM referrals WHERE referred_id = $1 AND first_paid_at IS NULL)")
        .bind(member_id)
        .fetch_one(pool)
        .await?)
}

/// Davet edilenin ilk başarılı ödemesi (ödül 14 gün sonra).
pub async fn mark_referral_paid<'e>(ex: impl PgExecutor<'e>, referred_id: i32, merchant_oid: &str) -> Result<()> {
    sqlx::query("UPDATE referrals SET first_paid_at = NOW(), first_payment_oid = $2 WHERE referred_id = $1 AND first_paid_at IS NULL")
        .bind(referred_id)
        .bind(merchant_oid)
        .execute(ex)
        .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
pub struct DueReward {
    pub referred_id: i32,
    pub referrer_id: i32,
    pub first_payment_oid: Option<String>,
}

/// İlk ödemesinden `wait_days` geçmiş, ödüllendirilmemiş davetler.
pub async fn due_rewards(pool: &PgPool, wait_days: i32, limit: i64) -> Result<Vec<DueReward>> {
    Ok(sqlx::query_as::<_, DueReward>(
        "SELECT referred_id, referrer_id, first_payment_oid FROM referrals
         WHERE rewarded_at IS NULL AND first_paid_at < NOW() - make_interval(days => $1)
         ORDER BY first_paid_at LIMIT $2",
    )
    .bind(wait_days)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// Davet edenin son 365 günde aldığı ödül sayısı.
pub async fn rewards_last_year(pool: &PgPool, referrer_id: i32) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM referrals WHERE referrer_id = $1 AND reward IN ('extension', 'credit')
           AND rewarded_at > NOW() - INTERVAL '365 days'",
    )
    .bind(referrer_id)
    .fetch_one(pool)
    .await?)
}

pub async fn mark_rewarded<'e>(ex: impl PgExecutor<'e>, referred_id: i32, reward: &str) -> Result<bool> {
    let r = sqlx::query("UPDATE referrals SET rewarded_at = NOW(), reward = $2 WHERE referred_id = $1 AND rewarded_at IS NULL")
        .bind(referred_id)
        .bind(reward)
        .execute(ex)
        .await?;
    Ok(r.rows_affected() > 0)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ReferralStats {
    pub signed_up: i64,
    pub paid: i64,
    pub rewarded: i64,
}

pub async fn referral_stats(pool: &PgPool, referrer_id: i32) -> Result<ReferralStats> {
    Ok(sqlx::query_as::<_, ReferralStats>(
        "SELECT COUNT(*) AS signed_up,
                COUNT(first_paid_at) AS paid,
                COUNT(*) FILTER (WHERE reward IN ('extension', 'credit')) AS rewarded
         FROM referrals WHERE referrer_id = $1",
    )
    .bind(referrer_id)
    .fetch_one(pool)
    .await?)
}

// ── Kredi ────────────────────────────────────────────────────────────────────

pub async fn credit_balance<'e>(ex: impl PgExecutor<'e>, member_id: i32) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COALESCE(SUM(amount_kurus), 0)::BIGINT FROM member_credits WHERE member_id = $1")
        .bind(member_id)
        .fetch_one(ex)
        .await?)
}

/// Kredi hareketi; aynı (reason, ref) ikinci kez yazılmaz. Yazıldıysa true.
pub async fn add_credit<'e>(ex: impl PgExecutor<'e>, member_id: i32, amount_kurus: i64, reason: &str, reference: &str) -> Result<bool> {
    let r = sqlx::query(
        "INSERT INTO member_credits (member_id, amount_kurus, reason, ref) VALUES ($1, $2, $3, $4)
         ON CONFLICT (reason, ref) DO NOTHING",
    )
    .bind(member_id)
    .bind(amount_kurus)
    .bind(reason)
    .bind(reference)
    .execute(ex)
    .await?;
    Ok(r.rows_affected() > 0)
}
