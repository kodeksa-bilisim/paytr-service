use std::time::Duration;

use chrono::Utc;

use crate::{
    cards,
    crypto::{generate_payment_token, generate_status_query_token},
    db::{customer_repo, models::PaytrPaymentRecord, payment_repo, subscription_repo},
    db::subscription_repo::DueSubscription,
    email, email_templates,
    handlers::callback,
    models::callback::CallbackPayload,
    paytr_client,
    pricing::{amount_to_kurus, format_tl, tl_to_kurus},
    AppState,
};

/// Callback'i gelmemiş pending ödemeler bu kadar saat sonra PayTR'a sorulur.
const STALE_PENDING_HOURS: i32 = 48;
/// Durumu bu kadar gün öğrenilemeyen ödeme elle incelemeye alınır.
const STATUS_UNKNOWN_REVIEW_DAYS: i64 = 7;
/// Bir çalışmada sorulan en fazla ödeme.
const STALE_BATCH: i64 = 50;

/// Scheduler'ı arka planda başlatır. Her çalışma ayrı bir görevde yürür: bir panic yalnızca o
/// çalışmayı düşürür, döngü sürer (eskiden görev sessizce ölüyor, yenilemeler duruyordu).
/// Son başarılı çalışma zamanı `/health`'te görünür.
pub fn start(state: AppState) {
    let interval = Duration::from_secs(state.config.scheduler_interval_secs);

    tokio::spawn(async move {
        // Servis başlangıcında kısa bekleme — migration ve bağlantının oturması için.
        // (SCHEDULER_START_DELAY_SECS: e2e testlerinde kısaltmak için.)
        let start_delay = std::env::var("SCHEDULER_START_DELAY_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        tokio::time::sleep(Duration::from_secs(start_delay)).await;

        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            tracing::info!("Subscription scheduler çalışıyor");

            let run_state = state.clone();
            match tokio::spawn(async move { process_due(&run_state).await }).await {
                Ok(Ok(())) => state
                    .scheduler_last_ok
                    .store(Utc::now().timestamp(), std::sync::atomic::Ordering::Relaxed),
                Ok(Err(e)) => tracing::error!("Scheduler genel hatası: {:?}", e),
                Err(e) => tracing::error!("Scheduler çalışması panic ile düştü, sonraki çalışmada devam: {:?}", e),
            }
        }
    });
}

/// PayTR durum sorgusu sonucu.
enum PaytrStatus {
    /// Başarılı ödeme var; müşterinin ödediği (kuruş) ve iade yapılmış mı.
    Paid { total_kurus: i64, refunded: bool },
    /// `004`: bu sipariş numarasıyla başarılı ödeme yok.
    NotPaid,
}

