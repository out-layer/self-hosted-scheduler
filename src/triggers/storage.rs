//! Storage-diff trigger.
//! Periodically reads public storage keys and triggers when values change.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{api, config::Config, TriggerReason};

type StorageCache = HashMap<String, Vec<u8>>;

/// Start the storage-diff polling task. Runs until the channel is closed.
pub async fn start(
    client: reqwest::Client,
    config: Arc<Config>,
    trigger_tx: mpsc::Sender<TriggerReason>,
) {
    let diff = &config.triggers.storage_diff;
    if !diff.enabled || diff.keys.is_empty() {
        return;
    }

    info!(
        "[storage_diff] Monitoring {} keys (threshold={}%)",
        diff.keys.len(),
        diff.threshold_percent,
    );

    let mut cache: StorageCache = HashMap::new();
    let poll_interval = Duration::from_secs(config.triggers.interval_secs);

    loop {
        tokio::time::sleep(poll_interval).await;

        match check_for_changes(&client, &config, &mut cache).await {
            Ok(changed_keys) => {
                if !changed_keys.is_empty() {
                    info!(
                        "[storage_diff] Changes detected in {} keys: {:?}",
                        changed_keys.len(),
                        changed_keys,
                    );
                    if trigger_tx
                        .send(TriggerReason::StorageDiff { changed_keys })
                        .await
                        .is_err()
                    {
                        warn!("[storage_diff] Channel closed, stopping");
                        return;
                    }
                }
            }
            Err(e) => {
                warn!("[storage_diff] Failed to check storage: {}", e);
            }
        }
    }
}

async fn check_for_changes(
    client: &reqwest::Client,
    config: &Config,
    cache: &mut StorageCache,
) -> anyhow::Result<Vec<String>> {
    let diff = &config.triggers.storage_diff;
    let current = api::read_storage_batch(
        client,
        &config.agent.coordinator_url,
        &diff.project_uuid,
        &diff.keys,
    )
    .await?;

    let mut changed = Vec::new();

    for key in &diff.keys {
        let new_value = current.get(key).and_then(|v| v.as_ref());
        let old_value = cache.get(key);

        match (old_value, new_value) {
            (None, Some(new_bytes)) => {
                // First read: populate cache without triggering
                debug!("[storage_diff] Initial cache for key: {}", key);
                cache.insert(key.clone(), new_bytes.clone());
            }
            (Some(old_bytes), Some(new_bytes)) => {
                if old_bytes != new_bytes {
                    if exceeds_threshold(old_bytes, new_bytes, diff.threshold_percent) {
                        changed.push(key.clone());
                    }
                    cache.insert(key.clone(), new_bytes.clone());
                }
            }
            (Some(_), None) => {
                // Key was deleted
                changed.push(key.clone());
                cache.remove(key);
            }
            (None, None) => {}
        }
    }

    Ok(changed)
}

/// For numeric values, check if the difference exceeds threshold_percent.
/// Non-numeric values always trigger on any change.
fn exceeds_threshold(old: &[u8], new: &[u8], threshold_percent: f64) -> bool {
    let old_num = try_parse_numeric(old);
    let new_num = try_parse_numeric(new);

    match (old_num, new_num) {
        (Some(o), Some(n)) => {
            if o == 0.0 {
                return n != 0.0;
            }
            let diff_pct = ((n - o) / o).abs() * 100.0;
            diff_pct > threshold_percent
        }
        _ => true,
    }
}

fn try_parse_numeric(bytes: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(bytes).ok()?;
    if let Ok(n) = s.trim().parse::<f64>() {
        return Some(n);
    }
    // Try JSON: direct number or object with "price" field
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(n) = val.as_f64() {
            return Some(n);
        }
        if let Some(n) = val.get("price").and_then(|v| v.as_f64()) {
            return Some(n);
        }
    }
    None
}
