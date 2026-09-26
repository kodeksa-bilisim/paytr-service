pub mod card_repo;
pub mod customer_repo;
pub mod models;
pub mod payment_repo;
pub mod subscription_repo;

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;
use sqlx::Executor;

pub type Db = sqlx::PgPool;

pub async fn connect(database_url: &str) -> Result<Db> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        // Sunucunun varsayılan saat dilimi Europe/Istanbul; sütunlar `timestamp without time zone`
        // ve Rust tarafı UTC (`Utc::now().naive_utc()`) yazıyor. Oturumu UTC'ye sabitlemezsek
        // SQL'deki NOW() karşılaştırmaları 3 saat kayar (abonelikler erken expire olur).
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("SET TIME ZONE 'UTC'").await?;
                Ok(())
            })
        })
        .connect(database_url)
        .await?;
    Ok(pool)
}
