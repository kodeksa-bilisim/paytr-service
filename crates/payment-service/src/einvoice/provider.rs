//! Seçili entegratörün ortak yüzü: gönder, durum, PDF, alıcı türü. Worker ve PDF ucu yalnız bunu
//! kullanır; entegratöre özgü model (Turkcell JSON, Uyumsoft UBL) burada seçilir.

use super::{model, turkcell, ubl, uyumsoft, Created, DocType, ProviderError, Status};
use crate::config::{EinvoiceConfig, EinvoiceProvider};

pub enum Client<'a> {
    Uyumsoft(uyumsoft::Client<'a>),
    Turkcell(turkcell::Client<'a>),
}

impl<'a> Client<'a> {
    pub fn new(http: &'a reqwest::Client, cfg: &'a EinvoiceConfig) -> Self {
        match &cfg.provider {
            EinvoiceProvider::Uyumsoft { url, username, password } => Client::Uyumsoft(uyumsoft::Client { http, url, username, password }),
            EinvoiceProvider::Turkcell { base_url, api_key } => Client::Turkcell(turkcell::Client { http, base: base_url, key: api_key }),
        }
    }

    pub async fn create(&self, input: &model::Input<'_>, cfg: &EinvoiceConfig) -> Result<Created, ProviderError> {
        match self {
            Client::Uyumsoft(c) => {
                let seller = cfg.seller.as_ref().ok_or_else(|| ProviderError::Validation("satıcı bilgileri tanımlı değil".into()))?;
                c.send(&ubl::invoice_info(input, cfg, seller)).await
            }
            Client::Turkcell(c) => c.create(input.doc, &model::build(input, cfg)).await,
        }
    }

    pub async fn status(&self, doc: DocType, ettn: &str) -> Result<Status, ProviderError> {
        match self {
            Client::Uyumsoft(c) => c.status(ettn).await,
            Client::Turkcell(c) => c.status(doc, ettn).await,
        }
    }

    pub async fn pdf(&self, doc: DocType, ettn: &str) -> Result<Vec<u8>, ProviderError> {
        match self {
            Client::Uyumsoft(c) => c.pdf(ettn).await,
            Client::Turkcell(c) => c.pdf(doc, ettn).await,
        }
    }
}
