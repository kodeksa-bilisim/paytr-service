use anyhow::Result;
use chrono::NaiveDateTime;
use sqlx::PgPool;

use super::models::PaytrSubscription;

/// Kullanıcı isteğiyle aboneliği iptal eder.
/// Abonelik expires_at tarihine kadar aktif kalır (standart iptal davranışı).
/// member_id eşleşmesi zorunludur — başkasının aboneliğini iptal etmeyi engeller.
pub async fn cancel_by_member(pool: &PgPool, subscription_id: i32, member_id: i32) -> Result<bool> {
    let result = sqlx::query(
        r#"
        UPDATE paytr_subscriptions
        SET status = 'cancelled', cancelled_at = NOW(), updated_at = NOW()
        WHERE id = $1 AND member_id = $2 AND status = 'active'
        "#,
    )
    .bind(subscription_id)
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Yeni ödeme başlamadan önce aynı kullanıcının eski pending aboneliklerini temizler.
pub async fn cancel_pending(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_subscriptions SET status = 'cancelled', cancelled_at = NOW(), updated_at = NOW()
         WHERE member_id = $1 AND status = 'pending'",
    )
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn create(
    pool: &PgPool,
    member_id: i32,
    plan: &str,
    billing_cycle: &str,
    amount: &str,
    currency: &str,
    user_phone: &str,
    user_email: &str,
) -> Result<PaytrSubscription> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        r#"
        INSERT INTO paytr_subscriptions
            (member_id, plan, billing_cycle, amount, currency, user_phone, user_email)
        VALUES ($1,$2,$3,$4,$5,$6,$7)
        RETURNING *
        "#,
    )
    .bind(member_id)
    .bind(plan)
    .bind(billing_cycle)
    .bind(amount)
    .bind(currency)
    .bind(user_phone)
    .bind(user_email)
    .fetch_one(pool)
    .await?;
    Ok(sub)
}

pub async fn find_by_id(pool: &PgPool, id: i32) -> Result<Option<PaytrSubscription>> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        "SELECT * FROM paytr_subscriptions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(sub)
}

#[allow(dead_code)]
pub async fn find_active(pool: &PgPool, member_id: i32) -> Result<Option<PaytrSubscription>> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        "SELECT * FROM paytr_subscriptions WHERE member_id = $1 AND status = 'active' LIMIT 1",
    )
    .bind(member_id)
    .fetch_optional(pool)
    .await?;
    Ok(sub)
}

/// pending → active: ilk başarılı ödeme sonrası çağrılır.
pub async fn activate(
    pool: &PgPool,
    id: i32,
    utoken: &str,
    ctoken: &str,
    started_at: NaiveDateTime,
    expires_at: NaiveDateTime,
    next_payment_date: NaiveDateTime,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE paytr_subscriptions
        SET status            = 'active',
            utoken            = $1,
            ctoken            = $2,
            started_at        = $3,
            expires_at        = $4,
            next_payment_date = $5,
            updated_at        = NOW()
        WHERE id = $6 AND status = 'pending'
        "#,
    )
    .bind(utoken)
    .bind(ctoken)
    .bind(started_at)
    .bind(expires_at)
    .bind(next_payment_date)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Aboneliği yeniler: expires_at ve next_payment_date güncellenir.
pub async fn renew(
    pool: &PgPool,
    id: i32,
    expires_at: NaiveDateTime,
    next_payment_date: NaiveDateTime,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE paytr_subscriptions
        SET expires_at        = $1,
            next_payment_date = $2,
            updated_at        = NOW()
        WHERE id = $3
        "#,
    )
    .bind(expires_at)
    .bind(next_payment_date)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Scheduler'ın işleyeceği vadesi gelen aktif abonelikleri döner.
/// require_cvv=TRUE olan kartlar dahil edilmez (CVV olmadan ödeme yapılamaz).
/// 3+ başarısız denemesi olan müşteriler hariç tutulur.
#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
pub struct DueSubscription {
    pub subscription_id: i32,
    pub member_id: i32,
    pub plan: String,
    pub billing_cycle: String,
    pub amount: String,
    pub currency: String,
    pub utoken: String,
    pub ctoken: String,
    pub require_cvv: bool,
    pub user_phone: String,
    pub user_email: String,
}

pub async fn query_due(pool: &PgPool, max_failed_attempts: i32) -> Result<Vec<DueSubscription>> {
    let rows = sqlx::query_as::<_, DueSubscription>(
        r#"
        SELECT
            s.id            AS subscription_id,
            s.member_id,
            s.plan,
            s.billing_cycle,
            s.amount,
            s.currency,
            s.utoken,
            s.ctoken,
            c.require_cvv,
            COALESCE(s.user_phone, '') AS user_phone,
            COALESCE(s.user_email, cu.email, '') AS user_email
        FROM paytr_subscriptions s
        JOIN paytr_cards c
            ON c.ctoken = s.ctoken AND c.is_active = TRUE
        JOIN customers cu
            ON cu.member_id = s.member_id
        WHERE s.status = 'active'
          AND s.next_payment_date <= NOW()
          AND s.utoken IS NOT NULL
          AND s.ctoken IS NOT NULL
          AND c.require_cvv = FALSE
          AND COALESCE(cu.failed_payment_attempts, 0) < $1
        "#,
    )
    .bind(max_failed_attempts)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Süresi dolmuş ama hâlâ 'active' veya 'cancelled' olan abonelikleri 'expired' yapar.
/// Scheduler tarafından çağrılır; dönen liste customer_repo::set_subscription_expired için kullanılır.
pub async fn mark_expired(pool: &PgPool) -> Result<Vec<i32>> {
    let member_ids: Vec<i32> = sqlx::query_scalar(
        r#"
        UPDATE paytr_subscriptions
        SET status = 'expired', updated_at = NOW()
        WHERE status IN ('active', 'cancelled')
          AND expires_at < NOW()
        RETURNING member_id
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(member_ids)
}

/// Başarısız yenileme denemesini sayar.
pub async fn increment_renewal_attempts(pool: &PgPool, subscription_id: i32) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_subscriptions
         SET renewal_attempts = renewal_attempts + 1, updated_at = NOW()
         WHERE id = $1",
    )
    .bind(subscription_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Ödeme başarısından sonra renewal_attempts sıfırla.
pub async fn reset_renewal_attempts(pool: &PgPool, subscription_id: i32) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_subscriptions
         SET renewal_attempts = 0, updated_at = NOW()
         WHERE id = $1",
    )
    .bind(subscription_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn cancel(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE paytr_subscriptions
        SET status = 'cancelled', cancelled_at = NOW(), updated_at = NOW()
        WHERE member_id = $1 AND status = 'active'
        "#,
    )
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}
