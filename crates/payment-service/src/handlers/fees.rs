//! Yönetici: ödeme kuruluşu komisyon oranları (iç API, X-Internal-Token). Oranlar geçerlilik
//! tarihli eklenir; yanlış girilen satır silinebilir. Her değişiklik iz kaydına yazılır.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{Duration, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::AppError;
use crate::fees::{self, FeeRate};
use crate::handlers::admin_members::{audit, Actor};
use crate::pricing::tl_to_kurus;
use crate::AppState;

/// İz kaydında üyeye bağlı olmayan (sistem geneli) işlemler.
const SYSTEM_MEMBER: i32 = 0;

fn utc(t: NaiveDateTime) -> String {
    t.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 24900 → "2.49", 28750 → "2.875"
fn percent(ppm: i32) -> String {
    let s = format!("{}.{:04}", ppm / 10_000, ppm % 10_000);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn dto(r: &FeeRate, current: bool) -> Value {
    json!({
        "id": r.id,
        "effective_from": utc(r.effective_from),
        "rate_percent": percent(r.rate_ppm),
        "rate_ppm": r.rate_ppm,
        "fixed_kurus": r.fixed_kurus,
        "vat_rate": r.vat_rate,
        "note": r.note,
        "created_by_email": r.created_by_email,
        "created_at": utc(r.created_at),
        "current": current,
    })
}

/// GET /api/v1/admin/fee-rates — en yeni geçerlilik önce; `current` şu an geçerli olan.
pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let rates = fees::list(&state.db).await?;
    let now = Utc::now().naive_utc();
    let current = rates.iter().find(|r| r.effective_from <= now).map(|r| r.id);
    Ok(Json(json!({ "items": rates.iter().map(|r| dto(r, Some(r.id) == current)).collect::<Vec<_>>() })))
}

#[derive(Deserialize)]
pub struct NewRateRequest {
    actor: Actor,
    /// Yüzde: "2,49"
    rate: String,
    /// İşlem başına sabit ücret, TL ("0,25"); boş = 0
    #[serde(default)]
    fixed: Option<String>,
    /// Komisyona eklenen KDV (%); oran KDV dahilse 0
    vat_rate: i16,
    /// "YYYY-MM-DD", Türkiye saatiyle gün başı; boş = şimdi
    #[serde(default)]
    effective_from: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

/// POST /api/v1/admin/fee-rates
pub async fn create(State(state): State<AppState>, Json(req): Json<NewRateRequest>) -> Result<Json<Value>, AppError> {
    let bad = |m: &str| AppError::BadRequest(m.to_string());
    let rate_ppm = fees::parse_percent_ppm(&req.rate).ok_or_else(|| bad("Oran 0–20 arasında bir yüzde olmalı (ör. 2,49)"))?;
    let fixed_kurus = match req.fixed.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => 0,
        Some(s) => tl_to_kurus(&s.replace(',', "."))
            .filter(|k| (0..=100_000).contains(k))
            .ok_or_else(|| bad("Sabit ücret 0–1000 TL arasında olmalı (ör. 0,25)"))? as i32,
    };
    if !(0..=50).contains(&req.vat_rate) {
        return Err(bad("KDV oranı 0–50 arasında olmalı"));
    }
    let effective_from = match req.effective_from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Utc::now().naive_utc(),
        Some(s) => {
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| bad("Tarih YYYY-AA-GG olmalı"))?;
            // Türkiye gece yarısı = önceki gün 21:00 UTC.
            d.and_hms_opt(0, 0, 0).expect("geçerli saat") - Duration::hours(3)
        }
    };
    let note: Option<String> = req.note.map(|n| n.trim().chars().take(300).collect()).filter(|n: &String| !n.is_empty());

    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    let id = fees::insert(
        &mut *tx,
        &fees::NewRate {
            effective_from,
            rate_ppm,
            fixed_kurus,
            vat_rate: req.vat_rate,
            note: note.as_deref(),
            actor_id: req.actor.actor_id,
            actor_email: req.actor.actor_email.as_deref(),
        },
    )
    .await?
    .ok_or_else(|| bad("Bu tarihte başlayan bir oran zaten var; önce onu silin"))?;
    let after = json!({
        "id": id, "effective_from": utc(effective_from), "rate_percent": percent(rate_ppm),
        "fixed_kurus": fixed_kurus, "vat_rate": req.vat_rate,
    });
    audit(&mut *tx, &req.actor, SYSTEM_MEMBER, "fee_rate.create", &Value::Null, &after, note.as_deref()).await?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    tracing::info!(id, rate_ppm, fixed_kurus, actor = req.actor.actor_id, "Komisyon oranı eklendi");
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct DeleteRequest {
    actor: Actor,
    #[serde(default)]
    note: Option<String>,
}

/// POST /api/v1/admin/fee-rates/:id/delete — yanlış girilen oranı kaldırır.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i32>, Json(req): Json<DeleteRequest>) -> Result<Json<Value>, AppError> {
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    let Some(old) = fees::delete(&mut *tx, id).await? else {
        return Err(AppError::BadRequest("Oran bulunamadı".into()));
    };
    audit(&mut *tx, &req.actor, SYSTEM_MEMBER, "fee_rate.delete", &dto(&old, false), &Value::Null, req.note.as_deref()).await?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    tracing::info!(id, actor = req.actor.actor_id, "Komisyon oranı silindi");
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::percent;

    #[test]
    fn percent_format() {
        assert_eq!(percent(24_900), "2.49");
        assert_eq!(percent(28_750), "2.875");
        assert_eq!(percent(30_000), "3");
        assert_eq!(percent(0), "0");
        assert_eq!(percent(5), "0.0005");
    }
}
