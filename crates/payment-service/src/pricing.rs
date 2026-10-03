//! Fiyat tablosu, tutar biçimleri ve yükseltme (upgrade) fark ücreti hesabı.
//! Tutarlar kuruş (i64) olarak hesaplanır; PayTR'a ve DB'ye TL biçiminde ("349.00") yazılır.

use chrono::{Datelike, Months, NaiveDateTime};

/// Planın sıralaması: düşük = düşük plan. Upgrade/downgrade tespiti için.
pub fn plan_rank(plan: &str) -> u8 {
    match plan.to_lowercase().as_str() {
        "silver"     => 1,
        "gold"       => 2,
        "enterprise" => 3,
        _            => 0, // standard / free
    }
}

/// Yıllık faturalandırmada uygulanan indirim (%).
const YEARLY_DISCOUNT_PCT: i64 = 20;

/// Aylık tutarı faturalandırma dönemine çevirir: yıllık = 12 ay − %20.
pub fn cycle_amount_kurus(monthly_kurus: i64, billing_cycle: &str) -> Option<i64> {
    match billing_cycle {
        "monthly" => Some(monthly_kurus),
        "yearly" => Some(monthly_kurus * 12 * (100 - YEARLY_DISCOUNT_PCT) / 100),
        _ => None,
    }
}

/// Plan ve döneme karşılık gelen tutar (kuruş). Fiyat tablosu yalnızca burada tanımlı;
/// mevcut abonelikler yenilemede kayıtlı tutarlarıyla devam eder.
pub fn plan_amount_kurus(plan: &str, billing_cycle: &str) -> Option<i64> {
    let monthly = match plan {
        "silver" => 34_900,
        "gold" => 89_900,
        _ => return None,
    };
    cycle_amount_kurus(monthly, billing_cycle)
}

pub const ENTERPRISE_MAX_USERS: i32 = 100;
pub const ENTERPRISE_MAX_EXTRA: i32 = 10_000;

/// Enterprise dönem fiyatı (kuruş). Negatif/aşırı değerler reddedilir — aksi halde
/// `extra_links=-9` gibi değerlerle fiyat düşürülebiliyordu. Hata: kullanıcı mesajı.
pub fn enterprise_price_kurus(users: i32, extra_links: i32, extra_clicks: i32, billing_cycle: &str) -> Result<i64, String> {
    if !(1..=ENTERPRISE_MAX_USERS).contains(&users) {
        return Err(format!("Kullanıcı sayısı 1–{} arasında olmalı", ENTERPRISE_MAX_USERS));
    }
    if !(0..=ENTERPRISE_MAX_EXTRA).contains(&extra_links) || !(0..=ENTERPRISE_MAX_EXTRA).contains(&extra_clicks) {
        return Err(format!("Ek link/tıklama paketi 0–{} arasında olmalı", ENTERPRISE_MAX_EXTRA));
    }
    let monthly = 199_900
        + (users as i64 - 1) * 15_000
        + extra_links as i64 * 10_000
        + extra_clicks as i64 * 5_000;
    cycle_amount_kurus(monthly, billing_cycle).ok_or_else(|| "Geçersiz faturalandırma dönemi".to_string())
}

/// Kuruşu PayTR'ın beklediği TL biçimine çevirir: 34900 → "349.00".
pub fn format_tl(kurus: i64) -> String {
    format!("{}.{:02}", kurus / 100, kurus % 100)
}

/// Kayıtlı TL tutarını kuruşa çevirir ("149.00" → 14900). Eski (noktasız) kayıtlar
/// kuruş kabul edilir.
pub fn amount_to_kurus(amount: &str) -> Option<i64> {
    let a = amount.trim();
    match a.split_once('.') {
        Some((tl, kr)) if !kr.is_empty() && kr.len() <= 2 => {
            let tl: i64 = tl.parse().ok()?;
            let kr: i64 = format!("{:0<2}", kr).parse().ok()?;
            Some(tl * 100 + kr)
        }
        Some(_) => None,
        None => a.parse().ok(),
    }
}

