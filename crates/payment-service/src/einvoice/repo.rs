//! Fatura kesim kuyruğu ve GİB e-Fatura kullanıcı listesi sorguları.

use anyhow::Result;
use chrono::NaiveDateTime;
use sqlx::PgPool;

/// Kesilmek üzere kiralanmış kayıt.
#[derive(Debug, sqlx::FromRow)]
pub struct Claimed {
    pub id: i32,
    pub merchant_oid: String,
    pub member_id: i32,
    pub buyer: serde_json::Value,
    pub lines: serde_json::Value,
    pub vat_rate: i16,
    pub net_kurus: i64,
    pub vat_kurus: i64,
    pub total_kurus: i64,
    pub created_at: NaiveDateTime,
    pub ettn: String,
    pub doc_type: Option<String>,
    /// Bu denemeden ÖNCEKİ deneme sayısı (> 0: önceki deneme gönderilmiş olabilir).
    pub prior_attempts: i32,
}

/// Bekleyen kayıtları kiralar: 10 dk başka çalışma almaz. ETTN yoksa burada (gönderimden önce)
/// üretilip yazılır; deneme sayısı artar. Süreç gönderimden sonra düşse bile bir sonraki deneme
/// aynı ETTN'i sorgular.
pub async fn claim_pending(pool: &PgPool, start_at: Option<NaiveDateTime>, limit: i64) -> Result<Vec<Claimed>> {
    Ok(sqlx::query_as::<_, Claimed>(
        r#"
        UPDATE invoices SET
            next_attempt_at = NOW() + INTERVAL '10 minutes',
            ettn = COALESCE(ettn, gen_random_uuid()),
            attempts = attempts + 1,
            updated_at = NOW()
        WHERE id IN (
            SELECT id FROM invoices
            WHERE kind = 'sale' AND status = 'pending'
              AND COALESCE(next_attempt_at, '-infinity') <= NOW()
              AND ($1::timestamp IS NULL OR created_at >= $1)
              -- Geriye dönük kayıtlar (e-belge geçişinden önceki ödemeler) elle kesilir.
              AND source IS DISTINCT FROM 'backfill'
            ORDER BY created_at, id
            LIMIT $2
            FOR UPDATE SKIP LOCKED
        )
        RETURNING id, merchant_oid, member_id, buyer, lines, vat_rate, net_kurus, vat_kurus, total_kurus,
                  created_at, ettn::text AS ettn, doc_type, attempts - 1 AS prior_attempts
        "#,
    )
    .bind(start_at)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// Belge türü ilk denemede belirlenir ve sabitlenir (ETTN o türe aittir).
pub async fn set_doc_type(pool: &PgPool, id: i32, doc_type: &str) -> Result<()> {
    sqlx::query("UPDATE invoices SET doc_type = $2, updated_at = NOW() WHERE id = $1 AND doc_type IS NULL")
        .bind(id)
        .bind(doc_type)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn mark_issued(pool: &PgPool, id: i32, invoice_no: Option<&str>, provider_status: Option<i32>) -> Result<()> {
    sqlx::query(
        "UPDATE invoices SET status = 'issued', provider = 'turkcell', provider_ref = ettn::text,
                invoice_no = COALESCE($2, invoice_no), provider_status = $3,
                issued_at = COALESCE(issued_at, NOW()), invoice_date = COALESCE(invoice_date, created_at),
                last_error = NULL, next_attempt_at = NULL, checked_at = NOW(), updated_at = NOW()
         WHERE id = $1",
    )
    .bind(id)
    .bind(invoice_no)
    .bind(provider_status.map(|s| s as i16))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_failed(pool: &PgPool, id: i32, error: &str, provider_status: Option<i32>) -> Result<()> {
    sqlx::query(
        "UPDATE invoices SET status = 'failed', last_error = $2, provider_status = COALESCE($3, provider_status),
                next_attempt_at = NULL, updated_at = NOW()
         WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .bind(provider_status.map(|s| s as i16))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_cancelled(pool: &PgPool, id: i32) -> Result<()> {
    sqlx::query("UPDATE invoices SET status = 'cancelled', provider_status = 100, next_attempt_at = NULL, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn schedule_retry(pool: &PgPool, id: i32, error: &str, after_secs: i64) -> Result<()> {
    sqlx::query(
        "UPDATE invoices SET last_error = $2, next_attempt_at = NOW() + make_interval(secs => $3), updated_at = NOW()
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(error)
    .bind(after_secs as f64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Kesilmiş ama GİB onayı henüz görülmemiş kayıt (durum takibi).
#[derive(Debug, sqlx::FromRow)]
pub struct Unconfirmed {
    pub id: i32,
    pub merchant_oid: String,
    pub member_id: i32,
    pub ettn: String,
    pub doc_type: String,
    pub invoice_no: Option<String>,
}

/// Son 2 günde kesilmiş, onayı (60) görülmemiş, 5 dakikadır sorgulanmamış kayıtlar.
pub async fn unconfirmed(pool: &PgPool, limit: i64) -> Result<Vec<Unconfirmed>> {
    Ok(sqlx::query_as::<_, Unconfirmed>(
        "SELECT id, merchant_oid, member_id, ettn::text AS ettn, doc_type, invoice_no FROM invoices
         WHERE status = 'issued' AND provider = 'turkcell' AND ettn IS NOT NULL AND doc_type IS NOT NULL
           AND COALESCE(provider_status, 0) < 60
           AND issued_at > NOW() - INTERVAL '2 days'
           AND COALESCE(checked_at, '-infinity') <= NOW() - INTERVAL '5 minutes'
         ORDER BY issued_at LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

pub async fn set_provider_status(pool: &PgPool, id: i32, status: i32) -> Result<()> {
    sqlx::query("UPDATE invoices SET provider_status = $2, checked_at = NOW(), updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(status as i16)
        .execute(pool)
        .await?;
    Ok(())
}

/// GİB e-Fatura kullanıcısıysa alıcı posta kutusu.
pub async fn einvoice_alias(pool: &PgPool, identifier: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT alias FROM einvoice_users WHERE identifier = $1")
        .bind(identifier)
        .fetch_optional(pool)
        .await?)
}

/// Liste en son ne zaman yenilendi (boşsa None).
pub async fn users_synced_at(pool: &PgPool) -> Result<Option<NaiveDateTime>> {
    Ok(sqlx::query_scalar("SELECT max(synced_at) FROM einvoice_users").fetch_one(pool).await?)
}

/// Listeyi tek işlemde değiştirir (yarım liste görülmez).
pub async fn replace_users(pool: &PgPool, users: &std::collections::HashMap<String, String>) -> Result<usize> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM einvoice_users").execute(&mut *tx).await?;
    let all: Vec<(&String, &String)> = users.iter().collect();
    for chunk in all.chunks(10_000) {
        let ids: Vec<&str> = chunk.iter().map(|(k, _)| k.as_str()).collect();
        let aliases: Vec<&str> = chunk.iter().map(|(_, v)| v.as_str()).collect();
        sqlx::query("INSERT INTO einvoice_users (identifier, alias) SELECT * FROM UNNEST($1::text[], $2::text[])")
            .bind(&ids)
            .bind(&aliases)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(all.len())
}

// ── Üye ve yönetici ─────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct MemberInvoice {
    pub id: i32,
    pub created_at: NaiveDateTime,
    pub invoice_no: Option<String>,
    pub total_kurus: i64,
    pub status: String,
    pub doc_type: Option<String>,
    pub has_pdf: bool,
}

pub async fn member_invoices(pool: &PgPool, member_id: i32) -> Result<Vec<MemberInvoice>> {
    Ok(sqlx::query_as::<_, MemberInvoice>(
        "SELECT id, created_at, invoice_no, total_kurus, status, doc_type,
                (status = 'issued' AND provider = 'turkcell' AND ettn IS NOT NULL) AS has_pdf
         FROM invoices WHERE member_id = $1 AND kind = 'sale' AND status <> 'cancelled'
         ORDER BY created_at DESC, id DESC LIMIT 200",
    )
    .bind(member_id)
    .fetch_all(pool)
    .await?)
}

/// PDF için: (üye, belge türü, ETTN, numara) — yalnızca Turkcell'de kesilmişse.
pub async fn pdf_ref(pool: &PgPool, id: i32) -> Result<Option<(i32, String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT member_id, doc_type, ettn::text, invoice_no FROM invoices
         WHERE id = $1 AND status = 'issued' AND provider = 'turkcell' AND ettn IS NOT NULL AND doc_type IS NOT NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct InvoiceState {
    pub member_id: i32,
    pub status: String,
    pub invoice_no: Option<String>,
    pub last_error: Option<String>,
}

pub async fn state<'e>(ex: impl sqlx::PgExecutor<'e>, id: i32) -> Result<Option<InvoiceState>> {
    Ok(sqlx::query_as::<_, InvoiceState>("SELECT member_id, status, invoice_no, last_error FROM invoices WHERE id = $1")
        .bind(id)
        .fetch_optional(ex)
        .await?)
}

/// Hatalı kaydı yeniden kuyruğa alır (ETTN ve belge türü korunur: önce durum sorgulanır).
pub async fn requeue<'e>(ex: impl sqlx::PgExecutor<'e>, id: i32) -> Result<bool> {
    let r = sqlx::query(
        "UPDATE invoices SET status = 'pending', attempts = GREATEST(attempts, 1), next_attempt_at = NULL,
                last_error = NULL, updated_at = NOW()
         WHERE id = $1 AND status IN ('failed', 'pending')",
    )
    .bind(id)
    .execute(ex)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Entegratör dışında kesildi (ör. e-belge geçişinden önceki ödeme).
pub async fn mark_manual<'e>(ex: impl sqlx::PgExecutor<'e>, id: i32, invoice_no: Option<&str>) -> Result<bool> {
    let r = sqlx::query(
        "UPDATE invoices SET status = 'manual', invoice_no = COALESCE($2, invoice_no), next_attempt_at = NULL, updated_at = NOW()
         WHERE id = $1 AND status IN ('failed', 'pending')",
    )
    .bind(id)
    .bind(invoice_no)
    .execute(ex)
    .await?;
    Ok(r.rows_affected() > 0)
}
