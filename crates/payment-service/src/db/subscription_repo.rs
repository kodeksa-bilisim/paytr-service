use anyhow::Result;
use chrono::NaiveDateTime;
use sqlx::{PgExecutor, PgPool};

use super::models::PaytrSubscription;

// Durumlar: pending → active → (cancelled →) expired. Upgrade ile yerine yenisi geçen
// abonelik `replaced` olur: expire/geri alma/yenileme bunu görmez (eskiden `cancelled`
// kalıyordu; dönem sonunda kullanıcıyı Standard'a düşürüyor ya da geri almayla
// yeniden canlanıp çift tahsilata yol açıyordu).

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

/// İptal edilen aboneliği geri alır. Yalnızca müşterinin güncel aboneliği
/// (`customers.subscription_id`) ve süresi dolmamışsa. Planlanmış downgrade de temizlenir:
/// müşteri tarafında görünmüyorken yenilemede sessizce uygulanmasın.
pub async fn reactivate_by_member(pool: &PgPool, member_id: i32) -> Result<bool> {
    let result = sqlx::query(
        r#"
        UPDATE paytr_subscriptions s
        SET status = 'active', cancelled_at = NULL, scheduled_plan = NULL, scheduled_amount = NULL,
            updated_at = NOW()
        FROM customers c
        WHERE c.member_id = $1
          AND s.member_id = $1
          AND s.id::text = c.subscription_id
          AND s.status = 'cancelled'
          AND s.expires_at > NOW()
        "#,
    )
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
    metadata: Option<serde_json::Value>,
) -> Result<PaytrSubscription> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        r#"
        INSERT INTO paytr_subscriptions
            (member_id, plan, billing_cycle, amount, currency, user_phone, user_email, metadata)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
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
    .bind(metadata)
    .fetch_one(pool)
    .await?;
    Ok(sub)
}

pub async fn find_by_id<'e>(ex: impl PgExecutor<'e>, id: i32) -> Result<Option<PaytrSubscription>> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        "SELECT * FROM paytr_subscriptions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(ex)
    .await?;
    Ok(sub)
}

/// Callback transaction'ı içinde satırı kilitler.
pub async fn find_by_id_for_update<'e>(ex: impl PgExecutor<'e>, id: i32) -> Result<Option<PaytrSubscription>> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        "SELECT * FROM paytr_subscriptions WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(ex)
    .await?;
    Ok(sub)
}

/// Üyenin aktif aboneliği (birden fazlaysa en yenisi).
pub async fn find_active(pool: &PgPool, member_id: i32) -> Result<Option<PaytrSubscription>> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        "SELECT * FROM paytr_subscriptions WHERE member_id = $1 AND status = 'active'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(member_id)
    .fetch_optional(pool)
    .await?;
    Ok(sub)
}

/// Üyenin hâlâ geçerli aboneliği (aktif ya da süresi dolmamış iptal; birden fazlaysa en yenisi).
/// Yükseltme fark ücreti ve "bu plan zaten var" kontrolü bunun üzerinden yapılır.
pub async fn find_live(pool: &PgPool, member_id: i32) -> Result<Option<PaytrSubscription>> {
    let sub = sqlx::query_as::<_, PaytrSubscription>(
        r#"
        SELECT * FROM paytr_subscriptions
        WHERE member_id = $1 AND started_at IS NOT NULL
          AND (status = 'active' OR (status = 'cancelled' AND expires_at > NOW()))
        ORDER BY id DESC LIMIT 1
        "#,
    )
    .bind(member_id)
    .fetch_optional(pool)
    .await?;
    Ok(sub)
}

/// İlk ödeme aktivasyonunda: üyenin bu abonelik dışındaki geçerli abonelikleri (satırlar kilitli).
pub async fn live_others_for_update<'e>(
    ex: impl PgExecutor<'e>,
    member_id: i32,
    exclude_id: i32,
) -> Result<Vec<PaytrSubscription>> {
    let subs = sqlx::query_as::<_, PaytrSubscription>(
        r#"
        SELECT * FROM paytr_subscriptions
        WHERE member_id = $1 AND id <> $2 AND started_at IS NOT NULL
          AND (status = 'active' OR (status = 'cancelled' AND expires_at > NOW()))
        FOR UPDATE
        "#,
    )
    .bind(member_id)
    .bind(exclude_id)
    .fetch_all(ex)
    .await?;
    Ok(subs)
}

