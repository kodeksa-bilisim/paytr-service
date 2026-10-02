//! Yönetici paneli "Ödemeler" sayfası — salt-okunur iç uçlar (X-Internal-Token).
//!
//! Yönetici yetkisi Next.js server action'ında (qurlbackend profilindeki `is_admin`) kontrol
//! edilir; bu servis yalnızca iç token'ı doğrular.

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{Duration, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    db::admin_repo::{self, PaymentFilter, PaymentRow, ProblemSubscriptionRow},
    error::AppError,
    paytr_client,
    pricing::amount_to_kurus,
    AppState,
};

/// Ödemenin türü, merchant_oid önekinden: `r…` otomatik yenileme, `u…` sitede kartla ödeme
/// (ilk abonelik ya da yükseltme), `ent…` Enterprise. E-faturada satır açıklaması buradan çıkar.
pub fn payment_kind(merchant_oid: &str) -> &'static str {
    let digits_t_digits = |rest: &str| {
        rest.split_once('t').is_some_and(|(a, b)| {
            !a.is_empty() && !b.is_empty() && a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit())
        })
    };
    if let Some(rest) = merchant_oid.strip_prefix("ent") {
        if digits_t_digits(rest) {
            return "enterprise";
        }
    }
    if let Some(rest) = merchant_oid.strip_prefix('r') {
        if digits_t_digits(rest) {
            return "renewal";
        }
    }
    if let Some(rest) = merchant_oid.strip_prefix('u') {
        if digits_t_digits(rest) {
            return "card";
        }
    }
    "other"
}

/// Başarısız/incelemedeki ödemenin kimden kaynaklandığı:
/// - `bank`: banka/PayTR kartı reddetti (müşteri tarafı)
/// - `merchant`: PayTR mağaza tarafını reddetti (yetki, token, hash) — bizim düzeltmemiz gerekir
/// - `abandoned`: sitede başlatılan ödeme tamamlanmadı (callback hiç gelmedi)
/// - `system`: bizim iç işaretlerimiz (callback'siz yenileme, mükerrer pending temizliği)
/// - `review`: tahsil edilmiş ama otomatik işlenmemiş; iade kararı gerekebilir
pub fn failure_class(status: &str, is_3d: bool, reason: Option<&str>) -> Option<&'static str> {
    match status {
        "review" => Some("review"),
        "failed" => {
            let r = reason.map(str::trim).unwrap_or("");
            Some(match r {
                "no_callback" if is_3d => "abandoned",
                "no_callback" | "status_unknown" | "duplicate_pending_0009" => "system",
                _ if paytr_client::is_merchant_side_error(r) => "merchant",
                _ => "bank",
            })
        }
        _ => None,
    }
}

