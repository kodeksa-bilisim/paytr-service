//! Bekleyen faturaları entegratöre gönderen arka plan görevi.
//!
//! Her dakika: bekleyen kayıtları kirala → (önceki deneme varsa) ETTN ile durum sorgula →
//! yoksa gönder → numarayı yaz. Geçici hatada üstel bekleme, kalıcı hatada ya da 8 denemede
//! `failed` + yönetici e-postası. Kesilen faturaların GİB onayı 2 gün boyunca izlenir.
//! GİB e-Fatura kullanıcı listesi günde bir yenilenir.

use std::time::Duration;

use chrono::Utc;

use super::turkcell::{Client, TcError};
use super::{model, repo, DocType};
use crate::config::EinvoiceConfig;
use crate::{email, email_templates, AppState};

const BATCH: i64 = 20;
const MAX_ATTEMPTS: i32 = 8;
const USERS_MAX_AGE_HOURS: i64 = 24;

/// Turkcell durumları (e-Arşiv ve giden e-Fatura); onay (60) `repo::unconfirmed` sorgusunda.
const ST_ERROR: i32 = 40;
const ST_CANCELLED: i32 = 100;

/// n'inci başarısız denemeden sonra bekleme: 2, 4, 8 … dk, en çok 6 saat.
pub fn backoff_secs(attempt: i32) -> i64 {
    let mins = 1_i64 << attempt.clamp(1, 12);
    (mins * 60).min(6 * 3600)
}

pub fn start(state: AppState) {
    if state.config.einvoice.is_none() {
        tracing::warn!("e-Fatura kesimi kapalı (EINVOICE_ENABLED): faturalar 'pending' kalır");
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(env_secs("EINVOICE_START_DELAY_SECS", 20))).await;
        let mut ticker = tokio::time::interval(Duration::from_secs(env_secs("EINVOICE_INTERVAL_SECS", 60)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let s = state.clone();
            // Her çalışma ayrı görevde: panik yalnızca o çalışmayı düşürür.
            if let Err(e) = tokio::spawn(async move { run_once(&s).await }).await {
                tracing::error!("e-Fatura görevi panik ile düştü, sonraki çalışmada devam: {:?}", e);
            }
        }
    });
}

/// Süreler yalnızca uçtan uca testlerde kısaltılır.
fn env_secs(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(default)
}

fn client<'a>(state: &'a AppState, cfg: &'a EinvoiceConfig) -> Client<'a> {
    Client { http: &state.http, base: &cfg.base_url, key: &cfg.api_key }
}

pub async fn run_once(state: &AppState) {
    let Some(cfg) = state.config.einvoice.as_ref() else { return };
    sync_users_if_stale(state, cfg).await;
    if let Err(e) = issue_pending(state, cfg).await {
        tracing::error!("Fatura kuyruğu işlenemedi: {:?}", e);
    }
    if let Err(e) = follow_up(state, cfg).await {
        tracing::error!("Fatura durum takibi başarısız: {:?}", e);
    }
}

async fn sync_users_if_stale(state: &AppState, cfg: &EinvoiceConfig) {
    let fresh = match repo::users_synced_at(&state.db).await {
        Ok(Some(t)) => Utc::now().naive_utc() - t < chrono::Duration::hours(USERS_MAX_AGE_HOURS),
        Ok(None) => false,
        Err(e) => {
            tracing::warn!("e-Fatura kullanıcı listesi okunamadı: {:?}", e);
            return;
        }
    };
    if fresh {
        return;
    }
    let bytes = match client(state, cfg).users_zip().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("e-Fatura kullanıcı listesi indirilemedi (eski liste kullanılıyor): {e}");
            return;
        }
    };
    // Canlı liste büyük: ayrıştırma bloklayan iş parçacığında.
    let parsed = tokio::task::spawn_blocking(move || super::turkcell::parse_users_zip(&bytes)).await;
    match parsed {
        Ok(Ok(users)) if !users.is_empty() => match repo::replace_users(&state.db, &users).await {
            Ok(n) => tracing::info!(count = n, "e-Fatura kullanıcı listesi yenilendi"),
            Err(e) => tracing::error!("e-Fatura kullanıcı listesi yazılamadı: {:?}", e),
        },
        Ok(Ok(_)) => tracing::warn!("e-Fatura kullanıcı listesi boş geldi; eski liste korunuyor"),
        Ok(Err(e)) => tracing::error!("e-Fatura kullanıcı listesi ayrıştırılamadı: {:?}", e),
        Err(e) => tracing::error!("e-Fatura kullanıcı listesi görevi düştü: {:?}", e),
    }
}