/// Üyenin hâlâ geçerli (aktif ya da süresi dolmamış iptal) bir aboneliği var mı?
pub async fn has_live_subscription(pool: &PgPool, member_id: i32) -> Result<bool> {
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM paytr_subscriptions
            WHERE member_id = $1
              AND (status = 'active' OR (status = 'cancelled' AND expires_at > NOW()))
        )
        "#,
    )
    .bind(member_id)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// İlk başarılı ödeme: aktifleştirir. Hiç aktif olmamış (started_at NULL) abonelik
/// için geçerlidir — kullanıcı yeni bir ödeme başlatınca `cancel_pending` ile iptal
/// edilmiş eski pending abonelik de, o ödeme tamamlanırsa aktifleşir.
pub async fn activate<'e>(
    ex: impl PgExecutor<'e>,
    id: i32,
    utoken: &str,
    ctoken: &str,
    started_at: NaiveDateTime,
    expires_at: NaiveDateTime,
    next_payment_date: NaiveDateTime,
) -> Result<bool> {
    let r = sqlx::query(
        r#"
        UPDATE paytr_subscriptions
        SET status            = 'active',
            utoken            = NULLIF($1, ''),
            ctoken            = NULLIF($2, ''),
            started_at        = $3,
            expires_at        = $4,
            next_payment_date = $5,
            cancelled_at      = NULL,
            updated_at        = NOW()
        WHERE id = $6 AND started_at IS NULL
        "#,
    )
    .bind(utoken)
    .bind(ctoken)
    .bind(started_at)
    .bind(expires_at)
    .bind(next_payment_date)
    .bind(id)
    .execute(ex)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Aboneliği yeniler: expires_at ve next_payment_date güncellenir. Tahsilat callback'i
/// grace süresi dolduktan sonra gelirse (abonelik expired) yeniden aktifleşir; iptal
/// edilmişse iptal kalır (yeni dönem biter, sonrası yenilenmez).
pub async fn renew<'e>(
    ex: impl PgExecutor<'e>,
    id: i32,
    plan: Option<&str>,
    expires_at: NaiveDateTime,
    next_payment_date: NaiveDateTime,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE paytr_subscriptions
        SET plan              = COALESCE($1, plan),
            scheduled_plan    = CASE WHEN $1 IS NULL THEN scheduled_plan ELSE NULL END,
            scheduled_amount  = CASE WHEN $1 IS NULL THEN scheduled_amount ELSE NULL END,
            amount            = CASE WHEN $1 IS NULL THEN amount ELSE COALESCE(scheduled_amount, amount) END,
            status            = CASE WHEN status = 'expired' THEN 'active' ELSE status END,
            expires_at        = $2,
            next_payment_date = $3,
            renewal_attempts  = 0,
            last_renewal_attempt_at = NULL,
            updated_at        = NOW()
        WHERE id = $4
        "#,
    )
    .bind(plan)
    .bind(expires_at)
    .bind(next_payment_date)
    .bind(id)
    .execute(ex)
    .await?;
    Ok(())
}

/// Scheduler'ın işleyeceği vadesi gelen aktif abonelikler.
/// require_cvv=TRUE olan kartlar dahil edilmez (CVV olmadan ödeme yapılamaz).
/// Denemeler günde bir yapılır; `max_failed_attempts` başarısız denemeden sonra durur.
#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
pub struct DueSubscription {
    pub subscription_id: i32,
    pub member_id: i32,
    pub plan: String,           // Efektif plan (COALESCE ile scheduled_plan öncelikli)
    pub billing_cycle: String,
    pub amount: String,         // Efektif tutar (TL, "149.00")
    pub currency: String,
    pub utoken: String,
    pub ctoken: String,
    pub require_cvv: bool,
    pub user_phone: String,
    pub user_email: String,
    pub original_plan: String,  // Asıl plan (scheduled_plan'ın set edilip edilmediğini anlamak için)
}

