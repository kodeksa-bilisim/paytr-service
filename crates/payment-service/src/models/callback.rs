use serde::{Deserialize, Serialize};

/// PayTR'ın bildirim URL'sine POST ettiği payload.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct CallbackPayload {
    pub merchant_oid: String,
    pub status: String,
    pub total_amount: String,
    pub hash: String,
    /// store_card=1 ile yapılan ilk başarılı ödemede gelir — saklanmalıdır.
    pub utoken: Option<String>,
    pub failed_reason_code: Option<String>,
    pub failed_reason_msg: Option<String>,
    pub test_mode: Option<String>,
    pub payment_type: Option<String>,
    pub currency: Option<String>,
    pub payment_amount: Option<String>,
    pub installment_count: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Success,
    Failed,
    WaitCallback,
}

impl std::str::FromStr for PaymentStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            "wait_callback" => Ok(Self::WaitCallback),
            other => Err(format!("Bilinmeyen status: {}", other)),
        }
    }
}
