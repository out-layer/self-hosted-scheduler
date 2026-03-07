//! OutLayer API client for agent execution and public storage reads.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Deserialize)]
pub struct BatchStorageResponse {
    pub results: HashMap<String, BatchStorageItem>,
}

#[derive(Debug, Deserialize)]
pub struct BatchStorageItem {
    pub exists: bool,
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct WasiCallResponse {
    pub call_id: Option<String>,
    pub status: Option<String>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub compute_cost: Option<String>,
    pub instructions: Option<u64>,
    pub time_ms: Option<u64>,
    pub attestation_url: Option<String>,
}

/// Read public storage keys via batch API.
/// No authentication required.
pub async fn read_storage_batch(
    client: &reqwest::Client,
    coordinator_url: &str,
    project_uuid: &str,
    keys: &[String],
) -> Result<HashMap<String, Option<Vec<u8>>>> {
    let url = format!("{}/public/storage/batch", coordinator_url);
    let body = serde_json::json!({
        "project_uuid": project_uuid,
        "keys": keys,
    });

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to reach OutLayer API for storage read")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Storage read failed: HTTP {}: {}", status, text);
    }

    let batch_resp: BatchStorageResponse =
        resp.json().await.context("Failed to parse storage response")?;

    let mut result = HashMap::new();
    for (key, item) in batch_resp.results {
        if item.exists {
            if let Some(b64) = item.value {
                match BASE64.decode(&b64) {
                    Ok(bytes) => {
                        result.insert(key, Some(bytes));
                    }
                    Err(e) => {
                        tracing::warn!("Failed to decode base64 for key {}: {}", key, e);
                        result.insert(key, None);
                    }
                }
            } else {
                result.insert(key, None);
            }
        } else {
            result.insert(key, None);
        }
    }

    Ok(result)
}

/// Execute agent via HTTPS API.
pub async fn call_agent(
    client: &reqwest::Client,
    config: &crate::config::Config,
    input: serde_json::Value,
) -> Result<WasiCallResponse> {
    let url = format!(
        "{}/call/{}/{}",
        config.agent.coordinator_url, config.agent.project_owner, config.agent.project_name,
    );

    let mut body = serde_json::json!({
        "input": input,
        "async": false,
        "resource_limits": {
            "max_instructions": config.resources.max_instructions,
            "max_memory_mb": config.resources.max_memory_mb,
            "max_execution_seconds": config.resources.max_execution_seconds,
        },
    });

    if let (Some(profile), Some(account_id)) = (
        &config.agent.secrets_profile,
        &config.agent.secrets_account_id,
    ) {
        body["secrets_ref"] = serde_json::json!({
            "profile": profile,
            "account_id": account_id,
        });
    }

    debug!("Calling agent: POST {}", url);

    let mut req = client
        .post(&url)
        .header("X-Payment-Key", &config.agent.payment_key)
        .header("Content-Type", "application/json");

    if config.resources.compute_limit > 0 {
        req = req.header("X-Compute-Limit", config.resources.compute_limit.to_string());
    }
    if config.resources.attached_deposit > 0 {
        req = req.header(
            "X-Attached-Deposit",
            config.resources.attached_deposit.to_string(),
        );
    }

    let resp = req
        .json(&body)
        .send()
        .await
        .context("Failed to reach OutLayer API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Agent execution failed: HTTP {}: {}", status, text);
    }

    let wasi_resp: WasiCallResponse = resp
        .json()
        .await
        .context("Failed to parse execution response")?;

    if let Some(ref error) = wasi_resp.error {
        anyhow::bail!("WASI error: {}", error);
    }

    if wasi_resp.status.as_deref() == Some("failed") {
        anyhow::bail!("WASI execution status: failed");
    }

    Ok(wasi_resp)
}
