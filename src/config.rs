use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub agent: AgentConfig,
    pub triggers: TriggersConfig,
    #[serde(default)]
    pub input: InputConfig,
    #[serde(default)]
    pub resources: ResourcesConfig,
    #[serde(default)]
    pub alerts: AlertsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub project_owner: String,
    pub project_name: String,
    #[serde(default = "default_coordinator_url")]
    pub coordinator_url: String,
    pub payment_key: String,
    pub secrets_profile: Option<String>,
    pub secrets_account_id: Option<String>,
}

fn default_coordinator_url() -> String {
    "https://api.outlayer.fastnear.com".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct TriggersConfig {
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default)]
    pub storage_diff: StorageDiffConfig,
    #[serde(default)]
    pub webhook: WebhookConfig,
}

fn default_interval() -> u64 {
    60
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageDiffConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub project_uuid: String,
    #[serde(default = "default_threshold")]
    pub threshold_percent: f64,
}

fn default_threshold() -> f64 {
    1.0
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WebhookConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_webhook_port")]
    pub port: u16,
    #[serde(default = "default_webhook_path")]
    pub path: String,
    pub secret: Option<String>,
}

fn default_webhook_port() -> u16 {
    9090
}

fn default_webhook_path() -> String {
    "/trigger".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InputConfig {
    #[serde(default, rename = "static")]
    pub static_input: Option<toml::Value>,
    #[serde(default)]
    pub include_trigger_reason: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourcesConfig {
    #[serde(default = "default_max_instructions")]
    pub max_instructions: u64,
    #[serde(default = "default_max_memory")]
    pub max_memory_mb: u32,
    #[serde(default = "default_max_exec_secs")]
    pub max_execution_seconds: u64,
    #[serde(default = "default_compute_limit")]
    pub compute_limit: u64,
    #[serde(default)]
    pub attached_deposit: u64,
}

fn default_max_instructions() -> u64 {
    1_000_000_000
}
fn default_max_memory() -> u32 {
    128
}
fn default_max_exec_secs() -> u64 {
    60
}
fn default_compute_limit() -> u64 {
    10000
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        Self {
            max_instructions: default_max_instructions(),
            max_memory_mb: default_max_memory(),
            max_execution_seconds: default_max_exec_secs(),
            compute_limit: default_compute_limit(),
            attached_deposit: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AlertsConfig {
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    #[serde(default = "default_alert_cooldown")]
    pub alert_cooldown_secs: u64,
}

fn default_failure_threshold() -> u32 {
    3
}
fn default_alert_cooldown() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

/// Load and parse config from a TOML file with ${ENV_VAR} expansion.
pub fn load(path: &str) -> Result<Config> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read config: {}", path))?;

    let expanded = expand_env_vars(&raw);

    let config: Config =
        toml::from_str(&expanded).with_context(|| format!("Failed to parse config: {}", path))?;

    validate(&config)?;

    Ok(config)
}

/// Replace ${VAR_NAME} with environment variable values.
/// Unset variables are left as-is (will cause parse error if in required field).
fn expand_env_vars(input: &str) -> String {
    let re = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    re.replace_all(input, |caps: &regex::Captures| {
        let var_name = &caps[1];
        std::env::var(var_name).unwrap_or_else(|_| caps[0].to_string())
    })
    .to_string()
}

fn validate(config: &Config) -> Result<()> {
    anyhow::ensure!(
        !config.agent.project_owner.is_empty(),
        "agent.project_owner is required"
    );
    anyhow::ensure!(
        !config.agent.project_name.is_empty(),
        "agent.project_name is required"
    );
    anyhow::ensure!(
        !config.agent.payment_key.is_empty() && !config.agent.payment_key.contains("${"),
        "agent.payment_key is required (set the env var if using ${{...}} syntax)"
    );
    anyhow::ensure!(
        config.triggers.interval_secs > 0,
        "triggers.interval_secs must be > 0"
    );

    if config.triggers.storage_diff.enabled {
        anyhow::ensure!(
            !config.triggers.storage_diff.keys.is_empty(),
            "triggers.storage_diff.keys must be non-empty when enabled"
        );
        anyhow::ensure!(
            !config.triggers.storage_diff.project_uuid.is_empty(),
            "triggers.storage_diff.project_uuid is required when enabled"
        );
    }

    Ok(())
}

/// Convert a toml::Value to serde_json::Value.
pub fn toml_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::json!(b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(tbl) => {
            let map: serde_json::Map<String, serde_json::Value> =
                tbl.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}
