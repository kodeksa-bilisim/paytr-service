mod billing;
mod config;
mod cards;
mod crypto;
mod db;
mod email;
mod email_templates;
mod error;
mod growth;
mod handlers;
mod models;
mod paytr_client;
mod pricing;
mod scheduler;

use std::sync::Arc;

use std::time::Duration;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
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
    /// Scheduler'ın son başarılı çalışması (unix saniye; 0 = henüz yok).
    pub scheduler_last_ok: std::sync::atomic::AtomicI64,
}

pub type AppState = Arc<AppData>;

/// Next.js → servis çağrılarını doğrular: `X-Internal-Token` = INTERNAL_API_TOKEN.
/// Güvenlik yalnızca ağ izolasyonuna (firewall/127.0.0.1) bırakılmaz.
async fn require_internal_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get("X-Internal-Token")
        .map(|v| crypto::constant_time_eq(v.as_bytes(), state.config.internal_api_token.as_bytes()))
        .unwrap_or(false);
    if !ok {
        tracing::warn!(path = %req.uri().path(), "Geçersiz/eksik iç API token'ı");
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "Yetkisiz" })))
            .into_response();
    }
    next.run(req).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "payment_service=info,tower_http=info,sqlx=warn".into()),
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

    if let Some(m) = &mailer {
        tracing::info!("Email bildirimleri aktif");
        email::check_connection(m).await;
    } else {
        tracing::warn!("SMTP ayarları eksik — email bildirimleri devre dışı");
    }

    let state: AppState = Arc::new(AppData {
        config,
        http,
        db,
        mailer,
        scheduler_last_ok: std::sync::atomic::AtomicI64::new(0),
    });

    // Subscription scheduler'ı arka planda başlat
    scheduler::start(Arc::clone(&state));

    // Yalnızca Next.js sunucusunun çağırdığı iç API — X-Internal-Token zorunlu.
    let internal = Router::new()
        .route("/api/v1/payments/init", post(handlers::payment::init_payment))
        .route("/api/v1/payments/init-enterprise", post(handlers::payment::init_enterprise_payment))
        .route("/api/v1/subscriptions/cancel", post(handlers::subscription::cancel_subscription))
        .route("/api/v1/subscriptions/reactivate", post(handlers::subscription::reactivate_subscription))
        .route("/api/v1/subscriptions/schedule-downgrade", post(handlers::payment::schedule_downgrade))
        .route("/api/v1/subscriptions/cancel-schedule", post(handlers::payment::cancel_scheduled_downgrade))
        .route("/api/v1/subscriptions/upgrade-quote", post(handlers::payment::upgrade_quote))
        // Yönetici paneli (salt-okunur); admin yetkisi Next.js tarafında doğrulanır.
        .route("/api/v1/admin/payments", get(handlers::admin::list_payments))
        .route("/api/v1/admin/overview", get(handlers::admin::overview))
        .route("/api/v1/admin/invoices/export", get(handlers::admin::export_invoices))
        .route("/api/v1/admin/invoices/backfill", post(handlers::admin::backfill_invoices))
        // Üyenin fatura bilgisi; member_id Next.js'te oturumdan alınır.
        .route("/api/v1/billing-profile/:member_id", get(handlers::billing::get_profile))
        .route("/api/v1/billing-profile", put(handlers::billing::save_profile))
        // Hesap silme / KVKK dışa aktarma (qurlbackend)
        .route("/api/v1/members/:member_id/stop-renewal", post(handlers::member::stop_renewal))
        .route("/api/v1/members/:member_id/erase", post(handlers::member::erase))
        .route("/api/v1/members/:member_id/export", get(handlers::member::export))
        // Büyüme: deneme, referans, kupon
        .route("/api/v1/members/:member_id/growth", get(handlers::growth::summary))
        .route("/api/v1/trials/start", post(handlers::growth::start_trial))
        .route("/api/v1/referrals/claim", post(handlers::growth::claim_referral))
        .route("/api/v1/admin/coupons", get(handlers::growth::list_coupons).post(handlers::growth::create_coupon))
        .route("/api/v1/admin/coupons/:code/active", post(handlers::growth::set_coupon_active))
        .route_layer(middleware::from_fn_with_state(Arc::clone(&state), require_internal_token));

    let app = Router::new()
        .route("/health", get(handlers::health))
        // PayTR callback — kimlik doğrulaması hash ile (handler içinde)
        .route("/api/v1/payments/callback", post(handlers::callback::payment_callback))
        // Redirect placeholder (sync_mode olmayan akış için)
        .route("/api/v1/payments/ok",   get(handlers::payment_ok))
        .route("/api/v1/payments/fail", get(handlers::payment_fail))
        .merge(internal)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("payment-service dinleniyor: {}", bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}
