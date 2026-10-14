//! Uyumsoft e-Dönüşüm SOAP istemcisi (`/Services/Integration`, WSDL: `?wsdl`).
//!
//! SOAP 1.1, WS-Security `UsernameToken` (düz şifre, HTTPS üstünde) + zaman damgası. Her işlemin
//! sonucu `IsSucceded` / `Message` öznitelikleri taşır: `false` isteğin reddidir (kalıcı). SOAP
//! hatası (HTTP 500 + `Fault`) kimlik doğrulamaysa kalıcı, değilse geçici sayılır.

use std::collections::HashMap;

use base64::Engine;
use chrono::Utc;
use quick_xml::events::Event;

use super::{Created, ProviderError, Status};

const NS: &str = "http://tempuri.org/";

pub struct Client<'a> {
    pub http: &'a reqwest::Client,
    pub url: &'a str,
    pub username: &'a str,
    pub password: &'a str,
}

/// Ayrıştırılmış öğe: yerel ad (ön eksiz), öznitelikler, doğrudan metin.
#[derive(Debug, Default)]
pub struct Elem {
    pub name: String,
    pub attrs: HashMap<String, String>,
    pub text: String,
}

impl Elem {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.get(k).map(String::as_str).filter(|v| !v.is_empty())
    }
}

fn local(qname: &[u8]) -> String {
    let n = String::from_utf8_lossy(qname);
    n.rsplit(':').next().unwrap_or_default().to_string()
}

/// Belgedeki tüm öğeler, belge sırasıyla (yanıtlar küçük ve düz; ağaç gerekmez).
pub fn flatten(xml: &str) -> Result<Vec<Elem>, String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out: Vec<Elem> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                out.push(elem(&e)?);
                stack.push(out.len() - 1);
            }
            Ok(Event::Empty(e)) => out.push(elem(&e)?),
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Text(t)) => {
                if let Some(&i) = stack.last() {
                    out[i].text.push_str(&t.unescape().map_err(|e| e.to_string())?);
                }
            }
            Ok(Event::CData(t)) => {
                if let Some(&i) = stack.last() {
                    out[i].text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("XML okunamadı: {e}")),
        }
    }
    Ok(out)
}

fn elem(e: &quick_xml::events::BytesStart) -> Result<Elem, String> {
    let mut attrs = HashMap::new();
    for a in e.attributes() {
        let a = a.map_err(|e| e.to_string())?;
        let v = a.unescape_value().map_err(|e| e.to_string())?.into_owned();
        attrs.insert(local(a.key.as_ref()), v);
    }
    Ok(Elem { name: local(e.name().as_ref()), attrs, text: String::new() })
}

/// SOAP hatası → hata sınıfı. Kimlik doğrulama hatası kalıcı, diğerleri geçici.
pub fn classify_fault(fault: &str) -> ProviderError {
    let msg: String = fault.chars().take(500).collect();
    let l = msg.to_lowercase();
    let auth = ["authenticat", "security", "unauthorized", "kullanıcı adı", "şifre", "yetki"];
    if auth.iter().any(|w| l.contains(w)) {
        ProviderError::Auth(msg)
    } else {
        ProviderError::Transient(format!("SOAP hatası: {msg}"))
    }
}

/// `{İşlem}Result` öğesi ve ardından gelen öğeler; `IsSucceded=false` → kalıcı ret
/// (mesaj "bulunamadı" içeriyorsa kayıt yok).
pub fn result<'e>(elems: &'e [Elem], op: &str) -> Result<&'e [Elem], ProviderError> {
    let name = format!("{op}Result");
    let Some(i) = elems.iter().position(|e| e.name == name) else {
        if let Some(f) = elems.iter().find(|e| e.name == "faultstring") {
            return Err(classify_fault(&f.text));
        }
        return Err(ProviderError::Transient(format!("beklenmeyen yanıt: {op}Result yok")));
    };
    let r = &elems[i];
    if r.attr("IsSucceded").map(|v| v.eq_ignore_ascii_case("true")) != Some(true) {
        let msg: String = r.attr("Message").unwrap_or("-").chars().take(500).collect();
        let l = msg.to_lowercase();
        if l.contains("bulunamad") || l.contains("not found") {
            return Err(ProviderError::NotFound);
        }
        return Err(ProviderError::Validation(msg));
    }
    Ok(&elems[i..])
}

/// Uyumsoft durumu → Turkcell ölçeği (worker ve kayıt aynı kodları kullanır).
pub fn status_code(s: &str) -> i32 {
    match s {
        "Queued" => 20,
        "Processing" => 30,
        "SentToGib" | "WaitingForAprovement" => 50,
        "Approved" => 60,
        "Error" | "Declined" | "Return" => 40,
        "Canceled" | "EArchivedCanceled" => 100,
        // NotPrepared, NotSend, Draft
        _ => 0,
    }
}

