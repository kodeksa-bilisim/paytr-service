//! Yönetici paneli (Ödemeler sayfası) için salt-okunur sorgular. Hiçbir şey yazmaz.

use anyhow::Result;
use chrono::NaiveDateTime;
use sqlx::{PgPool, Postgres, QueryBuilder};

/// Ödeme satırı + üye e-postası + abonelik planı.
#[derive(Debug, sqlx::FromRow)]
pub struct PaymentRow {
    pub id: i32,
    pub merchant_oid: String,
    pub member_id: i32,
    pub email: Option<String>,
    pub subscription_id: Option<i32>,
    pub plan: Option<String>,
    pub billing_cycle: Option<String>,
    pub amount: String,
    pub currency: String,
    pub status: String,
    pub failed_reason_code: Option<String>,
    pub failed_reason_msg: Option<String>,
    pub is_3d: bool,
    pub test_mode: bool,
    pub installment_count: i32,
    pub callback_received_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub invoice_status: Option<String>,
    pub invoice_no: Option<String>,
    pub invoice_pdf_url: Option<String>,
}

const PAYMENT_SELECT: &str = r#"
    SELECT p.id, p.merchant_oid, p.member_id, cu.email, p.subscription_id,
           s.plan, s.billing_cycle, p.amount, p.currency, p.status,
           p.failed_reason_code, p.failed_reason_msg, p.is_3d, p.test_mode,
           p.installment_count, p.callback_received_at, p.created_at,
           i.status AS invoice_status, i.invoice_no, i.pdf_url AS invoice_pdf_url
    FROM paytr_payments p
    LEFT JOIN customers cu ON cu.member_id = p.member_id
    LEFT JOIN paytr_subscriptions s ON s.id = p.subscription_id
    LEFT JOIN invoices i ON i.merchant_oid = p.merchant_oid AND i.kind = 'sale'
"#;

/// Faturalamanın başladığı andan (ilk fatura kaydı) sonra başarılı olup fatura kaydı olmayan
/// ödemeler. Normalde 0; callback'te fatura kaydı atlandıysa (bkz. `record_invoice`) artar.
pub async fn payments_without_invoice(pool: &PgPool, exempt_members: &[i32]) -> Result<i64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM paytr_payments p
         WHERE p.status = 'success' AND NOT p.test_mode
           AND NOT (p.member_id = ANY($1))
           AND p.created_at >= (SELECT min(created_at) FROM invoices)
           AND NOT EXISTS (SELECT 1 FROM invoices i WHERE i.merchant_oid = p.merchant_oid AND i.kind = 'sale')",
    )
    .bind(exempt_members)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Liste filtreleri; handler doğrular (status/kind yalnızca bilinen değerler).
pub struct PaymentFilter<'a> {
    pub status: Option<&'a str>,
    /// `renewal` | `card` | `enterprise` — merchant_oid önekinden (r…, u…, ent…).
    pub kind: Option<&'a str>,
    /// E-posta, sipariş no (merchant_oid) ya da üye no.
    pub q: Option<&'a str>,
    pub limit: i64,
    pub offset: i64,
}

pub fn kind_pattern(kind: &str) -> Option<&'static str> {
    match kind {
        "renewal" => Some("^r[0-9]+t[0-9]+$"),
        "card" => Some("^u[0-9]+t[0-9]+$"),
        "enterprise" => Some("^ent[0-9]+t[0-9]+$"),
        _ => None,
    }
}

/// En yeni önce. `limit + 1` satır döner: fazlası "sonraki sayfa var" demektir.
pub async fn list_payments(pool: &PgPool, f: &PaymentFilter<'_>) -> Result<Vec<PaymentRow>> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(PAYMENT_SELECT);
    qb.push(" WHERE TRUE");
    if let Some(status) = f.status {
        qb.push(" AND p.status = ").push_bind(status.to_string());
    }
    if let Some(pattern) = f.kind.and_then(kind_pattern) {
        qb.push(" AND p.merchant_oid ~ ").push_bind(pattern);
    }
    if let Some(q) = f.q.map(str::trim).filter(|q| !q.is_empty()) {
        match q.parse::<i32>() {
            Ok(member_id) => {
                qb.push(" AND (p.member_id = ").push_bind(member_id);
                qb.push(" OR p.subscription_id = ").push_bind(member_id).push(")");
            }
            Err(_) => {
                let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
                qb.push(" AND (cu.email ILIKE ").push_bind(like.clone());
                qb.push(" OR p.merchant_oid ILIKE ").push_bind(like).push(")");
            }
        }
    }
    qb.push(" ORDER BY p.created_at DESC, p.id DESC LIMIT ")
        .push_bind(f.limit + 1)
        .push(" OFFSET ")
        .push_bind(f.offset);
    Ok(qb.build_query_as::<PaymentRow>().fetch_all(pool).await?)
}

