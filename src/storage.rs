use crate::config::StorageConfig;
use crate::engine::compaction::CompactionManager;
use crate::engine::memtable::Memtable;
use crate::engine::sstable::{SSTableReader, SSTableWriter};
use crate::engine::wal::WriteAheadLog;
use crate::error::LoraDbError;
use crate::model::device::DeviceRegistry;
use crate::model::frames::Frame;
use crate::model::lorawan::DevEui;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

/// Storage engine that manages WAL, memtable, SSTables, and compaction
pub struct StorageEngine {
    data_dir: PathBuf,
    wal: Arc<RwLock<WriteAheadLog>>,
    memtable: Arc<RwLock<Memtable>>,
    sstables: Arc<RwLock<Vec<SSTableReader>>>,
    compaction_manager: Arc<RwLock<CompactionManager>>,
    device_registry: Arc<DeviceRegistry>,
    config: StorageConfig,
}

impl StorageEngine {
    /// Create a new storage engine
    pub async fn new(config: StorageConfig) -> Result<Self> {
        let data_dir = PathBuf::from(&config.data_dir);

        // Create data directory if it doesn't exist
        tokio::fs::create_dir_all(&data_dir).await?;

        // Set strict permissions on data directory
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))
                .await?;
        }

        // Initialize WAL
        let wal = WriteAheadLog::open(&data_dir, config.wal_sync_interval_ms)?;

        // Replay WAL to recover memtable
        info!("Replaying WAL to recover state...");
        let recovered_frames = wal.replay()?;
        info!("Recovered {} frames from WAL", recovered_frames.len());

        // Initialize memtable and populate with recovered frames
        let memtable = Memtable::new();
        for frame in recovered_frames {
            memtable.insert(frame).map_err(|e| LoraDbError::StorageError(e))?;
        }

        // Initialize compaction manager and open existing SSTables
        let mut compaction_manager =
            CompactionManager::new(data_dir.clone(), config.compaction_threshold);
        let sstables = compaction_manager.open_all_sstables()?;

        info!(
            "Opened {} existing SSTables, next ID: {}",
            sstables.len(),
            compaction_manager.next_sstable_id()
        );

        Ok(Self {
            data_dir,
            wal: Arc::new(RwLock::new(wal)),
            memtable: Arc::new(RwLock::new(memtable)),
            sstables: Arc::new(RwLock::new(sstables)),
            compaction_manager: Arc::new(RwLock::new(compaction_manager)),
            device_registry: Arc::new(DeviceRegistry::new()),
            config,
        })
    }

    /// Write a frame to the storage engine
    pub async fn write(&self, frame: Frame) -> Result<()> {
        // Register device
        self.device_registry.register_or_update(
            frame.dev_eui().clone(),
            match &frame {
                Frame::Uplink(f) => f.device_name.clone(),
                Frame::Downlink(_) => None,
                _ => None,
            },
            frame
                .application_id()
                .map(|id| id.as_str().to_string())
                .unwrap_or_default(),
        );

        // Append to WAL first (for durability)
        {
            let wal = self.wal.read().await;
            wal.append(&frame)?;
        }

        // Insert into memtable
        {
            let memtable = self.memtable.read().await;
            memtable.insert(frame).map_err(|e| LoraDbError::StorageError(e))?;
        }

        // Check if memtable should be flushed
        let should_flush = {
            let memtable = self.memtable.read().await;
            memtable.should_flush(self.config.memtable_size_mb)
        };

        if should_flush {
            debug!("Memtable flush triggered");
            self.flush_memtable().await?;
        }

        Ok(())
    }

    /// Flush memtable to SSTable
    async fn flush_memtable(&self) -> Result<()> {
        info!("Flushing memtable to SSTable");

        // Get next SSTable ID
        let sstable_id = {
            let mut compaction = self.compaction_manager.write().await;
            compaction.allocate_sstable_id()
        };

        // Create new SSTable writer
        let mut writer = SSTableWriter::new(sstable_id, &self.data_dir);

        // Copy all entries from memtable to SSTable
        let entries: Vec<_> = {
            let memtable = self.memtable.read().await;
            memtable.iter().collect()
        };

        for (key, frame) in entries {
            writer.add(key, frame)?;
        }

        let metadata = writer.finish()?;
        info!(
            "Created SSTable {} with {} entries",
            sstable_id, metadata.num_entries
        );

        // Open the new SSTable and add to list
        let sstable_path = self.data_dir.join(format!("sstable-{:08}.sst", sstable_id));
        let reader = SSTableReader::open(sstable_path)?;

        {
            let mut sstables = self.sstables.write().await;
            sstables.push(reader);
        }

        // Clear memtable
        {
            let memtable = self.memtable.write().await;
            memtable.clear();
        }

        // Truncate WAL (frames are now in SSTable)
        {
            let wal = self.wal.read().await;
            wal.truncate()?;
        }

        // Check if compaction should be triggered
        let should_compact = {
            let sstables = self.sstables.read().await;
            let compaction = self.compaction_manager.read().await;
            compaction.should_compact(sstables.len())
        };

        if should_compact {
            info!("Compaction triggered");
            self.compact().await?;
        }

        Ok(())
    }

    /// Compact SSTables
    async fn compact(&self) -> Result<()> {
        info!("Starting compaction");

        // Collect SSTable paths (to reopen them in compaction)
        let sstable_paths: Vec<_> = {
            let sstables = self.sstables.read().await;
            sstables.iter().map(|s| s.path().to_path_buf()).collect()
        };

        // Reopen SSTables for compaction
        let old_sstables: Result<Vec<_>> = sstable_paths
            .into_iter()
            .map(SSTableReader::open)
            .collect();
        let old_sstables = old_sstables?;

        // Perform compaction
        let (new_metadata, old_paths) = {
            let mut compaction = self.compaction_manager.write().await;
            compaction.compact(old_sstables)?
        };

        // Open new SSTable
        let new_sstable_path = self
            .data_dir
            .join(format!("sstable-{:08}.sst", new_metadata.id));
        let new_reader = SSTableReader::open(new_sstable_path)?;

        // Replace SSTables list with just the new one
        {
            let mut sstables = self.sstables.write().await;
            *sstables = vec![new_reader];
        }

        // Delete old SSTables
        {
            let compaction = self.compaction_manager.read().await;
            compaction.delete_old_sstables(old_paths)?;
        }

        info!("Compaction complete");

        Ok(())
    }

    /// Query frames for a device in a time range
    pub async fn query(
        &self,
        dev_eui: &DevEui,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
    ) -> Result<Vec<Frame>> {
        let mut results = Vec::new();

        // Query memtable
        {
            let memtable = self.memtable.read().await;
            let memtable_results = memtable.scan_device_range(dev_eui, start_time, end_time);
            results.extend(memtable_results);
        }

        // Query SSTables
        {
            let sstables = self.sstables.read().await;
            for sstable in sstables.iter() {
                let sstable_results = sstable.scan(dev_eui, start_time, end_time)?;
                results.extend(sstable_results);
            }
        }

        // Sort by timestamp (memtable and SSTables might have different orders)
        results.sort_by_key(|f| f.timestamp());

        debug!(
            "Query returned {} frames for device {}",
            results.len(),
            dev_eui.as_str()
        );

        Ok(results)
    }

    /// Get device registry
    pub fn device_registry(&self) -> &Arc<DeviceRegistry> {
        &self.device_registry
    }

    /// Start background processing of frames from MQTT
    pub async fn start_frame_processor(
        self: Arc<Self>,
        mut frame_rx: mpsc::Receiver<Frame>,
    ) {
        info!("Starting frame processor");

        while let Some(frame) = frame_rx.recv().await {
            let dev_eui = frame.dev_eui().as_str();
            match self.write(frame).await {
                Ok(_) => {
                    info!("Successfully stored frame for device {}", dev_eui);
                }
                Err(e) => {
                    warn!("Failed to write frame for device {}: {}", dev_eui, e);
                }
            }
        }

        warn!("Frame processor stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::frames::UplinkFrame;
    use crate::model::lorawan::*;
    use tempfile::TempDir;

    fn create_test_config(data_dir: &std::path::Path) -> StorageConfig {
        StorageConfig {
            data_dir: data_dir.to_path_buf(),
            wal_sync_interval_ms: 1000,
            memtable_size_mb: 1, // Small for testing
            compaction_threshold: 3,
            enable_encryption: false,
            encryption_key: None,
        }
    }

    fn create_test_frame(dev_eui: &str, timestamp: DateTime<Utc>) -> Frame {
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
    async fn test_storage_engine_write_and_query() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let engine = StorageEngine::new(config).await.unwrap();

        let dev_eui = DevEui::new("0123456789ABCDEF".to_string()).unwrap();
        let now = Utc::now();

        // Write a frame
        let frame = create_test_frame("0123456789ABCDEF", now);
        engine.write(frame).await.unwrap();

        // Query it back
        let results = engine.query(&dev_eui, None, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].dev_eui(), &dev_eui);
    }

    #[tokio::test]
    async fn test_storage_engine_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());

        let dev_eui = DevEui::new("0123456789ABCDEF".to_string()).unwrap();
        let now = Utc::now();

        // Write frames and close
        {
            let engine = StorageEngine::new(config.clone()).await.unwrap();
            for i in 0..3 {
                let frame = create_test_frame(
                    "0123456789ABCDEF",
                    now + chrono::Duration::seconds(i),
                );
                engine.write(frame).await.unwrap();
            }
        }

        // Reopen and verify recovery
        let engine = StorageEngine::new(config).await.unwrap();
        let results = engine.query(&dev_eui, None, None).await.unwrap();
        assert_eq!(results.len(), 3);
    }
}
