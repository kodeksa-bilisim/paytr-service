//! Yönetici üye işlemleri (iç API; yetki Next.js/qurlbackend'de) ve iz kaydı (`admin_actions`).
//!
//! - Ödemesiz plan atama: deneme gibi kartsız, tutarsız abonelik (`metadata.trial` + `manual`);
//!   deneme kuralları aynen geçerli (ücretli sayılmaz, süresi dolunca Standard'a düşer, içindeyken
//!   satın alma ilk ödemedir). Aktif ücretli aboneliği olan üyeye atanmaz.
//! - Enterprise sınırları: `custom_plan` güncellenir; koltuk sayısı ücretli Enterprise
//!   aboneliğinin `metadata.users`'ına da yazılır (yenilemede ezilmesin).
//! - Her değişiklik aynı transaction'da iz kaydına yazılır.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{Duration, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgExecutor;

use super::member::json_rows;
use crate::{
    db::{customer_repo, growth_repo, subscription_repo},
    error::AppError,
    pricing::format_tl,
    AppState,
};

const PLANS: [&str; 3] = ["silver", "gold", "enterprise"];
/// Atanabilecek en uzun süre.
const MAX_GRANT_DAYS: i64 = 730;

fn bad(msg: &str) -> AppError {
    AppError::BadRequest(msg.to_string())
}

async fn audit<'e>(
    ex: impl PgExecutor<'e>,
    actor: &Actor,
    member_id: i32,
    action: &str,
    before: &Value,
    after: &Value,
    note: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO admin_actions (actor_id, actor_email, member_id, action, before, after, note)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(actor.actor_id)
    .bind(&actor.actor_email)
    .bind(member_id)
    .bind(action)
    .bind(before)
    .bind(after)
    .bind(note.map(str::trim).filter(|n| !n.is_empty()))
    .execute(ex)
    .await
    .map_err(anyhow::Error::from)?;
    Ok(())
}

/// Üyenin plan durumu (iz kaydının önce/sonra değeri).
async fn plan_state<'e>(ex: impl PgExecutor<'e>, member_id: i32) -> Result<Option<Value>, AppError> {
    let row: Option<(String, Option<String>, Option<NaiveDateTime>, Option<String>, Option<bool>)> = sqlx::query_as(
        "SELECT user_type, subscription_status, subscription_expires_at, custom_plan, is_active
         FROM customers WHERE member_id = $1",
    )
    .bind(member_id)
    .fetch_optional(ex)
    .await
    .map_err(anyhow::Error::from)?;
    Ok(row.map(|(t, s, e, cp, active)| {
        json!({
            "user_type": t,
            "subscription_status": s,
            "expires_at": e.map(|e| e.format("%Y-%m-%dT%H:%M:%S").to_string()),
            "custom_plan": cp.and_then(|c| serde_json::from_str::<Value>(&c).ok()),
            "is_active": active,
        })
    }))
}

#[derive(Deserialize)]
pub struct Actor {
    actor_id: i32,
    actor_email: Option<String>,
}

// ── Üye özeti ────────────────────────────────────────────────────────────────

