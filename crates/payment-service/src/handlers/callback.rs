use axum::{extract::State, response::IntoResponse, Form};

use crate::{
    billing,
    crypto::{generate_card_list_token, verify_callback_hash},
    db::{
        billing_repo, card_repo, customer_repo,
        models::{PaytrPaymentRecord, PaytrSubscription},
        payment_repo, subscription_repo,
    },
    email, email_templates,
    error::AppError,
    models::{
        callback::{CallbackPayload, PaymentStatus},
        card::CardItem,
    },
    paytr_client,
    pricing::{amount_to_kurus, billing_dates, renewal_dates},
    AppState,
};

pub async fn payment_callback(
    State(state): State<AppState>,
    Form(payload): Form<CallbackPayload>,
) -> Result<impl IntoResponse, AppError> {
    // 1. Hash doğrulama
    if !verify_callback_hash(
        &payload.merchant_oid,
        &state.config.merchant_salt,
        &payload.status,
        &payload.total_amount,
        &state.config.merchant_key,
        &payload.hash,
    ) {
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Geçersiz callback hash");
        return Err(AppError::InvalidHash);
    }

    let status: PaymentStatus = payload
        .status
        .parse()
        .map_err(|e: String| AppError::BadRequest(e))?;

    match status {
        PaymentStatus::Success => handle_success(&state, &payload).await?,
        PaymentStatus::Failed => handle_failed(&state, &payload).await?,
        PaymentStatus::WaitCallback => {
            tracing::info!(merchant_oid = %payload.merchant_oid, "Ödeme bekleniyor");
        }
    }

    Ok("OK")
}

/// İlk ödeme aktivasyonu çakışıyor mu? Üyenin başka geçerli aboneliği yalnızca bu ödemenin
/// yükselttiği abonelikse (`metadata.upgrade_from`) aktivasyon serbesttir. Aksi halde iki ayrı
/// satın alma tamamlanmış demektir (ör. iki sekmede ödeme; ya da yeni ödeme başlatılınca iptal
/// edilen eski ödeme sonradan tamamlandı) — yeni aboneliği `replaced` yapmak yerine incelenir.
fn first_payment_conflicts(sub: &PaytrSubscription, others: &[PaytrSubscription]) -> bool {
    let meta = sub.metadata.as_ref();
    let upgrade_from = meta.and_then(|m| m.get("upgrade_from")?.as_i64());
    // Eski kayıtlar: yükseltme `previous_expires_at` ile işaretlenirdi; o anki (daha eski) abonelik.
    let legacy_upgrade = upgrade_from.is_none() && meta.is_some_and(|m| m.get("previous_expires_at").is_some());
    others.iter().any(|o| {
        let allowed = upgrade_from == Some(o.id as i64) || (legacy_upgrade && o.created_at < sub.created_at);
        !allowed
    })
}

