//! e-Arşiv / e-Fatura kesimi (Turkcell e-Şirket).
//!
//! Ödeme callback'inde `invoices` kaydı `pending` açılır (bkz. `handlers::callback`). Arka plan
//! görevi (`worker`) bekleyen kayıtları entegratöre gönderir, numarayı ve durumu geri yazar.
//! Alıcının VKN/TCKN'si GİB e-Fatura listesindeyse e-Fatura (temel senaryo), değilse e-Arşiv.

pub mod model;
pub mod repo;
pub mod turkcell;
pub mod worker;

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
