use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_service_name")]
    pub service_name: String,

    #[serde(default = "default_http_port")]
    pub http_port: u16,

    pub redpanda_brokers: String,

    pub cache_url: String,

    /// Base64 Ed25519 public key of the Go auth service. Handshake tokens are
    /// verified against it; when it is empty the gateway refuses every upgrade
    /// (503) instead of trusting an unverified user id.
    #[serde(default)]
    pub auth_ed25519_public_key: String,

    #[serde(default = "default_max_connections")]
    pub max_connections: u64,

    #[serde(default = "default_channel_buffer")]
    pub channel_buffer_size: usize,

    #[serde(default = "default_max_subscriptions")]
    pub max_subscriptions_per_connection: usize,
}

impl AppConfig {
    pub fn load() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        let config = config::Config::builder()
            .add_source(config::Environment::with_prefix("APP").separator("__"))
            .build()?;

        let app_config: Self = config.try_deserialize()?;
        Ok(app_config)
    }
}

fn default_service_name() -> String {
    "websocket-gateway".into()
}
fn default_http_port() -> u16 {
    8080
}
fn default_max_connections() -> u64 {
    100_000
}
fn default_channel_buffer() -> usize {
    256
}
fn default_max_subscriptions() -> usize {
    50
}
