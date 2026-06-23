use anyhow::Result;
use chrono::NaiveDateTime;
use sqlx::PgPool;

/// Email'den member_id'yi çeker (frontend member_id göndermediğinde fallback).
pub async fn find_member_id_by_email(pool: &PgPool, email: &str) -> Result<Option<i32>> {
    let id = sqlx::query_scalar::<_, i32>(
        "SELECT member_id FROM customers WHERE lower(email) = lower($1) LIMIT 1",
    )
    .bind(email)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// plan adını PascalCase'e çevirir ("silver" → "Silver").
/// qurlbackend Membership enum'ı PascalCase bekler.
fn plan_to_user_type(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Ödeme başarılıysa customers tablosunu günceller.
/// paytr_subscription_id → customers.subscription_id (VARCHAR) olarak saklanır;
/// qurlbackend bu değeri ProfileDetails.subscription_id'ye parse eder.
pub async fn set_subscription_active(
    pool: &PgPool,
    member_id: i32,
    plan: &str,
    paytr_subscription_id: i32,
    expires_at: NaiveDateTime,
    next_payment_date: NaiveDateTime,
) -> Result<()> {
    let user_type = plan_to_user_type(plan); // "Silver" veya "Gold"
    sqlx::query(
        r#"
        UPDATE customers
        SET subscription_status   = 'active',
            subscription_plan     = $1,
            subscription_id       = $2,
            subscription_expires_at = $3,
            next_payment_date     = $4,
            payment_status        = 'success',
            last_payment_date     = NOW(),
            payment_method        = 'paytr',
            user_type             = $5,
            failed_payment_attempts = 0
        WHERE member_id = $6
        "#,
    )
    .bind(plan)
    .bind(paytr_subscription_id.to_string())
    .bind(expires_at)
    .bind(next_payment_date)
    .bind(user_type)
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Abonelik iptal edildiğinde subscription_status = 'cancelled' yazar.
/// user_type ve expires_at DEĞİŞTİRİLMEZ — kullanıcı expires_at'e kadar plan erişimini korur.
/// Süresi dolduğunda scheduler 'expired' olarak işaretler ve user_type'ı 'Standard'a çeker.
pub async fn set_subscription_cancelled(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query(
        "UPDATE customers SET subscription_status = 'cancelled' WHERE member_id = $1",
    )
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Abonelik süresi dolduğunda customers'ı Standard'a düşürür.
pub async fn set_subscription_expired(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE customers
        SET subscription_status = 'expired',
            user_type           = 'Standard',
            subscription_id     = NULL
        WHERE member_id = $1
        "#,
    )
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Ödeme başarısızsa deneme sayısını artırır.
pub async fn increment_failed_attempts(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE customers
        SET payment_status          = 'failed',
            failed_payment_attempts = COALESCE(failed_payment_attempts, 0) + 1
        WHERE member_id = $1
        "#,
    )
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}