async fn query_payment_status(state: &AppState, merchant_oid: &str) -> anyhow::Result<PaytrStatus> {
    let token = generate_status_query_token(
        &state.config.merchant_id,
        merchant_oid,
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );
    let v: serde_json::Value = state
        .http
        .post(paytr_client::status_query_endpoint())
        .form(&[
            ("merchant_id", state.config.merchant_id.as_str()),
            ("merchant_oid", merchant_oid),
            ("paytr_token", token.as_str()),
        ])
        .send()
        .await?
        .json()
        .await?;
    let text = |k: &str| match &v[k] {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    match v["status"].as_str() {
        Some("success") => {
            let total = text("payment_total")
                .and_then(|t| tl_to_kurus(&t))
                .ok_or_else(|| anyhow::anyhow!("payment_total okunamadı: {}", v))?;
            let refunded = v["returns"].as_array().is_some_and(|r| !r.is_empty());
            Ok(PaytrStatus::Paid { total_kurus: total, refunded })
        }
        Some("error") if text("err_no").as_deref() == Some("004") => Ok(PaytrStatus::NotPaid),
        _ => Err(anyhow::anyhow!("durum sorgusu yanıtı: {}", v)),
    }
}

/// Callback'i gelmemiş eski pending ödemeler: körlemesine `failed` yapmak yerine PayTR'a sorulur.
/// - Başarılıysa callback ile aynı yoldan işlenir (tutar ve abonelik kontrolleri dahil); iade
///   edilmişse incelemeye alınır.
/// - `004` (başarılı ödeme yok) → `failed`; abonelik sonraki denemede yeniden çekilebilir.
/// - Durum öğrenilemezse pending kalır (yenileme de beklemede kalır); 7 günü aşarsa incelemeye alınır.
async fn resolve_stale_pending(state: &AppState) {
    let list = match payment_repo::list_stale_pending(&state.db, STALE_PENDING_HOURS, STALE_BATCH).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Eski pending ödemeler okunamadı: {:?}", e);
            return;
        }
    };
    for p in list {
        let oid = p.merchant_oid.as_str();
        match query_payment_status(state, oid).await {
            Ok(PaytrStatus::Paid { total_kurus, refunded: false }) => {
                tracing::warn!(merchant_oid = oid, "Callback'i gelmemiş başarılı ödeme bulundu, işleniyor");
                let payload = synthetic_payload(&p, total_kurus);
                if let Err(e) = callback::handle_success(state, &payload).await {
                    tracing::error!(merchant_oid = oid, "Callback'siz başarılı ödeme işlenemedi: {:?}", e);
                }
            }
            Ok(PaytrStatus::Paid { total_kurus, refunded: true }) => {
                review(state, &p, total_kurus, "paid_and_refunded_without_callback").await;
            }
            Ok(PaytrStatus::NotPaid) => {
                if let Err(e) = payment_repo::set_failed(&state.db, oid, Some("004"), Some("no_callback")).await {
                    tracing::error!(merchant_oid = oid, "Pending ödeme failed yapılamadı: {:?}", e);
                } else {
                    tracing::info!(merchant_oid = oid, "Callback'i gelmeyen ödeme PayTR'da başarısız, failed işaretlendi");
                }
            }
            Err(e) => {
                tracing::warn!(merchant_oid = oid, "Ödeme durumu sorgulanamadı: {:?}", e);
                let age = Utc::now().naive_utc() - p.created_at;
                if age > chrono::Duration::days(STATUS_UNKNOWN_REVIEW_DAYS) {
                    let expected = amount_to_kurus(&p.amount).unwrap_or(0);
                    review(state, &p, expected, "status_unknown").await;
                }
            }
        }
    }
}

/// PayTR'ın doğruladığı başarılı ödemeyi callback ile aynı yoldan işlemek için yük.
fn synthetic_payload(p: &PaytrPaymentRecord, total_kurus: i64) -> CallbackPayload {
    CallbackPayload {
        merchant_oid: p.merchant_oid.clone(),
        status: "success".to_string(),
        total_amount: total_kurus.to_string(),
        hash: String::new(),
        utoken: None,
        failed_reason_code: None,
        failed_reason_msg: None,
        test_mode: None,
        payment_type: None,
        currency: None,
        payment_amount: None,
        installment_count: None,
    }
}

async fn review(state: &AppState, p: &PaytrPaymentRecord, total_kurus: i64, reason: &str) {
    match payment_repo::set_review(&state.db, &p.merchant_oid, reason).await {
        Ok(true) => {
            tracing::warn!(merchant_oid = %p.merchant_oid, amount = %format_tl(total_kurus), reason, "Ödeme incelemeye alındı");
            callback::alert_review(state, reason, &synthetic_payload(p, total_kurus), p.member_id, p.subscription_id).await;
        }
        Ok(false) => {}
        Err(e) => tracing::error!(merchant_oid = %p.merchant_oid, "Ödeme incelemeye alınamadı: {:?}", e),
    }
}

