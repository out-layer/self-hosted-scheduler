//! Webhook trigger: HTTP server that accepts external POST requests to trigger the agent.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{config::Config, TriggerReason};

struct WebhookState {
    config: Arc<Config>,
    trigger_tx: mpsc::Sender<TriggerReason>,
    start_time: std::time::Instant,
    last_execution: Arc<tokio::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
}

/// Start the webhook HTTP server. Blocks until shutdown.
pub async fn start(
    config: Arc<Config>,
    trigger_tx: mpsc::Sender<TriggerReason>,
    start_time: std::time::Instant,
    last_execution: Arc<tokio::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
) {
    if !config.triggers.webhook.enabled {
        return;
    }

    let port = config.triggers.webhook.port;
    let path = config.triggers.webhook.path.clone();

    let state = Arc::new(WebhookState {
        config,
        trigger_tx,
        start_time,
        last_execution,
    });

    let app = Router::new()
        .route(&path, post(handle_webhook))
        .route("/health", get(handle_health))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    info!("[webhook] Listening on http://0.0.0.0:{}{}", port, path);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind webhook port");
    axum::serve(listener, app)
        .await
        .expect("Webhook server error");
}

async fn handle_webhook(
    State(state): State<Arc<WebhookState>>,
    headers: HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Validate secret if configured
    if let Some(ref expected) = state.config.triggers.webhook.secret {
        let provided = headers
            .get("X-Webhook-Secret")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected {
            warn!("[webhook] Invalid secret");
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let data = body.map(|Json(v)| v).unwrap_or(serde_json::json!({}));
    info!("[webhook] Received trigger");

    match state
        .trigger_tx
        .send(TriggerReason::Webhook { data })
        .await
    {
        Ok(_) => Ok(Json(serde_json::json!({
            "status": "accepted",
            "message": "Trigger queued for execution"
        }))),
        Err(_) => {
            warn!("[webhook] Failed to queue trigger");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn handle_health(
    State(state): State<Arc<WebhookState>>,
) -> Json<serde_json::Value> {
    let uptime_secs = state.start_time.elapsed().as_secs();
    let last_exec = {
        let lock = state.last_execution.lock().await;
        lock.map(|t| t.to_rfc3339())
    };

    Json(serde_json::json!({
        "status": "ok",
        "uptime_secs": uptime_secs,
        "last_execution": last_exec,
    }))
}
