//! Satın almada indirim ve kredi: kupon, referans indirimi (ilk ödeme %20), kredi bakiyesi.
//!
//! Sıra: liste fiyatı → yükseltme kredisi (kalan süre) → indirim (kupon ya da referans; ikisi
//! birden uygunsa büyük olan) → kredi bakiyesi. Tahsilat en az `MIN_CHARGE_KURUS` kalır.
//! Kupon kullanımı ve kredi düşümü ödeme BAŞARILI olunca yazılır (callback).

use chrono::NaiveDateTime;

use crate::db::growth_repo::{self, Coupon};
use crate::error::AppError;
use crate::pricing::{normalize_coupon_code, Discount, MIN_CHARGE_KURUS, REFERRAL_DISCOUNT_PCT};

/// Kuponun bu satın almada kullanılabilir olup olmadığı (kullanıcıya gösterilecek mesajla).
pub fn check_coupon(c: &Coupon, plan: &str, billing_cycle: &str, now: NaiveDateTime) -> Result<(), String> {
    if !c.active {
        return Err("Bu indirim kodu artık geçerli değil.".into());
    }
    if c.valid_until.is_some_and(|u| u < now) {
        return Err("Bu indirim kodunun süresi dolmuş.".into());
    }
    if c.max_redemptions.is_some_and(|m| c.redemptions >= m) {
        return Err("Bu indirim kodunun kullanım sınırı doldu.".into());
    }
    if c.plans.as_ref().is_some_and(|p| !p.is_empty() && !p.iter().any(|x| x == plan)) {
        return Err("Bu indirim kodu seçilen planda geçerli değil.".into());
    }
    if c.billing_cycles.as_ref().is_some_and(|p| !p.is_empty() && !p.iter().any(|x| x == billing_cycle)) {
        return Err(if billing_cycle == "yearly" {
            "Bu indirim kodu yıllık ödemede geçerli değil.".into()
        } else {
            "Bu indirim kodu aylık ödemede geçerli değil.".into()
        });
    }
    Ok(())
}

pub fn coupon_discount(c: &Coupon) -> Discount {
    let (percent, fixed_kurus) = if c.kind == "percent" { (Some(c.value as i64), None) } else { (None, Some(c.value as i64)) };
    Discount {
        source: "coupon".into(),
        code: Some(c.code.clone()),
        percent,
        fixed_kurus,
        // İlk ödeme bir dönem sayılır.
        cycles_left: c.duration_cycles.map(|d| (d as i64 - 1).max(0)),
    }
}

pub fn referral_discount() -> Discount {
    Discount { source: "referral".into(), code: None, percent: Some(REFERRAL_DISCOUNT_PCT), fixed_kurus: None, cycles_left: Some(0) }
}

/// Fiyat dökümü (kuruş).
#[derive(Debug, Clone, Default)]
pub struct Adjusted {
    pub discount: Option<Discount>,
    pub discount_kurus: i64,
    pub credit_used_kurus: i64,
    pub final_kurus: i64,
}

impl Adjusted {
    /// Abonelik metadata'sına eklenecek alanlar.
    pub fn metadata(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let mut m = serde_json::Map::new();
        if let Some(d) = &self.discount {
            m.insert("discount".into(), serde_json::to_value(d).ok()?);
            m.insert("discount_kurus".into(), self.discount_kurus.into());
        }
        if self.credit_used_kurus > 0 {
            m.insert("credit_used_kurus".into(), self.credit_used_kurus.into());
        }
        (!m.is_empty()).then_some(m)
    }
}

/// `charge_kurus` (yükseltme kredisi düşülmüş tutar) üzerine indirim ve krediyi uygular.
/// Geçersiz kupon → 400 (kullanıcı yazdı, sessizce yok sayılmaz).
pub async fn adjust(
    state: &crate::AppData,
    member_id: i32,
    plan: &str,
    billing_cycle: &str,
    coupon_code: Option<&str>,
    charge_kurus: i64,
) -> Result<Adjusted, AppError> {
    let now = chrono::Utc::now().naive_utc();
    let mut candidates: Vec<Discount> = Vec::new();

    if let Some(raw) = coupon_code.map(str::trim).filter(|c| !c.is_empty()) {
        let code = normalize_coupon_code(raw).ok_or_else(|| AppError::BadRequest("İndirim kodu geçersiz.".into()))?;
        let coupon = growth_repo::find_coupon(&state.db, &code)
            .await?
            .ok_or_else(|| AppError::BadRequest("İndirim kodu bulunamadı.".into()))?;
        check_coupon(&coupon, plan, billing_cycle, now).map_err(AppError::BadRequest)?;
        if growth_repo::member_redeemed(&state.db, &code, member_id).await? {
            return Err(AppError::BadRequest("Bu indirim kodunu daha önce kullandınız.".into()));
        }
        candidates.push(coupon_discount(&coupon));
    }
    if growth_repo::has_unpaid_referral(&state.db, member_id).await? {
        candidates.push(referral_discount());
    }

    let best = candidates.into_iter().max_by_key(|d| d.amount_off(charge_kurus));
    let discount_kurus = best.as_ref().map_or(0, |d| d.amount_off(charge_kurus));
    let after = charge_kurus - discount_kurus;

    let balance = growth_repo::credit_balance(&state.db, member_id).await?;
    let credit_used_kurus = balance.min((after - MIN_CHARGE_KURUS).max(0)).max(0);

    Ok(Adjusted {
        discount: best.filter(|_| discount_kurus > 0),
        discount_kurus,
        credit_used_kurus,
        final_kurus: after - credit_used_kurus,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coupon() -> Coupon {
        let now = chrono::Utc::now().naive_utc();
        Coupon {
            code: "YAZ30".into(),
            kind: "percent".into(),
            value: 30,
            plans: Some(vec!["gold".into()]),
            billing_cycles: None,
            duration_cycles: Some(3),
            max_redemptions: Some(2),
            redemptions: 0,
            valid_until: Some(now + chrono::Duration::days(1)),
            active: true,
            note: None,
            created_at: now,
        }
    }

    #[test]
    fn coupon_rules() {
        let now = chrono::Utc::now().naive_utc();
        let c = coupon();
        assert!(check_coupon(&c, "gold", "monthly", now).is_ok());
        assert!(check_coupon(&c, "silver", "monthly", now).is_err());
        assert!(check_coupon(&Coupon { redemptions: 2, ..c.clone() }, "gold", "monthly", now).is_err());
        assert!(check_coupon(&Coupon { active: false, ..c.clone() }, "gold", "monthly", now).is_err());
        assert!(check_coupon(&c, "gold", "monthly", now + chrono::Duration::days(2)).is_err());
        let yearly_only = Coupon { billing_cycles: Some(vec!["yearly".into()]), ..c.clone() };
        assert!(check_coupon(&yearly_only, "gold", "monthly", now).is_err());
        // "İlk 3 ödeme": ilk ödemeden sonra 2 yenileme daha.
        assert_eq!(coupon_discount(&c).cycles_left, Some(2));
        assert_eq!(coupon_discount(&Coupon { duration_cycles: None, ..c }).cycles_left, None);
        assert_eq!(referral_discount().amount_off(89_900), 17_980);
    }
}