/// Alıcı GİB e-Fatura kullanıcısıysa e-Fatura (posta kutusuyla), değilse e-Arşiv.
async fn decide(state: &AppState, buyer: &serde_json::Value) -> anyhow::Result<(DocType, Option<String>)> {
    if let Some(id) = super::buyer_identifier(buyer) {
        if let Some(alias) = repo::einvoice_alias(&state.db, &id).await? {
            return Ok((DocType::Efatura, Some(alias)));
        }
    }
    Ok((DocType::Earchive, None))
}

async fn issue_pending(state: &AppState, cfg: &EinvoiceConfig) -> anyhow::Result<()> {
    let batch = repo::claim_pending(&state.db, cfg.start_at, BATCH).await?;
    for inv in batch {
        issue_one(state, cfg, &inv).await;
    }
    Ok(())
}

async fn issue_one(state: &AppState, cfg: &EinvoiceConfig, inv: &repo::Claimed) {
    let tc = client(state, cfg);
    let attempt = inv.prior_attempts + 1;

    // Belge türü ilk denemede sabitlenir.
    let (doc, alias) = match inv.doc_type.as_deref().and_then(DocType::parse) {
        Some(DocType::Efatura) => match super::buyer_identifier(&inv.buyer) {
            Some(id) => (DocType::Efatura, repo::einvoice_alias(&state.db, &id).await.ok().flatten()),
            None => (DocType::Efatura, None),
        },
        Some(d) => (d, None),
        None => match decide(state, &inv.buyer).await {
            Ok((d, a)) => {
                if let Err(e) = repo::set_doc_type(&state.db, inv.id, d.as_str()).await {
                    tracing::error!(invoice = inv.id, "Belge türü yazılamadı: {:?}", e);
                    return;
                }
                (d, a)
            }
            Err(e) => {
                retry_or_fail(state, inv, attempt, &format!("belge türü belirlenemedi: {e}")).await;
                return;
            }
        },
    };
    if doc == DocType::Efatura && alias.is_none() {
        fail(state, inv, "e-Fatura alıcısının posta kutusu bulunamadı (GİB listesi)", None).await;
        return;
    }

    // Önceki deneme gönderilmiş olabilir: aynı ETTN Turkcell'de varsa yeniden gönderilmez.
    if inv.prior_attempts > 0 {
        match tc.status(doc, &inv.ettn).await {
            Ok(st) => {
                settle(state, inv, st.status, st.invoice_number.as_deref(), st.message.as_deref()).await;
                return;
            }
            Err(TcError::NotFound) => {}
            Err(e) => {
                retry_or_fail(state, inv, attempt, &e.to_string()).await;
                return;
            }
        }
    }

    let line_name = inv.lines.get(0).and_then(|l| l.get("name")).and_then(|v| v.as_str()).unwrap_or("nlink hizmet bedeli");
    let body = model::build(
        &model::Input {
            merchant_oid: &inv.merchant_oid,
            ettn: &inv.ettn,
            doc,
            alias: alias.as_deref(),
            buyer: &inv.buyer,
            line_name,
            vat_rate: inv.vat_rate as i64,
            net_kurus: inv.net_kurus,
            vat_kurus: inv.vat_kurus,
            paid_at: inv.created_at,
        },
        cfg,
    );
    match tc.create(doc, &body).await {
        Ok(c) => {
            if let Err(e) = repo::mark_issued(&state.db, inv.id, Some(&c.invoice_number), None).await {
                // Fatura kesildi ama yazılamadı: kayıt kirada kalır, sonraki deneme durumdan bulur.
                tracing::error!(invoice = inv.id, ettn = %inv.ettn, "Kesilen fatura kaydedilemedi: {:?}", e);
                return;
            }
            tracing::info!(invoice = inv.id, merchant_oid = %inv.merchant_oid, doc = doc.as_str(), number = %c.invoice_number, "Fatura kesildi");
        }
        // Aynı ETTN zaten var (önceki gönderim yazılamamış): durumdan tamamlanır.
        Err(e) if e.is_duplicate_ettn() => match tc.status(doc, &inv.ettn).await {
            Ok(st) => settle(state, inv, st.status, st.invoice_number.as_deref(), st.message.as_deref()).await,
            Err(e2) => retry_or_fail(state, inv, attempt, &e2.to_string()).await,
        },
        Err(e) if e.is_permanent() => fail(state, inv, &e.to_string(), None).await,
        Err(e) => retry_or_fail(state, inv, attempt, &e.to_string()).await,
    }
}

