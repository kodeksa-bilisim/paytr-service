//! Üyenin kayıtlı kartlarını PayTR'dan ve DB'den silme.

use crate::{
    crypto::generate_card_delete_token,
    db::card_repo,
    models::card::PaytrErrorResponse,
    paytr_client,
    AppData,
};

/// Üyenin tüm kayıtlı kartlarını PayTR'dan siler, ardından DB'yi temizler.
/// Abonelik süresi bitip üyenin geçerli aboneliği kalmadığında çağrılır (iptal anında
/// değil: iptal yalnızca otomatik yenilemeyi durdurur, dönem sonuna kadar geri alınabilir).
/// Hata olursa loglayıp devam eder.
pub async fn delete_member_cards(state: &AppData, member_id: i32) {
    let cards = match card_repo::list_by_member(&state.db, member_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(member_id, error = %e, "Kart listesi alınamadı, silme atlandı");
            return;
        }
    };

    let utoken = match card_repo::get_user_token(&state.db, member_id).await {
        Ok(Some(t)) => t.utoken,
        Ok(None) => {
            tracing::info!(member_id, "Kayıtlı utoken yok, kart silme adımı atlandı");
            return;
        }
        Err(e) => {
            tracing::error!(member_id, error = %e, "utoken alınamadı, silme atlandı");
            return;
        }
    };

    for card in &cards {
        let token = generate_card_delete_token(
            &card.ctoken,
            &utoken,
            &state.config.merchant_salt,
            &state.config.merchant_key,
        );

        let result = state
            .http
            .post(paytr_client::card_delete_endpoint())
            .form(&[
                ("merchant_id", state.config.merchant_id.as_str()),
                ("utoken",      utoken.as_str()),
                ("ctoken",      card.ctoken.as_str()),
                ("paytr_token", token.as_str()),
            ])
            .send()
            .await;

        match result {
            Err(e) => {
                tracing::error!(member_id, card_id = card.id, error = %e, "PayTR kart silme isteği başarısız");
            }
            Ok(resp) => match resp.json::<PaytrErrorResponse>().await {
                Ok(body) if body.status == "error" => {
                    tracing::error!(member_id, card_id = card.id, err_msg = ?body.err_msg, "PayTR kart silme reddedildi");
                }
                Ok(_) => tracing::info!(member_id, card_id = card.id, "PayTR kart silindi"),
                Err(e) => {
                    tracing::error!(member_id, card_id = card.id, error = %e, "PayTR silme yanıtı parse hatası");
                }
            },
        }
    }

    // PayTR adımı tamamlandıktan sonra DB'yi temizle
    if let Err(e) = card_repo::purge_member_cards(&state.db, member_id).await {
        tracing::error!(member_id, error = %e, "DB kart temizleme hatası");
    } else {
        tracing::info!(member_id, cards = cards.len(), "Kart verileri DB'den temizlendi");
    }
}
