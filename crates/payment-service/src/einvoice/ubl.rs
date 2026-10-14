//! `invoices` kaydından UBL-TR 2.1 fatura ve Uyumsoft `InvoiceInfo` XML'i (saf; ağ yok).
//!
//! Tutarlar Turkcell modelindeki gibi kayıttaki ayrımdan aynen yazılır (net + KDV = tahsilat).
//! UUID bizim ETTN'imizdir; Uyumsoft durum ve PDF'i bu kimlikle sorgulanır. Fatura numarası
//! (`cbc:ID`) boş gönderilir: numarayı entegratör seriden verir.

use chrono::{FixedOffset, NaiveDateTime};
use serde_json::Value;

use super::model::{split_name, Input, END_CONSUMER_ID};
use super::DocType;
use crate::config::{EinvoiceConfig, Seller};

const CAC: &str = "urn:oasis:names:specification:ubl:schema:xsd:CommonAggregateComponents-2";
const CBC: &str = "urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2";
const UNKNOWN_DISTRICT: &str = "Merkez";
const UNKNOWN_CITY: &str = "İstanbul";
const COUNTRY: &str = "Türkiye";

/// XML metin ve öznitelik kaçışı.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0'da izinsiz denetim karakterleri atılır.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// Kuruş → "249.17".
fn amount(kurus: i64) -> String {
    let sign = if kurus < 0 { "-" } else { "" };
    let k = kurus.unsigned_abs();
    format!("{sign}{}.{:02}", k / 100, k % 100)
}

fn tr(t: NaiveDateTime) -> chrono::DateTime<FixedOffset> {
    t.and_utc().with_timezone(&FixedOffset::east_opt(3 * 3600).expect("geçerli ofset"))
}

fn s<'a>(b: &'a Value, k: &str) -> Option<&'a str> {
    b.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

struct Party<'a> {
    id: &'a str,
    /// VKN'de unvan, TCKN'de ad soyad.
    name: &'a str,
    street: Option<&'a str>,
    district: &'a str,
    city: &'a str,
    postal_zone: Option<&'a str>,
    country: &'a str,
    tax_office: Option<&'a str>,
    email: Option<&'a str>,
    phone: Option<&'a str>,
    website: Option<&'a str>,
    /// MERSİS ve ticaret sicil no (yalnız satıcıda).
    extra_ids: Vec<(&'static str, &'a str)>,
}

fn tag(out: &mut String, name: &str, value: &str) {
    out.push_str(&format!("<{name}>{}</{name}>", esc(value)));
}

fn party(p: &Party) -> String {
    let scheme = if p.id.len() == 11 { "TCKN" } else { "VKN" };
    let mut x = String::from("<cac:Party>");
    if let Some(w) = p.website {
        tag(&mut x, "cbc:WebsiteURI", w);
    }
    x.push_str(&format!("<cac:PartyIdentification><cbc:ID schemeID=\"{scheme}\">{}</cbc:ID></cac:PartyIdentification>", esc(p.id)));
    for (scheme, id) in &p.extra_ids {
        x.push_str(&format!("<cac:PartyIdentification><cbc:ID schemeID=\"{scheme}\">{}</cbc:ID></cac:PartyIdentification>", esc(id)));
    }
    if scheme == "VKN" {
        x.push_str("<cac:PartyName>");
        tag(&mut x, "cbc:Name", p.name);
        x.push_str("</cac:PartyName>");
    }
    x.push_str("<cac:PostalAddress>");
    if let Some(st) = p.street {
        tag(&mut x, "cbc:StreetName", st);
    }
    tag(&mut x, "cbc:CitySubdivisionName", p.district);
    tag(&mut x, "cbc:CityName", p.city);
    if let Some(z) = p.postal_zone {
        tag(&mut x, "cbc:PostalZone", z);
    }
    x.push_str("<cac:Country>");
    tag(&mut x, "cbc:Name", p.country);
    x.push_str("</cac:Country></cac:PostalAddress>");
    if let Some(o) = p.tax_office {
        x.push_str("<cac:PartyTaxScheme><cac:TaxScheme>");
        tag(&mut x, "cbc:Name", o);
        x.push_str("</cac:TaxScheme></cac:PartyTaxScheme>");
    }
    if p.email.is_some() || p.phone.is_some() {
        x.push_str("<cac:Contact>");
        if let Some(ph) = p.phone {
            tag(&mut x, "cbc:Telephone", ph);
        }
        if let Some(e) = p.email {
            tag(&mut x, "cbc:ElectronicMail", e);
        }
        x.push_str("</cac:Contact>");
    }
    // 11 haneli kimlikte (kişi ya da şahıs şirketi) ad ve soyad zorunlu.
    if scheme == "TCKN" {
        let (first, last) = split_name(p.name);
        x.push_str("<cac:Person>");
        tag(&mut x, "cbc:FirstName", &first);
        tag(&mut x, "cbc:FamilyName", &last);
        x.push_str("</cac:Person>");
    }
    x.push_str("</cac:Party>");
    x
}

