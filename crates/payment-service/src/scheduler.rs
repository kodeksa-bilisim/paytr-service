use std::time::Duration;

use chrono::Utc;

use crate::{
    cards,
    crypto::generate_payment_token,
    db::{customer_repo, payment_repo, subscription_repo},
    db::subscription_repo::DueSubscription,
    email, email_templates,
    paytr_client,
    AppState,
};

/// Callback'i gelmemiş pending ödemeler bu kadar saat sonra `failed (no_callback)` olur.
const STALE_PENDING_HOURS: i32 = 48;

/// Scheduler'ı arka planda başlatır. Servis ayakta olduğu sürece döngü çalışır.
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

            if let Err(e) = process_due(&state).await {
                tracing::error!("Scheduler genel hatası: {:?}", e);
            }
        }
    });
}

/// Sıra önemli: önce vadesi gelenler tahsil edilir, sonra süresi dolanlar expire edilir.
/// (Eskiden önce expire çalışıyordu; `next_payment_date == expires_at` olduğundan vadesi
/// gelen abonelik tahsil edilmeden expired oluyor, otomatik yenileme hiç çalışmıyordu.)
pub async fn process_due(state: &AppState) -> anyhow::Result<()> {
    match payment_repo::fail_stale_pending(&state.db, STALE_PENDING_HOURS).await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(count = n, "Callback'i gelmeyen eski pending ödemeler failed işaretlendi"),
        Err(e) => tracing::error!("Eski pending ödemeler temizlenemedi: {:?}", e),
    }

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
            Err(e) => {
                tracing::error!(
                    subscription_id = sub.subscription_id,
                    member_id = sub.member_id,
                    "Yenileme hatası: {:?}", e
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
                        // Kullanıcıya iç hata ayrıntısı gönderilmez.
                        let name = customer_repo::find_name(&state.db, sub.member_id).await;
                        let content = email_templates::payment_failed(
                            name.as_deref(),
                            &sub.plan,
                            &sub.billing_cycle,
                            &sub.amount,
                            None,
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

async fn charge(state: &AppState, sub: &DueSubscription) -> anyhow::Result<()> {
    // Çift ödeme koruması: aynı abonelik için zaten bekleyen ödeme varsa atla
    if payment_repo::has_pending(&state.db, sub.subscription_id).await? {
        tracing::warn!(
            subscription_id = sub.subscription_id,
            "Beklemede ödeme mevcut, bu döngü atlandı"
        );
        return Ok(());
    }

    let phone = if sub.user_phone.is_empty() { "5305861333" } else { sub.user_phone.as_str() };

    let merchant_oid = renewal_merchant_oid(sub.subscription_id, Utc::now().timestamp_millis());
    let test_mode_str = state.config.test_mode.to_string();

    let paytr_token = generate_payment_token(
        &state.config.merchant_id,
        "127.0.0.1",
        &merchant_oid,
        &sub.user_email,
        &sub.amount,
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
            amount: &sub.amount,
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
        return Err(e);
    }

    let basket = build_basket(&sub.plan, &sub.amount)?;
    let ok_url = format!("{}/api/v1/payments/ok", state.config.base_url);
    let fail_url = format!("{}/api/v1/payments/fail", state.config.base_url);

    let form = [
        ("merchant_id",       state.config.merchant_id.as_str()),
        ("paytr_token",       paytr_token.as_str()),
        ("user_ip",           "127.0.0.1"),
        ("merchant_oid",      merchant_oid.as_str()),
        ("email",             sub.user_email.as_str()),
        ("payment_type",      "card"),
        ("payment_amount",    sub.amount.as_str()),
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
        ("sync_mode",         "1"),
    ];

    let resp: serde_json::Value = match async {
        state
            .http
            .post(paytr_client::payment_endpoint())
            .form(&form)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await
    }
    .await
    {
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

            Err(anyhow::anyhow!(
                "Ödeme reddedildi: {}",
                reason.unwrap_or("bilinmeyen hata")
            ))
        }
    }
}

/// PayTR sepet formatı: JSON.stringify([["Plan Adı", "Fiyat", 1]]). Tutar zaten TL ("149.00").
fn build_basket(plan: &str, amount_tl: &str) -> anyhow::Result<String> {
    let label = match plan {
        "gold"       => "Gold Plan Aboneliği",
        "silver"     => "Silver Plan Aboneliği",
        "enterprise" => "Enterprise Plan Aboneliği",
        other        => other,
    };
    let amount = amount_tl
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("Geçersiz tutar formatı: {}", amount_tl))?;
    let price = format!("{:.2}", amount);
    Ok(serde_json::to_string(&vec![[
        serde_json::Value::String(label.to_string()),
        serde_json::Value::String(price),
        serde_json::Value::Number(1.into()),
    ]])?)
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
        assert_eq!(build_basket("gold", "299.00").unwrap(), r#"[["Gold Plan Aboneliği","299.00",1]]"#);
        assert_eq!(build_basket("enterprise", "101099.00").unwrap(), r#"[["Enterprise Plan Aboneliği","101099.00",1]]"#);
        assert!(build_basket("gold", "abc").is_err());
    }
}
