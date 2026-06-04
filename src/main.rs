mod api;
mod config;
mod executor;
mod telegram;
mod triggers;

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "outlayer-scheduler", version, about = "Generic scheduler for OutLayer agents")]
struct Cli {
    /// Path to scheduler.toml config file
    #[arg(short, long, default_value = "scheduler.toml")]
    config: String,
}

#[derive(Debug, Clone)]
pub enum TriggerReason {
    Interval,
    StorageDiff { changed_keys: Vec<String> },
    Webhook { data: serde_json::Value },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let config = config::load(&cli.config)?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("outlayer_scheduler={}", config.logging.level)
                    .parse()
                    .unwrap()
            }),
        )
        .init();

    info!("Starting outlayer-scheduler v{}", env!("CARGO_PKG_VERSION"));
    info!("Project: {}/{}", config.agent.project_owner, config.agent.project_name);
    info!("Coordinator: {}", config.agent.coordinator_url);
    info!(
        "Triggers: interval={}s, storage_diff={}, webhook={}",
        config.triggers.interval_secs,
        if config.triggers.storage_diff.enabled {
            format!("enabled ({} keys)", config.triggers.storage_diff.keys.len())
        } else {
            "disabled".to_string()
        },
        if config.triggers.webhook.enabled {
            format!("enabled (port {})", config.triggers.webhook.port)
        } else {
            "disabled".to_string()
        },
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        // Never follow redirects: requests carry the payment key in an
        // X-Payment-Key header, and reqwest does NOT strip custom headers on a
        // cross-host redirect — a malicious/compromised coordinator_url could
        // 3xx the call elsewhere and harvest the key. The API needs no redirects.
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let config = Arc::new(config);
    let last_execution: Arc<tokio::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let start_time = Instant::now();

    let (trigger_tx, mut trigger_rx) = tokio::sync::mpsc::channel::<TriggerReason>(32);

    // Spawn webhook server if enabled
    if config.triggers.webhook.enabled {
        let cfg = config.clone();
        let tx = trigger_tx.clone();
        let last = last_execution.clone();
        tokio::spawn(async move {
            triggers::webhook::start(cfg, tx, start_time, last).await;
        });
    }

    // Spawn storage-diff poller if enabled
    if config.triggers.storage_diff.enabled {
        let cfg = config.clone();
        let tx = trigger_tx.clone();
        let cl = client.clone();
        tokio::spawn(async move {
            triggers::storage::start(cl, cfg, tx).await;
        });
    }

    // Main loop state
    let mut last_interval_trigger: Option<Instant> = None;
    let mut consecutive_failures: u32 = 0;
    let mut alert_throttle = telegram::AlertThrottle::new(Duration::from_secs(
        config.alerts.alert_cooldown_secs,
    ));
    let executing = Arc::new(tokio::sync::Mutex::new(()));

    let mut tick = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = tick.tick() => {
                if triggers::interval::is_due(last_interval_trigger, config.triggers.interval_secs) {
                    let _guard = executing.lock().await;
                    last_interval_trigger = Some(Instant::now());

                    match executor::execute(&client, &config, &TriggerReason::Interval, &last_execution).await {
                        Ok(_) => {
                            consecutive_failures = 0;
                        }
                        Err(e) => {
                            error!("[interval] Execution failed: {}", e);
                            consecutive_failures += 1;
                            if consecutive_failures >= config.alerts.failure_threshold {
                                alert_throttle.send(
                                    &client,
                                    config.alerts.telegram_bot_token.as_deref(),
                                    config.alerts.telegram_chat_id.as_deref(),
                                    "Scheduler Error",
                                    &format!(
                                        "Project: {}/{}\n{} consecutive failures\nLast error: {}",
                                        config.agent.project_owner, config.agent.project_name,
                                        consecutive_failures, e
                                    ),
                                ).await;
                            }
                        }
                    }
                }
            }
            Some(trigger) = trigger_rx.recv() => {
                let _guard = executing.lock().await;

                match executor::execute(&client, &config, &trigger, &last_execution).await {
                    Ok(_) => {
                        consecutive_failures = 0;
                    }
                    Err(e) => {
                        let trigger_name = match &trigger {
                            TriggerReason::StorageDiff { .. } => "storage_diff",
                            TriggerReason::Webhook { .. } => "webhook",
                            TriggerReason::Interval => "interval",
                        };
                        error!("[{}] Execution failed: {}", trigger_name, e);
                        consecutive_failures += 1;
                        if consecutive_failures >= config.alerts.failure_threshold {
                            alert_throttle.send(
                                &client,
                                config.alerts.telegram_bot_token.as_deref(),
                                config.alerts.telegram_chat_id.as_deref(),
                                "Scheduler Error",
                                &format!(
                                    "Project: {}/{}\n{} consecutive failures\nLast error: {}",
                                    config.agent.project_owner, config.agent.project_name,
                                    consecutive_failures, e
                                ),
                            ).await;
                        }
                    }
                }
            }
            _ = shutdown_signal() => {
                info!("Shutting down...");
                break;
            }
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}