fn seller_party<'a>(seller: &'a Seller, website: &'a str) -> Party<'a> {
    let mut extra_ids = Vec::new();
    if let Some(m) = seller.mersis.as_deref() {
        extra_ids.push(("MERSISNO", m));
    }
    if let Some(t) = seller.trade_registry.as_deref() {
        extra_ids.push(("TICARETSICILNO", t));
    }
    Party {
        id: &seller.vkn,
        name: &seller.title,
        street: Some(&seller.street),
        district: &seller.district,
        city: &seller.city,
        postal_zone: seller.postal_zone.as_deref(),
        country: COUNTRY,
        tax_office: Some(&seller.tax_office),
        email: seller.email.as_deref(),
        phone: seller.phone.as_deref(),
        website: Some(website),
        extra_ids,
    }
}

/// Alıcı kimliği: girilmişse VKN/TCKN, yoksa nihai tüketici.
pub fn buyer_id(buyer: &Value) -> String {
    super::buyer_identifier(buyer).unwrap_or_else(|| END_CONSUMER_ID.to_string())
}

fn buyer_party<'a>(b: &'a Value, id: &'a str) -> Party<'a> {
    let corporate = s(b, "type") == Some("corporate");
    Party {
        id,
        name: if corporate { s(b, "title") } else { s(b, "name") }.unwrap_or("Müşteri"),
        street: s(b, "address"),
        district: s(b, "district").unwrap_or(UNKNOWN_DISTRICT),
        city: s(b, "city").unwrap_or(UNKNOWN_CITY),
        postal_zone: None,
        country: s(b, "country").unwrap_or(COUNTRY),
        tax_office: s(b, "tax_office"),
        email: s(b, "email"),
        phone: None,
        website: None,
        extra_ids: Vec::new(),
    }
}

