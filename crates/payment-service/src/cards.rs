//! Üyenin kayıtlı kartlarını PayTR'dan ve DB'den silme.

use crate::{
    crypto::generate_card_delete_token,
    db::card_repo,
    models::card::PaytrErrorResponse,
    paytr_client,
    AppData,
};

/// Üyenin tüm kayıtlı kartlarını PayTR'dan siler; DB'de yalnızca PayTR'ın sildiğini onayladığı
/// kartlar pasiflenir (başarısızlar scheduler'da yeniden denenir). Her kart kendi utoken'ıyla
/// imzalanır: üyenin birden fazla utoken'ı olabilir (eskiden hepsi tek utoken ile imzalanıyor,
/// diğer utoken'lardaki kartlar PayTR'da kalıyordu).
/// Abonelik süresi bitip üyenin geçerli aboneliği kalmadığında çağrılır (iptal anında değil:
/// iptal yalnızca otomatik yenilemeyi durdurur, dönem sonuna kadar geri alınabilir).
pub async fn delete_member_cards(state: &AppData, member_id: i32) {
    let cards = match card_repo::list_by_member(&state.db, member_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(member_id, error = %e, "Kart listesi alınamadı, silme atlandı");
            return;
        }
    };

    let mut deleted = 0;
    for card in &cards {
        if delete_at_paytr(state, member_id, card.id, &card.utoken, &card.ctoken).await {
            match card_repo::deactivate_card(&state.db, &card.ctoken).await {
                Ok(()) => deleted += 1,
                Err(e) => tracing::error!(member_id, card_id = card.id, error = %e, "Kart DB'de pasiflenemedi"),
            }
        }
    }

    if let Err(e) = card_repo::deactivate_empty_tokens(&state.db, member_id).await {
        tracing::error!(member_id, error = %e, "utoken'lar pasiflenemedi");
    }
    tracing::info!(member_id, deleted, total = cards.len(), "Kart silme tamamlandı");
}

/// PayTR'dan tek kart siler. true: PayTR silmeyi onayladı.
async fn delete_at_paytr(state: &AppData, member_id: i32, card_id: i32, utoken: &str, ctoken: &str) -> bool {
    let token = generate_card_delete_token(ctoken, utoken, &state.config.merchant_salt, &state.config.merchant_key);
    let resp = state
        .http
        .post(paytr_client::card_delete_endpoint())
        .form(&[
            ("merchant_id", state.config.merchant_id.as_str()),
            ("utoken", utoken),
            ("ctoken", ctoken),
            ("paytr_token", token.as_str()),
        ])
        .send()
        .await;

    match resp {
        Err(e) => {
            tracing::error!(member_id, card_id, error = %e, "PayTR kart silme isteği başarısız");
            false
        }
        Ok(resp) => match resp.json::<PaytrErrorResponse>().await {
            Ok(body) if body.status == "success" => {
                tracing::info!(member_id, card_id, "PayTR kart silindi");
                true
            }
            Ok(body) => {
                tracing::error!(member_id, card_id, status = %body.status, err_msg = ?body.err_msg, "PayTR kart silme reddedildi");
                false
            }
            Err(e) => {
                tracing::error!(member_id, card_id, error = %e, "PayTR silme yanıtı parse hatası");
                false
            }
        },
    }
}

/// Geçerli aboneliği kalmamış ama kartı duran üyelerin kartlarını siler (başarısız silmelerin
/// yeniden denenmesi). Scheduler her çalışmada sınırlı sayıda üye işler.
pub async fn retry_orphan_cards(state: &AppData) {
    match card_repo::members_with_orphan_cards(&state.db, 20).await {
        Ok(members) => {
            for m in members {
                delete_member_cards(state, m).await;
            }
        }
        Err(e) => tracing::error!(error = %e, "Yetim kart sorgusu başarısız"),
    }
}
