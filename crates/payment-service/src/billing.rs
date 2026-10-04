//! Fatura hesapları (entegratörden bağımsız): KDV ayrımı, satır açıklaması, alıcı kopyası ve
//! kurumsal fatura bilgisi doğrulaması. Fiyatlar KDV DAHİL, oran %20 (2 Ekim 2026 kararı).

use chrono::{FixedOffset, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::email_templates::{cycle_label, plan_label};

/// KDV oranı (%). Fiyatlar bu oran dahil.
pub const VAT_RATE: i64 = 20;

/// KDV dahil toplamı KDV hariç tutar + KDV'ye ayırır (kuruş, yarım yukarı yuvarlama).
/// Toplam her zaman korunur: net + kdv = toplam.
pub fn split_vat(total_kurus: i64, rate_pct: i64) -> (i64, i64) {
    let divisor = 100 + rate_pct;
    let net = (total_kurus * 100 + divisor / 2).div_euclid(divisor);
    (net, total_kurus - net)
}

/// Faturanın neyi kapsadığı.
pub enum InvoiceSubject<'a> {
    /// İlk abonelik ya da yenileme; dönem başı–sonu.
    Period { plan: &'a str, billing_cycle: &'a str, start: NaiveDateTime, end: NaiveDateTime },
    /// Yükseltme: yalnızca fark ücreti alındı.
    Upgrade { plan: &'a str, billing_cycle: &'a str },
    /// Dönem tarihleri bilinmeyen abonelik ödemesi (geriye dönük kayıt: abonelik o günden beri
    /// değişmiş olabilir, dönem yeniden hesaplanamaz).
    Untimed { plan: &'a str, billing_cycle: &'a str, renewal: bool },
    /// Aboneliğe bağlanamayan tahsilat.
    Other,
}

/// Türkiye saati (UTC+3, yaz saati yok). Sütunlar UTC tutulur.
fn tr_date(t: NaiveDateTime) -> String {
    let tz = FixedOffset::east_opt(3 * 3600).expect("geçerli ofset");
    t.and_utc().with_timezone(&tz).format("%d.%m.%Y").to_string()
}

pub fn line_description(subject: &InvoiceSubject) -> String {
    match subject {
        InvoiceSubject::Period { plan, billing_cycle, start, end } => format!(
            "nlink {} plan aboneliği ({}) — {} – {}",
            plan_label(plan),
            cycle_label(billing_cycle),
            tr_date(*start),
            tr_date(*end)
        ),
        InvoiceSubject::Upgrade { plan, billing_cycle } => format!(
            "nlink {} plan yükseltmesi ({}) — fark ücreti",
            plan_label(plan),
            cycle_label(billing_cycle)
        ),
        InvoiceSubject::Untimed { plan, billing_cycle, renewal } => format!(
            "nlink {} plan aboneliği ({}){}",
            plan_label(plan),
            cycle_label(billing_cycle),
            if *renewal { " — yenileme" } else { "" }
        ),
        InvoiceSubject::Other => "nlink hizmet bedeli".to_string(),
    }
}

#[derive(Debug, Serialize)]
pub struct InvoiceLine {
    pub name: String,
    pub quantity: i32,
    pub vat_rate: i64,
    pub net_kurus: i64,
    pub vat_kurus: i64,
    pub total_kurus: i64,
}

/// Tek kalemli fatura satırı + toplamlar.
pub fn single_line(description: String, total_kurus: i64) -> InvoiceLine {
    let (net, vat) = split_vat(total_kurus, VAT_RATE);
    InvoiceLine { name: description, quantity: 1, vat_rate: VAT_RATE, net_kurus: net, vat_kurus: vat, total_kurus }
}

/// Üyenin kayıtlı fatura bilgisi (`billing_profiles`).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct BillingProfile {
    pub kind: String,
    /// Bireysel: faturadaki ad soyad (boşsa hesap adı).
    pub full_name: Option<String>,
    pub company_title: Option<String>,
    /// Kurumsal: VKN/TCKN (zorunlu). Bireysel: TCKN (isteğe bağlı).
    pub tax_number: Option<String>,
    pub tax_office: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub district: Option<String>,
    pub country: String,
}

/// Faturaya yazılan alıcı (kesim anındaki kopya; sonradan profil değişse de fatura değişmez).
/// Bireyselde ad + e-posta, girildiyse TCKN ve adres; kurumsalda vergi bilgileri ve adres.
pub fn buyer_snapshot(profile: Option<&BillingProfile>, name: &str, email: &str) -> serde_json::Value {
    match profile {
        Some(p) if p.kind == "individual" => {
            let mut b = serde_json::json!({
                "type": "individual",
                "name": p.full_name.as_deref().unwrap_or(name),
                "email": email,
            });
            if let Some(t) = &p.tax_number {
                b["tax_number"] = t.as_str().into();
            }
            if let (Some(a), Some(c), Some(d)) = (&p.address, &p.city, &p.district) {
                b["address"] = a.as_str().into();
                b["city"] = c.as_str().into();
                b["district"] = d.as_str().into();
                b["country"] = p.country.as_str().into();
            }
            b
        }
        Some(p) if p.kind == "corporate" => serde_json::json!({
            "type": "corporate",
            "title": p.company_title,
            "tax_number": p.tax_number,
            "tax_office": p.tax_office,
            "address": p.address,
            "city": p.city,
            "district": p.district,
            "country": p.country,
            "email": email,
        }),
        _ => serde_json::json!({ "type": "individual", "name": name, "email": email }),
    }
}

