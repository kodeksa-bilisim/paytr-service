use chrono::NaiveDateTime;
use serde::Serialize;

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
pub struct PaytrUserToken {
    pub id: i32,
    pub member_id: i32,
    pub utoken: String,
    pub is_active: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PaytrCard {
    pub id: i32,
    pub utoken: String,
    pub ctoken: String,
    pub last_4: String,
    pub card_bank: Option<String>,
    pub card_schema: Option<String>,
    pub card_type: Option<String>,
    pub expiry_month: String,
    pub expiry_year: String,
    pub require_cvv: bool,
    pub is_default: bool,
    pub is_active: bool,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PaytrSubscription {
    pub id: i32,
    pub member_id: i32,
    pub plan: String,
    pub status: String,
    pub utoken: Option<String>,
    pub ctoken: Option<String>,
    pub billing_cycle: String,
    pub amount: String,
    pub currency: String,
    pub user_phone: Option<String>,
    pub user_email: Option<String>,
    pub renewal_attempts: i32,
    pub started_at: Option<NaiveDateTime>,
    pub expires_at: Option<NaiveDateTime>,
    pub next_payment_date: Option<NaiveDateTime>,
    pub cancelled_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PaytrPaymentRecord {
    pub id: i32,
    pub member_id: i32,
    pub subscription_id: Option<i32>,
    pub merchant_oid: String,
    pub amount: String,
    pub currency: String,
    pub status: String,
    pub payment_type: String,
    pub installment_count: i32,
    pub is_3d: bool,
    pub test_mode: bool,
    pub failed_reason_code: Option<String>,
    pub failed_reason_msg: Option<String>,
    pub utoken: Option<String>,
    pub ctoken: Option<String>,
    pub callback_received_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}
