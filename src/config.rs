use crate::error::LoraDbError;
use anyhow::{Context, Result};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub mqtt: MqttConfig,
    pub storage: StorageConfig,
    pub api: ApiConfig,
}

#[derive(Debug, Clone)]
pub struct MqttConfig {
    pub chirpstack_broker: Option<String>,
    pub ttn_broker: Option<String>,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls_ca_cert: Option<PathBuf>,
    pub tls_client_cert: Option<PathBuf>,
    pub tls_client_key: Option<PathBuf>,
    pub reconnect_interval_secs: u64,
    pub max_reconnect_interval_secs: u64,
}

#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub data_dir: PathBuf,
    pub wal_sync_interval_ms: u64,
    pub memtable_size_mb: usize,
    pub compaction_threshold: usize,
    pub enable_encryption: bool,
    pub encryption_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub bind_addr: SocketAddr,
    pub enable_tls: bool,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub jwt_secret: String,
    pub rate_limit_per_minute: u32,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        // Load .env file if present (for development)
        dotenvy::dotenv().ok();

        let mqtt = MqttConfig {
            chirpstack_broker: env::var("LORADB_MQTT_CHIRPSTACK_BROKER").ok(),
            ttn_broker: env::var("LORADB_MQTT_TTN_BROKER").ok(),
            client_id: env::var("LORADB_MQTT_CLIENT_ID").unwrap_or_else(|_| {
                format!("loradb-{}", uuid::Uuid::new_v4())
            }),
            username: env::var("LORADB_MQTT_USERNAME").ok(),
            password: env::var("LORADB_MQTT_PASSWORD").ok(),
            tls_ca_cert: env::var("LORADB_MQTT_CA_CERT").ok().map(PathBuf::from),
            tls_client_cert: env::var("LORADB_MQTT_CLIENT_CERT")
                .ok()
                .map(PathBuf::from),
            tls_client_key: env::var("LORADB_MQTT_CLIENT_KEY")
                .ok()
                .map(PathBuf::from),
            reconnect_interval_secs: parse_env(
                "LORADB_MQTT_RECONNECT_INTERVAL_SECS",
                5,
            )?,
            max_reconnect_interval_secs: parse_env(
                "LORADB_MQTT_MAX_RECONNECT_INTERVAL_SECS",
                300,
            )?,
        };

        let storage = StorageConfig {
            data_dir: parse_env_path(
                "LORADB_STORAGE_DATA_DIR",
                "/var/lib/loradb",
            )?,
            wal_sync_interval_ms: parse_env(
                "LORADB_STORAGE_WAL_SYNC_INTERVAL_MS",
                1000,
            )?,
            memtable_size_mb: parse_env("LORADB_STORAGE_MEMTABLE_SIZE_MB", 64)?,
            compaction_threshold: parse_env(
                "LORADB_STORAGE_COMPACTION_THRESHOLD",
                10,
            )?,
            enable_encryption: parse_env(
                "LORADB_STORAGE_ENABLE_ENCRYPTION",
                false,
            )?,
            encryption_key: env::var("LORADB_STORAGE_ENCRYPTION_KEY").ok(),
        };

        // Validate encryption configuration
        if storage.enable_encryption && storage.encryption_key.is_none() {
            return Err(LoraDbError::ConfigError(
                "Encryption enabled but LORADB_STORAGE_ENCRYPTION_KEY not set"
                    .to_string(),
            )
            .into());
        }

        let enable_tls = parse_env("LORADB_API_ENABLE_TLS", false)?;

        let api = ApiConfig {
            bind_addr: parse_env(
                "LORADB_API_BIND_ADDR",
                "0.0.0.0:8080".parse().context("Invalid default bind address")?,
            )?,
            enable_tls,
            tls_cert: if enable_tls {
                Some(parse_env_path_required("LORADB_API_TLS_CERT")?)
            } else {
                env::var("LORADB_API_TLS_CERT").ok().map(PathBuf::from)
            },
            tls_key: if enable_tls {
                Some(parse_env_path_required("LORADB_API_TLS_KEY")?)
            } else {
                env::var("LORADB_API_TLS_KEY").ok().map(PathBuf::from)
            },
            jwt_secret: env::var("LORADB_API_JWT_SECRET").context(
                "LORADB_API_JWT_SECRET must be set",
            )?,
            rate_limit_per_minute: parse_env(
                "LORADB_API_RATE_LIMIT_PER_MINUTE",
                60,
            )?,
        };

        // Validate JWT secret length
        if api.jwt_secret.len() < 32 {
            return Err(LoraDbError::ConfigError(
                "JWT secret must be at least 32 characters".to_string(),
            )
            .into());
        }

        Ok(Config {
            mqtt,
            storage,
            api,
        })
    }

    pub fn validate(&self) -> Result<()> {
        // Ensure at least one MQTT broker is configured
        if self.mqtt.chirpstack_broker.is_none()
            && self.mqtt.ttn_broker.is_none()
        {
            return Err(LoraDbError::ConfigError(
                "At least one MQTT broker must be configured".to_string(),
            )
            .into());
        }

        // Validate TLS certificate paths exist if TLS is enabled
        if self.api.enable_tls {
            if let Some(ref cert) = self.api.tls_cert {
                if !cert.exists() {
                    return Err(LoraDbError::ConfigError(format!(
                        "API TLS certificate not found: {:?}",
                        cert
                    ))
                    .into());
                }
            } else {
                return Err(LoraDbError::ConfigError(
                    "TLS enabled but LORADB_API_TLS_CERT not set".to_string(),
                )
                .into());
            }

            if let Some(ref key) = self.api.tls_key {
                if !key.exists() {
                    return Err(LoraDbError::ConfigError(format!(
                        "API TLS key not found: {:?}",
                        key
                    ))
                    .into());
                }
            } else {
                return Err(LoraDbError::ConfigError(
                    "TLS enabled but LORADB_API_TLS_KEY not set".to_string(),
                )
                .into());
            }
        }

        // Validate MQTT CA cert if provided
        if let Some(ref ca_cert) = self.mqtt.tls_ca_cert {
            if !ca_cert.exists() {
                return Err(LoraDbError::ConfigError(format!(
                    "MQTT CA certificate not found: {:?}",
                    ca_cert
                ))
                .into());
            }
        }

        // Validate encryption key if encryption is enabled
        if self.storage.enable_encryption {
            if let Some(ref key) = self.storage.encryption_key {
                // Try to decode base64 key
                base64::decode(key).map_err(|e| {
                    LoraDbError::ConfigError(format!(
                        "Invalid base64 encryption key: {}",
                        e
                    ))
                })?;
            }
        }

        Ok(())
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    env::var(key)
        .ok()
        .map(|s| {
            s.parse().map_err(|e| {
                anyhow::anyhow!("Failed to parse {}: {}", key, e)
            })
        })
        .transpose()
        .map(|opt| opt.unwrap_or(default))
}

fn parse_env_path(key: &str, default: &str) -> Result<PathBuf> {
    Ok(env::var(key).unwrap_or_else(|_| default.to_string()).into())
}

fn parse_env_path_required(key: &str) -> Result<PathBuf> {
    env::var(key)
        .context(format!("{} must be set", key))
        .map(PathBuf::from)
}