/// GET /api/v1/admin/members/:member_id — ödeme tarafındaki her şey (kart token'ları hariç).
pub async fn detail(State(state): State<AppState>, Path(member_id): Path<i32>) -> Result<Json<Value>, AppError> {
    let Some(plan) = plan_state(&state.db, member_id).await? else {
        return Err(bad("Üye bulunamadı"));
    };
    let subscriptions = json_rows(
        &state,
        "SELECT id, plan, status, billing_cycle, amount, started_at, expires_at, next_payment_date,
                cancelled_at, metadata, created_at
         FROM paytr_subscriptions WHERE member_id = $1 ORDER BY id DESC LIMIT 15",
        member_id,
    )
    .await?;
    let payments = json_rows(
        &state,
        "SELECT id, subscription_id, merchant_oid, amount, status, failed_reason_msg, created_at
         FROM paytr_payments WHERE member_id = $1 ORDER BY id DESC LIMIT 15",
        member_id,
    )
    .await?;
    let invoices = json_rows(
        &state,
        "SELECT id, kind, status, total_kurus, buyer, invoice_no, last_error, created_at
         FROM invoices WHERE member_id = $1 ORDER BY id DESC LIMIT 15",
        member_id,
    )
    .await?;
    let coupons = json_rows(
        &state,
        "SELECT code, subscription_id, discount_kurus, created_at FROM coupon_redemptions WHERE member_id = $1 ORDER BY id DESC",
        member_id,
    )
    .await?;
    let referred_by = json_rows(
        &state,
        "SELECT referrer_id, created_at, first_paid_at, rewarded_at, reward FROM referrals WHERE referred_id = $1",
        member_id,
    )
    .await?;
    let billing = json_rows(
        &state,
        "SELECT kind, full_name, company_title, tax_number, tax_office, address, city, district, updated_at
         FROM billing_profiles WHERE member_id = $1",
        member_id,
    )
    .await?;
    let cards: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM paytr_cards c JOIN paytr_user_tokens t ON t.utoken = c.utoken
         WHERE t.member_id = $1 AND c.is_active",
    )
    .bind(member_id)
    .fetch_one(&state.db)
    .await
    .map_err(anyhow::Error::from)?;
    let stats = growth_repo::referral_stats(&state.db, member_id).await?;
    let credit = growth_repo::credit_balance(&state.db, member_id).await?;
    let has_paid_history: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM paytr_subscriptions WHERE member_id = $1 AND started_at IS NOT NULL
                       AND NOT COALESCE((metadata->>'trial')::boolean, false))",
    )
    .bind(member_id)
    .fetch_one(&state.db)
    .await
    .map_err(anyhow::Error::from)?;

    Ok(Json(json!({
        "plan": plan,
        "live_paid_subscription_id": subscription_repo::find_live(&state.db, member_id).await?.map(|s| s.id),
        "trial_eligible": subscription_repo::trial_eligible(&state.db, member_id).await?,
        "has_paid_history": has_paid_history,
        "subscriptions": subscriptions,
        "payments": payments,
        "invoices": invoices,
        "coupons": coupons,
        "referred_by": referred_by.get(0).cloned(),
        "referrals": { "signed_up": stats.signed_up, "paid": stats.paid, "rewarded": stats.rewarded },
        "credit": format_tl(credit.max(0)),
        "billing_profile": billing.get(0).cloned(),
        "active_cards": cards,
    })))
}

// ── Plan atama / Enterprise sınırları / geri alma ───────────────────────────

#[derive(Deserialize)]
pub struct PlanRequest {
    #[serde(flatten)]
    actor: Actor,
    /// "grant" | "limits" | "revoke"
    mode: String,
    plan: Option<String>,
    /// Son gün (Türkiye saatiyle gün sonuna kadar).
    until: Option<NaiveDate>,
    users: Option<i64>,
    links: Option<i64>,
    clicks: Option<i64>,
    note: Option<String>,
}

fn check_range(v: Option<i64>, name: &str, min: i64, max: i64) -> Result<Option<i64>, AppError> {
    match v {
        Some(x) if x < min || x > max => Err(AppError::BadRequest(format!("{name} {min}–{max} arasında olmalı"))),
        other => Ok(other),
    }
}

/// Seçilen günün Türkiye saatiyle 23:59:59'u (UTC olarak).
fn end_of_day_tr(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(23, 59, 59).expect("geçerli saat") - Duration::hours(3)
}