fn utc(t: NaiveDateTime) -> String {
    t.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[derive(Serialize)]
pub struct PaymentDto {
    id: i32,
    merchant_oid: String,
    member_id: i32,
    email: Option<String>,
    subscription_id: Option<i32>,
    plan: Option<String>,
    billing_cycle: Option<String>,
    kind: &'static str,
    /// Kuruş; kayıttaki tutar okunamazsa null (ham değer `amount`'ta).
    amount_kurus: Option<i64>,
    amount: String,
    currency: String,
    status: String,
    failure_class: Option<&'static str>,
    reason: Option<String>,
    reason_code: Option<String>,
    is_3d: bool,
    test_mode: bool,
    installment_count: i32,
    created_at: String,
    callback_received_at: Option<String>,
}

impl From<PaymentRow> for PaymentDto {
    fn from(p: PaymentRow) -> Self {
        Self {
            kind: payment_kind(&p.merchant_oid),
            failure_class: failure_class(&p.status, p.is_3d, p.failed_reason_msg.as_deref()),
            amount_kurus: amount_to_kurus(&p.amount),
            created_at: utc(p.created_at),
            callback_received_at: p.callback_received_at.map(utc),
            id: p.id,
            merchant_oid: p.merchant_oid,
            member_id: p.member_id,
            email: p.email,
            subscription_id: p.subscription_id,
            plan: p.plan,
            billing_cycle: p.billing_cycle,
            amount: p.amount,
            currency: p.currency,
            status: p.status,
            reason: p.failed_reason_msg,
            reason_code: p.failed_reason_code,
            is_3d: p.is_3d,
            test_mode: p.test_mode,
            installment_count: p.installment_count,
        }
    }
}

#[derive(Deserialize)]
pub struct PaymentListQuery {
    status: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
pub struct PaymentListResponse {
    items: Vec<PaymentDto>,
    has_more: bool,
}

const STATUSES: [&str; 4] = ["pending", "success", "failed", "review"];
const KINDS: [&str; 3] = ["renewal", "card", "enterprise"];

/// GET /api/v1/admin/payments?status=&kind=&q=&limit=&offset=
pub async fn list_payments(
    State(state): State<AppState>,
    Query(q): Query<PaymentListQuery>,
) -> Result<Json<PaymentListResponse>, AppError> {
    let status = q.status.as_deref().filter(|s| !s.is_empty());
    if status.is_some_and(|s| !STATUSES.contains(&s)) {
        return Err(AppError::BadRequest("Geçersiz durum filtresi".into()));
    }
    let kind = q.kind.as_deref().filter(|s| !s.is_empty());
    if kind.is_some_and(|k| !KINDS.contains(&k)) {
        return Err(AppError::BadRequest("Geçersiz tür filtresi".into()));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let filter = PaymentFilter {
        status,
        kind,
        q: q.q.as_deref().map(|s| &s[..s.len().min(100)]),
        limit,
        offset: q.offset.unwrap_or(0).clamp(0, 100_000),
    };
    let mut rows = admin_repo::list_payments(&state.db, &filter).await?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    Ok(Json(PaymentListResponse { items: rows.into_iter().map(Into::into).collect(), has_more }))
}

#[derive(Serialize)]
pub struct ProblemSubscriptionDto {
    id: i32,
    member_id: i32,
    email: Option<String>,
    plan: String,
    billing_cycle: String,
    amount_kurus: Option<i64>,
    renewal_attempts: i32,
    max_attempts: i32,
    expires_at: Option<String>,
    /// Ek süre bitişi (expires_at + GRACE_DAYS); bekleyen ödeme varken abonelik düşmez.
    grace_ends_at: Option<String>,
    next_payment_date: Option<String>,
    last_attempt_at: Option<String>,
    last_failure: Option<String>,
    last_failure_class: Option<&'static str>,
    has_pending: bool,
    /// Makinece okunur sebepler: `in_grace`, `failed_attempts`, `cvv_required`, `no_card`.
    reasons: Vec<&'static str>,
}

fn problem_dto(s: ProblemSubscriptionRow, grace_days: i32, max_attempts: i32) -> ProblemSubscriptionDto {
    let now = Utc::now().naive_utc();
    let soon = now + Duration::days(7);
    let mut reasons = Vec::new();
    if s.expires_at.is_some_and(|e| e < now) {
        reasons.push("in_grace");
    }
    if s.renewal_attempts > 0 {
        reasons.push("failed_attempts");
    }
    let renews_soon = s.next_payment_date.is_some_and(|d| d < soon);
    if renews_soon && s.has_card && s.require_cvv {
        reasons.push("cvv_required");
    }
    if renews_soon && !s.has_card {
        reasons.push("no_card");
    }
    ProblemSubscriptionDto {
        amount_kurus: amount_to_kurus(&s.amount),
        grace_ends_at: s.expires_at.map(|e| utc(e + Duration::days(grace_days.into()))),
        expires_at: s.expires_at.map(utc),
        next_payment_date: s.next_payment_date.map(utc),
        last_attempt_at: s.last_renewal_attempt_at.map(utc),
        last_failure_class: s.last_failure.as_deref().and(failure_class("failed", false, s.last_failure.as_deref())),
        id: s.id,
        member_id: s.member_id,
        email: s.email,
        plan: s.plan,
        billing_cycle: s.billing_cycle,
        renewal_attempts: s.renewal_attempts,
        max_attempts,
        last_failure: s.last_failure,
        has_pending: s.has_pending,
        reasons,
    }
}

#[derive(Serialize)]
pub struct SchedulerDto {
    interval_secs: u64,
    last_ok_age_secs: Option<i64>,
    /// Son çalışma aralığın 3 katından eskiyse true (yenilemeler duruyor olabilir).
    stalled: bool,
    sync_mode: bool,
}

#[derive(Serialize)]
pub struct OverviewResponse {
    generated_at: String,
    scheduler: SchedulerDto,
    last_renewal_success_at: Option<String>,
    /// Son 7 gün, durum → adet (pending/success/failed/review).
    counts_7d: BTreeMap<&'static str, i64>,
    /// Son 7 gündeki başarısızların kaynağa göre dağılımı (bank/merchant/abandoned/system).
    failed_7d_by_class: BTreeMap<&'static str, i64>,
    /// Son 30 günde başarılı ödemelerin toplamı (test ödemeleri hariç), kuruş.
    revenue_30d_kurus: i64,
    subscriptions: Vec<ProblemSubscriptionDto>,
    payments: Vec<PaymentDto>,
}

/// GET /api/v1/admin/overview — özet sayılar, zamanlayıcı durumu, dikkat isteyen kayıtlar.
pub async fn overview(State(state): State<AppState>) -> Result<Json<OverviewResponse>, AppError> {
    let (statuses_30d, last_renewal, subs, payments) = tokio::try_join!(
        admin_repo::recent_statuses(&state.db, 30),
        admin_repo::last_renewal_success(&state.db),
        admin_repo::problem_subscriptions(&state.db),
        admin_repo::attention_payments(&state.db),
    )?;

    let week_ago = Utc::now().naive_utc() - Duration::days(7);
    let mut counts_7d: BTreeMap<&'static str, i64> = STATUSES.iter().map(|s| (*s, 0)).collect();
    let mut failed_7d_by_class: BTreeMap<&'static str, i64> =
        ["bank", "merchant", "abandoned", "system"].iter().map(|s| (*s, 0)).collect();
    let mut revenue_30d_kurus = 0;
    for r in &statuses_30d {
        if r.status == "success" && !r.test_mode {
            revenue_30d_kurus += amount_to_kurus(&r.amount).unwrap_or(0);
        }
        if r.created_at < week_ago {
            continue;
        }
        if let Some(c) = STATUSES.iter().find(|s| **s == r.status).and_then(|s| counts_7d.get_mut(s)) {
            *c += 1;
        }
        if r.status == "failed" {
            if let Some(class) = failure_class("failed", r.is_3d, r.failed_reason_msg.as_deref()) {
                *failed_7d_by_class.entry(class).or_insert(0) += 1;
            }
        }
    }

    let interval = state.config.scheduler_interval_secs;
    let last = state.scheduler_last_ok.load(std::sync::atomic::Ordering::Relaxed);
    let last_ok_age_secs = (last > 0).then(|| Utc::now().timestamp() - last);
    // Açılıştan sonraki ilk çalışma ~30 sn'dir; henüz çalışmadıysa (None) durmuş sayılmaz.
    let stalled = last_ok_age_secs.is_some_and(|age| age > (interval as i64) * 3);

    Ok(Json(OverviewResponse {
        generated_at: utc(Utc::now().naive_utc()),
        scheduler: SchedulerDto { interval_secs: interval, last_ok_age_secs, stalled, sync_mode: state.config.sync_mode },
        last_renewal_success_at: last_renewal.map(utc),
        counts_7d,
        failed_7d_by_class,
        revenue_30d_kurus,
        subscriptions: subs
            .into_iter()
            .map(|s| problem_dto(s, state.config.grace_days, state.config.max_failed_attempts))
            .collect(),
        payments: payments.into_iter().map(Into::into).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_merchant_oid() {
        assert_eq!(payment_kind("r41t1790916975049"), "renewal");
        assert_eq!(payment_kind("u12t1790000000000"), "card");
        assert_eq!(payment_kind("ent7t1790000000000"), "enterprise");
        assert_eq!(payment_kind("old6"), "other");
        assert_eq!(payment_kind("rt1"), "other");
        assert_eq!(payment_kind("u12t"), "other");
    }

    #[test]
    fn failure_classes() {
        assert_eq!(failure_class("success", false, None), None);
        assert_eq!(failure_class("review", false, Some("amount_mismatch")), Some("review"));
        assert_eq!(
            failure_class("failed", false, Some("Bu islem icin magazanin yetkisi yok (sync_mode)")),
            Some("merchant")
        );
        assert_eq!(failure_class("failed", false, Some("Yetersiz bakiye")), Some("bank"));
        assert_eq!(failure_class("failed", false, None), Some("bank"));
        assert_eq!(failure_class("failed", true, Some("no_callback")), Some("abandoned"));
        assert_eq!(failure_class("failed", false, Some("no_callback")), Some("system"));
        assert_eq!(failure_class("failed", false, Some("duplicate_pending_0009")), Some("system"));
    }
}
