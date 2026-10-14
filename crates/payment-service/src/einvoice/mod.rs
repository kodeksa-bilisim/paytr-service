//! e-Arşiv / e-Fatura kesimi (entegratör: Uyumsoft ya da Turkcell e-Şirket, `EINVOICE_PROVIDER`).
//!
//! Ödeme callback'inde `invoices` kaydı `pending` açılır (bkz. `handlers::callback`). Arka plan
//! görevi (`worker`) bekleyen kayıtları entegratöre gönderir, numarayı ve durumu geri yazar.
//! Alıcının VKN/TCKN'si GİB e-Fatura kullanıcısıysa e-Fatura (temel senaryo), değilse e-Arşiv.

pub mod model;
pub mod provider;
pub mod repo;
pub mod turkcell;
pub mod ubl;
pub mod uyumsoft;
pub mod worker;

/// Entegratör hatası. Sınıf yeniden deneme kararını belirler: `Validation` ve `Auth` kalıcıdır
/// (aynı istek yine reddedilir), `NotFound` kayıt yok, `Transient` ağ ve sunucu hataları.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("Entegratör isteği reddetti: {0}")]
    Validation(String),
    #[error("Entegratör yetki hatası: {0}")]
    Auth(String),
    #[error("Entegratörde kayıt yok")]
    NotFound,
    #[error("Entegratöre ulaşılamadı: {0}")]
    Transient(String),
}

impl ProviderError {
    /// Yeniden denemek sonucu değiştirmez.
    pub fn is_permanent(&self) -> bool {
        matches!(self, ProviderError::Validation(_) | ProviderError::Auth(_))
    }

    /// Aynı ETTN zaten kayıtlı (Turkcell: "… ettn '…' sistemde mevcut.").
    pub fn is_duplicate_ettn(&self) -> bool {
        matches!(self, ProviderError::Validation(m) if m.contains("sistemde mevcut") || m.contains("daha önce gönderil") || m.contains("zaten kayıtlı"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    pub invoice_number: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    /// Turkcell ölçeği (Uyumsoft durumları buna eşlenir): 0 taslak, 20 kuyruk, 30 GİB'e gidiyor,
    /// 40 hata, 50 iletildi, 60 onaylandı, 100 iptal.
    pub status: i32,
    pub invoice_number: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocType {
    Earchive,
    Efatura,
}

impl DocType {
    pub fn as_str(self) -> &'static str {
        match self {
            DocType::Earchive => "earchive",
            DocType::Efatura => "efatura",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "earchive" => Some(DocType::Earchive),
            "efatura" => Some(DocType::Efatura),
            _ => None,
        }
    }

    /// Turkcell yol öneki (durum, PDF).
    pub fn path(self) -> &'static str {
        match self {
            DocType::Earchive => "earchive",
            DocType::Efatura => "outboxinvoice",
        }
    }
}

/// Faturaya yazılacak VKN/TCKN: kurumsalda zorunlu, bireyselde girildiyse.
pub fn buyer_identifier(buyer: &serde_json::Value) -> Option<String> {
    buyer
        .get("tax_number")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| (t.len() == 10 || t.len() == 11) && t.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_string)
}