/// UBL-TR `Invoice` gövdesi (kök öğe `Invoice`, Uyumsoft'un `InvoiceInfo` içindeki adıyla).
pub fn invoice(i: &Input, cfg: &EinvoiceConfig, seller: &Seller) -> String {
    let when = tr(i.paid_at);
    let date = when.format("%Y-%m-%d").to_string();
    let time = when.format("%H:%M:%S").to_string();
    let net = amount(i.net_kurus);
    let vat = amount(i.vat_kurus);
    let total = amount(i.net_kurus + i.vat_kurus);
    let profile = match i.doc {
        DocType::Earchive => "EARSIVFATURA",
        DocType::Efatura => "TEMELFATURA",
    };
    let id = buyer_id(i.buyer);
    let name: String = i.line_name.chars().take(250).collect();
    let cur = "currencyID=\"TRY\"";

    let mut x = format!("<Invoice xmlns:cac=\"{CAC}\" xmlns:cbc=\"{CBC}\">");
    x.push_str("<cbc:UBLVersionID>2.1</cbc:UBLVersionID><cbc:CustomizationID>TR1.2</cbc:CustomizationID>");
    x.push_str(&format!("<cbc:ProfileID>{profile}</cbc:ProfileID>"));
    x.push_str("<cbc:ID></cbc:ID><cbc:CopyIndicator>false</cbc:CopyIndicator>");
    x.push_str(&format!("<cbc:UUID>{}</cbc:UUID>", esc(i.ettn)));
    x.push_str(&format!("<cbc:IssueDate>{date}</cbc:IssueDate><cbc:IssueTime>{time}</cbc:IssueTime>"));
    x.push_str("<cbc:InvoiceTypeCode>SATIS</cbc:InvoiceTypeCode>");
    tag(&mut x, "cbc:Note", &format!("Sipariş no: {}", i.merchant_oid));
    x.push_str("<cbc:DocumentCurrencyCode>TRY</cbc:DocumentCurrencyCode><cbc:LineCountNumeric>1</cbc:LineCountNumeric>");
    // GİB şeması imza öğesini bekler; imzayı entegratör atar.
    x.push_str(&format!(
        "<cac:Signature><cbc:ID schemeID=\"VKN_TCKN\">{v}</cbc:ID><cac:SignatoryParty><cac:PartyIdentification><cbc:ID schemeID=\"{s}\">{v}</cbc:ID></cac:PartyIdentification></cac:SignatoryParty><cac:DigitalSignatureAttachment><cac:ExternalReference><cbc:URI>#Signature_{e}</cbc:URI></cac:ExternalReference></cac:DigitalSignatureAttachment></cac:Signature>",
        v = esc(&seller.vkn),
        s = if seller.vkn.len() == 11 { "TCKN" } else { "VKN" },
        e = esc(i.ettn),
    ));
    x.push_str("<cac:AccountingSupplierParty>");
    x.push_str(&party(&seller_party(seller, &cfg.website)));
    x.push_str("</cac:AccountingSupplierParty><cac:AccountingCustomerParty>");
    x.push_str(&party(&buyer_party(i.buyer, &id)));
    x.push_str("</cac:AccountingCustomerParty>");
    // 48: kredi/banka kartı.
    x.push_str(&format!(
        "<cac:PaymentMeans><cbc:PaymentMeansCode>48</cbc:PaymentMeansCode><cbc:PaymentDueDate>{date}</cbc:PaymentDueDate><cbc:InstructionNote>Kredi/banka kartı</cbc:InstructionNote></cac:PaymentMeans>"
    ));
    let tax_total = format!(
        "<cac:TaxTotal><cbc:TaxAmount {cur}>{vat}</cbc:TaxAmount><cac:TaxSubtotal><cbc:TaxableAmount {cur}>{net}</cbc:TaxableAmount><cbc:TaxAmount {cur}>{vat}</cbc:TaxAmount><cbc:Percent>{rate}</cbc:Percent><cac:TaxCategory><cac:TaxScheme><cbc:Name>KDV</cbc:Name><cbc:TaxTypeCode>0015</cbc:TaxTypeCode></cac:TaxScheme></cac:TaxCategory></cac:TaxSubtotal></cac:TaxTotal>",
        rate = i.vat_rate
    );
    x.push_str(&tax_total);
    x.push_str(&format!(
        "<cac:LegalMonetaryTotal><cbc:LineExtensionAmount {cur}>{net}</cbc:LineExtensionAmount><cbc:TaxExclusiveAmount {cur}>{net}</cbc:TaxExclusiveAmount><cbc:TaxInclusiveAmount {cur}>{total}</cbc:TaxInclusiveAmount><cbc:PayableAmount {cur}>{total}</cbc:PayableAmount></cac:LegalMonetaryTotal>"
    ));
    x.push_str(&format!(
        "<cac:InvoiceLine><cbc:ID>1</cbc:ID><cbc:InvoicedQuantity unitCode=\"C62\">1</cbc:InvoicedQuantity><cbc:LineExtensionAmount {cur}>{net}</cbc:LineExtensionAmount>{tax_total}<cac:Item><cbc:Name>{}</cbc:Name></cac:Item><cac:Price><cbc:PriceAmount {cur}>{net}</cbc:PriceAmount></cac:Price></cac:InvoiceLine>",
        esc(&name)
    ));
    x.push_str("</Invoice>");
    x
}

