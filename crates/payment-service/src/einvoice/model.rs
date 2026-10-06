//! `invoices` kaydından Turkcell fatura modeli (saf; ağ ve veritabanı yok).
//!
//! KDV tutarı entegratöre hesaplatılmaz: kayıttaki ayrım (`billing::split_vat`, net + KDV = tahsil
//! edilen toplam) aynen gönderilir. Entegratör kendisi `round(net × %20)` hesaplasaydı bazı
//! tutarlarda fatura toplamı tahsilattan 1 kuruş sapardı (ör. 1,05 TL → 0,88 + 0,18).

use chrono::{FixedOffset, NaiveDateTime};
use serde_json::{json, Value};

use super::DocType;
use crate::config::EinvoiceConfig;

/// Nihai tüketici (TCKN'si bilinmeyen bireysel alıcı) için GİB'in kabul ettiği kimlik.
pub const END_CONSUMER_ID: &str = "11111111111";
/// Adresi bilinmeyen bireysel alıcı (ilçe ve il zorunlu alan).
const UNKNOWN_DISTRICT: &str = "Merkez";
const UNKNOWN_CITY: &str = "İstanbul";
const COUNTRY: &str = "Türkiye";

pub struct Input<'a> {
    pub merchant_oid: &'a str,
    pub ettn: &'a str,
    pub doc: DocType,
    /// e-Fatura alıcı posta kutusu (`urn:mail:…pk@…`).
    pub alias: Option<&'a str>,
    pub buyer: &'a Value,
    pub line_name: &'a str,
    pub vat_rate: i64,
    pub net_kurus: i64,
    pub vat_kurus: i64,
    /// Ödeme anı (UTC); fatura ve ödeme tarihi olarak Türkiye saatiyle yazılır.
    pub paid_at: NaiveDateTime,
}

fn tr_datetime(t: NaiveDateTime) -> String {
    let tz = FixedOffset::east_opt(3 * 3600).expect("geçerli ofset");
    t.and_utc().with_timezone(&tz).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Kuruş → TL (JSON sayısı, iki basamak).
fn tl(kurus: i64) -> Value {
    json!((kurus as f64) / 100.0)
}

fn s<'a>(b: &'a Value, k: &str) -> Option<&'a str> {
    b.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

/// "Ayşe Nur Yılmaz" → ("Ayşe Nur", "Yılmaz"). Tek kelimede soyad olarak da aynı kelime yazılır
/// (11 haneli kimlikte soyad zorunlu).
pub fn split_name(full: &str) -> (String, String) {
    let words: Vec<&str> = full.split_whitespace().collect();
    match words.as_slice() {
        [] => ("Müşteri".into(), "Müşteri".into()),
        [one] => ((*one).into(), (*one).into()),
        [rest @ .., last] => (rest.join(" "), (*last).into()),
    }
}

fn address_book(i: &Input) -> Value {
    let b = i.buyer;
    let corporate = s(b, "type") == Some("corporate");
    let id = super::buyer_identifier(b).unwrap_or_else(|| END_CONSUMER_ID.to_string());
    let display = if corporate { s(b, "title") } else { s(b, "name") }.unwrap_or("Müşteri");
    let mut book = json!({
        "identificationNumber": id,
        "receiverCountry": s(b, "country").unwrap_or(COUNTRY),
        "receiverCity": s(b, "city").unwrap_or(UNKNOWN_CITY),
        "receiverDistrict": s(b, "district").unwrap_or(UNKNOWN_DISTRICT),
    });
    // 11 haneli kimlikte (kişi ya da şahıs şirketi) ad ve soyad ayrı alanlarda.
    if id.len() == 11 {
        let (first, last) = split_name(display);
        book["name"] = first.into();
        book["receiverPersonSurName"] = last.into();
    } else {
        book["name"] = display.into();
    }
    if let Some(a) = s(b, "address") {
        book["receiverStreet"] = a.into();
    }
    if let Some(t) = s(b, "tax_office") {
        book["receiverTaxOffice"] = t.into();
    }
    if let Some(e) = s(b, "email") {
        book["receiverEmail"] = e.into();
    }
    if let (DocType::Efatura, Some(alias)) = (i.doc, i.alias) {
        book["alias"] = alias.into();
    }
    book
}

