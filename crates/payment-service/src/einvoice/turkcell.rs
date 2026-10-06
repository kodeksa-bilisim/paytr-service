//! Turkcell e-Şirket e-Fatura/e-Arşiv REST istemcisi (https://developer.turkcellesirket.com/fatura).
//!
//! Kimlik doğrulama `x-api-key` başlığıyla. Yanıt sınıflandırması yeniden deneme kararını belirler:
//! 400/422 ve 401/403 kalıcıdır (aynı istek yine reddedilir), 404 kayıt yok, 429/5xx ve ağ
//! hataları geçicidir.

use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::DocType;

#[derive(Debug, thiserror::Error)]
pub enum TcError {
    #[error("Turkcell isteği reddetti: {0}")]
    Validation(String),
    #[error("Turkcell yetki hatası: {0}")]
    Auth(String),
    #[error("Turkcell'de kayıt yok")]
    NotFound,
    #[error("Turkcell'e ulaşılamadı: {0}")]
    Transient(String),
}

impl TcError {
    /// Yeniden denemek sonucu değiştirmez.
    pub fn is_permanent(&self) -> bool {
        matches!(self, TcError::Validation(_) | TcError::Auth(_))
    }

    /// Aynı ETTN zaten kayıtlı ("… ettn '…' sistemde mevcut.").
    pub fn is_duplicate_ettn(&self) -> bool {
        matches!(self, TcError::Validation(m) if m.contains("sistemde mevcut"))
    }
}

/// HTTP durum koduna göre hata türü; gövde mesajı kısaltılarak taşınır.
pub fn classify(status: u16, body: &str) -> TcError {
    let msg: String = body.chars().take(500).collect();
    match status {
        400 | 422 => TcError::Validation(msg),
        401 | 403 => TcError::Auth(msg),
        404 => TcError::NotFound,
        _ => TcError::Transient(format!("HTTP {status}: {msg}")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    pub id: String,
    pub invoice_number: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    /// e-Arşiv: 0 taslak, 20 kuyruk, 30 GİB'e gidiyor, 40 hata, 50 iletildi, 60 onaylandı, 100 iptal.
    pub status: i32,
    pub invoice_number: Option<String>,
    pub message: Option<String>,
}

/// Yanıt alanları belgelerde PascalCase, gerçekte camelCase geliyor: ikisi de kabul edilir.
fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    v.as_object()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

fn text(v: &Value, name: &str) -> Option<String> {
    field(v, name).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty())
}

pub fn parse_created(v: &Value) -> Option<Created> {
    Some(Created { id: text(v, "id")?, invoice_number: text(v, "invoiceNumber")? })
}

pub fn parse_status(v: &Value) -> Option<Status> {
    Some(Status {
        status: field(v, "status")?.as_i64()? as i32,
        invoice_number: text(v, "invoiceNumber"),
        message: text(v, "message"),
    })
}

pub struct Client<'a> {
    pub http: &'a reqwest::Client,
    pub base: &'a str,
    pub key: &'a str,
}

impl Client<'_> {
    async fn call(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, TcError> {
        let resp = req
            .header("x-api-key", self.key)
            .send()
            .await
            .map_err(|e| TcError::Transient(e.without_url().to_string()))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Err(classify(status, &body))
    }

    async fn json(&self, req: reqwest::RequestBuilder) -> Result<Value, TcError> {
        self.call(req).await?.json::<Value>().await.map_err(|e| TcError::Transient(format!("yanıt okunamadı: {e}")))
    }

    /// Faturayı oluşturur ve gönderir (model `status: 20`).
    pub async fn create(&self, doc: DocType, model: &Value) -> Result<Created, TcError> {
        let path = match doc {
            DocType::Earchive => "/v2/earchive/create",
            DocType::Efatura => "/v1/outboxinvoice/create",
        };
        let v = self.json(self.http.post(format!("{}{path}", self.base)).json(model)).await?;
        parse_created(&v).ok_or_else(|| TcError::Transient(format!("beklenmeyen yanıt: {}", truncate(&v))))
    }

    pub async fn status(&self, doc: DocType, id: &str) -> Result<Status, TcError> {
        let v = self.json(self.http.get(format!("{}/v2/{}/{id}/status", self.base, doc.path()))).await?;
        parse_status(&v).ok_or_else(|| TcError::Transient(format!("beklenmeyen yanıt: {}", truncate(&v))))
    }

    /// Faturanın PDF'i (Turkcell'in standart görünümü). Yanıt ham PDF'tir.
    pub async fn pdf(&self, doc: DocType, id: &str) -> Result<Vec<u8>, TcError> {
        let resp = self.call(self.http.get(format!("{}/v2/{}/{id}/pdf/true", self.base, doc.path()))).await?;
        let bytes = resp.bytes().await.map_err(|e| TcError::Transient(e.to_string()))?;
        if !bytes.starts_with(b"%PDF") {
            return Err(TcError::Transient("PDF beklenirken başka içerik geldi".into()));
        }
        Ok(bytes.to_vec())
    }

    /// GİB e-Fatura kullanıcı listesi (zip). Canlıda büyüktür: uzun zaman aşımı.
    pub async fn users_zip(&self) -> Result<Vec<u8>, TcError> {
        let req = self.http.get(format!("{}/v2/gibuser/recipient/zip", self.base)).timeout(Duration::from_secs(600));
        let resp = self.call(req).await?;
        Ok(resp.bytes().await.map_err(|e| TcError::Transient(e.to_string()))?.to_vec())
    }
}

