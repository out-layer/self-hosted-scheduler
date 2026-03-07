//! Agent execution orchestrator.
//! Builds input JSON from config + trigger metadata, calls the API.

use anyhow::Result;
use tracing::{debug, info};

use crate::{api, config::Config, TriggerReason};

/// Execute the agent with the given trigger reason.
pub async fn execute(
    client: &reqwest::Client,
    config: &Config,
    trigger: &TriggerReason,
    last_execution: &std::sync::Arc<tokio::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
) -> Result<()> {
    let trigger_name = match trigger {
        TriggerReason::Interval => "interval",
        TriggerReason::StorageDiff { .. } => "storage_diff",
        TriggerReason::Webhook { .. } => "webhook",
    };

    info!("[{}] Triggering execution", trigger_name);

    let input = build_input(config, trigger);
    debug!("[{}] Input: {}", trigger_name, input);

    let resp = api::call_agent(client, config, input).await?;

    {
        let mut last = last_execution.lock().await;
        *last = Some(chrono::Utc::now());
    }

    info!(
        "[{}] Execution completed: status={}, cost={} micro-units, time={}ms",
        trigger_name,
        resp.status.as_deref().unwrap_or("unknown"),
        resp.compute_cost.as_deref().unwrap_or("?"),
        resp.time_ms.unwrap_or(0),
    );

    if let Some(ref output) = resp.output {
        debug!("[{}] Output: {}", trigger_name, output);
    }

    Ok(())
}

fn build_input(config: &Config, trigger: &TriggerReason) -> serde_json::Value {
    let mut input = match &config.input.static_input {
        Some(toml_val) => crate::config::toml_to_json(toml_val),
        None => serde_json::json!({}),
    };

    if config.input.include_trigger_reason {
        match trigger {
            TriggerReason::Interval => {
                input["trigger"] = serde_json::json!("interval");
            }
            TriggerReason::StorageDiff { changed_keys } => {
                input["trigger"] = serde_json::json!("storage_diff");
                input["changed_keys"] = serde_json::json!(changed_keys);
            }
            TriggerReason::Webhook { data } => {
                input["trigger"] = serde_json::json!("webhook");
                input["webhook_data"] = data.clone();
            }
        }
    } else if let TriggerReason::Webhook { data } = trigger {
        // Always include webhook_data even without include_trigger_reason
        input["webhook_data"] = data.clone();
    }

    input
}
