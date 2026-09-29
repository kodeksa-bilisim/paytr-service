use anyhow::Result;
use sqlx::{PgExecutor, PgPool};

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

pub async fn create<'e>(ex: impl PgExecutor<'e>, p: NewPayment<'_>) -> Result<PaytrPaymentRecord> {
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
    .fetch_one(ex)
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

/// Callback transaction'ı içinde ödeme satırını kilitler (eşzamanlı çift callback'e karşı).
pub async fn find_by_oid_for_update<'e>(
    ex: impl PgExecutor<'e>,
    merchant_oid: &str,
) -> Result<Option<PaytrPaymentRecord>> {
    let rec = sqlx::query_as::<_, PaytrPaymentRecord>(
        "SELECT * FROM paytr_payments WHERE merchant_oid = $1 FOR UPDATE",
    )
    .bind(merchant_oid)
    .fetch_optional(ex)
    .await?;
    Ok(rec)
}

/// Ödemeyi başarılı işaretler (callback transaction'ı içinde, satır kilitliyken).
pub async fn set_success<'e>(ex: impl PgExecutor<'e>, merchant_oid: &str) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_payments
         SET status = 'success', callback_received_at = NOW(),
             failed_reason_code = NULL, failed_reason_msg = NULL
         WHERE merchant_oid = $1",
    )
    .bind(merchant_oid)
    .execute(ex)
    .await?;
    Ok(())
}

/// Tahsil edilmiş ama otomatik işlenemeyen ödemeyi elle incelemeye alır (tutar tutarsızlığı,
/// artık geçerli olmayan aboneliğe gelen ödeme vb.). Sync yanıtında `failed` işaretlenmiş
/// ödemenin sonradan gelen başarılı callback'i de kapsanır. Dönüş: güncellendi mi.
pub async fn set_review<'e>(ex: impl PgExecutor<'e>, merchant_oid: &str, reason: &str) -> Result<bool> {
    let r = sqlx::query(
        "UPDATE paytr_payments
         SET status = 'review', failed_reason_msg = $1, callback_received_at = NOW()
         WHERE merchant_oid = $2 AND status IN ('pending', 'failed')",
    )
    .bind(reason)
    .bind(merchant_oid)
    .execute(ex)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Veritabanı tekil kısıt ihlali mi? (ör. abonelik başına tek pending ödeme indeksi)
pub fn is_unique_violation(err: &anyhow::Error) -> bool {
    err.downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error())
        .is_some_and(|d| d.code().as_deref() == Some("23505"))
}

/// Bekleyen ödemeyi başarısız işaretler. Yalnızca ilk bildirim etkili olur (PayTR aynı
/// callback'i yeniden gönderebilir; scheduler sync yanıtında zaten işaretlemiş olabilir).
/// Dönüş: güncellendiyse ödeme kaydı.
pub async fn set_failed(
    pool: &PgPool,
    merchant_oid: &str,
    reason_code: Option<&str>,
    reason_msg: Option<&str>,
) -> Result<Option<PaytrPaymentRecord>> {
    let rec = sqlx::query_as::<_, PaytrPaymentRecord>(
        "UPDATE paytr_payments
         SET status = 'failed',
             failed_reason_code = $1,
             failed_reason_msg  = $2,
             callback_received_at = NOW()
         WHERE merchant_oid = $3 AND status = 'pending'
         RETURNING *",
    )
    .bind(reason_code)
    .bind(reason_msg)
    .bind(merchant_oid)
    .fetch_optional(pool)
    .await?;
    Ok(rec)
}

/// Callback'i gelmemiş eski bekleyen ödemeler (en eskiler önce). Scheduler her birinin
/// durumunu PayTR'a sorar: callback kaybolmuş ama para çekilmişse yeniden çekim yapılmasın.
pub async fn list_stale_pending(pool: &PgPool, older_than_hours: i32, limit: i64) -> Result<Vec<PaytrPaymentRecord>> {
    let recs = sqlx::query_as::<_, PaytrPaymentRecord>(
        "SELECT * FROM paytr_payments
         WHERE status = 'pending' AND created_at < NOW() - make_interval(hours => $1)
         ORDER BY created_at LIMIT $2",
    )
    .bind(older_than_hours)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(recs)
}
