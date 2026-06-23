use anyhow::Result;
use sqlx::PgPool;

use super::models::PaytrPaymentRecord;

pub struct NewPayment<'a> {
    pub member_id: i32,
    pub subscription_id: Option<i32>,
    pub merchant_oid: &'a str,
    pub amount: &'a str,
    pub currency: &'a str,
    pub payment_type: &'a str,
    pub installment_count: i32,
    pub is_3d: bool,
    pub test_mode: bool,
    pub utoken: Option<&'a str>,
    pub ctoken: Option<&'a str>,
}

pub async fn create(pool: &PgPool, p: NewPayment<'_>) -> Result<PaytrPaymentRecord> {
    let rec = sqlx::query_as::<_, PaytrPaymentRecord>(
        r#"
        INSERT INTO paytr_payments
            (member_id, subscription_id, merchant_oid, amount, currency,
             payment_type, installment_count, is_3d, test_mode, utoken, ctoken)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
        RETURNING *
        "#,
    )
    .bind(p.member_id)
    .bind(p.subscription_id)
    .bind(p.merchant_oid)
    .bind(p.amount)
    .bind(p.currency)
    .bind(p.payment_type)
    .bind(p.installment_count)
    .bind(p.is_3d)
    .bind(p.test_mode)
    .bind(p.utoken)
    .bind(p.ctoken)
    .fetch_one(pool)
    .await?;
    Ok(rec)
}

/// Abonelik için beklemede (pending) ödeme var mı? Çift ödeme koruması için.
pub async fn has_pending(pool: &PgPool, subscription_id: i32) -> Result<bool> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM paytr_payments WHERE subscription_id = $1 AND status = 'pending')",
    )
    .bind(subscription_id)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

pub async fn find_by_oid(pool: &PgPool, merchant_oid: &str) -> Result<Option<PaytrPaymentRecord>> {
    let rec = sqlx::query_as::<_, PaytrPaymentRecord>(
        "SELECT * FROM paytr_payments WHERE merchant_oid = $1",
    )
    .bind(merchant_oid)
    .fetch_optional(pool)
    .await?;
    Ok(rec)
}

pub async fn set_success(pool: &PgPool, merchant_oid: &str) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_payments
         SET status = 'success', callback_received_at = NOW()
         WHERE merchant_oid = $1",
    )
    .bind(merchant_oid)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_failed(
    pool: &PgPool,
    merchant_oid: &str,
    reason_code: Option<&str>,
    reason_msg: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_payments
         SET status = 'failed',
             failed_reason_code = $1,
             failed_reason_msg  = $2,
             callback_received_at = NOW()
         WHERE merchant_oid = $3",
    )
    .bind(reason_code)
    .bind(reason_msg)
    .bind(merchant_oid)
    .execute(pool)
    .await?;
    Ok(())
}
