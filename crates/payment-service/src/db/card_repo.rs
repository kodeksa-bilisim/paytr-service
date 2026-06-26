use anyhow::Result;
use sqlx::PgPool;

use super::models::{PaytrCard, PaytrUserToken};
use crate::models::card::CardItem;

pub async fn upsert_user_token(pool: &PgPool, member_id: i32, utoken: &str) -> Result<PaytrUserToken> {
    let rec = sqlx::query_as::<_, PaytrUserToken>(
        r#"
        INSERT INTO paytr_user_tokens (member_id, utoken)
        VALUES ($1, $2)
        ON CONFLICT (utoken) DO UPDATE SET updated_at = NOW()
        RETURNING *
        "#,
    )
    .bind(member_id)
    .bind(utoken)
    .fetch_one(pool)
    .await?;
    Ok(rec)
}

pub async fn get_user_token(pool: &PgPool, member_id: i32) -> Result<Option<PaytrUserToken>> {
    let rec = sqlx::query_as::<_, PaytrUserToken>(
        "SELECT * FROM paytr_user_tokens WHERE member_id = $1 AND is_active = TRUE LIMIT 1",
    )
    .bind(member_id)
    .fetch_optional(pool)
    .await?;
    Ok(rec)
}

/// PayTR'dan gelen kart listesini DB ile senkronize eder.
/// Mevcut kartları inactive yapar, sonra PayTR listesini upsert eder.
pub async fn sync_cards(pool: &PgPool, utoken: &str, cards: &[CardItem]) -> Result<()> {
    sqlx::query("UPDATE paytr_cards SET is_active = FALSE WHERE utoken = $1")
        .bind(utoken)
        .execute(pool)
        .await?;

    for card in cards {
        sqlx::query(
            r#"
            INSERT INTO paytr_cards
                (utoken, ctoken, last_4, card_bank, card_schema, card_type,
                 expiry_month, expiry_year, require_cvv, is_active)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, TRUE)
            ON CONFLICT (ctoken) DO UPDATE SET
                is_active    = TRUE,
                require_cvv  = EXCLUDED.require_cvv,
                expiry_month = EXCLUDED.expiry_month,
                expiry_year  = EXCLUDED.expiry_year
            "#,
        )
        .bind(utoken)
        .bind(&card.ctoken)
        .bind(&card.last_4)
        .bind(&card.c_bank)
        .bind(&card.schema)
        .bind(&card.c_type)
        .bind(&card.month)
        .bind(&card.year)
        .bind(card.require_cvv == 1)
        .execute(pool)
        .await?;
    }

    // Eğer default kart yoksa ilk eklenen kartı default yap
    sqlx::query(
        r#"
        WITH first_card AS (
            SELECT id FROM paytr_cards
            WHERE utoken = $1 AND is_active = TRUE
            ORDER BY created_at ASC
            LIMIT 1
        )
        UPDATE paytr_cards SET is_default = TRUE
        FROM first_card
        WHERE paytr_cards.id = first_card.id
          AND NOT EXISTS (
              SELECT 1 FROM paytr_cards
              WHERE utoken = $1 AND is_default = TRUE AND is_active = TRUE
          )
        "#,
    )
    .bind(utoken)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_default_card(pool: &PgPool, utoken: &str) -> Result<Option<PaytrCard>> {
    let card = sqlx::query_as::<_, PaytrCard>(
        "SELECT * FROM paytr_cards WHERE utoken = $1 AND is_default = TRUE AND is_active = TRUE LIMIT 1",
    )
    .bind(utoken)
    .fetch_optional(pool)
    .await?;
    Ok(card)
}

pub async fn list_by_member(pool: &PgPool, member_id: i32) -> Result<Vec<PaytrCard>> {
    let cards = sqlx::query_as::<_, PaytrCard>(
        r#"
        SELECT c.* FROM paytr_cards c
        JOIN paytr_user_tokens t ON t.utoken = c.utoken
        WHERE t.member_id = $1 AND c.is_active = TRUE AND t.is_active = TRUE
        ORDER BY c.is_default DESC, c.created_at ASC
        "#,
    )
    .bind(member_id)
    .fetch_all(pool)
    .await?;
    Ok(cards)
}

pub async fn deactivate_card(pool: &PgPool, ctoken: &str, member_id: i32) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE paytr_cards SET is_active = FALSE
        WHERE ctoken = $1
          AND utoken IN (SELECT utoken FROM paytr_user_tokens WHERE member_id = $2)
        "#,
    )
    .bind(ctoken)
    .bind(member_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Abonelik iptali / KVKK silme: üyenin tüm aktif kartlarını ve utoken'ını
/// deaktive eder. PayTR'a silme isteği atmadan önce çağrılmamalı; bu fonksiyon
/// yalnızca DB tarafını temizler.
pub async fn purge_member_cards(pool: &PgPool, member_id: i32) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE paytr_cards SET is_active = FALSE
        WHERE utoken IN (SELECT utoken FROM paytr_user_tokens WHERE member_id = $1)
        "#,
    )
    .bind(member_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "UPDATE paytr_user_tokens SET is_active = FALSE WHERE member_id = $1",
    )
    .bind(member_id)
    .execute(pool)
    .await?;

    Ok(())
}
