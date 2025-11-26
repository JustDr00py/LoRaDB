use crate::error::LoraDbError;
use crate::query::dsl::QueryResult;
use crate::query::executor::QueryExecutor;
use crate::query::parser::QueryParser;
use crate::security::jwt::Claims;
use crate::storage::StorageEngine;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    Extension,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<StorageEngine>,
    pub query_executor: Arc<QueryExecutor>,
    pub query_parser: Arc<QueryParser>,
}

/// Query request body
#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub query: String,
}

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Device list response
#[derive(Debug, Serialize)]
pub struct DeviceListResponse {
    pub total_devices: usize,
    pub devices: Vec<DeviceInfo>,
}

/// Device information
#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub dev_eui: String,
    pub device_name: Option<String>,
    pub application_id: String,
    pub last_seen: Option<String>,
}

/// Error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

impl IntoResponse for LoraDbError {
    fn into_response(self) -> Response {
        let (status, error_type) = match self {
            LoraDbError::QueryParseError(_) => (StatusCode::BAD_REQUEST, "QueryParseError"),
            LoraDbError::QueryExecutionError(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "QueryExecutionError")
            }
            LoraDbError::AuthError(_) => (StatusCode::UNAUTHORIZED, "AuthError"),
            LoraDbError::InvalidDevEui(_) => (StatusCode::BAD_REQUEST, "InvalidDevEui"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "InternalError"),
        };

        let body = Json(ErrorResponse {
            error: error_type.to_string(),
            message: self.to_string(),
        });

        (status, body).into_response()
    }
}

/// Health check endpoint
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Execute a query
pub async fn execute_query(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<QueryResult>, LoraDbError> {
    tracing::info!(
        user = claims.sub,
        query = request.query,
        "Executing query"
    );

    // Parse query
    let query = state
        .query_parser
        .parse(&request.query)
        .map_err(|e| LoraDbError::QueryParseError(e.to_string()))?;

    // Execute query
    let result = state
        .query_executor
        .execute(&query)
        .await
        .map_err(|e| LoraDbError::QueryExecutionError(e.to_string()))?;

    Ok(Json(result))
}

/// List all devices
pub async fn list_devices(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Json<DeviceListResponse> {
    let registry = state.storage.device_registry();
    let devices: Vec<DeviceInfo> = registry
        .list_devices()
        .into_iter()
        .map(|device| DeviceInfo {
            dev_eui: device.dev_eui.as_str().to_string(),
            device_name: device.device_name,
            application_id: device.application_id,
            last_seen: device.last_seen.map(|dt| dt.to_rfc3339()),
        })
        .collect();

    Json(DeviceListResponse {
        total_devices: devices.len(),
        devices,
    })
}

/// Get device information
pub async fn get_device(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Path(dev_eui): Path<String>,
) -> Result<Json<DeviceInfo>, LoraDbError> {
    let registry = state.storage.device_registry();

    if let Some(device) = registry.get_device(&dev_eui) {
        Ok(Json(DeviceInfo {
            dev_eui: device.dev_eui.as_str().to_string(),
            device_name: device.device_name,
            application_id: device.application_id,
            last_seen: device.last_seen.map(|dt| dt.to_rfc3339()),
        }))
    } else {
        Err(LoraDbError::InvalidDevEui(format!(
            "Device {} not found",
            dev_eui
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageConfig;
    use crate::model::frames::UplinkFrame;
    use crate::model::lorawan::*;
    use chrono::Utc;
    use tempfile::TempDir;

    async fn create_test_state() -> AppState {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig {
            data_dir: temp_dir.path().to_path_buf(),
            wal_sync_interval_ms: 1000,
            memtable_size_mb: 1,
            compaction_threshold: 3,
            enable_encryption: false,
            encryption_key: None,
        };

        let storage = Arc::new(StorageEngine::new(config).await.unwrap());
        let query_executor = Arc::new(QueryExecutor::new(storage.clone()));
        let query_parser = Arc::new(QueryParser::new());

        AppState {
            storage,
            query_executor,
            query_parser,
        }
    }

    fn create_test_uplink(dev_eui: &str) -> crate::model::frames::Frame {
        crate::model::frames::Frame::Uplink(UplinkFrame {
            dev_eui: DevEui::new(dev_eui.to_string()).unwrap(),
            application_id: ApplicationId::new("test-app".to_string()),
            device_name: Some("test-device".to_string()),
            received_at: Utc::now(),
            f_port: 1,
            f_cnt: 42,
            confirmed: false,
            adr: true,
            dr: DataRate::new_lora(125000, 7),
            frequency: 868100000,
            rx_info: vec![],
            decoded_payload: None,
            raw_payload: Some("aGVsbG8=".to_string()),
        })
    }

    #[tokio::test]
    async fn test_health_check() {
        let response = health_check().await;
        assert_eq!(response.0.status, "ok");
    }

    #[tokio::test]
    async fn test_execute_query() {
        let state = create_test_state().await;
        let claims = Claims::new("test-user".to_string());

        // Write a test frame
        let dev_eui = "0123456789ABCDEF";
        let frame = create_test_uplink(dev_eui);
        state.storage.write(frame).await.unwrap();

        // Execute query
        let request = QueryRequest {
            query: format!("SELECT * FROM device '{}'", dev_eui),
        };

        let result = execute_query(
            State(state),
            Extension(claims),
            Json(request),
        )
        .await
        .unwrap();

        assert_eq!(result.0.total_frames, 1);
    }

    #[tokio::test]
    async fn test_list_devices() {
        let state = create_test_state().await;
        let claims = Claims::new("test-user".to_string());

        // Write test frames for different devices
        for i in 0..3 {
            let dev_eui = format!("012345678{:07X}", i);
            let frame = create_test_uplink(&dev_eui);
            state.storage.write(frame).await.unwrap();
        }

        let response = list_devices(State(state), Extension(claims)).await;
        assert_eq!(response.0.total_devices, 3);
    }
}