/// Sıra önemli: önce vadesi gelenler tahsil edilir, sonra süresi dolanlar expire edilir.
/// (Eskiden önce expire çalışıyordu; `next_payment_date == expires_at` olduğundan vadesi
/// gelen abonelik tahsil edilmeden expired oluyor, otomatik yenileme hiç çalışmıyordu.)
pub async fn process_due(state: &AppState) -> anyhow::Result<()> {
    resolve_stale_pending(state).await;

    // CVV gerektiren ve vadesi gelen kartlara email gönder (otomatik çekilemiyor)
    notify_cvv_required(state).await;

    let due = subscription_repo::query_due(&state.db, state.config.max_failed_attempts).await?;

    if due.is_empty() {
        tracing::debug!("Vadesi gelen abonelik yok");
    } else {
        tracing::info!(count = due.len(), "Vadesi gelen abonelikler işleniyor");
    }

    for sub in &due {
        // Deneme önce sahiplenilir (koşullu): PayTR'a ulaşılamasa bile aynı gün tekrar denenmez,
        // aynı anda çalışan ikinci bir süreç de aynı aboneliği çekemez.
        match subscription_repo::claim_renewal_attempt(&state.db, sub.subscription_id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(subscription_id = sub.subscription_id, "Yenileme başka bir süreçte, atlandı");
                continue;
            }
            Err(e) => {
                tracing::error!(subscription_id = sub.subscription_id, "Deneme zamanı yazılamadı, atlandı: {:?}", e);
                continue;
            }
        }

        match charge(state, sub).await {
            Ok(()) => {
                tracing::info!(
                    subscription_id = sub.subscription_id,
                    member_id = sub.member_id,
                    "Yenileme isteği PayTR'a iletildi"
                );
            }
            // Mağaza tarafı (PayTR yetkisi/yapılandırma) ya da iç hata: müşterinin kartıyla ilgisiz.
            // Deneme hakkından düşülmez, müşteriye "ödemeniz alınamadı" gitmez, yönetici uyarılır;
            // ertesi gün yeniden denenir. (Eskiden bunlar da "banka onaylamadı" diye müşteriye
            // gidiyor, haklar tükenince abonelik düşüyordu.)
            Err(ChargeError::Merchant(reason)) => {
                tracing::error!(
                    subscription_id = sub.subscription_id,
                    member_id = sub.member_id,
                    reason = %reason,
                    "Yenileme mağaza kaynaklı hatayla yapılamadı (deneme sayılmadı)"
                );
                alert_renewal_blocked(state, sub, &reason).await;
            }
            Err(ChargeError::Internal(e)) => {
                tracing::error!(
                    subscription_id = sub.subscription_id,
                    member_id = sub.member_id,
                    "Yenileme iç hatası (deneme sayılmadı): {:?}", e
                );
                alert_renewal_blocked(state, sub, &format!("iç hata: {e}")).await;
            }
            Err(ChargeError::Declined(reason)) => {
                tracing::warn!(
                    subscription_id = sub.subscription_id,
                    member_id = sub.member_id,
                    reason = ?reason,
                    "Yenileme reddedildi"
                );
                let _ = subscription_repo::increment_renewal_attempts(&state.db, sub.subscription_id).await;
                let _ = customer_repo::increment_failed_attempts(&state.db, sub.member_id).await;

                if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
                    let to = sub.user_email.as_str();
                    if !to.is_empty() {
                        let remaining = match subscription_repo::find_by_id(&state.db, sub.subscription_id).await {
                            Ok(Some(s)) => (state.config.max_failed_attempts - s.renewal_attempts).max(0),
                            _ => 0,
                        };
                        // Bankanın/PayTR'ın ret sebebi gösterilir (iç hatalar buraya gelmez).
                        let name = customer_repo::find_name(&state.db, sub.member_id).await;
                        let content = email_templates::payment_failed(
                            name.as_deref(),
                            &sub.plan,
                            &sub.billing_cycle,
                            &sub.amount,
                            reason.as_deref(),
                            remaining,
                            &email_cfg.site_url,
                        );
                        email::send(mailer, email_cfg, to, content).await;
                    }
                }
            }
        }
    }

    // Süresi dolmuş abonelikleri 'expired' yap, kullanıcıları Standard'a düşür, kartları sil.
    expire_subscriptions(state).await?;
    // Daha önce silinemeyen kartları yeniden dene.
    cards::retry_orphan_cards(state).await;

    Ok(())
}

