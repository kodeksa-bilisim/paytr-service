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
    /// PayTR bu alanı bazen string ("0"/"1") bazen integer olarak döner.
    #[serde(deserialize_with = "deserialize_u8_or_str")]
    pub require_cvv: u8,
    pub month: String,
    pub year: String,
    pub c_bank: String,
    pub c_type: String,
    pub schema: String,
}

fn deserialize_u8_or_str<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Deserialize;
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(|v| v as u8)
            .ok_or_else(|| serde::de::Error::custom("geçersiz u8")),
        serde_json::Value::String(s) => s.parse::<u8>().map_err(serde::de::Error::custom),
        other => Err(serde::de::Error::custom(format!(
            "u8 veya string bekleniyor, alınan: {other}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
pub struct PaytrErrorResponse {
    pub status: String,
    pub err_msg: Option<String>,
}
