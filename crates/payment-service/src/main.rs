mod config;
mod crypto;
mod db;
mod email;
mod error;
mod handlers;
mod models;
mod paytr_client;
mod scheduler;

use std::sync::Arc;

use std::time::Duration;

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use config::Config;
use db::Db;

pub struct AppData {
    pub config: Config,
    pub http: reqwest::Client,
    pub db: Db,
    pub mailer: Option<email::Mailer>,
}

pub type AppState = Arc<AppData>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "payment_service=debug,tower_http=debug,sqlx=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    dotenvy::dotenv().ok();

    let config = Config::from_env()?;
    let bind_addr = format!("{}:{}", config.host, config.port);

    tracing::info!("Veritabanına bağlanılıyor...");
    let db = db::connect(&config.database_url).await?;

    tracing::info!("Migration'lar çalıştırılıyor...");
    sqlx::migrate!("./migrations").run(&db).await?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let mailer = config
        .email
        .as_ref()
        .map(email::build_mailer)
        .transpose()
        .map_err(|e| anyhow::anyhow!("SMTP mailer oluşturulamadı: {}", e))?;

    if mailer.is_some() {
        tracing::info!("Email bildirimleri aktif");
    } else {
        tracing::warn!("SMTP ayarları eksik — email bildirimleri devre dışı");
    }

    let state: AppState = Arc::new(AppData {
        config,
        http,
        db,
        mailer,
    });

    // Subscription scheduler'ı arka planda başlat
    scheduler::start(Arc::clone(&state));

    let app = Router::new()
        .route("/health", get(handlers::health))
        // Ödeme başlatma
        .route("/api/v1/payments/init", post(handlers::payment::init_payment))
        .route("/api/v1/payments/init-enterprise", post(handlers::payment::init_enterprise_payment))
        .route("/api/v1/payments/stored-card", post(handlers::payment::stored_card_payment))
        // PayTR callback
        .route("/api/v1/payments/callback", post(handlers::callback::payment_callback))
        // Redirect placeholder (sync_mode olmayan akış için)
        .route("/api/v1/payments/ok",   get(handlers::payment_ok))
        .route("/api/v1/payments/fail", get(handlers::payment_fail))
        // Abonelik yönetimi
        .route("/api/v1/subscriptions/cancel", post(handlers::subscription::cancel_subscription))
        // Kart yönetimi
        .route("/api/v1/cards/list",   post(handlers::card::list_cards))
        .route("/api/v1/cards/delete", post(handlers::card::delete_card))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("payment-service dinleniyor: {}", bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}