pub fn build(i: &Input, cfg: &EinvoiceConfig) -> Value {
    let when = tr_datetime(i.paid_at);
    let mut general = json!({
        "ettn": i.ettn,
        // e-Arşiv: 4 · e-Fatura temel senaryo: 0
        "invoiceProfileType": match i.doc { DocType::Earchive => 4, DocType::Efatura => 0 },
        "type": 1, // satış
        "issueDate": when,
        "currencyCode": "TRY",
    });
    if let Some(p) = &cfg.prefix {
        general["prefix"] = p.as_str().into();
    }
    let name: String = i.line_name.chars().take(250).collect();
    let mut model = json!({
        "recordType": match i.doc { DocType::Earchive => 0, DocType::Efatura => 1 },
        "status": 20, // kaydet ve gönder
        "localReferenceId": i.merchant_oid,
        "note": format!("Sipariş no: {}", i.merchant_oid),
        "addressBook": address_book(i),
        "generalInfoModel": general,
        "invoiceLines": [{
            "inventoryCard": name,
            "amount": 1,
            "unitCode": "C62",
            "unitPrice": tl(i.net_kurus),
            "lineExtensionAmount": tl(i.net_kurus),
            "vatRate": i.vat_rate,
            "vatAmount": tl(i.vat_kurus),
        }],
        // Tutarlar bizden: KDV ve dip toplam yeniden hesaplanmaz (tahsilatla kuruşu kuruşuna aynı).
        "ublSettingsModel": { "useCalculatedVatAmount": true, "UseCalculatedTotalSummary": true },
        "paymentMeansModel": { "paymentMeansCode": 48, "paymentDueDate": when, "instructionNote": "Kredi/banka kartı" },
    });
    if i.doc == DocType::Earchive {
        model["archiveInfoModel"] = json!({
            "IsInternetSale": true,
            "websiteUrl": cfg.website,
            "shipmentSendType": "ELEKTRONIK",
            "shipmentSenderTcknVkn": cfg.shipment_id,
            "shipmentSenderName": cfg.shipment_name,
            "shipmentDate": when,
            "hideDespatchMessage": true,
        });
        // Fatura e-postasını Turkcell gönderir (e-Arşivde elektronik teslim kaydı entegratörde).
        model["eArsivInfo"] = json!({ "sendEMail": s(i.buyer, "email").is_some() });
    }
    model
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EinvoiceConfig {
        EinvoiceConfig {
            base_url: "https://x".into(),
            api_key: "k".into(),
            start_at: None,
            shipment_id: "1111111111".into(),
            shipment_name: "Elektronik teslimat".into(),
            website: "https://nlink.tr".into(),
            prefix: None,
        }
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn input<'a>(buyer: &'a Value, doc: DocType, alias: Option<&'a str>) -> Input<'a> {
        let (net, vat) = crate::billing::split_vat(29900, 20);
        Input {
            merchant_oid: "s101t1",
            ettn: "7a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d",
            doc,
            alias,
            buyer,
            line_name: "nlink Gold plan aboneliği (aylık)",
            vat_rate: 20,
            net_kurus: net,
            vat_kurus: vat,
            paid_at: at("2026-10-06 22:30:00"),
        }
    }

    #[test]
    fn individual_without_tckn_is_end_consumer_earchive() {
        let b = json!({"type":"individual","name":"Ayşe Nur Yılmaz","email":"a@x.test"});
        let m = build(&input(&b, DocType::Earchive, None), &cfg());
        assert_eq!(m["recordType"], 0);
        assert_eq!(m["status"], 20);
        assert_eq!(m["generalInfoModel"]["invoiceProfileType"], 4);
        // 22:30 UTC → ertesi gün 01:30 TR
        assert_eq!(m["generalInfoModel"]["issueDate"], "2026-10-07 01:30:00");
        let a = &m["addressBook"];
        assert_eq!(a["identificationNumber"], END_CONSUMER_ID);
        assert_eq!(a["name"], "Ayşe Nur");
        assert_eq!(a["receiverPersonSurName"], "Yılmaz");
        assert_eq!(a["receiverCity"], "İstanbul");
        assert_eq!(a["receiverEmail"], "a@x.test");
        assert_eq!(m["eArsivInfo"]["sendEMail"], true);
        assert_eq!(m["archiveInfoModel"]["IsInternetSale"], true);
        assert_eq!(m["archiveInfoModel"]["hideDespatchMessage"], true);
        assert_eq!(m["archiveInfoModel"]["shipmentDate"], "2026-10-07 01:30:00");
        let l = &m["invoiceLines"][0];
        assert_eq!(l["unitPrice"], 249.17);
        assert_eq!(l["vatAmount"], 49.83);
        assert_eq!(l["unitCode"], "C62");
        assert_eq!(m["ublSettingsModel"]["useCalculatedVatAmount"], true);
        assert_eq!(m["localReferenceId"], "s101t1");
        assert!(m["addressBook"].get("alias").is_none());
    }

    #[test]
    fn individual_with_tckn_and_address() {
        let b = json!({"type":"individual","name":"Ali","email":"a@x.test","tax_number":"12345678901",
                       "address":"Örnek Mah. 1","city":"Ankara","district":"Çankaya","country":"Türkiye"});
        let a = &build(&input(&b, DocType::Earchive, None), &cfg())["addressBook"];
        assert_eq!(a["identificationNumber"], "12345678901");
        assert_eq!(a["name"], "Ali");
        assert_eq!(a["receiverPersonSurName"], "Ali");
        assert_eq!(a["receiverStreet"], "Örnek Mah. 1");
        assert_eq!(a["receiverDistrict"], "Çankaya");
    }

    #[test]
    fn corporate_efatura_uses_alias_and_basic_profile() {
        let b = json!({"type":"corporate","title":"Kodeksa Bilişim A.Ş.","tax_number":"1234567802","tax_office":"Kadıköy",
                       "address":"Örnek Mah. 1","city":"İstanbul","district":"Kadıköy","country":"Türkiye","email":"f@x.test"});
        let m = build(&input(&b, DocType::Efatura, Some("urn:mail:defaultpk@x.com")), &cfg());
        assert_eq!(m["recordType"], 1);
        assert_eq!(m["generalInfoModel"]["invoiceProfileType"], 0);
        let a = &m["addressBook"];
        assert_eq!(a["name"], "Kodeksa Bilişim A.Ş.");
        assert!(a.get("receiverPersonSurName").is_none());
        assert_eq!(a["identificationNumber"], "1234567802");
        assert_eq!(a["receiverTaxOffice"], "Kadıköy");
        assert_eq!(a["alias"], "urn:mail:defaultpk@x.com");
        assert!(m.get("archiveInfoModel").is_none() && m.get("eArsivInfo").is_none());
    }

    #[test]
    fn amounts_are_sent_exactly_as_charged() {
        // Entegratör kendisi hesaplasa 1,05 TL → 0,88 + 0,18 = 1,06 olurdu.
        let b = json!({"type":"individual","name":"A","email":"a@x.test"});
        for total in [105_i64, 29900, 89900, 335_040, 1_010_990] {
            let (net, vat) = crate::billing::split_vat(total, 20);
            let mut i = input(&b, DocType::Earchive, None);
            i.net_kurus = net;
            i.vat_kurus = vat;
            let l = &build(&i, &cfg())["invoiceLines"][0];
            let sum = (l["lineExtensionAmount"].as_f64().unwrap() * 100.0).round() as i64
                + (l["vatAmount"].as_f64().unwrap() * 100.0).round() as i64;
            assert_eq!(sum, total, "toplam {total}");
        }
    }

    #[test]
    fn names_split() {
        assert_eq!(split_name("Ayşe Nur  Yılmaz"), ("Ayşe Nur".into(), "Yılmaz".into()));
        assert_eq!(split_name("Ali"), ("Ali".into(), "Ali".into()));
        assert_eq!(split_name("  "), ("Müşteri".into(), "Müşteri".into()));
    }

    #[test]
    fn prefix_and_no_email() {
        let b = json!({"type":"individual","name":"A"});
        let mut c = cfg();
        c.prefix = Some("NLK".into());
        let m = build(&input(&b, DocType::Earchive, None), &c);
        assert_eq!(m["generalInfoModel"]["prefix"], "NLK");
        assert_eq!(m["eArsivInfo"]["sendEMail"], false);
    }
}