pub fn parse_send(xml: &str) -> Result<Created, ProviderError> {
    let elems = flatten(xml).map_err(ProviderError::Transient)?;
    let r = result(&elems, "SendInvoice")?;
    let v = r.iter().find(|e| e.name == "Value").ok_or_else(|| ProviderError::Transient("gönderim yanıtında fatura yok".into()))?;
    let number = v.attr("Number").ok_or_else(|| ProviderError::Transient("gönderim yanıtında fatura numarası yok".into()))?;
    Ok(Created { invoice_number: number.to_string() })
}

pub fn parse_status(xml: &str) -> Result<Status, ProviderError> {
    let elems = flatten(xml).map_err(ProviderError::Transient)?;
    let r = result(&elems, "QueryOutboxInvoiceStatus")?;
    let v = r.iter().find(|e| e.name == "Value").ok_or(ProviderError::NotFound)?;
    let st = v.attr("Status").unwrap_or("");
    Ok(Status {
        status: status_code(st),
        invoice_number: None,
        message: Some(match v.attr("Message") {
            Some(m) => format!("{st}: {m}"),
            None => st.to_string(),
        })
        .filter(|m| !m.is_empty()),
    })
}

pub fn parse_pdf(xml: &str) -> Result<Vec<u8>, ProviderError> {
    let elems = flatten(xml).map_err(ProviderError::Transient)?;
    let r = result(&elems, "GetOutboxInvoicePdf")?;
    let data = r.iter().find(|e| e.name == "Data").ok_or(ProviderError::NotFound)?;
    let clean: String = data.text.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean)
        .map_err(|e| ProviderError::Transient(format!("PDF çözülemedi: {e}")))?;
    if !bytes.starts_with(b"%PDF") {
        return Err(ProviderError::Transient("PDF beklenirken başka içerik geldi".into()));
    }
    Ok(bytes)
}

pub fn parse_flag(xml: &str, op: &str) -> Result<bool, ProviderError> {
    let elems = flatten(xml).map_err(ProviderError::Transient)?;
    let r = result(&elems, op)?;
    Ok(r[0].attr("Value").is_some_and(|v| v.eq_ignore_ascii_case("true")))
}