/// Süresi dolmuş abonelikleri `expired` yapar; güncel aboneliği bitenleri Standard'a
/// düşürür; geçerli aboneliği kalmayan üyelerin kayıtlı kartlarını siler (iptalde kart
/// silinmiyor, dönem sonuna kadar geri alma için tutuluyor).
async fn expire_subscriptions(state: &AppState) -> anyhow::Result<()> {
    let expired = subscription_repo::mark_expired(&state.db, state.config.grace_days).await?;
    if expired.is_empty() {
        return Ok(());
    }

    tracing::info!(count = expired.len(), "Süresi dolmuş abonelikler işleniyor");

    for (subscription_id, member_id) in expired {
        match customer_repo::set_subscription_expired(&state.db, member_id, subscription_id).await {
            Ok(true) => tracing::info!(member_id, subscription_id, "Kullanıcı Standard'a düşürüldü"),
            Ok(false) => tracing::info!(member_id, subscription_id, "Güncel abonelik değil, kullanıcı planı korunuyor"),
            Err(e) => tracing::error!(member_id, subscription_id, "Expired downgrade hatası: {:?}", e),
        }

        match subscription_repo::has_live_subscription(&state.db, member_id).await {
            Ok(false) => cards::delete_member_cards(state, member_id).await,
            Ok(true) => {}
            Err(e) => tracing::error!(member_id, "Abonelik kontrolü başarısız, kart silme atlandı: {:?}", e),
        }
    }

    Ok(())
}

/// CVV gerektiren (otomatik çekilemeyen) vadesi gelmiş aboneliklerin sahiplerine günde
/// bir email gönderir.
async fn notify_cvv_required(state: &AppState) {
    let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) else {
        return;
    };

    #[derive(sqlx::FromRow)]
    struct CvvRow {
        id: i32,
        plan: String,
        billing_cycle: String,
        email: String,
        name: String,
        expires_at: Option<chrono::NaiveDateTime>,
    }

    let rows = sqlx::query_as::<_, CvvRow>(
        r#"
        SELECT s.id, s.plan, s.billing_cycle, COALESCE(s.user_email, cu.email, '') AS email, cu.name, s.expires_at
        FROM paytr_subscriptions s
        JOIN paytr_cards c ON c.ctoken = s.ctoken AND c.is_active = TRUE
        JOIN customers cu ON cu.member_id = s.member_id
        WHERE s.status = 'active'
          AND s.next_payment_date <= NOW()
          AND c.require_cvv = TRUE
          AND (s.last_renewal_attempt_at IS NULL OR s.last_renewal_attempt_at < NOW() - INTERVAL '1 day')
        "#,
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Err(e) => tracing::error!("CVV sorgulama hatası: {:?}", e),
        Ok(rows) => {
            for row in rows {
                let _ = subscription_repo::mark_renewal_attempt(&state.db, row.id).await;
                if row.email.is_empty() { continue; }
                let content = email_templates::cvv_required(
                    Some(&row.name),
                    &row.plan,
                    &row.billing_cycle,
                    row.expires_at,
                    &email_cfg.site_url,
                );
                email::send(mailer, email_cfg, &row.email, content).await;
            }
        }
    }
}

/// PayTR merchant_oid yalnızca harf ve rakam içerebilir.
fn renewal_merchant_oid(subscription_id: i32, now_ms: i64) -> String {
    format!("r{}t{}", subscription_id, now_ms)
}

