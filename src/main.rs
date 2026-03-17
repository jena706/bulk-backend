
use axum::{
    extract::{Path, State},
    http::{HeaderValue, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing::info;

// ─── APP STATE ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    db: PgPool,
}

// ─── REQUEST / RESPONSE TYPES ────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct UserState {
    pubkey: String,
    daily_loss: f64,
    daily_preds: i32,
    last_pred_time: i64,
    prediction_history: Value,
    risk_settings: Value,
    daily_reset_at: NaiveDate,
}

#[derive(Debug, Deserialize)]
struct SaveStateRequest {
    daily_loss: f64,
    daily_preds: i32,
    last_pred_time: i64,
    prediction_history: Value,
    risk_settings: Value,
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    pubkey: String,
    coin: String,
    direction: String,
    entry_price: f64,
    trade_size_usdt: f64,
}

#[derive(Debug, Deserialize)]
struct ResolveRequest {
    id: String,
    exit_price: f64,
    won: bool,
}

#[derive(Debug, Serialize)]
struct ApiResponse<T: Serialize> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    fn ok(data: T) -> Json<Self> {
        Json(Self { ok: true, data: Some(data), error: None })
    }
}

fn err_response(msg: &str) -> Json<ApiResponse<Value>> {
    Json(ApiResponse { ok: false, data: None, error: Some(msg.to_string()) })
}

// ─── HELPERS ─────────────────────────────────────────────────────────────────

/// Validate a base58 Solana pubkey (32 bytes decoded)
fn validate_pubkey(pubkey: &str) -> bool {
    match bs58::decode(pubkey).into_vec() {
        Ok(bytes) => bytes.len() == 32,
        Err(_) => false,
    }
}

/// Check if a daily reset is needed (date has rolled over since last save)
fn needs_daily_reset(reset_date: NaiveDate) -> bool {
    let today = Utc::now().date_naive();
    reset_date < today
}

// ─── HANDLERS ─────────────────────────────────────────────────────────────────