/// Aktif ücretsiz erişimi (deneme ya da atanmış plan) şimdi bitirir; üye Standard'a düşer.
/// Bitirilecek kayıt yoksa false.
async fn end_free_access(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, member_id: i32) -> Result<bool, AppError> {
    let ids: Vec<i32> = sqlx::query_scalar(
        "UPDATE paytr_subscriptions SET status = 'expired', expires_at = NOW(), updated_at = NOW()
         WHERE member_id = $1 AND status = 'active' AND COALESCE((metadata->>'trial')::boolean, false)
         RETURNING id",
    )
    .bind(member_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(anyhow::Error::from)?;
    if ids.is_empty() {
        return Ok(false);
    }
    // Yalnızca müşterinin güncel aboneliği bu kayıtlardan biriyse plan düşer (scheduler ile aynı kural).
    sqlx::query(
        "UPDATE customers
         SET subscription_status = 'expired', user_type = 'Standard', subscription_id = NULL,
             scheduled_plan = NULL, custom_plan = NULL, subscription_expires_at = NOW()
         WHERE member_id = $1 AND subscription_id = ANY($2::int[]::text[])",
    )
    .bind(member_id)
    .bind(&ids)
    .execute(&mut **tx)
    .await
    .map_err(anyhow::Error::from)?;
    Ok(true)
}

/// POST /api/v1/admin/members/:member_id/plan
pub async fn set_plan(
    State(state): State<AppState>,
    Path(member_id): Path<i32>,
    Json(req): Json<PlanRequest>,
) -> Result<Json<Value>, AppError> {
    let users = check_range(req.users, "Koltuk", 1, 1000)?;
    let links = check_range(req.links, "Aylık link", 1, 10_000_000)?;
    let clicks = check_range(req.clicks, "Aylık tıklama", 1, 100_000_000)?;

    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    subscription_repo::lock_member(&mut *tx, member_id).await?;
    let Some(before) = plan_state(&mut *tx, member_id).await? else {
        return Err(bad("Üye bulunamadı"));
    };
    if before["is_active"] == json!(false) {
        return Err(bad("Hesap silinmiş ya da pasif"));
    }

    let action = match req.mode.as_str() {
        "grant" => {
            let plan = req.plan.as_deref().map(str::to_lowercase).unwrap_or_default();
            if !PLANS.contains(&plan.as_str()) {
                return Err(bad("Plan silver, gold ya da enterprise olmalı"));
            }
            let until = req.until.ok_or_else(|| bad("Bitiş tarihi gerekli"))?;
            let today = (Utc::now() + Duration::hours(3)).date_naive();
            if until < today || until > today + Duration::days(MAX_GRANT_DAYS) {
                return Err(bad("Bitiş tarihi bugünden itibaren en fazla 2 yıl olmalı"));
            }
            if subscription_repo::find_live(&state.db, member_id).await?.is_some() {
                return Err(bad(
                    "Üyenin aktif ücretli aboneliği var; plan atanamaz. Enterprise sınırları için 'Sınırlar'ı kullanın.",
                ));
            }
            end_free_access(&mut tx, member_id).await?;
            let now = Utc::now().naive_utc();
            let expires_at = end_of_day_tr(until);
            let sub_id: i32 = sqlx::query_scalar(
                r#"
                INSERT INTO paytr_subscriptions
                    (member_id, plan, status, billing_cycle, amount, currency, started_at, expires_at, metadata)
                VALUES ($1, $2, 'active', 'monthly', '0.00', 'TL', $3, $4, $5)
                RETURNING id
                "#,
            )
            .bind(member_id)
            .bind(&plan)
            .bind(now)
            .bind(expires_at)
            .bind(json!({ "trial": true, "manual": true, "by": req.actor.actor_id }))
            .fetch_one(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
            customer_repo::set_trial(&mut *tx, member_id, &plan, sub_id, expires_at).await?;
            if plan == "enterprise" {
                let cp = json!({
                    "links_limit": links.unwrap_or(10_000),
                    "clicks_limit": clicks.unwrap_or(100_000),
                    "users_limit": users.unwrap_or(1),
                });
                sqlx::query("UPDATE customers SET custom_plan = $2 WHERE member_id = $1")
                    .bind(member_id)
                    .bind(cp.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(anyhow::Error::from)?;
            }
            "plan.grant"
        }
        "limits" => {
            if before["user_type"] != json!("Enterprise") {
                return Err(bad("Sınırlar yalnızca Enterprise üyelerde değiştirilebilir"));
            }
            if users.is_none() && links.is_none() && clicks.is_none() {
                return Err(bad("En az bir sınır girin"));
            }
            let mut cp = before["custom_plan"].as_object().cloned().unwrap_or_default();
            if let Some(u) = users {
                cp.insert("users_limit".into(), json!(u));
            }
            if let Some(l) = links {
                cp.insert("links_limit".into(), json!(l));
            }
            if let Some(c) = clicks {
                cp.insert("clicks_limit".into(), json!(c));
            }
            sqlx::query("UPDATE customers SET custom_plan = $2 WHERE member_id = $1")
                .bind(member_id)
                .bind(Value::Object(cp).to_string())
                .execute(&mut *tx)
                .await
                .map_err(anyhow::Error::from)?;
            // Koltuk, ücretli Enterprise aboneliğinin paketine de yazılır: yenileme ödemesi
            // custom_plan'ı abonelik metadata'sından yeniden kurar.
            if let Some(u) = users {
                sqlx::query(
                    "UPDATE paytr_subscriptions
                     SET metadata = jsonb_set(COALESCE(metadata, '{}'::jsonb), '{users}', to_jsonb($2::int)), updated_at = NOW()
                     WHERE member_id = $1 AND plan = 'enterprise' AND status IN ('active', 'cancelled')
                       AND NOT COALESCE((metadata->>'trial')::boolean, false)",
                )
                .bind(member_id)
                .bind(u as i32)
                .execute(&mut *tx)
                .await
                .map_err(anyhow::Error::from)?;
            }
            "plan.limits"
        }
        "revoke" => {
            if !end_free_access(&mut tx, member_id).await? {
                return Err(bad("Bitirilecek ücretsiz erişim (deneme/atanmış plan) yok"));
            }
            "plan.revoke"
        }
        _ => return Err(bad("mode grant, limits ya da revoke olmalı")),
    };

    let after = plan_state(&mut *tx, member_id).await?.unwrap_or(Value::Null);
    audit(&mut *tx, &req.actor, member_id, action, &before, &after, req.note.as_deref()).await?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    tracing::info!(member_id, actor = req.actor.actor_id, action, "Yönetici plan işlemi");
    Ok(Json(json!({ "ok": true, "plan": after })))
}

// ── Deneme hakkı ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TrialAdminRequest {
    #[serde(flatten)]
    actor: Actor,
    /// "end" | "reset"
    action: String,
    note: Option<String>,
}

/// POST /api/v1/admin/members/:member_id/trial — denemeyi bitir ya da hakkı yeniden tanı.
pub async fn trial(
    State(state): State<AppState>,
    Path(member_id): Path<i32>,
    Json(req): Json<TrialAdminRequest>,
) -> Result<Json<Value>, AppError> {
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    subscription_repo::lock_member(&mut *tx, member_id).await?;
    let Some(before) = plan_state(&mut *tx, member_id).await? else {
        return Err(bad("Üye bulunamadı"));
    };
    let action = match req.action.as_str() {
        "end" => {
            if !end_free_access(&mut tx, member_id).await? {
                return Err(bad("Aktif deneme yok"));
            }
            "trial.end"
        }
        "reset" => {
            let paid: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM paytr_subscriptions WHERE member_id = $1 AND started_at IS NOT NULL
                               AND NOT COALESCE((metadata->>'trial')::boolean, false))",
            )
            .bind(member_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
            if paid {
                return Err(bad("Ücretli aboneliği olmuş üyede deneme hakkı yenilenmez"));
            }
            end_free_access(&mut tx, member_id).await?;
            let r = sqlx::query(
                "UPDATE paytr_subscriptions
                 SET metadata = metadata || '{\"trial_voided\": true}'::jsonb, updated_at = NOW()
                 WHERE member_id = $1 AND COALESCE((metadata->>'trial')::boolean, false)
                   AND NOT COALESCE((metadata->>'trial_voided')::boolean, false)",
            )
            .bind(member_id)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
            if r.rows_affected() == 0 {
                return Err(bad("Üyenin deneme hakkı zaten var"));
            }
            "trial.reset"
        }
        _ => return Err(bad("action end ya da reset olmalı")),
    };
    let after = plan_state(&mut *tx, member_id).await?.unwrap_or(Value::Null);
    audit(&mut *tx, &req.actor, member_id, action, &before, &after, req.note.as_deref()).await?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    tracing::info!(member_id, actor = req.actor.actor_id, action, "Yönetici deneme işlemi");
    Ok(Json(json!({ "ok": true, "plan": after })))
}

// ── İz kaydı ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogRequest {
    #[serde(flatten)]
    actor: Actor,
    member_id: i32,
    action: String,
    before: Option<Value>,
    after: Option<Value>,
    note: Option<String>,
}

