use crate::error::LoraDbError;
use crate::model::frames::Frame;
use crate::model::lorawan::DevEui;
use crate::query::dsl::{Query, QueryResult, SelectClause};
use crate::storage::StorageEngine;
use anyhow::Result;
use std::sync::Arc;

/// Query executor that runs queries against the storage engine
pub struct QueryExecutor {
    storage: Arc<StorageEngine>,
}

impl QueryExecutor {
    pub fn new(storage: Arc<StorageEngine>) -> Self {
        Self { storage }
    }

    /// Execute a query and return results
    pub async fn execute(&self, query: &Query) -> Result<QueryResult> {
        // Parse DevEUI
        let dev_eui = DevEui::new(query.from.dev_eui.clone())
            .map_err(|e| LoraDbError::QueryExecutionError(e.to_string()))?;

        // Get time range
        let (start_time, end_time) = query.time_range();

        // Query storage engine
        let mut frames = self
            .storage
            .query(&dev_eui, start_time, end_time)
            .await?;

        // Apply SELECT clause filtering
        frames = self.filter_frames(frames, &query.select);

        // Convert frames to JSON
        let json_frames: Vec<serde_json::Value> = frames
            .iter()
            .map(|frame| {
                // Serialize frame to JSON
                let json = serde_json::to_value(frame).unwrap_or(serde_json::json!({}));

                // Apply field projection if needed
                self.project_fields(json, &query.select)
            })
            .collect();

        Ok(QueryResult {
            dev_eui: query.from.dev_eui.clone(),
            total_frames: json_frames.len(),
            frames: json_frames,
        })
    }

    /// Filter frames based on SELECT clause
    fn filter_frames(&self, frames: Vec<Frame>, select: &SelectClause) -> Vec<Frame> {
        match select {
            SelectClause::All => frames,
            SelectClause::Uplink => frames
                .into_iter()
                .filter(|f| matches!(f, Frame::Uplink(_)))
                .collect(),
            SelectClause::Downlink => frames
                .into_iter()
                .filter(|f| matches!(f, Frame::Downlink(_)))
                .collect(),
            SelectClause::Join => frames
                .into_iter()
                .filter(|f| matches!(f, Frame::JoinRequest(_) | Frame::JoinAccept(_)))
                .collect(),
            SelectClause::Fields(_) => frames, // Field projection happens later
        }
    }

    /// Project specific fields from JSON
    fn project_fields(&self, mut json: serde_json::Value, select: &SelectClause) -> serde_json::Value {
        if let SelectClause::Fields(fields) = select {
            if let serde_json::Value::Object(ref mut map) = json {
                // Keep only requested fields
                map.retain(|key, _| fields.contains(&key.to_string()));
            }
        }
        json
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageConfig;
    use crate::model::frames::UplinkFrame;
    use crate::model::lorawan::*;
    use crate::query::dsl::{FilterClause, FromClause};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    fn create_test_config(data_dir: &std::path::Path) -> StorageConfig {
        StorageConfig {
            data_dir: data_dir.to_path_buf(),
            wal_sync_interval_ms: 1000,
            memtable_size_mb: 1,
            compaction_threshold: 3,
            enable_encryption: false,
            encryption_key: None,
        }
    }

    fn create_test_uplink(dev_eui: &str, timestamp: chrono::DateTime<Utc>) -> Frame {
        Frame::Uplink(UplinkFrame {
            dev_eui: DevEui::new(dev_eui.to_string()).unwrap(),
            application_id: ApplicationId::new("test-app".to_string()),
            device_name: Some("test-device".to_string()),
            received_at: timestamp,
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
    async fn test_execute_query_all() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let storage = Arc::new(StorageEngine::new(config).await.unwrap());
        let executor = QueryExecutor::new(storage.clone());

        // Write some test frames
        let dev_eui_str = "0123456789ABCDEF";
        let now = Utc::now();
        for i in 0..3 {
            let frame = create_test_uplink(dev_eui_str, now + Duration::seconds(i));
            storage.write(frame).await.unwrap();
        }

        // Execute query
        let query = Query::new(
            SelectClause::All,
            FromClause {
                dev_eui: dev_eui_str.to_string(),
            },
            None,
        );

        let result = executor.execute(&query).await.unwrap();
        assert_eq!(result.total_frames, 3);
        assert_eq!(result.dev_eui, dev_eui_str);
    }

    #[tokio::test]
    async fn test_execute_query_with_time_filter() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let storage = Arc::new(StorageEngine::new(config).await.unwrap());
        let executor = QueryExecutor::new(storage.clone());

        // Write frames at different times
        let dev_eui_str = "0123456789ABCDEF";
        let base_time = Utc::now() - Duration::hours(2);

        for i in 0..5 {
            let frame = create_test_uplink(dev_eui_str, base_time + Duration::minutes(i * 30));
            storage.write(frame).await.unwrap();
        }

        // Query for last 1 hour (should get 2-3 frames)
        let query = Query::new(
            SelectClause::All,
            FromClause {
                dev_eui: dev_eui_str.to_string(),
            },
            Some(FilterClause::Last(Duration::hours(1))),
        );

        let result = executor.execute(&query).await.unwrap();
        assert!(result.total_frames >= 2);
    }

    #[tokio::test]
    async fn test_execute_query_uplink_only() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let storage = Arc::new(StorageEngine::new(config).await.unwrap());
        let executor = QueryExecutor::new(storage.clone());

        // Write uplink frames
        let dev_eui_str = "0123456789ABCDEF";
        let now = Utc::now();
        for i in 0..3 {
            let frame = create_test_uplink(dev_eui_str, now + Duration::seconds(i));
            storage.write(frame).await.unwrap();
        }

        // Execute query for uplink only
        let query = Query::new(
            SelectClause::Uplink,
            FromClause {
                dev_eui: dev_eui_str.to_string(),
            },
            None,
        );

        let result = executor.execute(&query).await.unwrap();
        assert_eq!(result.total_frames, 3);
    }

    #[tokio::test]
    async fn test_execute_query_nonexistent_device() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let storage = Arc::new(StorageEngine::new(config).await.unwrap());
        let executor = QueryExecutor::new(storage);

        // Query for device that doesn't exist
        let query = Query::new(
            SelectClause::All,
            FromClause {
                dev_eui: "FEDCBA9876543210".to_string(),
            },
            None,
        );

        let result = executor.execute(&query).await.unwrap();
        assert_eq!(result.total_frames, 0);
    }
}