/// Yenileme denemesinin başarısızlık türü; müşteriye ve deneme sayacına etkisi farklıdır.
#[derive(Debug)]
enum ChargeError {
    /// Banka/PayTR kartı reddetti: deneme sayılır, müşteriye sebebiyle bildirilir.
    Declined(Option<String>),
    /// PayTR mağaza tarafını reddetti (yetki, token, hash): deneme sayılmaz, yönetici uyarılır.
    Merchant(String),
    /// Bizim tarafımızdaki hata (DB, geçersiz tutar): deneme sayılmaz, yönetici uyarılır.
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ChargeError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

/// PayTR'ın JSON `failed` yanıtını sınıflandırır.
fn classify_failure(reason: Option<&str>) -> ChargeError {
    match reason {
        Some(r) if paytr_client::is_merchant_side_error(r) => ChargeError::Merchant(r.to_string()),
        r => ChargeError::Declined(r.map(str::to_string)),
    }
}

/// Mağaza kaynaklı yenileme hatasını yöneticiye bildirir (e-posta yoksa yalnızca log).
async fn alert_renewal_blocked(state: &AppState, sub: &DueSubscription, reason: &str) {
    let (Some(mailer), Some(email_cfg), Some(to)) = (&state.mailer, &state.config.email, &state.config.alert_email) else {
        return;
    };
    let content = email_templates::renewal_blocked_alert(
        reason,
        vec![
            ("Abonelik", sub.subscription_id.to_string()),
            ("Üye", sub.member_id.to_string()),
            ("Plan", format!("{} ({})", sub.plan, sub.billing_cycle)),
            ("Tutar", email_templates::format_tl(&sub.amount)),
        ],
        &email_cfg.site_url,
    );
    email::send(mailer, email_cfg, to, content).await;
}

async fn charge(state: &AppState, sub: &DueSubscription) -> Result<(), ChargeError> {
    // Çift ödeme koruması: aynı abonelik için zaten bekleyen ödeme varsa atla
    if payment_repo::has_pending(&state.db, sub.subscription_id).await? {
        tracing::warn!(
            subscription_id = sub.subscription_id,
            "Beklemede ödeme mevcut, bu döngü atlandı"
        );
        return Ok(());
    }

    // Tutar normalize edilir: kayıtta "149.00" (TL) ya da eski biçimde kuruş olabilir; PayTR
    // TL bekler (ham "14900" gönderilseydi 14.900 TL çekilmeye çalışılırdı).
    let amount_kurus = amount_to_kurus(&sub.amount)
        .filter(|k| *k > 0)
        .ok_or_else(|| anyhow::anyhow!("Geçersiz abonelik tutarı: {}", sub.amount))?;
    let amount = format_tl(amount_kurus);

    let phone = if sub.user_phone.trim().is_empty() { state.config.fallback_phone.as_str() } else { sub.user_phone.as_str() };

    let merchant_oid = renewal_merchant_oid(sub.subscription_id, Utc::now().timestamp_millis());
    let test_mode_str = state.config.test_mode.to_string();

    let paytr_token = generate_payment_token(
        &state.config.merchant_id,
        "127.0.0.1",
        &merchant_oid,
        &sub.user_email,
        &amount,
        "card",
        "0",
        &sub.currency,
        &test_mode_str,
        "1", // non_3d: subscription ödemesi her zaman Non-3D
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    // Pending ödeme kaydı oluştur. Abonelik başına tek pending (migration 0009): yarışta
    // ikinci kayıt reddedilir ve PayTR'a istek gitmez.
    if let Err(e) = payment_repo::create(
        &state.db,
        payment_repo::NewPayment {
            member_id: sub.member_id,
            subscription_id: Some(sub.subscription_id),
            merchant_oid: &merchant_oid,
            amount: &amount,
            currency: &sub.currency,
            payment_type: "card",
            installment_count: 0,
            is_3d: false,
            test_mode: state.config.test_mode == 1,
            utoken: Some(&sub.utoken),
            ctoken: Some(&sub.ctoken),
        },
    )
    .await
    {
        if payment_repo::is_unique_violation(&e) {
            tracing::warn!(subscription_id = sub.subscription_id, "Beklemede ödeme mevcut (eşzamanlı), atlandı");
            return Ok(());
        }
        return Err(e.into());
    }

    let basket = build_basket(&sub.plan, amount_kurus);
    let ok_url = format!("{}/api/v1/payments/ok", state.config.base_url);
    let fail_url = format!("{}/api/v1/payments/fail", state.config.base_url);

    let form = [
        ("merchant_id",       state.config.merchant_id.as_str()),
        ("paytr_token",       paytr_token.as_str()),
        ("user_ip",           "127.0.0.1"),
        ("merchant_oid",      merchant_oid.as_str()),
        ("email",             sub.user_email.as_str()),
        ("payment_type",      "card"),
        ("payment_amount",    amount.as_str()),
        ("installment_count", "0"),
        ("currency",          sub.currency.as_str()),
        ("test_mode",         test_mode_str.as_str()),
        ("non_3d",            "1"),
        ("utoken",            sub.utoken.as_str()),
        ("ctoken",            sub.ctoken.as_str()),
        ("require_cvv",       "0"),
        ("user_name",         sub.user_email.as_str()),
        ("user_address",      "Online"),
        ("user_phone",        phone),
        ("user_basket",       basket.as_str()),
        ("merchant_ok_url",   ok_url.as_str()),
        ("merchant_fail_url", fail_url.as_str()),
        ("lang",              "tr"),
        ("sync_mode",         if state.config.sync_mode { "1" } else { "0" }),
    ];

    let sent = async {
        let r = state.http.post(paytr_client::payment_endpoint()).form(&form).send().await?;
        let final_url = r.url().clone();
        let body = r.text().await?;
        Ok::<_, reqwest::Error>((final_url, body))
    }
    .await;
    let (final_url, body) = match sent {
        Ok(v) => v,
        Err(e) => {
            // PayTR'a ulaşılamadı / yanıt okunamadı: ödeme durumu bilinmiyor. Başarısız
            // sayılmaz, kullanıcıya bildirim gitmez; ödeme pending bırakılır — başarılıysa
            // callback gelir, gelmezse 48 saat sonra failed olur ve yeniden denenir.
            tracing::error!(
                subscription_id = sub.subscription_id,
                merchant_oid = %merchant_oid,
                "PayTR yanıtı alınamadı, ödeme pending bırakıldı: {}", e
            );
            return Ok(());
        }
    };

    // sync_mode=0: PayTR JSON döndürmez, ok/fail adresimize yönlendirir; kesin sonuç (ret
    // sebebiyle) callback'e gelir ve `handle_failed`/`handle_success` işler. Ödeme pending kalır.
    // PayTR isteği baştan reddederse yine JSON dönebilir; o durum aşağıda sync gibi işlenir.
    let resp = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) if v.get("status").is_some() => v,
        _ => {
            if state.config.sync_mode {
                tracing::error!(
                    subscription_id = sub.subscription_id,
                    merchant_oid = %merchant_oid,
                    "PayTR sync yanıtı JSON değil, ödeme pending bırakıldı"
                );
            } else {
                tracing::info!(
                    subscription_id = sub.subscription_id,
                    merchant_oid = %merchant_oid,
                    redirected_to = %final_url.path(),
                    "Yenileme PayTR'a iletildi, sonuç callback ile gelecek"
                );
            }
            return Ok(());
        }
    };

