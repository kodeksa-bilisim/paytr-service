//! Üye hesabı silme / KVKK veri dışa aktarma — iç API (X-Internal-Token), qurlbackend çağırır.
//!
//! Ödeme ve fatura kayıtları yasal saklama yükümlülüğü (VUK/TTK) nedeniyle SİLİNMEZ; kişisel
//! iletişim bilgisi (abonelikteki e-posta/telefon), fatura profili ve kayıtlı kartlar silinir.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;
use serde_json::Value;

use crate::{cards, db::customer_repo, error::AppError, AppState};

#[derive(Serialize)]
pub struct StopRenewalResponse {
    /// Otomatik yenilemesi durdurulan abonelik sayısı.
    stopped: u64,
}

/// POST /api/v1/members/{member_id}/stop-renewal
/// Hesap silme onaylandığında: aktif aboneliğin otomatik yenilemesi durur (erişim dönem sonuna
/// kadar sürer), bekleyen ödeme başlatmaları iptal edilir. İptal e-postası gönderilmez — kullanıcıya
/// hesap silme e-postası gider. Silmeden vazgeçen kullanıcı yenilemeyi ayarlardan geri açabilir.
pub async fn stop_renewal(
    State(state): State<AppState>,
    Path(member_id): Path<i32>,
) -> Result<Json<StopRenewalResponse>, AppError> {
    let res = sqlx::query(
        "UPDATE paytr_subscriptions SET status = 'cancelled', cancelled_at = NOW(), updated_at = NOW()
         WHERE member_id = $1 AND status IN ('active', 'pending')",
    )
    .bind(member_id)
    .execute(&state.db)
    .await
    .map_err(anyhow::Error::from)?;
    if res.rows_affected() > 0 {
        customer_repo::set_subscription_cancelled(&state.db, member_id).await?;
    }
    tracing::info!(member_id, stopped = res.rows_affected(), "Hesap silme: yenileme durduruldu");
    Ok(Json(StopRenewalResponse { stopped: res.rows_affected() }))
}

#[derive(Serialize)]
pub struct EraseResponse {
    /// PayTR'dan silinemeyip pasif olmayan kart sayısı (scheduler yeniden dener).
    cards_remaining: i64,
}

/// POST /api/v1/members/{member_id}/erase — bekleme süresi dolmuş hesabın kalıcı silinmesi.
/// Idempotent: tekrar çağrılabilir. Sıra önemli: önce abonelikler sonlanır (scheduler artık
/// tahsilat denemez), sonra kartlar PayTR'dan silinir, en son kişisel alanlar temizlenir.
pub async fn erase(
    State(state): State<AppState>,
    Path(member_id): Path<i32>,
) -> Result<Json<EraseResponse>, AppError> {
    sqlx::query(
        "UPDATE paytr_subscriptions
         SET status = CASE WHEN status IN ('active', 'pending', 'cancelled') THEN 'expired' ELSE status END,
             expires_at = CASE WHEN status IN ('active', 'pending', 'cancelled') THEN LEAST(COALESCE(expires_at, NOW()), NOW()) ELSE expires_at END,
             next_payment_date = NULL, scheduled_plan = NULL, scheduled_amount = NULL, updated_at = NOW()
         WHERE member_id = $1",
    )
    .bind(member_id)
    .execute(&state.db)
    .await
    .map_err(anyhow::Error::from)?;

    cards::delete_member_cards(&state, member_id).await;

    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    sqlx::query("UPDATE paytr_subscriptions SET user_email = NULL, user_phone = NULL, updated_at = NOW() WHERE member_id = $1")
        .bind(member_id)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    // PayTR'ın silmeyi onayladığı (pasif) kartların maskeli bilgisi de silinir.
    sqlx::query(
        "DELETE FROM paytr_cards c USING paytr_user_tokens t
         WHERE c.utoken = t.utoken AND t.member_id = $1 AND NOT c.is_active",
    )
    .bind(member_id)
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM billing_profiles WHERE member_id = $1")
        .bind(member_id)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;

    let cards_remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM paytr_cards c JOIN paytr_user_tokens t ON t.utoken = c.utoken
         WHERE t.member_id = $1 AND c.is_active",
    )
    .bind(member_id)
    .fetch_one(&state.db)
    .await
    .map_err(anyhow::Error::from)?;

    tracing::info!(member_id, cards_remaining, "Hesap silme: ödeme tarafı kişisel veriler silindi");
    Ok(Json(EraseResponse { cards_remaining }))
}

/// Sorgunun satırlarını JSON dizisi olarak döner (sqlx `json` özelliği olmadan: metin üzerinden).
async fn json_rows(state: &AppState, inner_sql: &str, member_id: i32) -> Result<Value, AppError> {
    let sql = format!("SELECT COALESCE(json_agg(row_to_json(x)), '[]'::json)::text FROM ({inner_sql}) x");
    let text: String = sqlx::query_scalar(&sql)
        .bind(member_id)
        .fetch_one(&state.db)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(serde_json::from_str(&text).map_err(anyhow::Error::from)?)
}

/// GET /api/v1/members/{member_id}/export — KVKK md. 11: üyenin ödeme tarafındaki verileri.
/// Kart token'ları ve PayTR iç kimlikleri dahil edilmez.
pub async fn export(
    State(state): State<AppState>,
    Path(member_id): Path<i32>,
) -> Result<Json<Value>, AppError> {
    let subscriptions = json_rows(
        &state,
        "SELECT id, plan, status, billing_cycle, amount, currency, started_at, expires_at,
                next_payment_date, cancelled_at, user_email, user_phone, created_at
         FROM paytr_subscriptions WHERE member_id = $1 AND status <> 'pending' ORDER BY id",
        member_id,
    )
    .await?;
    let payments = json_rows(
        &state,
        "SELECT merchant_oid AS order_no, amount, currency, status, payment_type, installment_count,
                created_at
         FROM paytr_payments WHERE member_id = $1 ORDER BY id",
        member_id,
    )
    .await?;
    let invoices = json_rows(
        &state,
        "SELECT invoice_no, invoice_date, kind, status, buyer, lines, currency, vat_rate,
                net_kurus, vat_kurus, total_kurus, created_at
         FROM invoices WHERE member_id = $1 ORDER BY id",
        member_id,
    )
    .await?;
    let billing_profile = json_rows(
        &state,
        "SELECT kind, full_name, company_title, tax_number, tax_office, address, city, district, country, updated_at
         FROM billing_profiles WHERE member_id = $1",
        member_id,
    )
    .await?;
    let cards = json_rows(
        &state,
        "SELECT c.last_4, c.card_bank, c.card_schema, c.card_type, c.expiry_month, c.expiry_year, c.created_at
         FROM paytr_cards c JOIN paytr_user_tokens t ON t.utoken = c.utoken
         WHERE t.member_id = $1 AND c.is_active ORDER BY c.id",
        member_id,
    )
    .await?;
    Ok(Json(serde_json::json!({
        "subscriptions": subscriptions,
        "payments": payments,
        "invoices": invoices,
        "billing_profile": billing_profile.get(0).cloned(),
        "saved_cards": cards,
    })))
}
