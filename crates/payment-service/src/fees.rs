//! Ödeme kuruluşu komisyonu (tahmini). Oranlar `payment_fee_rates`'te geçerlilik tarihiyle
//! tutulur; bir ödemenin komisyonu ödeme anında geçerli orandan hesaplanır. PayTR callback'i
//! kesintiyi taşımaz, bu yüzden değerler tahmindir (gerçeği PayTR hesap özetinde).

use anyhow::Result;
use chrono::NaiveDateTime;
use serde::Serialize;
use sqlx::{PgExecutor, PgPool};

pub const PROVIDER: &str = "paytr";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FeeRate {
    pub id: i32,
    pub effective_from: NaiveDateTime,
    pub rate_ppm: i32,
    pub fixed_kurus: i32,
    pub vat_rate: i16,
    pub note: Option<String>,
    pub created_by_email: Option<String>,
    pub created_at: NaiveDateTime,
}

impl FeeRate {
    /// Tutarın komisyonu (KDV dahil), kuruş. Yuvarlama: yarım kuruş yukarı.
    pub fn fee_kurus(&self, amount_kurus: i64) -> i64 {
        let base = (amount_kurus * self.rate_ppm as i64 + 500_000) / 1_000_000 + self.fixed_kurus as i64;
        (base * (100 + self.vat_rate as i64) + 50) / 100
    }
}

/// Geçerlilik tarihine göre artan sıralı oran listesi; `at` anında geçerli olanı bulur.
#[derive(Debug, Default)]
pub struct FeeSchedule(Vec<FeeRate>);

impl FeeSchedule {
    pub fn new(mut rates: Vec<FeeRate>) -> Self {
        rates.sort_by_key(|r| r.effective_from);
        Self(rates)
    }

    pub fn at(&self, at: NaiveDateTime) -> Option<&FeeRate> {
        let i = self.0.partition_point(|r| r.effective_from <= at);
        i.checked_sub(1).map(|i| &self.0[i])
    }

