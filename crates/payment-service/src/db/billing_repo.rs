//! Fatura bilgisi (`billing_profiles`) ve faturalar (`invoices`).

use anyhow::Result;
use chrono::NaiveDateTime;
use sqlx::{PgExecutor, PgPool};

use crate::billing::{BillingProfile, InvoiceLine};

pub async fn find_profile<'e>(ex: impl PgExecutor<'e>, member_id: i32) -> Result<Option<BillingProfile>> {
    Ok(sqlx::query_as::<_, BillingProfile>(
        "SELECT kind, company_title, tax_number, tax_office, address, city, district, country
         FROM billing_profiles WHERE member_id = $1",
    )
    .bind(member_id)
    .fetch_optional(ex)
    .await?)
}

/// Kaydeder; üye yoksa (customers FK) false.
pub async fn upsert_profile(pool: &PgPool, member_id: i32, p: &BillingProfile) -> Result<bool> {
    let r = sqlx::query(
        r#"
        INSERT INTO billing_profiles
            (member_id, kind, company_title, tax_number, tax_office, address, city, district, country)
        SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9
        WHERE EXISTS (SELECT 1 FROM customers WHERE member_id = $1)
        ON CONFLICT (member_id) DO UPDATE SET
            kind = EXCLUDED.kind, company_title = EXCLUDED.company_title,
            tax_number = EXCLUDED.tax_number, tax_office = EXCLUDED.tax_office,
            address = EXCLUDED.address, city = EXCLUDED.city, district = EXCLUDED.district,
            country = EXCLUDED.country, updated_at = NOW()
        "#,
    )
    .bind(member_id)
    .bind(&p.kind)
    .bind(&p.company_title)
    .bind(&p.tax_number)
    .bind(&p.tax_office)
    .bind(&p.address)
    .bind(&p.city)
    .bind(&p.district)
    .bind(&p.country)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Faturadaki ad ve e-posta (customers).
pub async fn customer_contact<'e>(ex: impl PgExecutor<'e>, member_id: i32) -> Result<(String, String)> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT name, email FROM customers WHERE member_id = $1")
            .bind(member_id)
            .fetch_optional(ex)
            .await?;
    Ok(row.unwrap_or_default())
}

pub struct NewInvoice<'a> {
    pub payment_id: i32,
    pub merchant_oid: &'a str,
    pub member_id: i32,
    pub buyer: serde_json::Value,
    pub line: InvoiceLine,
    /// Geriye dönük kayıtta ödeme anı (CSV'de doğru aya düşsün); normalde None = şimdi.
    pub created_at: Option<NaiveDateTime>,
    /// Kaydın kaynağı: None = callback, "backfill" = geriye dönük.
    pub source: Option<&'a str>,
}

/// Satış faturası kaydı (`pending`). Aynı ödemeye ikinci kez oluşturulmaz.
pub async fn create_sale<'e>(ex: impl PgExecutor<'e>, inv: NewInvoice<'_>) -> Result<bool> {
    let r = sqlx::query(
        r#"
        INSERT INTO invoices
            (payment_id, merchant_oid, member_id, kind, buyer, lines, vat_rate,
             net_kurus, vat_kurus, total_kurus, created_at, provider)
        VALUES ($1, $2, $3, 'sale', $4, $5, $6, $7, $8, $9, COALESCE($10, NOW()), $11)
        ON CONFLICT (merchant_oid, kind) DO NOTHING
        "#,
    )
    .bind(inv.payment_id)
    .bind(inv.merchant_oid)
    .bind(inv.member_id)
    .bind(&inv.buyer)
    .bind(serde_json::json!([&inv.line]))
    .bind(inv.line.vat_rate as i16)
    .bind(inv.line.net_kurus)
    .bind(inv.line.vat_kurus)
    .bind(inv.line.total_kurus)
    .bind(inv.created_at)
    .bind(inv.source)
    .execute(ex)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Fatura kaydı olmayan başarılı (test dışı) ödeme — geriye dönük kayıt adayı.
#[derive(Debug, sqlx::FromRow)]
pub struct UninvoicedPayment {
    pub id: i32,
    pub merchant_oid: String,
    pub member_id: i32,
    pub amount: String,
    pub paid_at: NaiveDateTime,
    pub plan: Option<String>,
    pub billing_cycle: Option<String>,
    pub is_upgrade: bool,
}

pub async fn uninvoiced_payments(pool: &PgPool) -> Result<Vec<UninvoicedPayment>> {
    Ok(sqlx::query_as::<_, UninvoicedPayment>(
        r#"
        SELECT p.id, p.merchant_oid, p.member_id, p.amount,
               COALESCE(p.callback_received_at, p.created_at)     AS paid_at,
               s.plan, s.billing_cycle,
               COALESCE(s.metadata ? 'upgrade_from', FALSE)       AS is_upgrade
        FROM paytr_payments p
        LEFT JOIN paytr_subscriptions s ON s.id = p.subscription_id
        WHERE p.status = 'success' AND NOT p.test_mode
          AND NOT EXISTS (SELECT 1 FROM invoices i WHERE i.merchant_oid = p.merchant_oid AND i.kind = 'sale')
        ORDER BY paid_at, p.id
        "#,
    )
    .fetch_all(pool)
    .await?)
}

/// Muhasebe dışa aktarımı için fatura satırı.
#[derive(Debug, sqlx::FromRow)]
pub struct InvoiceExportRow {
    pub created_at: NaiveDateTime,
    pub merchant_oid: String,
    pub member_id: i32,
    pub buyer: serde_json::Value,
    pub lines: serde_json::Value,
    pub vat_rate: i16,
    pub net_kurus: i64,
    pub vat_kurus: i64,
    pub total_kurus: i64,
    pub status: String,
    pub invoice_no: Option<String>,
    pub pdf_url: Option<String>,
}

/// [from, to) aralığındaki satış faturaları (UTC), eskiden yeniye.
pub async fn sales_between(pool: &PgPool, from: NaiveDateTime, to: NaiveDateTime) -> Result<Vec<InvoiceExportRow>> {
    Ok(sqlx::query_as::<_, InvoiceExportRow>(
        "SELECT created_at, merchant_oid, member_id, buyer, lines, vat_rate, net_kurus, vat_kurus,
                total_kurus, status, invoice_no, pdf_url
         FROM invoices
         WHERE kind = 'sale' AND created_at >= $1 AND created_at < $2
         ORDER BY created_at, id",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?)
}