impl Client<'_> {
    /// SOAP zarfı: güvenlik başlığı + gövde.
    pub fn envelope(&self, body: &str) -> String {
        let now = Utc::now();
        let created = now.format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let expires = (now + chrono::Duration::minutes(5)).format("%Y-%m-%dT%H:%M:%S%.3fZ");
        format!(
            concat!(
                r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:u="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">"#,
                r#"<s:Header><o:Security s:mustUnderstand="1" xmlns:o="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd">"#,
                r#"<u:Timestamp u:Id="_0"><u:Created>{created}</u:Created><u:Expires>{expires}</u:Expires></u:Timestamp>"#,
                r#"<o:UsernameToken u:Id="uuid-nlink-1"><o:Username>{user}</o:Username>"#,
                r#"<o:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordText">{pass}</o:Password>"#,
                r#"</o:UsernameToken></o:Security></s:Header><s:Body>{body}</s:Body></s:Envelope>"#
            ),
            created = created,
            expires = expires,
            user = super::ubl::esc(self.username),
            pass = super::ubl::esc(self.password),
            body = body,
        )
    }

    async fn call(&self, op: &str, inner: &str) -> Result<String, ProviderError> {
        let body = format!("<{op} xmlns=\"{NS}\">{inner}</{op}>");
        let resp = self
            .http
            .post(self.url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", format!("\"{NS}IIntegration/{op}\""))
            .body(self.envelope(&body))
            .send()
            .await
            .map_err(|e| ProviderError::Transient(e.without_url().to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| ProviderError::Transient(format!("yanıt okunamadı: {e}")))?;
        // SOAP hataları 500 ile gelir; gövdeyi ayrıştırıcı sınıflar.
        if status == 200 || (status == 500 && text.contains("Fault")) {
            return Ok(text);
        }
        let msg: String = text.chars().take(300).collect();
        Err(match status {
            401 | 403 => ProviderError::Auth(format!("HTTP {status}: {msg}")),
            _ => ProviderError::Transient(format!("HTTP {status}: {msg}")),
        })
    }

    /// Tek faturayı gönderir (`InvoiceInfo` XML'i, bkz. `ubl::invoice_info`).
    pub async fn send(&self, invoice_info: &str) -> Result<Created, ProviderError> {
        parse_send(&self.call("SendInvoice", &format!("<invoices>{invoice_info}</invoices>")).await?)
    }

    /// Giden faturanın durumu (e-Fatura ve e-Arşiv), ETTN ile.
    pub async fn status(&self, ettn: &str) -> Result<Status, ProviderError> {
        let inner = format!("<invoiceIds><string>{}</string></invoiceIds>", super::ubl::esc(ettn));
        parse_status(&self.call("QueryOutboxInvoiceStatus", &inner).await?)
    }

    pub async fn pdf(&self, ettn: &str) -> Result<Vec<u8>, ProviderError> {
        let inner = format!("<invoiceId>{}</invoiceId>", super::ubl::esc(ettn));
        parse_pdf(&self.call("GetOutboxInvoicePdf", &inner).await?)
    }

    /// Alıcı GİB e-Fatura kullanıcısı mı (anlık sorgu; liste indirmeye gerek yok).
    pub async fn is_einvoice_user(&self, vkn_tckn: &str) -> Result<bool, ProviderError> {
        let inner = format!("<vknTckn>{}</vknTckn><alias></alias>", super::ubl::esc(vkn_tckn));
        parse_flag(&self.call("IsEInvoiceUser", &inner).await?, "IsEInvoiceUser")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!(r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>{body}</s:Body></s:Envelope>"#)
    }

    #[test]
    fn send_response_gives_number() {
        let x = wrap(r#"<SendInvoiceResponse xmlns="http://tempuri.org/"><SendInvoiceResult IsSucceded="true"><Value Id="7a1b" Number="NLK2026000000001" InvoiceScenario="eArchive"/></SendInvoiceResult></SendInvoiceResponse>"#);
        assert_eq!(parse_send(&x).unwrap(), Created { invoice_number: "NLK2026000000001".into() });
    }

    #[test]
    fn rejected_send_is_permanent() {
        let x = wrap(r#"<SendInvoiceResponse xmlns="http://tempuri.org/"><SendInvoiceResult IsSucceded="false" Message="Şema hatası: &lt;cbc:ID&gt;"/></SendInvoiceResponse>"#);
        match parse_send(&x) {
            Err(ProviderError::Validation(m)) => assert_eq!(m, "Şema hatası: <cbc:ID>"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn faults_are_classified() {
        let f = |s: &str| wrap(&format!(r#"<s:Fault><faultcode>s:Client</faultcode><faultstring xml:lang="tr-TR">{s}</faultstring></s:Fault>"#));
        assert!(matches!(parse_send(&f("An error occurred when verifying security for the message.")), Err(ProviderError::Auth(_))));
        assert!(matches!(parse_send(&f("Object reference not set")), Err(ProviderError::Transient(_))));
        assert!(matches!(parse_send("<html>bakım</html>"), Err(ProviderError::Transient(_))));
    }

    #[test]
    fn status_maps_to_common_scale() {
        let st = |s: &str| {
            parse_status(&wrap(&format!(r#"<QueryOutboxInvoiceStatusResponse xmlns="http://tempuri.org/"><QueryOutboxInvoiceStatusResult IsSucceded="true"><Value Status="{s}" StatusCode="1" InvoiceId="7a1b" Message="m"/></QueryOutboxInvoiceStatusResult></QueryOutboxInvoiceStatusResponse>"#)))
                .unwrap()
        };
        assert_eq!(st("Approved").status, 60);
        assert_eq!(st("Error").status, 40);
        assert_eq!(st("Declined").status, 40);
        assert_eq!(st("EArchivedCanceled").status, 100);
        assert_eq!(st("Queued").status, 20);
        assert_eq!(st("SentToGib").status, 50);
        assert_eq!(st("Draft").status, 0);
        assert_eq!(st("Error").message.as_deref(), Some("Error: m"));
        let empty = wrap(r#"<QueryOutboxInvoiceStatusResponse xmlns="http://tempuri.org/"><QueryOutboxInvoiceStatusResult IsSucceded="true"/></QueryOutboxInvoiceStatusResponse>"#);
        assert!(matches!(parse_status(&empty), Err(ProviderError::NotFound)));
        let missing = wrap(r#"<QueryOutboxInvoiceStatusResponse xmlns="http://tempuri.org/"><QueryOutboxInvoiceStatusResult IsSucceded="false" Message="Fatura bulunamadı"/></QueryOutboxInvoiceStatusResponse>"#);
        assert!(matches!(parse_status(&missing), Err(ProviderError::NotFound)));
    }

    #[test]
    fn pdf_and_flag() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4 test");
        let x = wrap(&format!(r#"<GetOutboxInvoicePdfResponse xmlns="http://tempuri.org/"><GetOutboxInvoicePdfResult IsSucceded="true"><Value InvoiceId="7a1b"><Data>{}
{}</Data></Value></GetOutboxInvoicePdfResult></GetOutboxInvoicePdfResponse>"#, &b64[..8], &b64[8..]));
        assert_eq!(parse_pdf(&x).unwrap(), b"%PDF-1.4 test");
        let f = |v: &str| wrap(&format!(r#"<IsEInvoiceUserResponse xmlns="http://tempuri.org/"><IsEInvoiceUserResult IsSucceded="true" Value="{v}"/></IsEInvoiceUserResponse>"#));
        assert!(parse_flag(&f("true"), "IsEInvoiceUser").unwrap());
        assert!(!parse_flag(&f("false"), "IsEInvoiceUser").unwrap());
    }

    #[test]
    fn envelope_escapes_credentials() {
        let http = reqwest::Client::new();
        let c = Client { http: &http, url: "https://x", username: "u&1", password: "p<2" };
        let e = c.envelope("<X/>");
        assert!(e.contains("<o:Username>u&amp;1</o:Username>"));
        assert!(e.contains("PasswordText\">p&lt;2</o:Password>"));
        assert!(e.contains("<u:Timestamp") && e.contains("<s:Body><X/></s:Body>"));
        assert!(flatten(&e).is_ok());
    }
}