/// Turkcell'deki duruma göre kaydı kapatır.
async fn settle(state: &AppState, inv: &repo::Claimed, status: i32, number: Option<&str>, message: Option<&str>) {
    let r = match status {
        ST_ERROR => {
            fail(state, inv, &format!("Turkcell/GİB hatası: {}", message.unwrap_or("-")), Some(status)).await;
            return;
        }
        ST_CANCELLED => repo::mark_cancelled(&state.db, inv.id).await,
        s => repo::mark_issued(&state.db, inv.id, number, Some(s)).await,
    };
    if let Err(e) = r {
        tracing::error!(invoice = inv.id, "Fatura durumu yazılamadı: {:?}", e);
    }
}

async fn retry_or_fail(state: &AppState, inv: &repo::Claimed, attempt: i32, error: &str) {
    if attempt >= MAX_ATTEMPTS {
        fail(state, inv, &format!("{MAX_ATTEMPTS} denemede kesilemedi: {error}"), None).await;
        return;
    }
    let wait = backoff_secs(attempt);
    tracing::warn!(invoice = inv.id, attempt, wait_secs = wait, "Fatura kesilemedi, yeniden denenecek: {error}");
    if let Err(e) = repo::schedule_retry(&state.db, inv.id, error, wait).await {
        tracing::error!(invoice = inv.id, "Yeniden deneme zamanı yazılamadı: {:?}", e);
    }
}

async fn fail(state: &AppState, inv: &repo::Claimed, error: &str, provider_status: Option<i32>) {
    tracing::error!(invoice = inv.id, merchant_oid = %inv.merchant_oid, "Fatura kesilemedi: {error}");
    if let Err(e) = repo::mark_failed(&state.db, inv.id, error, provider_status).await {
        tracing::error!(invoice = inv.id, "Hata durumu yazılamadı: {:?}", e);
    }
    alert(
        state,
        error,
        vec![
            ("Fatura kaydı", inv.id.to_string()),
            ("Sipariş no", inv.merchant_oid.clone()),
            ("Üye", inv.member_id.to_string()),
            ("Tutar", format!("{:.2} TL", inv.total_kurus as f64 / 100.0)),
            ("ETTN", inv.ettn.clone()),
        ],
    )
    .await;
}

async fn alert(state: &AppState, reason: &str, details: Vec<(&str, String)>) {
    let (Some(mailer), Some(email_cfg), Some(to)) = (&state.mailer, &state.config.email, &state.config.alert_email) else {
        return;
    };
    let content = email_templates::invoice_failed_alert(reason, details, &email_cfg.site_url);
    email::send(mailer, email_cfg, to, content).await;
}

/// Kesilen faturaların GİB sonucunu izler (onay 60, hata 40, iptal 100).
async fn follow_up(state: &AppState, cfg: &EinvoiceConfig) -> anyhow::Result<()> {
    let tc = client(state, cfg);
    for u in repo::unconfirmed(&state.db, 50).await? {
        let Some(doc) = DocType::parse(&u.doc_type) else { continue };
        match tc.status(doc, &u.ettn).await {
            Ok(st) if st.status == ST_ERROR => {
                let msg = format!("GİB'e iletilemedi: {}", st.message.as_deref().unwrap_or("-"));
                repo::mark_failed(&state.db, u.id, &msg, Some(st.status)).await?;
                alert(
                    state,
                    &msg,
                    vec![
                        ("Fatura kaydı", u.id.to_string()),
                        ("Fatura no", u.invoice_no.clone().unwrap_or_default()),
                        ("Sipariş no", u.merchant_oid.clone()),
                        ("Üye", u.member_id.to_string()),
                    ],
                )
                .await;
            }
            Ok(st) if st.status == ST_CANCELLED => repo::mark_cancelled(&state.db, u.id).await?,
            Ok(st) => repo::set_provider_status(&state.db, u.id, st.status).await?,
            Err(e) => tracing::warn!(invoice = u.id, "Fatura durumu sorgulanamadı: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff_secs(1), 120);
        assert_eq!(backoff_secs(2), 240);
        assert_eq!(backoff_secs(5), 32 * 60);
        assert_eq!(backoff_secs(8), 256 * 60);
        assert_eq!(backoff_secs(9), 6 * 3600);
        assert_eq!(backoff_secs(0), 120);
    }
}