/// PayTR yanıtlarındaki TL tutarını kuruşa çevirir ("10.8" → 1080, "299" → 29900).
/// `amount_to_kurus`'tan farkı: noktasız değer TL'dir (eski DB kaydı değil).
pub fn tl_to_kurus(amount: &str) -> Option<i64> {
    let a = amount.trim();
    match a.split_once('.') {
        None => a.parse::<i64>().ok().map(|tl| tl * 100),
        Some(_) => amount_to_kurus(a),
    }
}

pub fn cycle_months(billing_cycle: &str) -> u32 {
    if billing_cycle == "yearly" { 12 } else { 1 }
}

/// Fatura döngüsüne göre bitiş ve sonraki ödeme tarihleri. Sonraki ödeme = bitiş anı;
/// ödeme alınamazsa grace süresince günlük tekrar denenir.
pub fn billing_dates(billing_cycle: &str, from: NaiveDateTime) -> (NaiveDateTime, NaiveDateTime) {
    let expires_at = from + Months::new(cycle_months(billing_cycle));
    (expires_at, expires_at)
}

fn days_in_month(d: NaiveDateTime) -> u32 {
    let first = d.date().with_day(1).expect("1. gün her ayda var");
    let next = first + Months::new(1);
    (next - first).num_days() as u32
}

/// Yenileme dönemi: `billing_dates` gibi, ancak önceki bitiş ay sonuna kırpılmışsa
/// (31 Oca → 28 Şub) abonelik başladığı güne geri döner (28 Şub → 31 Mar, 28 Mar değil).
pub fn renewal_dates(billing_cycle: &str, from: NaiveDateTime, anchor_day: u32) -> (NaiveDateTime, NaiveDateTime) {
    let (mut exp, _) = billing_dates(billing_cycle, from);
    let from_was_clamped = from.day() == days_in_month(from) && anchor_day > from.day();
    if from_was_clamped {
        let day = anchor_day.min(days_in_month(exp));
        exp = exp.with_day(day).unwrap_or(exp);
    }
    (exp, exp)
}

/// Yükseltmede alınacak en düşük tutar (kuruş). Kalan değer yeni planı karşılıyorsa
/// ödeme alınamaz; kullanıcı daha uzun bir dönem seçmelidir.
pub const MIN_CHARGE_KURUS: i64 = 100;

/// Mevcut dönemin kullanılmamış kısmının değeri (kuruş): dönem tutarı × kalan süre / dönem süresi.
/// Dönem başı `expires_at − döngü` kabul edilir; kalan süre dönem süresini aşamaz.
pub fn unused_credit_kurus(period_kurus: i64, billing_cycle: &str, expires_at: NaiveDateTime, now: NaiveDateTime) -> i64 {
    if period_kurus <= 0 || expires_at <= now {
        return 0;
    }
    let start = expires_at - Months::new(cycle_months(billing_cycle));
    let total = (expires_at - start).num_seconds();
    if total <= 0 {
        return 0;
    }
    let remaining = (expires_at - now).num_seconds().min(total);
    (period_kurus as i128 * remaining as i128 / total as i128) as i64
}

/// Referans linkiyle gelen üyenin ilk ödemesindeki indirim (%).
pub const REFERRAL_DISCOUNT_PCT: i64 = 20;
/// Referans ödülü kredisi: bir aylık Gold (kuruş).
pub const REFERRAL_CREDIT_KURUS: i64 = 89_900;

/// Kupon ya da referans indirimi. Abonelik metadata'sında `discount` olarak saklanır; yenilemede
/// `cycles_left` > 0 (ya da süresiz: None) oldukça uygulanır.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Discount {
    /// "coupon" | "referral"
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_kurus: Option<i64>,
    /// İlk ödemeden SONRAKİ kaç yenilemede daha geçerli (None = süresiz).
    pub cycles_left: Option<i64>,
}