/// Dikkat isteyen ödemeler: incelemedekiler (iade kararı gerekebilir), callback'i 1 saati
/// geçen yenilemeler (normalde saniyeler içinde gelir) ve 48 saati geçen her bekleyen ödeme.
pub async fn attention_payments(pool: &PgPool) -> Result<Vec<PaymentRow>> {
    let sql = format!(
        "{PAYMENT_SELECT}
         WHERE p.status = 'review'
            OR (p.status = 'pending' AND (
                   (NOT p.is_3d AND p.created_at < NOW() - INTERVAL '1 hour')
                OR p.created_at < NOW() - INTERVAL '48 hours'))
         ORDER BY p.created_at DESC
         LIMIT 100"
    );
    Ok(sqlx::query_as::<_, PaymentRow>(&sql).fetch_all(pool).await?)
}

/// Son N gündeki ödemelerin durum ve ret sebebi (özet sayıları Rust tarafında sınıflanır).
#[derive(Debug, sqlx::FromRow)]
pub struct StatusRow {
    pub member_id: i32,
    pub status: String,
    pub is_3d: bool,
    pub failed_reason_msg: Option<String>,
    pub amount: String,
    pub test_mode: bool,
    pub created_at: NaiveDateTime,
}

pub async fn recent_statuses(pool: &PgPool, days: i32) -> Result<Vec<StatusRow>> {
    Ok(sqlx::query_as::<_, StatusRow>(
        "SELECT member_id, status, is_3d, failed_reason_msg, amount, test_mode, created_at
         FROM paytr_payments
         WHERE created_at > NOW() - make_interval(days => $1)",
    )
    .bind(days)
    .fetch_all(pool)
    .await?)
}

/// Son başarılı otomatik yenilemenin callback zamanı.
pub async fn last_renewal_success(pool: &PgPool) -> Result<Option<NaiveDateTime>> {
    let row: (Option<NaiveDateTime>,) = sqlx::query_as(
        "SELECT max(callback_received_at) FROM paytr_payments
         WHERE status = 'success' AND merchant_oid ~ '^r[0-9]+t[0-9]+$'",
    )
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Yenilemesi sorunlu ya da yakında sorun çıkacak aktif abonelikler.
#[derive(Debug, sqlx::FromRow)]
pub struct ProblemSubscriptionRow {
    pub id: i32,
    pub member_id: i32,
    pub email: Option<String>,
    pub plan: String,
    pub billing_cycle: String,
    pub amount: String,
    pub renewal_attempts: i32,
    pub expires_at: Option<NaiveDateTime>,
    pub next_payment_date: Option<NaiveDateTime>,
    pub last_renewal_attempt_at: Option<NaiveDateTime>,
    pub last_failure: Option<String>,
    pub has_pending: bool,
    pub has_card: bool,
    pub require_cvv: bool,
}

/// Aktif ve: süresi dolmuş (ek sürede), başarısız denemesi olan, ya da 7 gün içinde yenilenecek
/// ama otomatik çekilemeyecek (CVV gerekli / kayıtlı kart yok) abonelikler.
pub async fn problem_subscriptions(pool: &PgPool) -> Result<Vec<ProblemSubscriptionRow>> {
    Ok(sqlx::query_as::<_, ProblemSubscriptionRow>(
        r#"
        SELECT s.id, s.member_id, cu.email, s.plan, s.billing_cycle, s.amount, s.renewal_attempts,
               s.expires_at, s.next_payment_date, s.last_renewal_attempt_at,
               (SELECT p.failed_reason_msg FROM paytr_payments p
                 WHERE p.subscription_id = s.id AND p.status = 'failed'
                 ORDER BY p.created_at DESC LIMIT 1)                        AS last_failure,
               EXISTS (SELECT 1 FROM paytr_payments p
                        WHERE p.subscription_id = s.id AND p.status = 'pending') AS has_pending,
               (c.id IS NOT NULL)                                           AS has_card,
               COALESCE(c.require_cvv, FALSE)                               AS require_cvv
        FROM paytr_subscriptions s
        LEFT JOIN customers cu ON cu.member_id = s.member_id
        LEFT JOIN paytr_cards c ON c.ctoken = s.ctoken AND c.is_active = TRUE
        WHERE s.status = 'active'
          AND (   s.expires_at < NOW()
               OR s.renewal_attempts > 0
               OR (s.next_payment_date < NOW() + INTERVAL '7 days'
                   AND (c.id IS NULL OR c.require_cvv)))
        ORDER BY s.expires_at NULLS LAST
        LIMIT 100
        "#,
    )
    .fetch_all(pool)
    .await?)
}
