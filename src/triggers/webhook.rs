//! Webhook trigger: HTTP server that accepts external POST requests to trigger the agent.

use axum::{
    extract::{DefaultBodyLimit, State},
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
    /// (window_start, count) for a fixed 60s rate-limit window.
    rate: tokio::sync::Mutex<(std::time::Instant, u32)>,
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
    let bind = config.triggers.webhook.bind.clone();

    let state = Arc::new(WebhookState {
        config,
        trigger_tx,
        start_time,
        last_execution,
        rate: tokio::sync::Mutex::new((std::time::Instant::now(), 0)),
    });

    let app = Router::new()
        .route(&path, post(handle_webhook))
        .route("/health", get(handle_health))
        // Cap request bodies — the JSON is parsed and cloned into the agent input.
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state);

    // Bind from config (default 127.0.0.1 = local-only). A non-loopback bind
    // without a secret is rejected at config-validation time.
    let ip: std::net::IpAddr = bind.parse().unwrap_or_else(|_| {
        warn!("[webhook] invalid bind address '{}', falling back to 127.0.0.1", bind);
        std::net::IpAddr::from([127, 0, 0, 1])
    });
    let addr = std::net::SocketAddr::new(ip, port);
    info!("[webhook] Listening on http://{}{}", addr, path);

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
    // Validate secret if configured (constant-time compare to avoid timing leaks)
    if let Some(ref expected) = state.config.triggers.webhook.secret {
        let provided = headers
            .get("X-Webhook-Secret")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !ct_eq(provided.as_bytes(), expected.as_bytes()) {
            warn!("[webhook] Invalid secret");
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    // Rate limit: each accepted trigger is a PAID execution, so bound the rate to
    // protect the payment-key budget from flooding (max_per_minute = 0 disables).
    let max = state.config.triggers.webhook.max_per_minute;
    if max > 0 {
        let mut guard = state.rate.lock().await;
        if guard.0.elapsed().as_secs() >= 60 {
            *guard = (std::time::Instant::now(), 0);
        }
        if guard.1 >= max {
            warn!("[webhook] rate limit exceeded ({}/min)", max);
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        guard.1 += 1;
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

/// Constant-time byte comparison so the secret isn't leaked via response timing.
/// The length check leaks only the secret's length, which is acceptable.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