/// GET /state/:pubkey
/// Load a user's persisted state. If none exists, returns sensible defaults.
/// Also auto-resets daily counters if the date has rolled over.
async fn get_state(
    Path(pubkey): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if !validate_pubkey(&pubkey) {
        return (StatusCode::BAD_REQUEST, err_response("Invalid pubkey")).into_response();
    }

    let row = sqlx::query_as!(
        UserState,
        r#"
        SELECT pubkey, daily_loss, daily_preds, last_pred_time,
               prediction_history, risk_settings, daily_reset_at
        FROM user_state
        WHERE pubkey = $1
        "#,
        pubkey
    )
    .fetch_optional(&state.db)
    .await;

    match row {
        Ok(Some(mut user)) => {
            // Auto-reset daily counters if date has rolled over
            if needs_daily_reset(user.daily_reset_at) {
                let _ = sqlx::query!(
                    r#"
                    UPDATE user_state
                    SET daily_loss = 0.0,
                        daily_preds = 0,
                        last_pred_time = 0,
                        daily_reset_at = CURRENT_DATE
                    WHERE pubkey = $1
                    "#,
                    pubkey
                )
                .execute(&state.db)
                .await;
                user.daily_loss = 0.0;
                user.daily_preds = 0;
                user.last_pred_time = 0;
                user.daily_reset_at = Utc::now().date_naive();
                info!("Daily reset applied for {}", &pubkey[..8]);
            }
            (StatusCode::OK, ApiResponse::ok(user)).into_response()
        }
        Ok(None) => {
            // First time user — return defaults, don't write yet
            let defaults = json!({
                "pubkey": pubkey,
                "daily_loss": 0.0,
                "daily_preds": 0,
                "last_pred_time": 0,
                "prediction_history": [],
                "risk_settings": {
                    "maxTrade": 200,
                    "dailyLoss": 500,
                    "maxPreds": 10,
                    "cooldown": 120
                },
                "daily_reset_at": Utc::now().date_naive().to_string()
            });
            (StatusCode::OK, ApiResponse::ok(defaults)).into_response()
        }
        Err(e) => {
            tracing::error!("DB error in get_state: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, err_response("Database error")).into_response()
        }
    }
}

/// POST /state/:pubkey
/// Upsert a user's full state after each prediction resolves.
/// Keeps prediction_history trimmed to the last 100 entries.
async fn save_state(
    Path(pubkey): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<SaveStateRequest>,
) -> impl IntoResponse {
    if !validate_pubkey(&pubkey) {
        return (StatusCode::BAD_REQUEST, err_response("Invalid pubkey")).into_response();
    }

    // Enforce sanity bounds
    let daily_loss = body.daily_loss.max(0.0);
    let daily_preds = body.daily_preds.max(0);
    let last_pred_time = body.last_pred_time.max(0);

    // Trim prediction history to last 100 entries
    let history = if let Some(arr) = body.prediction_history.as_array() {
        let trimmed: Vec<_> = arr.iter().take(100).cloned().collect();
        json!(trimmed)
    } else {
        json!([])
    };

    let result = sqlx::query!(
        r#"
        INSERT INTO user_state
            (pubkey, daily_loss, daily_preds, last_pred_time,
             prediction_history, risk_settings, daily_reset_at)
        VALUES ($1, $2, $3, $4, $5, $6, CURRENT_DATE)
        ON CONFLICT (pubkey) DO UPDATE SET
            daily_loss      = EXCLUDED.daily_loss,
            daily_preds     = EXCLUDED.daily_preds,
            last_pred_time  = EXCLUDED.last_pred_time,
            prediction_history = EXCLUDED.prediction_history,
            risk_settings   = EXCLUDED.risk_settings,
            daily_reset_at  = CASE
                WHEN user_state.daily_reset_at < CURRENT_DATE THEN CURRENT_DATE
                ELSE user_state.daily_reset_at
            END
        "#,
        pubkey,
        daily_loss,
        daily_preds,
        last_pred_time,
        history,
        body.risk_settings,
    )
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => {
            info!("State saved for {}", &pubkey[..8.min(pubkey.len())]);
            (StatusCode::OK, ApiResponse::ok(json!({ "saved": true }))).into_response()
        }
        Err(e) => {
            tracing::error!("DB error in save_state: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, err_response("Database error")).into_response()
        }
    }
}

/// POST /verify
/// Record a new prediction server-side at entry time (before the 60s window).
/// Returns a verification ID the frontend sends back when resolving.
async fn verify_prediction(
    State(state): State<AppState>,
    Json(body): Json<VerifyRequest>,
) -> impl IntoResponse {
    if !validate_pubkey(&body.pubkey) {
        return (StatusCode::BAD_REQUEST, err_response("Invalid pubkey")).into_response();
    }
    if body.direction != "UP" && body.direction != "DOWN" {
        return (StatusCode::BAD_REQUEST, err_response("direction must be UP or DOWN")).into_response();
    }
    if body.entry_price <= 0.0 || body.trade_size_usdt <= 0.0 {
        return (StatusCode::BAD_REQUEST, err_response("Invalid price or size")).into_response();
    }

    let result = sqlx::query!(
        r#"
        INSERT INTO prediction_verifications
            (pubkey, coin, direction, entry_price, trade_size_usdt)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        "#,
        body.pubkey,
        body.coin,
        body.direction,
        body.entry_price,
        body.trade_size_usdt,
    )
    .fetch_one(&state.db)
    .await;

    match result {
        Ok(row) => {
            info!("Prediction verified: {} {} {}", &body.pubkey[..8.min(body.pubkey.len())], body.coin, body.direction);
            (StatusCode::OK, ApiResponse::ok(json!({ "id": row.id }))).into_response()
        }
        Err(e) => {
            tracing::error!("DB error in verify_prediction: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, err_response("Database error")).into_response()
        }
    }
}

/// POST /resolve
/// Record the result of a prediction after the 60s window closes.
async fn resolve_prediction(
    State(state): State<AppState>,
    Json(body): Json<ResolveRequest>,
) -> impl IntoResponse {
    let id = match uuid::Uuid::parse_str(&body.id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, err_response("Invalid verification ID")).into_response(),
    };
    if body.exit_price <= 0.0 {
        return (StatusCode::BAD_REQUEST, err_response("Invalid exit price")).into_response();
    }

    let result = sqlx::query!(
        r#"
        UPDATE prediction_verifications
        SET exit_price  = $2,
            won         = $3,
            resolved_at = NOW()
        WHERE id = $1 AND resolved_at IS NULL
        RETURNING id
        "#,
        id,
        body.exit_price,
        body.won,
    )
    .fetch_optional(&state.db)
    .await;

    match result {
        Ok(Some(_)) => {
            (StatusCode::OK, ApiResponse::ok(json!({ "resolved": true }))).into_response()
        }
        Ok(None) => {
            (StatusCode::NOT_FOUND, err_response("Prediction not found or already resolved")).into_response()
        }
        Err(e) => {
            tracing::error!("DB error in resolve_prediction: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, err_response("Database error")).into_response()
        }
    }
}

/// GET /health
/// Simple liveness probe for Railway / Fly.io
async fn health() -> impl IntoResponse {
    Json(json!({ "ok": true, "service": "bulk-backend", "version": env!("CARGO_PKG_VERSION") }))
}

// ─── MAIN ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env in development (ignored if not present)
    let _ = dotenvy::dotenv();

    // Tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bulk_backend=info,tower_http=info".into()),
        )
        .init();

    // Database
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;

    // Run migrations
    sqlx::migrate!("./migrations").run(&pool).await?;
    info!("Migrations applied");

    // Allowed origins — set FRONTEND_URL env var in production
    let frontend_url = std::env::var("FRONTEND_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());

    let cors = CorsLayer::new()
        .allow_origin(
            frontend_url
                .parse::<HeaderValue>()
                .unwrap_or(HeaderValue::from_static("http://localhost:3000")),
        )
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any);

    // Router
    let app = Router::new()
        .route("/health",          get(health))
        .route("/state/:pubkey",   get(get_state))
        .route("/state/:pubkey",   post(save_state))
        .route("/verify",          post(verify_prediction))
        .route("/resolve",         post(resolve_prediction))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(AppState { db: pool });

    // Bind
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse()
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("bulk-backend listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