pub async fn query_due(pool: &PgPool, max_failed_attempts: i32) -> Result<Vec<DueSubscription>> {
    let rows = sqlx::query_as::<_, DueSubscription>(
        r#"
        SELECT
            s.id                                                        AS subscription_id,
            s.member_id,
            COALESCE(s.scheduled_plan, s.plan)                         AS plan,
            s.billing_cycle,
            COALESCE(s.scheduled_amount, s.amount)                     AS amount,
            s.currency,
            s.utoken,
            s.ctoken,
            c.require_cvv,
            COALESCE(s.user_phone, '')                                  AS user_phone,
            COALESCE(s.user_email, cu.email, '')                       AS user_email,
            s.plan                                                      AS original_plan
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
          AND s.renewal_attempts < $1
          AND (s.last_renewal_attempt_at IS NULL OR s.last_renewal_attempt_at < NOW() - INTERVAL '1 day')
        ORDER BY s.next_payment_date
        "#,
    )
    .bind(max_failed_attempts)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Yenileme denemesini sahiplenir: yalnızca son 1 günde denenmemişse deneme zamanını yazar ve
/// true döner. Koşullu olduğu için aynı anda çalışan iki scheduler aynı aboneliği çekemez.
pub async fn claim_renewal_attempt(pool: &PgPool, subscription_id: i32) -> Result<bool> {
    let r = sqlx::query(
        r#"
        UPDATE paytr_subscriptions SET last_renewal_attempt_at = NOW(), updated_at = NOW()
        WHERE id = $1
          AND (last_renewal_attempt_at IS NULL OR last_renewal_attempt_at < NOW() - INTERVAL '1 day')
        "#,
    )
    .bind(subscription_id)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// CVV bildirimi gönderildi: aynı gün tekrar gönderilmez.
pub async fn mark_renewal_attempt(pool: &PgPool, subscription_id: i32) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_subscriptions SET last_renewal_attempt_at = NOW(), updated_at = NOW() WHERE id = $1",
    )
    .bind(subscription_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Süresi dolmuş abonelikleri 'expired' yapar ve (id, member_id) döner.
/// - `cancelled`: expires_at geçince.
/// - `active`: ancak expires_at + `grace_days` geçince (yenileme denemeleri için süre) ve
///   bekleyen bir ödemesi yoksa.
pub async fn mark_expired(pool: &PgPool, grace_days: i32) -> Result<Vec<(i32, i32)>> {
    let rows: Vec<(i32, i32)> = sqlx::query_as(
        r#"
        UPDATE paytr_subscriptions s
        SET status = 'expired', updated_at = NOW()
        WHERE (
                (s.status = 'cancelled' AND s.expires_at < NOW())
             OR (s.status = 'active'    AND s.expires_at < NOW() - make_interval(days => $1))
              )
          AND NOT EXISTS (
                SELECT 1 FROM paytr_payments p
                WHERE p.subscription_id = s.id AND p.status = 'pending'
              )
        RETURNING s.id, s.member_id
        "#,
    )
    .bind(grace_days)
    .fetch_all(pool)
    .await?;
    Ok(rows)
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

/// Gold → Silver gibi downgrade için mevcut aktif aboneliğe gelecek dönem planı kaydeder.
pub async fn set_scheduled_downgrade(
    pool: &PgPool,
    subscription_id: i32,
    plan: &str,
    amount: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_subscriptions SET scheduled_plan = $1, scheduled_amount = $2, updated_at = NOW()
         WHERE id = $3",
    )
    .bind(plan)
    .bind(amount)
    .bind(subscription_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Planlanmış downgrade'i iptal eder.
pub async fn cancel_scheduled(pool: &PgPool, subscription_id: i32) -> Result<()> {
    sqlx::query(
        "UPDATE paytr_subscriptions SET scheduled_plan = NULL, scheduled_amount = NULL, updated_at = NOW()
         WHERE id = $1",
    )
    .bind(subscription_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Upgrade: yeni abonelik aktifleşince üyenin diğer aktif/iptal edilmiş aboneliklerini
/// `replaced` yapar; diğer pending abonelikleri iptal eder.
pub async fn replace_others<'e>(ex: impl PgExecutor<'e>, member_id: i32, keep_id: i32) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE paytr_subscriptions
        SET status = CASE WHEN status = 'pending' THEN 'cancelled' ELSE 'replaced' END,
            cancelled_at = COALESCE(cancelled_at, NOW()),
            updated_at = NOW()
        WHERE member_id = $1 AND id != $2 AND status IN ('active', 'cancelled', 'pending')
          AND NOT (status = 'cancelled' AND started_at IS NULL)
        "#,
    )
    .bind(member_id)
    .bind(keep_id)
    .execute(ex)
    .await?;
    Ok(())
}