    pub fn fee(&self, at: NaiveDateTime, amount_kurus: i64) -> Option<i64> {
        self.at(at).map(|r| r.fee_kurus(amount_kurus))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// "2,49" / "2.49" / "%2.49" → 24900 ppm (en çok 4 ondalık, 0–20 arası).
pub fn parse_percent_ppm(s: &str) -> Option<i32> {
    let s = s.trim().trim_start_matches('%').trim().replace(',', ".");
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    if int.is_empty() || int.len() > 2 || frac.len() > 4 || !int.bytes().chain(frac.bytes()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let ppm = int.parse::<i32>().ok()? * 10_000 + format!("{frac:0<4}").parse::<i32>().ok()?;
    (ppm <= 200_000).then_some(ppm)
}

pub async fn list(pool: &PgPool) -> Result<Vec<FeeRate>> {
    Ok(sqlx::query_as::<_, FeeRate>(
        "SELECT id, effective_from, rate_ppm, fixed_kurus, vat_rate, note, created_by_email, created_at
         FROM payment_fee_rates WHERE provider = $1 ORDER BY effective_from DESC",
    )
    .bind(PROVIDER)
    .fetch_all(pool)
    .await?)
}

pub async fn schedule(pool: &PgPool) -> Result<FeeSchedule> {
    Ok(FeeSchedule::new(list(pool).await?))
}

pub struct NewRate<'a> {
    pub effective_from: NaiveDateTime,
    pub rate_ppm: i32,
    pub fixed_kurus: i32,
    pub vat_rate: i16,
    pub note: Option<&'a str>,
    pub actor_id: i32,
    pub actor_email: Option<&'a str>,
}

/// Aynı geçerlilik anında oran varsa None.
pub async fn insert<'e>(ex: impl PgExecutor<'e>, r: &NewRate<'_>) -> Result<Option<i32>> {
    Ok(sqlx::query_scalar(
        "INSERT INTO payment_fee_rates (provider, effective_from, rate_ppm, fixed_kurus, vat_rate, note, created_by, created_by_email)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (provider, effective_from) DO NOTHING
         RETURNING id",
    )
    .bind(PROVIDER)
    .bind(r.effective_from)
    .bind(r.rate_ppm)
    .bind(r.fixed_kurus)
    .bind(r.vat_rate)
    .bind(r.note)
    .bind(r.actor_id)
    .bind(r.actor_email)
    .fetch_optional(ex)
    .await?)
}

/// Silinen satır (iz kaydı için) ya da None.
pub async fn delete<'e>(ex: impl PgExecutor<'e>, id: i32) -> Result<Option<FeeRate>> {
    Ok(sqlx::query_as::<_, FeeRate>(
        "DELETE FROM payment_fee_rates WHERE id = $1 AND provider = $2
         RETURNING id, effective_from, rate_ppm, fixed_kurus, vat_rate, note, created_by_email, created_at",
    )
    .bind(id)
    .bind(PROVIDER)
    .fetch_optional(ex)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn rate(from: &str, ppm: i32, fixed: i32, vat: i16) -> FeeRate {
        FeeRate {
            id: 0,
            effective_from: t(from),
            rate_ppm: ppm,
            fixed_kurus: fixed,
            vat_rate: vat,
            note: None,
            created_by_email: None,
            created_at: t(from),
        }
    }

    #[test]
    fn percent_parsing() {
        assert_eq!(parse_percent_ppm("2,49"), Some(24_900));
        assert_eq!(parse_percent_ppm("%2.49"), Some(24_900));
        assert_eq!(parse_percent_ppm("3"), Some(30_000));
        assert_eq!(parse_percent_ppm("2.8750"), Some(28_750));
        assert_eq!(parse_percent_ppm("0"), Some(0));
        assert_eq!(parse_percent_ppm("20"), Some(200_000));
        assert_eq!(parse_percent_ppm("20.01"), None);
        assert_eq!(parse_percent_ppm("2.49999"), None);
        assert_eq!(parse_percent_ppm("-1"), None);
        assert_eq!(parse_percent_ppm(""), None);
        assert_eq!(parse_percent_ppm(".5"), None);
        assert_eq!(parse_percent_ppm("1e2"), None);
    }

    #[test]
    fn fee_amounts() {
        // 299 TL × %2,49 = 7,4451 → 7,45; + %20 KDV = 8,94
        assert_eq!(rate("2026-01-01 00:00:00", 24_900, 0, 20).fee_kurus(29_900), 894);
        // KDV dahil oran
        assert_eq!(rate("2026-01-01 00:00:00", 24_900, 0, 0).fee_kurus(29_900), 745);
        // sabit ücret: 1,05 TL × %1,99 = 0,02 + 0,25 = 0,27 → KDV'li 0,32
        assert_eq!(rate("2026-01-01 00:00:00", 19_900, 25, 20).fee_kurus(105), 32);
        assert_eq!(rate("2026-01-01 00:00:00", 0, 0, 20).fee_kurus(29_900), 0);
    }

    #[test]
    fn schedule_picks_rate_in_effect() {
        let s = FeeSchedule::new(vec![
            rate("2026-10-01 00:00:00", 30_000, 0, 0),
            rate("2026-01-01 00:00:00", 20_000, 0, 0),
        ]);
        assert_eq!(s.fee(t("2025-12-31 23:59:59"), 10_000), None);
        assert_eq!(s.fee(t("2026-01-01 00:00:00"), 10_000), Some(200));
        assert_eq!(s.fee(t("2026-09-30 23:59:59"), 10_000), Some(200));
        assert_eq!(s.fee(t("2026-10-01 00:00:00"), 10_000), Some(300));
        assert!(FeeSchedule::default().fee(t("2026-10-01 00:00:00"), 1).is_none());
    }
}