/// Kullanıcıdan gelen fatura bilgisi (iç API gövdesi).
#[derive(Debug, Deserialize)]
pub struct BillingProfileInput {
    pub kind: String,
    #[serde(default)]
    pub full_name: Option<String>,
    pub company_title: Option<String>,
    pub tax_number: Option<String>,
    pub tax_office: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub district: Option<String>,
}

fn clean(v: &Option<String>) -> Option<String> {
    v.as_deref().map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty())
}

fn required(v: &Option<String>, field: &str, min: usize, max: usize) -> Result<String, String> {
    let s = clean(v).ok_or_else(|| format!("{field} zorunlu"))?;
    let n = s.chars().count();
    if n < min || n > max {
        return Err(format!("{field} {min}–{max} karakter olmalı"));
    }
    Ok(s)
}

fn optional(v: &Option<String>, field: &str, min: usize, max: usize) -> Result<Option<String>, String> {
    if clean(v).is_none() {
        return Ok(None);
    }
    required(v, field, min, max).map(Some)
}

/// Doğrular ve normalleştirir.
/// - Bireysel: ad soyad, TCKN (11 hane) ve adres isteğe bağlı; adres girilirse il ve ilçe de
///   zorunlu (yarım adres faturaya yazılmaz). Unvan/vergi dairesi atılır.
/// - Kurumsal: VKN 10, TCKN (şahıs şirketi) 11 hane; sağlama kontrolünü entegratör yapar.
pub fn validate_profile(input: &BillingProfileInput) -> Result<BillingProfile, String> {
    match input.kind.as_str() {
        "individual" => {
            let tax_number = optional(&input.tax_number, "TC kimlik no", 11, 11)?;
            if tax_number.as_deref().is_some_and(|t| !t.bytes().all(|b| b.is_ascii_digit())) {
                return Err("TC kimlik no yalnızca rakam olmalı".into());
            }
            let address = optional(&input.address, "Adres", 5, 500)?;
            let city = optional(&input.city, "İl", 2, 60)?;
            let district = optional(&input.district, "İlçe", 2, 60)?;
            let any = address.is_some() || city.is_some() || district.is_some();
            let all = address.is_some() && city.is_some() && district.is_some();
            if any && !all {
                return Err("Adres girilecekse adres, il ve ilçe birlikte girilmeli".into());
            }
            Ok(BillingProfile {
                kind: "individual".into(),
                full_name: optional(&input.full_name, "Ad soyad", 2, 120)?,
                company_title: None,
                tax_number,
                tax_office: None,
                address,
                city,
                district,
                country: "Türkiye".into(),
            })
        }
        "corporate" => {
            let tax_number = required(&input.tax_number, "Vergi/TC kimlik no", 10, 11)?;
            if !tax_number.bytes().all(|b| b.is_ascii_digit()) {
                return Err("Vergi/TC kimlik no yalnızca rakam olmalı".into());
            }
            Ok(BillingProfile {
                kind: "corporate".into(),
                full_name: None,
                company_title: Some(required(&input.company_title, "Unvan", 2, 250)?),
                tax_number: Some(tax_number),
                tax_office: Some(required(&input.tax_office, "Vergi dairesi", 2, 100)?),
                address: Some(required(&input.address, "Adres", 5, 500)?),
                city: Some(required(&input.city, "İl", 2, 60)?),
                district: Some(required(&input.district, "İlçe", 2, 60)?),
                country: "Türkiye".into(),
            })
        }
        _ => Err("Geçersiz fatura türü".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn vat_split_keeps_total() {
        assert_eq!(split_vat(29900, 20), (24917, 4983)); // 249,17 + 49,83 = 299,00
        assert_eq!(split_vat(89900, 20), (74917, 14983));
        assert_eq!(split_vat(12, 20), (10, 2));
        assert_eq!(split_vat(0, 20), (0, 0));
        for t in [1, 7, 99, 14_900, 335_040, 1_010_990] {
            let (n, v) = split_vat(t, 20);
            assert_eq!(n + v, t);
            assert!((v * 100 - n * 20).abs() <= 100, "{t}: net {n} kdv {v}");
        }
    }

    #[test]
    fn descriptions() {
        let s = InvoiceSubject::Period { plan: "gold", billing_cycle: "monthly", start: dt("2026-11-01 01:13:12"), end: dt("2026-12-01 01:13:12") };
        assert_eq!(line_description(&s), "nlink Gold plan aboneliği (aylık) — 01.11.2026 – 01.12.2026");
        // 22:30 UTC = ertesi gün 01:30 TR
        let s = InvoiceSubject::Period { plan: "silver", billing_cycle: "yearly", start: dt("2026-10-02 22:30:00"), end: dt("2027-10-02 22:30:00") };
        assert_eq!(line_description(&s), "nlink Silver plan aboneliği (yıllık) — 03.10.2026 – 03.10.2027");
        assert_eq!(
            line_description(&InvoiceSubject::Upgrade { plan: "gold", billing_cycle: "monthly" }),
            "nlink Gold plan yükseltmesi (aylık) — fark ücreti"
        );
        assert_eq!(
            line_description(&InvoiceSubject::Untimed { plan: "gold", billing_cycle: "monthly", renewal: true }),
            "nlink Gold plan aboneliği (aylık) — yenileme"
        );
        assert_eq!(
            line_description(&InvoiceSubject::Untimed { plan: "silver", billing_cycle: "yearly", renewal: false }),
            "nlink Silver plan aboneliği (yıllık)"
        );
    }

    fn input(kind: &str) -> BillingProfileInput {
        BillingProfileInput {
            kind: kind.into(),
            full_name: None,
            company_title: Some("  Kodeksa   Bilişim A.Ş. ".into()),
            tax_number: Some("1234567890".into()),
            tax_office: Some("Kadıköy".into()),
            address: Some("Örnek Mah. 1. Sok. No:2".into()),
            city: Some("İstanbul".into()),
            district: Some("Kadıköy".into()),
        }
    }

    #[test]
    fn corporate_profile_validation() {
        let p = validate_profile(&input("corporate")).unwrap();
        assert_eq!(p.company_title.as_deref(), Some("Kodeksa Bilişim A.Ş."));
        let mut i = input("corporate");
        i.tax_number = Some("12345".into());
        assert!(validate_profile(&i).is_err());
        let mut i = input("corporate");
        i.tax_number = Some("12345678ab".into());
        assert!(validate_profile(&i).is_err());
        let mut i = input("corporate");
        i.tax_office = None;
        assert!(validate_profile(&i).is_err());
        assert!(validate_profile(&input("other")).is_err());
    }

    fn individual() -> BillingProfileInput {
        BillingProfileInput {
            kind: "individual".into(),
            full_name: Some("  Ayşe   Yılmaz ".into()),
            company_title: Some("Kodeksa".into()),
            tax_number: Some("12345678901".into()),
            tax_office: Some("Kadıköy".into()),
            address: Some("Örnek Mah. 1. Sok. No:2".into()),
            city: Some("İstanbul".into()),
            district: Some("Kadıköy".into()),
        }
    }

    #[test]
    fn individual_profile_validation() {
        // Kurumsal alanlar atılır, bireysel alanlar normalleşir
        let p = validate_profile(&individual()).unwrap();
        assert_eq!(p.full_name.as_deref(), Some("Ayşe Yılmaz"));
        assert!(p.company_title.is_none() && p.tax_office.is_none());
        assert_eq!(p.tax_number.as_deref(), Some("12345678901"));
        // Hepsi boş: geçerli (hesap adı + e-posta)
        let empty = BillingProfileInput {
            kind: "individual".into(),
            full_name: Some("  ".into()),
            company_title: None,
            tax_number: None,
            tax_office: None,
            address: None,
            city: None,
            district: None,
        };
        let p = validate_profile(&empty).unwrap();
        assert!(p.full_name.is_none() && p.tax_number.is_none() && p.address.is_none());
        // TCKN 11 hane rakam
        let mut i = individual();
        i.tax_number = Some("1234567890".into());
        assert!(validate_profile(&i).is_err());
        let mut i = individual();
        i.tax_number = Some("1234567890a".into());
        assert!(validate_profile(&i).is_err());
        // Yarım adres reddedilir
        let mut i = individual();
        i.district = None;
        assert!(validate_profile(&i).is_err());
    }

    #[test]
    fn buyer_snapshots() {
        let b = buyer_snapshot(None, "Ayşe", "a@x.test");
        assert_eq!(b["type"], "individual");
        assert_eq!(b["name"], "Ayşe");
        let p = validate_profile(&input("corporate")).unwrap();
        let b = buyer_snapshot(Some(&p), "Ayşe", "a@x.test");
        assert_eq!(b["type"], "corporate");
        assert_eq!(b["tax_number"], "1234567890");
        assert_eq!(b["email"], "a@x.test");
        // Bireysel profil: girilen ad hesap adının önüne geçer; TCKN ve adres eklenir
        let p = validate_profile(&individual()).unwrap();
        let b = buyer_snapshot(Some(&p), "Ayşe", "a@x.test");
        assert_eq!(b["type"], "individual");
        assert_eq!(b["name"], "Ayşe Yılmaz");
        assert_eq!(b["tax_number"], "12345678901");
        assert_eq!(b["city"], "İstanbul");
        // Bireysel, ad boş: hesap adı; TCKN/adres yoksa alan da yok
        let mut i = individual();
        i.full_name = None;
        i.tax_number = None;
        i.address = None;
        i.city = None;
        i.district = None;
        let p = validate_profile(&i).unwrap();
        let b = buyer_snapshot(Some(&p), "Ayşe", "a@x.test");
        assert_eq!(b["name"], "Ayşe");
        assert!(b.get("tax_number").is_none() && b.get("address").is_none());
    }
}
