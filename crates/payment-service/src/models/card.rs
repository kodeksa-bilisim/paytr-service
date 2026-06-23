use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct CardListRequest {
    pub member_id: i32,
}

#[derive(Debug, Deserialize)]
pub struct CardDeleteRequest {
    pub member_id: i32,
    pub utoken: String,
    pub ctoken: String,
}

/// PayTR /capi/list yanıtındaki tek kart.
#[derive(Debug, Serialize, Deserialize)]
pub struct CardItem {
    pub ctoken: String,
    pub last_4: String,
    pub require_cvv: u8,
    pub month: String,
    pub year: String,
    pub c_bank: String,
    pub c_type: String,
    pub schema: String,
}

#[derive(Debug, Deserialize)]
pub struct PaytrErrorResponse {
    pub status: String,
    pub err_msg: Option<String>,
}
