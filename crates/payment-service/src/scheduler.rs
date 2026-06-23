use std::time::Duration;

use chrono::Utc;

use crate::{
    crypto::generate_payment_token,
    db::{customer_repo, payment_repo, subscription_repo},
    db::subscription_repo::DueSubscription,
    email,
    paytr_client::PAYTR_PAYMENT_ENDPOINT,
    AppState,
};

/// Scheduler'ı arka planda başlatır. Servis ayakta olduğu sürece döngü çalışır.
pub fn start(state: AppState) {
    let interval = Duration::from_secs(state.config.scheduler_interval_secs);

    tokio::spawn(async move {
        // Servis başlangıcında kısa bekleme — migration ve bağlantının oturması için.
        tokio::time::sleep(Duration::from_secs(30)).await;

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

async fn process_due(state: &AppState) -> anyhow::Result<()> {
    // Süresi dolmuş abonelikleri 'expired' yap ve kullanıcıları Standard'a düşür
    expire_subscriptions(state).await?;

    // CVV gerektiren ve vadesi gelen kartlara email gönder (otomatik çekilemiyor)
    notify_cvv_required(state).await;

    let due = subscription_repo::query_due(&state.db, state.config.max_failed_attempts).await?;

    if due.is_empty() {
        tracing::debug!("Vadesi gelen abonelik yok");
        return Ok(());
    }

    tracing::info!(count = due.len(), "Vadesi gelen abonelikler işleniyor");

    for sub in &due {
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

                // Başarısız ödeme email bildirimi callback'ten gelir.
                // Scheduler hatasında (PayTR'a ulaşılamadı vb.) da bildir.
                if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
                    let to = sub.user_email.as_str();
                    if !to.is_empty() {
                        let (subject, html) = email::tpl_payment_failed(
                            &sub.plan,
                            Some(&e.to_string()),
                            state.config.max_failed_attempts - 1,
                            &email_cfg.site_url,
                        );
                        email::send(mailer, email_cfg, to, subject, html).await;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Süresi dolmuş abonelikleri 'expired' olarak işaretler ve
/// customers tablosunda user_type'ı 'Standard'a düşürür.
async fn expire_subscriptions(state: &AppState) -> anyhow::Result<()> {
    let expired_member_ids = subscription_repo::mark_expired(&state.db).await?;
    if expired_member_ids.is_empty() {
        return Ok(());
    }

    tracing::info!(count = expired_member_ids.len(), "Süresi dolmuş abonelikler işleniyor");

    for member_id in expired_member_ids {
        if let Err(e) = customer_repo::set_subscription_expired(&state.db, member_id).await {
            tracing::error!(member_id, "Expired downgrade hatası: {:?}", e);
        } else {
            tracing::info!(member_id, "Kullanıcı Standard'a düşürüldü");
        }
    }

    Ok(())
}

/// CVV gerektiren aktif aboneliklerin sahiplerine email gönderir.
async fn notify_cvv_required(state: &AppState) {
    let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) else {
        return;
    };

    #[derive(sqlx::FromRow)]
    struct CvvRow {
        plan: String,
        email: String,
    }

    let rows = sqlx::query_as::<_, CvvRow>(
        r#"
        SELECT s.plan, COALESCE(s.user_email, cu.email, '') AS email
        FROM paytr_subscriptions s
        JOIN paytr_cards c ON c.ctoken = s.ctoken AND c.is_active = TRUE
        JOIN customers cu ON cu.member_id = s.member_id
        WHERE s.status = 'active'
          AND s.next_payment_date <= NOW()
          AND c.require_cvv = TRUE
        "#,
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Err(e) => tracing::error!("CVV sorgulama hatası: {:?}", e),
        Ok(rows) => {
            for row in rows {
                if row.email.is_empty() { continue; }
                let (subject, html) = email::tpl_cvv_required(&row.plan, &email_cfg.site_url);
                email::send(mailer, email_cfg, &row.email, subject, html).await;
            }
        }
    }
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

    // Telefon numarası yoksa ödeme yapılamaz — PayTR zorunlu kılıyor
    if sub.user_phone.is_empty() {
        return Err(anyhow::anyhow!("Kayıtlı telefon numarası yok"));
    }

    let merchant_oid = format!("r{}_{}", sub.subscription_id, Utc::now().timestamp_millis());
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

    // Pending ödeme kaydı oluştur
    payment_repo::create(
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
    .await?;

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
        ("user_phone",        sub.user_phone.as_str()),
        ("user_basket",       basket.as_str()),
        ("merchant_ok_url",   ok_url.as_str()),
        ("merchant_fail_url", fail_url.as_str()),
        ("client_lang",       "tr"),
        ("sync_mode",         "1"),
    ];

    let resp: serde_json::Value = state
        .http
        .post(PAYTR_PAYMENT_ENDPOINT)
        .form(&form)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR bağlantı hatası: {}", e))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR yanıt parse hatası: {}", e))?;

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

    if status != "success" {
        let reason = resp
            .get("err_msg")
            .or_else(|| resp.get("failed_reason_msg"))
            .and_then(|v| v.as_str());

        // Ödeme başarısız: DB'yi güncelle (callback gelmeyebilir)
        payment_repo::set_failed(&state.db, &merchant_oid, None, reason).await?;

        return Err(anyhow::anyhow!(
            "Ödeme reddedildi: {}",
            reason.unwrap_or("bilinmeyen hata")
        ));
    }

    // Başarılı: PayTR callback'i gelince callback handler devralır.
    // Burada subscription/customer tablosu güncellenmez — tek kaynak callback'tir.
    Ok(())
}

/// PayTR sepet formatı: JSON.stringify([["Plan Adı", "Fiyat", 1]])
fn build_basket(plan: &str, amount_kurus: &str) -> anyhow::Result<String> {
    let label = match plan {
        "gold"   => "Gold Plan Aboneliği",
        "silver" => "Silver Plan Aboneliği",
        other    => other,
    };
    let price = format!(
        "{:.2}",
        amount_kurus.parse::<f64>().unwrap_or(0.0) / 100.0
    );
    Ok(serde_json::to_string(&vec![[
        serde_json::Value::String(label.to_string()),
        serde_json::Value::String(price),
        serde_json::Value::Number(1.into()),
    ]])?)
}