impl Discount {
    /// Bu tutardan düşülecek indirim (kuruş). Tahsilat en az `MIN_CHARGE_KURUS` kalır.
    pub fn amount_off(&self, kurus: i64) -> i64 {
        let raw = match (self.percent, self.fixed_kurus) {
            (Some(p), _) => kurus * p.clamp(0, 100) / 100,
            (None, Some(f)) => f.max(0),
            _ => 0,
        };
        raw.min((kurus - MIN_CHARGE_KURUS).max(0))
    }

    /// Yenilemede hâlâ geçerli mi?
    pub fn applies_to_renewal(&self) -> bool {
        self.cycles_left.map_or(true, |n| n > 0)
    }

    /// Başarılı yenilemeden sonraki hâli.
    pub fn after_renewal(&self) -> Discount {
        Discount { cycles_left: self.cycles_left.map(|n| (n - 1).max(0)), ..self.clone() }
    }
}

/// Kupon kodunu normalleştirir (büyük harf, boşluksuz); biçim dışıysa None.
pub fn normalize_coupon_code(raw: &str) -> Option<String> {
    let c = raw.trim().to_uppercase();
    (c.len() >= 3 && c.len() <= 40 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discounts() {
        let pct = Discount { source: "coupon".into(), code: Some("X".into()), percent: Some(30), fixed_kurus: None, cycles_left: Some(2) };
        assert_eq!(pct.amount_off(89_900), 26_970);
        let fixed = Discount { percent: None, fixed_kurus: Some(100_000), ..pct.clone() };
        // Sabit indirim tutarı aşamaz; en az MIN_CHARGE kalır.
        assert_eq!(fixed.amount_off(34_900), 34_900 - MIN_CHARGE_KURUS);
        assert!(pct.applies_to_renewal());
        let once = pct.after_renewal().after_renewal();
        assert_eq!(once.cycles_left, Some(0));
        assert!(!once.applies_to_renewal());
        let forever = Discount { cycles_left: None, ..pct };
        assert!(forever.after_renewal().applies_to_renewal());
        assert_eq!(normalize_coupon_code(" yaz30 ").as_deref(), Some("YAZ30"));
        assert_eq!(normalize_coupon_code("a b"), None);
        assert_eq!(normalize_coupon_code("ab"), None);
    }

    fn at(y: i32, m: u32, d: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap()
    }

    #[test]
    fn converts_amounts_to_kurus() {
        assert_eq!(amount_to_kurus("149.00"), Some(14_900));
        assert_eq!(amount_to_kurus("101099.00"), Some(10_109_900));
        assert_eq!(amount_to_kurus("12.5"), Some(1_250));
        assert_eq!(amount_to_kurus("99900"), Some(99_900)); // eski kayıt: kuruş
        assert_eq!(amount_to_kurus("1.234"), None);
        assert_eq!(amount_to_kurus("abc"), None);
        assert_eq!(tl_to_kurus("10.8"), Some(1_080));
        assert_eq!(tl_to_kurus("299"), Some(29_900));
        assert_eq!(tl_to_kurus("849.34"), Some(84_934));
        assert_eq!(tl_to_kurus("x"), None);
    }

    #[test]
    fn monthly_billing_adds_one_month() {
        let from = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap().and_hms_opt(10, 0, 0).unwrap();
        let (exp, next) = billing_dates("monthly", from);
        assert_eq!(exp.to_string(), "2026-02-28 10:00:00");
        assert_eq!(exp, next);
    }

    #[test]
    fn yearly_billing_adds_twelve_months() {
        let from = chrono::NaiveDate::from_ymd_opt(2028, 2, 29).unwrap().and_hms_opt(10, 0, 0).unwrap();
        let (exp, _) = billing_dates("yearly", from);
        assert_eq!(exp.to_string(), "2029-02-28 10:00:00");
    }

    #[test]
    fn renewal_returns_to_anchor_day_after_short_month() {
        let t = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(10, 0, 0).unwrap();
        // 31 Oca'da başladı: 28 Şub → 31 Mar → 30 Nis (ay sonu)
        assert_eq!(renewal_dates("monthly", t(2026, 2, 28), 31).0, t(2026, 3, 31));
        assert_eq!(renewal_dates("monthly", t(2026, 3, 31), 31).0, t(2026, 4, 30));
        // Kırpılmamış tarihte değişmez; ayın 15'i başlangıcı etkilenmez
        assert_eq!(renewal_dates("monthly", t(2026, 3, 5), 31).0, t(2026, 4, 5));
        assert_eq!(renewal_dates("monthly", t(2026, 2, 15), 15).0, t(2026, 3, 15));
        // Artık yılda yıllık: 29 Şub 2028 → 28 Şub 2029 → 29 Şub 2032'ye kadar ay sonu
        assert_eq!(renewal_dates("yearly", t(2029, 2, 28), 29).0, t(2030, 2, 28));
    }

    #[test]
    fn credit_is_unused_share_of_period() {
        // Aylık 30 günlük dönemin (Nis) yarısı kalmış → yarı tutar.
        assert_eq!(unused_credit_kurus(34_900, "monthly", at(2026, 5, 1), at(2026, 4, 16)), 17_450);
        // Süresi dolmuş ya da tutarsız → 0.
        assert_eq!(unused_credit_kurus(34_900, "monthly", at(2026, 5, 1), at(2026, 5, 2)), 0);
        assert_eq!(unused_credit_kurus(0, "monthly", at(2026, 5, 1), at(2026, 4, 16)), 0);
        // Kalan süre dönemi aşamaz (eski "süre aktarma" kayıtları) → en fazla dönem tutarı.
        assert_eq!(unused_credit_kurus(34_900, "monthly", at(2026, 7, 1), at(2026, 4, 1)), 34_900);
    }

    #[test]
    fn yearly_silver_to_monthly_gold_leaves_nothing_to_charge() {
        // Önceki açık: Silver yıllık aldıktan 1 gün sonra Gold aylık ≈ 13 ay Gold. Artık kalan
        // değer (~3.340 TL) Gold aylığı (899 TL) aşıyor → fark alınamaz, kullanıcı yıllık seçmeli.
        let silver_yearly = plan_amount_kurus("silver", "yearly").unwrap();
        let credit = unused_credit_kurus(silver_yearly, "yearly", at(2027, 4, 1), at(2026, 4, 2));
        assert!(plan_amount_kurus("gold", "monthly").unwrap() - credit < MIN_CHARGE_KURUS);
        // Gold yıllık: fark = 8.630,40 − kalan değer.
        let charge = plan_amount_kurus("gold", "yearly").unwrap() - credit;
        assert!(charge > 520_000 && charge < 530_000, "{charge}");
    }

    #[test]
    fn enterprise_price_rejects_negative_and_huge() {
        assert_eq!(enterprise_price_kurus(1, 0, 0, "monthly").unwrap(), 199_900);
        assert!(enterprise_price_kurus(1, -9, -1, "monthly").is_err());
        assert!(enterprise_price_kurus(0, 0, 0, "monthly").is_err());
        assert!(enterprise_price_kurus(-5, 0, 0, "monthly").is_err());
        assert!(enterprise_price_kurus(101, 0, 0, "monthly").is_err());
        assert!(enterprise_price_kurus(1, 10_001, 0, "monthly").is_err());
        assert!(enterprise_price_kurus(1, 0, i32::MAX, "monthly").is_err());
        assert!(enterprise_price_kurus(1, 0, 0, "weekly").is_err());
    }

    #[test]
    fn enterprise_add_ons_and_yearly() {
        // 1.999 + 1 ek kullanıcı (150) + 2 link paketi (200) + 3 tıklama paketi (150) = 2.499 TL
        assert_eq!(format_tl(enterprise_price_kurus(2, 2, 3, "monthly").unwrap()), "2499.00");
        assert_eq!(format_tl(enterprise_price_kurus(2, 2, 3, "yearly").unwrap()), "23990.40");
    }

    #[test]
    fn plan_ranks() {
        assert!(plan_rank("gold") > plan_rank("silver"));
        assert!(plan_rank("enterprise") > plan_rank("gold"));
        assert_eq!(plan_rank("standard"), 0);
    }
}
