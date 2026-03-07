use std::time::Instant;

/// Returns true if the interval trigger is due.
pub fn is_due(last_trigger: Option<Instant>, interval_secs: u64) -> bool {
    match last_trigger {
        Some(t) => t.elapsed().as_secs() >= interval_secs,
        None => true,
    }
}