/// POST /api/v1/admin/audit — başka servisin (qurlbackend: hesap silme) yaptığı yönetici işlemi.
pub async fn log_action(State(state): State<AppState>, Json(req): Json<LogRequest>) -> Result<Json<Value>, AppError> {
    let ok_name = (3..=40).contains(&req.action.len())
        && req.action.chars().all(|c| c.is_ascii_lowercase() || c == '.' || c == '_');
    if !ok_name {
        return Err(bad("Geçersiz işlem adı"));
    }
    audit(
        &state.db,
        &req.actor,
        req.member_id,
        &req.action,
        &req.before.unwrap_or(Value::Null),
        &req.after.unwrap_or(Value::Null),
        req.note.as_deref(),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    member_id: Option<i32>,
    limit: Option<i64>,
}

/// GET /api/v1/admin/audit?member_id=&limit= — en yeni önce.
pub async fn list_audit(State(state): State<AppState>, Query(q): Query<AuditQuery>) -> Result<Json<Value>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let text: String = sqlx::query_scalar(
        "SELECT COALESCE(json_agg(row_to_json(x)), '[]'::json)::text FROM (
            SELECT id, actor_id, actor_email, member_id, action, before, after, note, created_at
            FROM admin_actions WHERE ($1::int IS NULL OR member_id = $1)
            ORDER BY id DESC LIMIT $2) x",
    )
    .bind(q.member_id)
    .bind(limit)
    .fetch_one(&state.db)
    .await
    .map_err(anyhow::Error::from)?;
    Ok(Json(json!({ "items": serde_json::from_str::<Value>(&text).map_err(anyhow::Error::from)? })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_ends_at_tr_midnight() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(end_of_day_tr(d).format("%Y-%m-%d %H:%M:%S").to_string(), "2026-11-01 20:59:59");
    }

    #[test]
    fn ranges() {
        assert!(check_range(Some(0), "Koltuk", 1, 1000).is_err());
        assert!(check_range(Some(1001), "Koltuk", 1, 1000).is_err());
        assert_eq!(check_range(Some(3), "Koltuk", 1, 1000).unwrap(), Some(3));
        assert_eq!(check_range(None, "Koltuk", 1, 1000).unwrap(), None);
    }
}