/// Uyumsoft `SendInvoice` için tek `InvoiceInfo` öğesi (http://tempuri.org/ ad alanında).
pub fn invoice_info(i: &Input, cfg: &EinvoiceConfig, seller: &Seller) -> String {
    let id = buyer_id(i.buyer);
    let corporate = s(i.buyer, "type") == Some("corporate");
    let title = if corporate { s(i.buyer, "title") } else { s(i.buyer, "name") }.unwrap_or("Müşteri");
    let paid = i.paid_at.and_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut x = format!("<InvoiceInfo LocalDocumentId=\"{}\">", esc(i.merchant_oid));
    x.push_str(&invoice(i, cfg, seller));
    // e-Fatura: posta kutusu verilmezse Uyumsoft alıcının varsayılan kutusunu kullanır.
    x.push_str(&format!("<TargetCustomer VknTckn=\"{}\" Title=\"{}\"", esc(&id), esc(title)));
    if let (DocType::Efatura, Some(alias)) = (i.doc, i.alias) {
        x.push_str(&format!(" Alias=\"{}\"", esc(alias)));
    }
    x.push_str("/>");
    if i.doc == DocType::Earchive {
        x.push_str(&format!(
            "<EArchiveInvoiceInfo DeliveryType=\"Electronic\"><InternetSalesInfo><WebAddress>{}</WebAddress><PaymentMidierName>PayTR</PaymentMidierName><PaymentType>KREDIKARTI/BANKAKARTI</PaymentType><PaymentDate>{paid}</PaymentDate><ShipmentInfo><SendDate>{paid}</SendDate><Carier SenderTcknVkn=\"{}\" SenderName=\"{}\"/></ShipmentInfo></InternetSalesInfo></EArchiveInvoiceInfo>",
            esc(&cfg.website),
            esc(&cfg.shipment_id),
            esc(&cfg.shipment_name),
        ));
    }
    x.push_str(match i.doc {
        DocType::Earchive => "<Scenario>eArchive</Scenario>",
        DocType::Efatura => "<Scenario>eInvoice</Scenario>",
    });
    // e-Arşivde faturayı alıcıya Uyumsoft e-postalar (elektronik teslim kaydı entegratörde).
    if let (DocType::Earchive, Some(email)) = (i.doc, s(i.buyer, "email")) {
        x.push_str(&format!(
            "<Notification><Mailing EnableNotification=\"true\" To=\"{}\"><Subject>nlink faturanız</Subject><Attachment Xml=\"false\" Pdf=\"true\" Html=\"false\" AdditionalDocuments=\"false\"/></Mailing></Notification>",
            esc(email)
        ));
    }
    x.push_str(&format!("<CreateDateUtc>{paid}</CreateDateUtc></InvoiceInfo>"));
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EinvoiceProvider;
    use serde_json::json;

    pub fn seller() -> Seller {
        Seller {
            vkn: "1234567890".into(),
            title: "Örnek Bilişim A.Ş.".into(),
            tax_office: "Kadıköy".into(),
            street: "Örnek Mah. 1 Sok. No:2".into(),
            district: "Kadıköy".into(),
            city: "İstanbul".into(),
            postal_zone: Some("34710".into()),
            email: Some("bilgi@nlink.tr".into()),
            phone: None,
            mersis: Some("0123456789000015".into()),
            trade_registry: None,
        }
    }

    fn cfg() -> EinvoiceConfig {
        EinvoiceConfig {
            provider: EinvoiceProvider::Uyumsoft { url: "https://x".into(), username: "u".into(), password: "p".into() },
            start_at: None,
            shipment_id: "1111111111".into(),
            shipment_name: "Elektronik teslimat".into(),
            website: "https://nlink.tr".into(),
            prefix: None,
            seller: Some(seller()),
        }
    }

    fn input<'a>(buyer: &'a Value, doc: DocType, alias: Option<&'a str>) -> Input<'a> {
        let (net, vat) = crate::billing::split_vat(29900, 20);
        Input {
            merchant_oid: "s101t1",
            ettn: "7a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d",
            doc,
            alias,
            buyer,
            line_name: "nlink Gold plan aboneliği (aylık) <özel> & \"tırnak\"",
            vat_rate: 20,
            net_kurus: net,
            vat_kurus: vat,
            paid_at: NaiveDateTime::parse_from_str("2026-10-06 22:30:00", "%Y-%m-%d %H:%M:%S").unwrap(),
        }
    }

    /// Öğenin metni (ilk geçiş).
    fn text_of<'a>(xml: &'a str, open: &str) -> &'a str {
        let a = xml.find(open).unwrap_or_else(|| panic!("{open} yok")) + open.len();
        let a = a + xml[a..].find('>').unwrap() + 1;
        &xml[a..a + xml[a..].find('<').unwrap()]
    }

    #[test]
    fn earchive_end_consumer() {
        let b = json!({"type":"individual","name":"Ayşe Nur Yılmaz","email":"a@x.test"});
        let x = invoice_info(&input(&b, DocType::Earchive, None), &cfg(), &seller());
        assert!(x.starts_with("<InvoiceInfo LocalDocumentId=\"s101t1\">"));
        assert_eq!(text_of(&x, "<cbc:ProfileID"), "EARSIVFATURA");
        assert_eq!(text_of(&x, "<cbc:UUID"), "7a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d");
        // 22:30 UTC → ertesi gün 01:30 TR
        assert_eq!(text_of(&x, "<cbc:IssueDate"), "2026-10-07");
        assert_eq!(text_of(&x, "<cbc:IssueTime"), "01:30:00");
        assert!(x.contains("<cbc:ID schemeID=\"TCKN\">11111111111</cbc:ID>"));
        assert!(x.contains("<cbc:FirstName>Ayşe Nur</cbc:FirstName><cbc:FamilyName>Yılmaz</cbc:FamilyName>"));
        assert!(x.contains("<cbc:CityName>İstanbul</cbc:CityName>"));
        assert!(x.contains("<Scenario>eArchive</Scenario>"));
        assert!(x.contains("DeliveryType=\"Electronic\""));
        assert!(x.contains("<PaymentDate>2026-10-06T22:30:00Z</PaymentDate>"));
        assert!(x.contains("<Mailing EnableNotification=\"true\" To=\"a@x.test\">"));
        assert!(x.contains("<TargetCustomer VknTckn=\"11111111111\" Title=\"Ayşe Nur Yılmaz\"/>"));
        // Satıcı
        assert!(x.contains("<cbc:ID schemeID=\"VKN\">1234567890</cbc:ID>"));
        assert!(x.contains("<cbc:ID schemeID=\"MERSISNO\">0123456789000015</cbc:ID>"));
        assert!(x.contains("<cbc:Name>Örnek Bilişim A.Ş.</cbc:Name>"));
        assert!(x.contains("<cbc:WebsiteURI>https://nlink.tr</cbc:WebsiteURI>"));
        // Kaçış
        assert!(x.contains("&lt;özel&gt; &amp; &quot;tırnak&quot;"));
        assert!(!x.contains("<özel>"));
    }

    #[test]
    fn amounts_match_charge() {
        let b = json!({"type":"individual","name":"A"});
        let x = invoice(&input(&b, DocType::Earchive, None), &cfg(), &seller());
        assert_eq!(text_of(&x, "<cbc:TaxableAmount"), "249.17");
        assert_eq!(text_of(&x, "<cbc:TaxAmount"), "49.83");
        assert_eq!(text_of(&x, "<cbc:PayableAmount"), "299.00");
        assert_eq!(text_of(&x, "<cbc:TaxInclusiveAmount"), "299.00");
        assert_eq!(text_of(&x, "<cbc:Percent"), "20");
        assert_eq!(text_of(&x, "<cbc:PriceAmount"), "249.17");
        for (k, s) in [(105, "1.05"), (5, "0.05"), (0, "0.00"), (1_010_990, "10109.90")] {
            assert_eq!(amount(k), s);
        }
    }

    #[test]
    fn efatura_corporate_with_alias_and_no_earchive_info() {
        let b = json!({"type":"corporate","title":"Kurum A.Ş.","tax_number":"1234567802","tax_office":"Şişli",
                       "address":"Örnek 2","city":"İstanbul","district":"Şişli","country":"Türkiye","email":"f@x.test"});
        let x = invoice_info(&input(&b, DocType::Efatura, Some("urn:mail:defaultpk@x.com")), &cfg(), &seller());
        assert_eq!(text_of(&x, "<cbc:ProfileID"), "TEMELFATURA");
        assert!(x.contains("<Scenario>eInvoice</Scenario>"));
        assert!(x.contains("Alias=\"urn:mail:defaultpk@x.com\""));
        assert!(!x.contains("EArchiveInvoiceInfo") && !x.contains("<Notification>"));
        assert!(x.contains("<cbc:ID schemeID=\"VKN\">1234567802</cbc:ID>"));
        assert!(x.contains("<cbc:Name>Şişli</cbc:Name>"));
        assert!(!x.contains("<cbc:FamilyName>Kurum"));
    }

    #[test]
    fn sole_proprietor_seller_uses_tckn_and_person() {
        let mut sl = seller();
        sl.vkn = "12345678901".into();
        sl.title = "Ramazan Örnek".into();
        let b = json!({"type":"individual","name":"A"});
        let x = invoice(&input(&b, DocType::Earchive, None), &cfg(), &sl);
        assert!(x.contains("<cac:PartyIdentification><cbc:ID schemeID=\"TCKN\">12345678901</cbc:ID>"));
        assert!(x.contains("<cbc:FirstName>Ramazan</cbc:FirstName><cbc:FamilyName>Örnek</cbc:FamilyName>"));
    }

    #[test]
    fn control_chars_are_dropped() {
        assert_eq!(esc("a\u{1}b\tc"), "ab\tc");
    }
}