/// Başarılı ödemenin fatura kaydı — ödemeyle aynı transaction içinde, bir savepoint'te.
/// Normalde ikisi birlikte yazılır; fatura tarafında beklenmedik bir hata olursa yalnızca
/// savepoint geri alınır ve ödeme yine işlenir (fatura hatası abonelik aktivasyonunu
/// engellemesin). Atlanan kayıt loglanır ve panelde "faturası olmayan ödeme" olarak görünür.
async fn record_invoice(
    state: &crate::AppData,
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    payment: &PaytrPaymentRecord,
    subject: billing::InvoiceSubject<'_>,
) {
    if state.config.is_invoice_exempt(payment.member_id) {
        tracing::info!(merchant_oid = %payment.merchant_oid, member_id = payment.member_id, "Şirket içi hesap: fatura kaydı açılmadı");
        return;
    }
    let Some(total) = amount_to_kurus(&payment.amount).filter(|k| *k > 0) else {
        tracing::error!(merchant_oid = %payment.merchant_oid, amount = %payment.amount, "Fatura kaydı atlandı: tutar okunamadı");
        return;
    };
    let result: anyhow::Result<()> = async {
        let mut sp = sqlx::Acquire::begin(&mut **tx).await?;
        let profile = billing_repo::find_profile(&mut *sp, payment.member_id).await?;
        let (name, email) = billing_repo::customer_contact(&mut *sp, payment.member_id).await?;
        let created = billing_repo::create_sale(
            &mut *sp,
            billing_repo::NewInvoice {
                payment_id: payment.id,
                merchant_oid: &payment.merchant_oid,
                member_id: payment.member_id,
                buyer: billing::buyer_snapshot(profile.as_ref(), &name, &email),
                line: billing::single_line(billing::line_description(&subject), total),
                created_at: None,
                source: None,
            },
        )
        .await?;
        sp.commit().await?;
        if !created {
            tracing::info!(merchant_oid = %payment.merchant_oid, "Fatura kaydı zaten var");
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::error!(merchant_oid = %payment.merchant_oid, "Fatura kaydı oluşturulamadı (ödeme yine işlendi): {:?}", e);
    }
}

/// Yöneticiye inceleme uyarısı (e-posta yapılandırılmamışsa yalnızca log).
pub(crate) async fn alert_review(state: &crate::AppData, reason: &str, payload: &CallbackPayload, member_id: i32, subscription_id: Option<i32>) {
    tracing::error!(
        merchant_oid = %payload.merchant_oid, member_id, subscription_id, reason,
        "Ödeme incelemeye alındı — abonelik değiştirilmedi, iade gerekebilir"
    );
    let (Some(mailer), Some(email_cfg), Some(to)) = (&state.mailer, &state.config.email, &state.config.alert_email) else {
        return;
    };
    let content = email_templates::payment_review_alert(
        reason,
        vec![
            ("Sipariş no", payload.merchant_oid.clone()),
            ("Tahsil edilen (kuruş)", payload.total_amount.clone()),
            ("Üye", member_id.to_string()),
            ("Abonelik", subscription_id.map_or("-".to_string(), |s| s.to_string())),
        ],
        &email_cfg.site_url,
    );
    email::send(mailer, email_cfg, to, content).await;
}

pub(crate) async fn handle_success(state: &crate::AppData, payload: &CallbackPayload) -> Result<(), AppError> {
    // "Bulunamadı" durumunda OK döndür — PayTR'nin yeniden denemesi bu durumu düzeltemez.
    // Gerçek DB hatalarında Err döner, PayTR yeniden dener (geçici hata kurtarma).
    let Some(payment) = payment_repo::find_by_oid(&state.db, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?
    else {
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Callback geldi fakat ödeme kaydı bulunamadı — görmezden geliniyor");
        return Ok(());
    };
    if payment.status == "success" || payment.status == "review" {
        tracing::info!(merchant_oid = %payload.merchant_oid, status = %payment.status, "Tekrar callback, zaten işlendi");
        return Ok(());
    }

    // Savunma derinliği: tahsil edilen tutar (kuruş, taksit farkı dahil) beklenenden az olamaz.
    match (amount_to_kurus(&payment.amount), payload.total_amount.trim().parse::<i64>()) {
        (Some(expected), Ok(paid)) if paid < expected => {
            tracing::error!(merchant_oid = %payload.merchant_oid, expected, paid, "Tahsil edilen tutar beklenenden düşük");
            if payment_repo::set_review(&state.db, &payload.merchant_oid, "amount_mismatch")
                .await
                .map_err(anyhow::Error::from)?
            {
                alert_review(state, "amount_mismatch", payload, payment.member_id, payment.subscription_id).await;
            }
            return Ok(());
        }
        (Some(_), Ok(_)) => {}
        _ => tracing::warn!(
            merchant_oid = %payload.merchant_oid,
            amount = %payment.amount, total_amount = %payload.total_amount,
            "Tutar karşılaştırılamadı"
        ),
    }

    let member_id = payment.member_id;

    // Kart senkronizasyonu (PayTR HTTP) transaction dışında ve hata olsa da devam eder —
    // para çekilmişken kart listesi alınamadı diye abonelik aktifleşmemesin.
    let utoken: Option<String> = match payload.utoken.as_deref().filter(|u| !u.is_empty()) {
        Some(ut) => {
            if let Err(e) = card_repo::upsert_user_token(&state.db, member_id, ut).await {
                tracing::error!(member_id, error = %e, "utoken kaydedilemedi");
            }
            match fetch_paytr_cards(state, ut).await {
                Ok(cards) => match card_repo::sync_cards(&state.db, ut, &cards).await {
                    Ok(()) => tracing::info!(member_id, cards = cards.len(), "Kart listesi senkronize edildi"),
                    Err(e) => tracing::error!(member_id, error = %e, "Kart listesi DB'ye yazılamadı"),
                },
                Err(e) => tracing::error!(
                    member_id, error = ?e,
                    "PayTR kart listesi alınamadı — abonelik yine de aktifleştiriliyor"
                ),
            }
            Some(ut.to_string())
        }
        None => card_repo::get_user_token(&state.db, member_id)
            .await
            .map_err(anyhow::Error::from)?
            .map(|t| t.utoken),
    };
    let default_ctoken = match &utoken {
        Some(ut) => card_repo::get_default_card(&state.db, ut)
            .await
            .map_err(anyhow::Error::from)?
            .map(|c| c.ctoken),
        None => None,
    }
    .or_else(|| payment.ctoken.clone());

    // Abonelik + müşteri + ödeme durumu tek transaction'da; ödeme satırı kilitli.
    // Ara adımda hata olursa hiçbiri yazılmaz, PayTR yeniden dener (eskiden abonelik
    // aktifleşip ödeme pending kalıyor, retry'da "yenileme" sanılıp ek ay veriliyordu).
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;

    let Some(payment) = payment_repo::find_by_oid_for_update(&mut *tx, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?
    else {
        return Ok(());
    };
    if payment.status == "success" || payment.status == "review" {
        // Eşzamanlı çift callback: diğeri bizden önce işledi.
        tracing::info!(merchant_oid = %payload.merchant_oid, "Eşzamanlı tekrar callback, zaten işlendi");
        return Ok(());
    }

    let Some(subscription_id) = payment.subscription_id else {
        record_invoice(state, &mut tx, &payment, billing::InvoiceSubject::Other).await;
        payment_repo::set_success(&mut *tx, &payload.merchant_oid)
            .await
            .map_err(anyhow::Error::from)?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        tracing::warn!(merchant_oid = %payload.merchant_oid, "Subscription ID yok, sadece ödeme kaydedildi");
        return Ok(());
    };

    let sub = subscription_repo::find_by_id_for_update(&mut *tx, subscription_id)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(|| AppError::BadRequest("Abonelik kaydı bulunamadı".to_string()))?;

    let now = chrono::Utc::now().naive_utc();

    // Hiç aktif olmamış abonelik = ilk ödeme (kullanıcı yeni ödeme başlatınca
    // `cancel_pending` ile iptal edilmiş eski pending abonelik de dahil).
    let is_first_payment = sub.started_at.is_none();

    // Tahsilat artık geçerli olmayan bir aboneliğe geldiyse müşterinin güncel planı ezilmez:
    // ödeme incelemeye alınır, yönetici uyarılır (iade gerekebilir).
    let review_reason = if is_first_payment {
        let others = subscription_repo::live_others_for_update(&mut *tx, member_id, subscription_id)
            .await
            .map_err(anyhow::Error::from)?;
        first_payment_conflicts(&sub, &others).then_some("duplicate_purchase")
    } else if !matches!(sub.status.as_str(), "active" | "cancelled" | "expired") {
        Some("renewal_on_inactive_subscription")
    } else {
        // Süresi dolan abonelikte customers.subscription_id NULL'lanır; başka bir aboneliğe
        // geçilmişse o abonelik yazılıdır.
        let current = customer_repo::current_subscription_id_for_update(&mut *tx, member_id)
            .await
            .map_err(anyhow::Error::from)?;
        match current {
            Some(c) if c != subscription_id.to_string() => Some("renewal_not_current_subscription"),
            _ => None,
        }
    };
    if let Some(reason) = review_reason {
        let changed = payment_repo::set_review(&mut *tx, &payload.merchant_oid, reason)
            .await
            .map_err(anyhow::Error::from)?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        if changed {
            alert_review(state, reason, payload, member_id, Some(subscription_id)).await;
        }
        return Ok(());
    }

    // Eski kayıtlar: yükseltmede önceki aboneliğin kalan süresi aktarılırdı (`previous_expires_at`).
    // Yeni yükseltmelerde fark ücreti init'te alınır ve yeni dönem hemen başlar.
    let upgrade_start = if is_first_payment {
        sub.metadata.as_ref()
            .and_then(|m| m.get("previous_expires_at")?.as_str())
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
            .map(|exp| exp.max(now))
    } else {
        None
    };

    // Yenileme dönemi eski bitişe sabitlenir (grace süresinde geç tahsil edilse de kayma ve
    // bedava gün olmaz). Abonelik grace sonrası expire olmuşsa yeni dönem şimdiden başlar.
    let period_start = if is_first_payment {
        upgrade_start.unwrap_or(now)
    } else if sub.status == "expired" {
        now
    } else {
        sub.expires_at.unwrap_or(now)
    };

    // Yenilemede fatura günü abonelik başlangıcına sabitlenir (31 Oca → 28 Şub → 31 Mar).
    let (expires_at, next_payment_date) = match sub.started_at {
        Some(start) if !is_first_payment && sub.status != "expired" => {
            renewal_dates(&sub.billing_cycle, period_start, chrono::Datelike::day(&start))
        }
        _ => billing_dates(&sub.billing_cycle, period_start),
    };

    // Yenilemede planlanmış downgrade uygulanır.
    let effective_plan = if is_first_payment {
        sub.plan.clone()
    } else {
        sub.scheduled_plan.clone().unwrap_or_else(|| sub.plan.clone())
    };

    if is_first_payment {
        subscription_repo::activate(
            &mut *tx,
            subscription_id,
            utoken.as_deref().unwrap_or(""),
            default_ctoken.as_deref().unwrap_or(""),
            now,
            expires_at,
            next_payment_date,
        )
        .await
        .map_err(anyhow::Error::from)?;

        // Upgrade: önceki aboneliği `replaced` yap, diğer pending'leri iptal et.
        subscription_repo::replace_others(&mut *tx, member_id, subscription_id)
            .await
            .map_err(anyhow::Error::from)?;
    } else {
        subscription_repo::renew(
            &mut *tx,
            subscription_id,
            sub.scheduled_plan.as_deref(),
            expires_at,
            next_payment_date,
        )
        .await
        .map_err(anyhow::Error::from)?;
    }

    let custom_plan = if effective_plan == "enterprise" {
        sub.metadata.as_ref().and_then(|m| {
            let extra_links = m.get("extra_links")?.as_i64()?;
            let extra_clicks = m.get("extra_clicks")?.as_i64()?;
            // Ekip koltuğu (sahip dahil); qurlbackend davet sınırında kullanır.
            let users = m.get("users").and_then(|v| v.as_i64()).unwrap_or(1).max(1);
            Some(serde_json::json!({
                "links_limit": 10000 + extra_links * 1000,
                "clicks_limit": 100000 + extra_clicks * 10000,
                "users_limit": users,
            }).to_string())
        })
    } else {
        None
    };

    // Kullanıcı yenileme tahsil edildikten sonra iptal ettiyse iptal durumu korunur.
    let customer_status = if !is_first_payment && sub.status == "cancelled" { "cancelled" } else { "active" };

    customer_repo::set_subscription_active(
        &mut *tx,
        member_id,
        &effective_plan,
        subscription_id,
        expires_at,
        next_payment_date,
        custom_plan,
        customer_status,
    )
    .await
    .map_err(anyhow::Error::from)?;

    let is_upgrade = is_first_payment && sub.metadata.as_ref().is_some_and(|m| m.get("upgrade_from").is_some());
    let subject = if is_upgrade {
        billing::InvoiceSubject::Upgrade { plan: &effective_plan, billing_cycle: &sub.billing_cycle }
    } else {
        billing::InvoiceSubject::Period {
            plan: &effective_plan,
            billing_cycle: &sub.billing_cycle,
            start: period_start,
            end: expires_at,
        }
    };
    record_invoice(state, &mut tx, &payment, subject).await;

    payment_repo::set_success(&mut *tx, &payload.merchant_oid)
        .await
        .map_err(anyhow::Error::from)?;

    tx.commit().await.map_err(anyhow::Error::from)?;

    tracing::info!(
        member_id,
        merchant_oid = %payload.merchant_oid,
        plan = %effective_plan,
        first = is_first_payment,
        expires_at = %expires_at,
        "Abonelik güncellendi"
    );

    // Email bildirimi — hata olursa callback'i etkilemez
    if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
        let to = sub.user_email.as_deref().unwrap_or("");
        if !to.is_empty() {
            let name = customer_repo::find_name(&state.db, member_id).await;
            let content = email_templates::payment_success(
                &email_templates::PaymentInfo {
                    name: name.as_deref(),
                    plan: &effective_plan,
                    amount: &payment.amount,
                    billing_cycle: &sub.billing_cycle,
                    expires_at,
                    order_no: &payload.merchant_oid,
                    site_url: &email_cfg.site_url,
                },
                is_first_payment,
                customer_status == "active",
            );
            email::send(mailer, email_cfg, to, content).await;
        }
    }

    Ok(())
}

async fn handle_failed(state: &crate::AppData, payload: &CallbackPayload) -> Result<(), AppError> {
    // Yalnızca pending ödeme etkilenir: PayTR'ın tekrar bildirimleri ya da scheduler'ın
    // sync yanıtında zaten işaretlediği ödeme ikinci kez sayılmaz.
    let Some(p) = payment_repo::set_failed(
        &state.db,
        &payload.merchant_oid,
        payload.failed_reason_code.as_deref(),
        payload.failed_reason_msg.as_deref(),
    )
    .await
    .map_err(anyhow::Error::from)?
    else {
        tracing::info!(merchant_oid = %payload.merchant_oid, "Başarısız bildirimi: ödeme zaten işlenmiş");
        return Ok(());
    };

    tracing::warn!(
        merchant_oid = %payload.merchant_oid,
        code = ?payload.failed_reason_code,
        msg  = ?payload.failed_reason_msg,
        "Ödeme başarısız"
    );

    // İlk ödeme (3DS) başarısızlıkları yenileme hakkından düşmez; yalnızca yenilemeler.
    if p.is_3d {
        return Ok(());
    }

    // Mağaza tarafı ret (PayTR yetkisi, token, hash): müşterinin kartıyla ilgisiz. Deneme
    // sayılmaz, müşteriye bildirim gitmez, yönetici uyarılır; scheduler ertesi gün yeniden dener.
    if let Some(reason) = payload
        .failed_reason_msg
        .as_deref()
        .filter(|m| paytr_client::is_merchant_side_error(m))
    {
        tracing::error!(
            merchant_oid = %payload.merchant_oid, member_id = p.member_id, reason,
            "Yenileme mağaza kaynaklı hatayla reddedildi (deneme sayılmadı)"
        );
        if let (Some(mailer), Some(email_cfg), Some(to)) = (&state.mailer, &state.config.email, &state.config.alert_email) {
            let content = email_templates::renewal_blocked_alert(
                reason,
                vec![
                    ("Sipariş no", payload.merchant_oid.clone()),
                    ("Üye", p.member_id.to_string()),
                    ("Abonelik", p.subscription_id.map_or("-".to_string(), |s| s.to_string())),
                    ("Tutar", email_templates::format_tl(&p.amount)),
                ],
                &email_cfg.site_url,
            );
            email::send(mailer, email_cfg, to, content).await;
        }
        return Ok(());
    }

    customer_repo::increment_failed_attempts(&state.db, p.member_id)
        .await
        .map_err(anyhow::Error::from)?;
    if let Some(sub_id) = p.subscription_id {
        subscription_repo::increment_renewal_attempts(&state.db, sub_id)
            .await
            .map_err(anyhow::Error::from)?;
    }

    // Başarısız yenileme email bildirimi
    if let (Some(mailer), Some(email_cfg)) = (&state.mailer, &state.config.email) {
        if let Some(sub_id) = p.subscription_id {
            if let Ok(Some(sub)) = subscription_repo::find_by_id(&state.db, sub_id).await {
                let to = sub.user_email.as_deref().unwrap_or("");
                if !to.is_empty() {
                    let remaining = (state.config.max_failed_attempts - sub.renewal_attempts).max(0);
                    let name = customer_repo::find_name(&state.db, p.member_id).await;
                    let content = email_templates::payment_failed(
                        name.as_deref(),
                        &sub.plan,
                        &sub.billing_cycle,
                        &p.amount,
                        payload.failed_reason_msg.as_deref(),
                        remaining,
                        &email_cfg.site_url,
                    );
                    email::send(mailer, email_cfg, to, content).await;
                }
            }
        }
    }

    Ok(())
}

/// PayTR'dan güncel kart listesini çeker.
async fn fetch_paytr_cards(state: &crate::AppData, utoken: &str) -> Result<Vec<CardItem>, AppError> {
    let paytr_token = generate_card_list_token(
        utoken,
        &state.config.merchant_salt,
        &state.config.merchant_key,
    );

    let body: serde_json::Value = state
        .http
        .post(paytr_client::card_list_endpoint())
        // Callback yanıtı bunu bekler; PayTR bildirimi zaman aşımına uğramasın.
        .timeout(std::time::Duration::from_secs(10))
        .form(&[
            ("merchant_id", state.config.merchant_id.as_str()),
            ("utoken",      utoken),
            ("paytr_token", paytr_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR kart listesi hatası: {}", e))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("PayTR kart listesi parse hatası: {}", e))?;

    // Hata durumunda PayTR {"status":"error","err_msg":"..."} objesi döner.
    // Başarıda doğrudan kart array'i döner: [{...}, ...].
    if body.get("status").and_then(|s| s.as_str()) == Some("error") {
        let msg = body.get("err_msg").and_then(|v| v.as_str()).unwrap_or("Bilinmeyen hata");
        return Err(AppError::PaytrError(msg.to_string()));
    }

    let cards: Vec<CardItem> = serde_json::from_value(body)
        .map_err(|e| anyhow::anyhow!("Kart parse hatası: {}", e))?;

    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(id: i32, created_day: u32, metadata: Option<serde_json::Value>) -> PaytrSubscription {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 9, created_day).unwrap().and_hms_opt(0, 0, 0).unwrap();
        PaytrSubscription {
            id, member_id: 1, plan: "gold".into(), status: "active".into(), utoken: None, ctoken: None,
            billing_cycle: "monthly".into(), amount: "899.00".into(), currency: "TL".into(),
            user_phone: None, user_email: None, renewal_attempts: 0, started_at: Some(t),
            expires_at: None, next_payment_date: None, cancelled_at: None, created_at: t, updated_at: t,
            metadata, scheduled_plan: None, scheduled_amount: None,
        }
    }

    #[test]
    fn fresh_purchase_conflicts_with_any_live_subscription() {
        let new = sub(3, 10, None);
        assert!(!first_payment_conflicts(&new, &[]));
        assert!(first_payment_conflicts(&new, &[sub(2, 5, None)]));
    }

    #[test]
    fn upgrade_only_replaces_its_own_source() {
        let new = sub(3, 10, Some(serde_json::json!({ "upgrade_from": 2 })));
        assert!(!first_payment_conflicts(&new, &[sub(2, 5, None)]));
        // Bu arada başka bir satın alma aktifleşmiş → inceleme
        assert!(first_payment_conflicts(&new, &[sub(2, 5, None), sub(4, 11, None)]));
    }

    #[test]
    fn legacy_upgrade_allows_only_older_subscriptions() {
        let new = sub(3, 10, Some(serde_json::json!({ "previous_expires_at": "2026-10-01T00:00:00" })));
        assert!(!first_payment_conflicts(&new, &[sub(2, 5, None)]));
        assert!(first_payment_conflicts(&new, &[sub(4, 11, None)]));
    }
}
