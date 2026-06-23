pub mod card_repo;
pub mod customer_repo;
pub mod models;
pub mod payment_repo;
pub mod subscription_repo;

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;

pub type Db = sqlx::PgPool;

pub async fn connect(database_url: &str) -> Result<Db> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    Ok(pool)
}