    let status = resp
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("failed");

    tracing::info!(
        subscription_id = sub.subscription_id,
        merchant_oid = %merchant_oid,
        paytr_status = %status,
        "PayTR sync yanıtı"
    );

    match status {
        // Kesin sonuç callback ile gelir; abonelik/müşteri tablosu yalnızca orada güncellenir.
        "success" | "wait_callback" => Ok(()),
        _ => {
            let reason = resp
                .get("err_msg")
                .or_else(|| resp.get("failed_reason_msg"))
                .or_else(|| resp.get("reason"))
                .and_then(|v| v.as_str());

            // Ödeme başarısız: DB'yi güncelle (callback gelmeyebilir; gelirse ikinci kez sayılmaz)
            payment_repo::set_failed(&state.db, &merchant_oid, None, reason).await?;

            Err(classify_failure(reason))
        }
    }
}

/// PayTR sepet formatı: JSON.stringify([["Plan Adı", "Fiyat", 1]]); fiyat TL ("149.00").
fn build_basket(plan: &str, amount_kurus: i64) -> String {
    let label = match plan {
        "gold"       => "Gold Plan Aboneliği",
        "silver"     => "Silver Plan Aboneliği",
        "enterprise" => "Enterprise Plan Aboneliği",
        other        => other,
    };
    serde_json::json!([[label, format_tl(amount_kurus), 1]]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merchant_oid_is_alphanumeric() {
        let oid = renewal_merchant_oid(41, 1_790_000_000_123);
        assert_eq!(oid, "r41t1790000000123");
        assert!(oid.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn basket_price_is_tl_amount() {
        assert_eq!(build_basket("gold", 29_900), r#"[["Gold Plan Aboneliği","299.00",1]]"#);
        assert_eq!(build_basket("enterprise", 10_109_900), r#"[["Enterprise Plan Aboneliği","101099.00",1]]"#);
    }
}
