use anyhow::Result;
use chrono::NaiveDateTime;
use sqlx::{PgExecutor, PgPool};

/// E-posta hitabı için müşteri adı (bulunamazsa ya da hata olursa None).
pub async fn find_name(pool: &PgPool, member_id: i32) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT name FROM customers WHERE member_id = $1")
        .bind(member_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// Müşterinin güncel abonelik kimliği (`customers.subscription_id`); callback transaction'ı
/// içinde satırı kilitler. Müşteri ya da abonelik yoksa None.
pub async fn current_subscription_id_for_update<'e>(ex: impl PgExecutor<'e>, member_id: i32) -> Result<Option<String>> {
    let id: Option<Option<String>> =
        sqlx::query_scalar("SELECT subscription_id FROM customers WHERE member_id = $1 FOR UPDATE")
            .bind(member_id)
            .fetch_optional(ex)
            .await?;
    Ok(id.flatten())
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
pub async fn set_subscription_active<'e>(
    ex: impl PgExecutor<'e>,
    member_id: i32,
    plan: &str,
    paytr_subscription_id: i32,
    expires_at: NaiveDateTime,
    next_payment_date: NaiveDateTime,
    custom_plan: Option<String>,
    subscription_status: &str,
) -> Result<()> {
    let user_type = plan_to_user_type(plan);
    sqlx::query(
        r#"
        UPDATE customers
        SET subscription_status   = $8,
            subscription_plan     = $1,
            subscription_id       = $2,
            subscription_expires_at = $3,
            next_payment_date     = $4,
            payment_status        = 'success',
            last_payment_date     = NOW(),
            payment_method        = 'paytr',
            user_type             = $5,
            failed_payment_attempts = 0,
            custom_plan           = $7,
            scheduled_plan        = NULL
        WHERE member_id = $6
        "#,
    )
    .bind(plan)
    .bind(paytr_subscription_id.to_string())
    .bind(expires_at)
    .bind(next_payment_date)
    .bind(user_type)
    .bind(member_id)
    .bind(custom_plan)
    .bind(subscription_status)
    .execute(ex)
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

/// Abonelik iptali geri alındığında subscription_status = 'active' yazar.
pub async fn set_subscription_reactivated(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query(
        "UPDATE customers SET subscription_status = 'active' WHERE member_id = $1",
    )
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Abonelik süresi dolduğunda customers'ı Standard'a düşürür — yalnızca süresi dolan
/// abonelik müşterinin güncel aboneliğiyse (upgrade sonrası eski aboneliğin dönemi
/// bitince yeni planı olan kullanıcı düşürülmesin). Dönüş: düşürüldü mü.
pub async fn set_subscription_expired(pool: &PgPool, member_id: i32, subscription_id: i32) -> Result<bool> {
    let r = sqlx::query(
        r#"
        UPDATE customers
        SET subscription_status = 'expired',
            user_type           = 'Standard',
            subscription_id     = NULL,
            scheduled_plan      = NULL
        WHERE member_id = $1 AND subscription_id = $2::text
        "#,
    )
    .bind(member_id)
    .bind(subscription_id)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Ücretsiz deneme başladı: plan açılır, `subscription_status = 'trial'`, ödeme tarihi yok.
pub async fn set_trial<'e>(ex: impl PgExecutor<'e>, member_id: i32, plan: &str, subscription_id: i32, expires_at: NaiveDateTime) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE customers
        SET subscription_status = 'trial', subscription_plan = $2, subscription_id = $3::text,
            subscription_expires_at = $4, next_payment_date = NULL, user_type = $5,
            custom_plan = NULL, scheduled_plan = NULL
        WHERE member_id = $1
        "#,
    )
    .bind(member_id)
    .bind(plan)
    .bind(subscription_id)
    .bind(expires_at)
    .bind(plan_to_user_type(plan))
    .execute(ex)
    .await?;
    Ok(())
}

/// Referans ödülü uzatması: güncel abonelikse bitiş ve sonraki ödeme tarihi müşteri kaydına da yazılır.
pub async fn sync_extended<'e>(ex: impl PgExecutor<'e>, member_id: i32, subscription_id: i32, expires_at: NaiveDateTime) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE customers
        SET subscription_expires_at = $3,
            next_payment_date = CASE WHEN next_payment_date IS NULL THEN NULL ELSE next_payment_date + INTERVAL '1 month' END
        WHERE member_id = $1 AND subscription_id = $2::text
        "#,
    )
    .bind(member_id)
    .bind(subscription_id)
    .bind(expires_at)
    .execute(ex)
    .await?;
    Ok(())
}

/// Downgrade planlanmış olarak işaretler (dönem sonunda geçiş).
pub async fn set_scheduled_plan(pool: &PgPool, member_id: i32, plan: &str) -> Result<()> {
    sqlx::query("UPDATE customers SET scheduled_plan = $1 WHERE member_id = $2")
        .bind(plan)
        .bind(member_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Planlanmış downgrade'i iptal eder.
pub async fn clear_scheduled_plan(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query("UPDATE customers SET scheduled_plan = NULL WHERE member_id = $1")
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