fn truncate(v: &Value) -> String {
    v.to_string().chars().take(300).collect()
}

/// Listedeki tek kayıt (yalnızca gereken alanlar).
#[derive(Deserialize)]
struct GibUser {
    #[serde(rename = "Identifier")]
    identifier: String,
    #[serde(rename = "Alias")]
    alias: String,
    #[serde(rename = "AppType", default)]
    app_type: i64,
    #[serde(rename = "IsActive", default)]
    is_active: bool,
}

/// Bir alıcının birden çok posta kutusu olabilir: `defaultpk` içeren tercih edilir.
fn better(new: &str, old: &str) -> bool {
    new.contains("defaultpk") && !old.contains("defaultpk")
}

/// Zip içindeki JSON dizisini akışla okur (canlı listede milyonlarca kayıt olabilir; tümü belleğe
/// `Value` olarak alınmaz). Etkin e-Fatura (AppType 1) alıcı kutuları (`pk`) → VKN/TCKN → etiket.
pub fn parse_users_zip(zip_bytes: &[u8]) -> anyhow::Result<std::collections::HashMap<String, String>> {
    use serde::de::{SeqAccess, Visitor};

    struct Collect<'m>(&'m mut std::collections::HashMap<String, String>);

    impl<'de> Visitor<'de> for Collect<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("GİB kullanıcı dizisi")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while let Some(u) = seq.next_element::<GibUser>()? {
                let pk = u.alias.to_ascii_lowercase().contains("pk");
                if !u.is_active || u.app_type != 1 || !pk || u.identifier.len() < 10 || u.identifier.len() > 11 {
                    continue;
                }
                match self.0.get(&u.identifier) {
                    Some(old) if !better(&u.alias, old) => {}
                    _ => {
                        self.0.insert(u.identifier, u.alias);
                    }
                }
            }
            Ok(())
        }
    }

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))?;
    anyhow::ensure!(archive.len() > 0, "zip boş");
    let entry = archive.by_index(0)?;
    let mut reader = std::io::BufReader::new(entry);
    // Dosya UTF-8 BOM ile başlıyor.
    let mut head = [0u8; 3];
    let n = reader.read(&mut head)?;
    let prefix: &[u8] = if n == 3 && head == [0xEF, 0xBB, 0xBF] { &[] } else { &head[..n] };
    let mut map = std::collections::HashMap::new();
    let mut de = serde_json::Deserializer::from_reader(prefix.chain(reader));
    serde::Deserializer::deserialize_seq(&mut de, Collect(&mut map))?;
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errors_are_classified() {
        assert!(classify(422, "x").is_permanent());
        assert!(classify(400, "x").is_permanent());
        assert!(classify(403, "EFatura gönderme yetkiniz yoktur.").is_permanent());
        assert!(matches!(classify(404, "yok"), TcError::NotFound));
        assert!(!classify(500, "x").is_permanent());
        assert!(!classify(429, "x").is_permanent());
        assert!(classify(422, r#"{"uyarı":["Yeni kayıt için gönderdiğiniz ettn 'a' sistemde mevcut."]}"#).is_duplicate_ettn());
        assert!(!classify(422, "başka").is_duplicate_ettn());
    }

    #[test]
    fn responses_accept_both_casings() {
        assert_eq!(
            parse_created(&json!({"id":"abc","invoiceNumber":"OGV2026000000352"})),
            Some(Created { id: "abc".into(), invoice_number: "OGV2026000000352".into() })
        );
        assert_eq!(parse_created(&json!({"Id":"abc","InvoiceNumber":"X"})).unwrap().invoice_number, "X");
        assert!(parse_created(&json!({"id":"abc"})).is_none());
        let s = parse_status(&json!({"id":"a","invoiceNumber":"N","message":"","status":60})).unwrap();
        assert_eq!(s, Status { status: 60, invoice_number: Some("N".into()), message: None });
    }

    fn zip_of(json: &str) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file::<_, ()>("gibusers.json", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(json.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn users_zip_keeps_active_receiver_boxes_and_prefers_default() {
        let data = "\u{FEFF}".to_string()
            + r#"[
            {"Identifier":"1234567802","Title":"T","GibUserType":1,"Alias":"urn:mail:a_pk@x.com","AppType":1,"IsActive":true},
            {"Identifier":"1234567802","Title":"T","GibUserType":1,"Alias":"urn:mail:defaultpk@x.com","AppType":1,"IsActive":true},
            {"Identifier":"1234567802","Title":"T","GibUserType":1,"Alias":"urn:mail:z_pk@x.com","AppType":1,"IsActive":true},
            {"Identifier":"2222222222","Title":"Pasif","Alias":"urn:mail:defaultpk@p.com","AppType":1,"IsActive":false},
            {"Identifier":"3333333333","Title":"Gönderici","Alias":"urn:mail:defaultgb@g.com","AppType":1,"IsActive":true},
            {"Identifier":"4444444444","Title":"İrsaliye","Alias":"urn:mail:defaultpk@i.com","AppType":3,"IsActive":true},
            {"Identifier":"55555555555","Title":"Şahıs","Alias":"urn:mail:defaultpk@s.com","AppType":1,"IsActive":true,"DeletionTime":null}
        ]"#;
        let map = parse_users_zip(&zip_of(&data)).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["1234567802"], "urn:mail:defaultpk@x.com");
        assert_eq!(map["55555555555"], "urn:mail:defaultpk@s.com");
    }
}
