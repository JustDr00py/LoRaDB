use crate::engine::memtable::MemtableKey;
use crate::error::LoraDbError;
use crate::model::frames::Frame;
use crate::model::lorawan::DevEui;
use crate::util::bloom::BloomFilter;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crc32fast::Hasher;
use lz4::{Decoder, EncoderBuilder};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

const SSTABLE_MAGIC: u32 = 0x5353544C; // "SSTL"
const SSTABLE_VERSION: u16 = 2; // v2: Fixed bincode compatibility for Frame

/// SSTable metadata
#[derive(Debug, Clone)]
pub struct SSTableMetadata {
    pub id: u64,
    pub created_at: DateTime<Utc>,
    pub num_entries: u64,
    pub min_key: MemtableKey,
    pub max_key: MemtableKey,
    pub bloom_filter: BloomFilter,
    pub data_size_bytes: u64,
    pub compressed_size_bytes: u64,
    pub application_ids: HashSet<String>,
}

/// SSTable index entry for fast lookups
#[derive(Debug, Clone)]
struct IndexEntry {
    key: MemtableKey,
    offset: u64,
    size: u32,
}

/// SSTable file format:
/// - Header (magic, version, metadata)
/// - Bloom filter (serialized)
/// - Data blocks (compressed, checksummed entries)
/// - Index (array of IndexEntry)
/// - Footer (created_at, index_offset, min/max keys)
pub struct SSTableWriter {
    id: u64,
    writer: BufWriter<File>,
    bloom_header_size: u64,
    index_entries: Vec<IndexEntry>,
    bloom_filter: BloomFilter,
    application_ids: HashSet<String>,
    min_key: Option<MemtableKey>,
    max_key: Option<MemtableKey>,
    num_entries: u64,
    data_size_bytes: u64,
}

impl SSTableWriter {
    /// Create a new SSTableWriter. Opens the file immediately and writes a placeholder
    /// header; the real header is written by finish() after all entries are known.
    pub fn new(id: u64, output_dir: &Path) -> Result<Self> {
        let output_path = output_dir.join(format!("sstable-{:08}.sst", id));

        let bloom_filter = BloomFilter::new(10_000, 0.01);

        // Serialize a fresh bloom filter to determine the fixed placeholder size.
        // BloomFilter uses a fixed-size bit array (capacity + FP rate are constants),
        // so its serialized size is the same whether 0 or N elements are inserted.
        let placeholder_bloom = bincode::serialize(&bloom_filter)?;
        let bloom_header_size = placeholder_bloom.len() as u64;

        // Header layout: magic(4) + version(2) + id(8) + num_entries(8) + bloom_size(4) + bloom_data(N)
        let header_total = 4 + 2 + 8 + 8 + 4 + bloom_header_size as usize;

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }

        let mut writer = BufWriter::new(file);

        // Reserve space for the header; finish() seeks back and overwrites these zeros.
        writer.write_all(&vec![0u8; header_total])?;

        Ok(Self {
            id,
            writer,
            bloom_header_size,
            index_entries: Vec::new(),
            bloom_filter,
            application_ids: HashSet::new(),
            min_key: None,
            max_key: None,
            num_entries: 0,
            data_size_bytes: 0,
        })
    }

    /// Add an entry to the SSTable (must be added in sorted order).
    /// Each entry is written to disk immediately — no full-dataset buffering.
    pub fn add(&mut self, key: MemtableKey, frame: Frame) -> Result<()> {
        // Verify sorted order
        if let Some(ref last_key) = self.max_key {
            if &key <= last_key {
                return Err(LoraDbError::StorageError(
                    "SSTable entries must be added in sorted order".into(),
                )
                .into());
            }
        }

        if self.min_key.is_none() {
            self.min_key = Some(key.clone());
        }

        self.bloom_filter.insert(&key.dev_eui);

        if let Some(app_id) = frame.application_id() {
            self.application_ids.insert(app_id.as_str().to_string());
        }

        let entry_offset = self.writer.stream_position()?;

        let frame_data = bincode::serialize(&frame)?;

        let mut compressed = Vec::new();
        {
            let mut encoder = EncoderBuilder::new()
                .level(4)
                .build(&mut compressed)?;
            encoder.write_all(&frame_data)?;
            let (_, result) = encoder.finish();
            result?;
        }

        let compressed_size = compressed.len() as u32;

        let mut hasher = Hasher::new();
        hasher.update(&compressed);
        let checksum = hasher.finalize();

        // Write: [compressed_size(4) | compressed_data(N) | checksum(4)]
        self.writer.write_all(&compressed_size.to_le_bytes())?;
        self.writer.write_all(&compressed)?;
        self.writer.write_all(&checksum.to_le_bytes())?;

        let entry_size = 4 + compressed_size + 4;

        self.index_entries.push(IndexEntry {
            key: key.clone(),
            offset: entry_offset,
            size: entry_size,
        });

        self.max_key = Some(key);
        self.num_entries += 1;
        self.data_size_bytes += entry_size as u64;

        Ok(())
    }

    /// Finalize the SSTable: write index and footer, then seek back to write the real header.
    pub fn finish(mut self) -> Result<SSTableMetadata> {
        if self.num_entries == 0 {
            return Err(LoraDbError::StorageError("Cannot write empty SSTable".into()).into());
        }

        let min_key = self.min_key.unwrap();
        let max_key = self.max_key.unwrap();

        // Write index
        let index_offset = self.writer.stream_position()?;
        let index_count = self.index_entries.len() as u32;
        self.writer.write_all(&index_count.to_le_bytes())?;

        for entry in &self.index_entries {
            let key_data = bincode::serialize(&entry.key)?;
            let key_size = key_data.len() as u32;
            self.writer.write_all(&key_size.to_le_bytes())?;
            self.writer.write_all(&key_data)?;
            self.writer.write_all(&entry.offset.to_le_bytes())?;
            self.writer.write_all(&entry.size.to_le_bytes())?;
        }

        // Write footer: min_key | max_key | created_at | index_offset
        let min_key_data = bincode::serialize(&min_key)?;
        let max_key_data = bincode::serialize(&max_key)?;
        let min_key_size = min_key_data.len() as u32;
        let max_key_size = max_key_data.len() as u32;

        self.writer.write_all(&min_key_size.to_le_bytes())?;
        self.writer.write_all(&min_key_data)?;
        self.writer.write_all(&max_key_size.to_le_bytes())?;
        self.writer.write_all(&max_key_data)?;

        let created_at_micros = Utc::now().timestamp_micros();
        self.writer.write_all(&created_at_micros.to_le_bytes())?;
        self.writer.write_all(&index_offset.to_le_bytes())?;

        // into_inner() flushes the BufWriter and returns the underlying File
        let mut file = self.writer
            .into_inner()
            .map_err(|e| anyhow::anyhow!("Failed to flush SSTable writer: {}", e.error()))?;

        // Seek back to position 0 and overwrite the placeholder with the real header.
        // The bloom filter's bit array is fixed-size (capacity and FP rate are constants),
        // so its serialized size here always matches what was written in new().
        file.seek(SeekFrom::Start(0))?;

        let bloom_data = bincode::serialize(&self.bloom_filter)?;
        debug_assert_eq!(
            bloom_data.len() as u64,
            self.bloom_header_size,
            "Bloom filter serialized size changed between new() and finish()"
        );

        file.write_all(&SSTABLE_MAGIC.to_le_bytes())?;
        file.write_all(&SSTABLE_VERSION.to_le_bytes())?;
        file.write_all(&self.id.to_le_bytes())?;
        file.write_all(&self.num_entries.to_le_bytes())?;
        file.write_all(&(bloom_data.len() as u32).to_le_bytes())?;
        file.write_all(&bloom_data)?;
        file.flush()?;

        info!(
            "Wrote SSTable {} with {} entries, {} bytes (compressed: {})",
            self.id, self.num_entries, self.data_size_bytes, self.data_size_bytes
        );

        Ok(SSTableMetadata {
            id: self.id,
            created_at: DateTime::from_timestamp_micros(created_at_micros).unwrap(),
            num_entries: self.num_entries,
            min_key,
            max_key,
            bloom_filter: self.bloom_filter,
            data_size_bytes: self.data_size_bytes,
            compressed_size_bytes: self.data_size_bytes,
            application_ids: self.application_ids,
        })
    }
}

/// SSTable reader for querying data
pub struct SSTableReader {
    id: u64,
    path: PathBuf,
    metadata: SSTableMetadata,
    index: Vec<IndexEntry>,
    /// Lazily populated on first call to application_ids() for on-disk SSTables
    /// that pre-date the application_ids metadata field.
    app_ids_cache: Mutex<Option<HashSet<String>>>,
}

impl SSTableReader {
    /// Open an existing SSTable
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut file = File::open(&path)?;
        let mut reader = BufReader::new(&mut file);

        // Read and verify header
        let mut magic_buf = [0u8; 4];
        reader.read_exact(&mut magic_buf)?;
        let magic = u32::from_le_bytes(magic_buf);
        if magic != SSTABLE_MAGIC {
            return Err(LoraDbError::StorageError(format!(
                "Invalid SSTable magic: expected 0x{:08X}, got 0x{:08X}",
                SSTABLE_MAGIC, magic
            ))
            .into());
        }

        let mut version_buf = [0u8; 2];
        reader.read_exact(&mut version_buf)?;
        let version = u16::from_le_bytes(version_buf);
        if version != SSTABLE_VERSION {
            warn!(
                "Skipping SSTable {:?} with incompatible version {} (current: {})",
                path, version, SSTABLE_VERSION
            );
            return Err(LoraDbError::IncompatibleSStableVersion(version).into());
        }

        let mut id_buf = [0u8; 8];
        reader.read_exact(&mut id_buf)?;
        let id = u64::from_le_bytes(id_buf);

        let mut num_entries_buf = [0u8; 8];
        reader.read_exact(&mut num_entries_buf)?;
        let num_entries = u64::from_le_bytes(num_entries_buf);

        // Read bloom filter
        let mut bloom_size_buf = [0u8; 4];
        reader.read_exact(&mut bloom_size_buf)?;
        let bloom_size = u32::from_le_bytes(bloom_size_buf);

        let mut bloom_data = vec![0u8; bloom_size as usize];
        reader.read_exact(&mut bloom_data)?;
        let bloom_filter: BloomFilter = bincode::deserialize(&bloom_data)?;

        // Seek to footer to read metadata
        drop(reader); // Close BufReader before opening new file handle

        // Footer layout: min_key (size+data) | max_key (size+data) | created_at (8) | index_offset (8)
        // Read fixed-size footer from end first
        let mut footer_reader = File::open(&path)?;
        footer_reader.seek(SeekFrom::End(-16))?;

        let mut created_at_buf = [0u8; 8];
        footer_reader.read_exact(&mut created_at_buf)?;
        let created_at_micros = i64::from_le_bytes(created_at_buf);
        let created_at = DateTime::from_timestamp_micros(created_at_micros).unwrap();

        let mut index_offset_buf = [0u8; 8];
        footer_reader.read_exact(&mut index_offset_buf)?;
        let index_offset = u64::from_le_bytes(index_offset_buf);

        // Now read index to find where footer starts
        let mut index_reader = File::open(&path)?;
        index_reader.seek(SeekFrom::Start(index_offset))?;

        let mut index_count_buf = [0u8; 4];
        index_reader.read_exact(&mut index_count_buf)?;
        let index_count = u32::from_le_bytes(index_count_buf);

        let mut index = Vec::with_capacity(index_count as usize);
        for _ in 0..index_count {
            let mut key_size_buf = [0u8; 4];
            index_reader.read_exact(&mut key_size_buf)?;
            let key_size = u32::from_le_bytes(key_size_buf);

            let mut key_data = vec![0u8; key_size as usize];
            index_reader.read_exact(&mut key_data)?;
            let key: MemtableKey = bincode::deserialize(&key_data)?;

            let mut offset_buf = [0u8; 8];
            index_reader.read_exact(&mut offset_buf)?;
            let offset = u64::from_le_bytes(offset_buf);

            let mut size_buf = [0u8; 4];
            index_reader.read_exact(&mut size_buf)?;
            let size = u32::from_le_bytes(size_buf);

            index.push(IndexEntry { key, offset, size });
        }

        // Now read min/max keys from after the index
        let mut min_key_size_buf = [0u8; 4];
        index_reader.read_exact(&mut min_key_size_buf)?;
        let min_key_size = u32::from_le_bytes(min_key_size_buf);

        let mut min_key_data = vec![0u8; min_key_size as usize];
        index_reader.read_exact(&mut min_key_data)?;
        let min_key: MemtableKey = bincode::deserialize(&min_key_data)?;

        let mut max_key_size_buf = [0u8; 4];
        index_reader.read_exact(&mut max_key_size_buf)?;
        let max_key_size = u32::from_le_bytes(max_key_size_buf);

        let mut max_key_data = vec![0u8; max_key_size as usize];
        index_reader.read_exact(&mut max_key_data)?;
        let max_key: MemtableKey = bincode::deserialize(&max_key_data)?;

        let metadata = SSTableMetadata {
            id,
            created_at,
            num_entries,
            min_key,
            max_key,
            bloom_filter,
            data_size_bytes: 0, // Not stored in file
            compressed_size_bytes: 0,
            application_ids: HashSet::new(), // Will be populated lazily if needed for retention
        };

        debug!("Opened SSTable {} with {} entries", id, num_entries);

        Ok(Self {
            id,
            path,
            metadata,
            index,
            app_ids_cache: Mutex::new(None),
        })
    }

    /// Check if a device might exist in this SSTable (using bloom filter)
    pub fn might_contain(&self, dev_eui: &DevEui) -> bool {
        self.metadata.bloom_filter.contains(&dev_eui.normalized())
    }

    /// Get metadata
    pub fn metadata(&self) -> &SSTableMetadata {
        &self.metadata
    }

    /// Iterate over all frames in this SSTable
    /// Used for rebuilding device registry on startup
    pub fn iter_all(&self) -> Result<Vec<Frame>> {
        let mut results = Vec::new();

        for entry in &self.index {
            let frame = self.read_frame(entry)?;
            results.push(frame);
        }

        Ok(results)
    }

    /// Get one frame per unique device in this SSTable.
    /// Far more efficient than iter_all() for device registry rebuilding,
    /// since we only need one frame per DevEUI to register a device.
    /// The index is sorted by (dev_eui, timestamp, sequence), so unique
    /// devices are discovered by checking for DevEUI changes in order.
    pub fn iter_unique_device_frames(&self) -> Result<Vec<Frame>> {
        let mut results = Vec::new();
        let mut last_dev_eui: Option<&str> = None;

        for entry in &self.index {
            let dev_eui = entry.key.dev_eui.as_str();
            if last_dev_eui != Some(dev_eui) {
                last_dev_eui = Some(dev_eui);
                let frame = self.read_frame(entry)?;
                results.push(frame);
            }
        }

        Ok(results)
    }

    /// Scan for entries matching a device and time range
    pub fn scan(
        &self,
        dev_eui: &DevEui,
        start_time: Option<DateTime<Utc>>,
        end_time: Option<DateTime<Utc>>,
    ) -> Result<Vec<Frame>> {
        // Quick bloom filter check
        if !self.might_contain(dev_eui) {
            return Ok(Vec::new());
        }

        let start_key = MemtableKey::range_start(dev_eui, start_time);
        let end_key = MemtableKey::range_end(dev_eui, end_time);

        let mut results = Vec::new();

        // Binary search to find starting point
        let start_idx = self
            .index
            .binary_search_by(|entry| entry.key.cmp(&start_key))
            .unwrap_or_else(|idx| idx);

        // Scan from start_idx until we exceed end_key
        for entry in &self.index[start_idx..] {
            if entry.key > end_key {
                break;
            }

            if &entry.key >= &start_key && &entry.key <= &end_key {
                // Read and decompress frame
                let frame = self.read_frame(entry)?;
                results.push(frame);
            }
        }

        Ok(results)
    }

    /// Read a single frame at a given index entry
    fn read_frame(&self, entry: &IndexEntry) -> Result<Frame> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset))?;

        let mut reader = BufReader::new(file);

        // Read compressed size
        let mut size_buf = [0u8; 4];
        reader.read_exact(&mut size_buf)?;
        let compressed_size = u32::from_le_bytes(size_buf);

        // Read compressed data
        let mut compressed_data = vec![0u8; compressed_size as usize];
        reader.read_exact(&mut compressed_data)?;

        // Read checksum
        let mut checksum_buf = [0u8; 4];
        reader.read_exact(&mut checksum_buf)?;
        let stored_checksum = u32::from_le_bytes(checksum_buf);

        // Verify checksum
        let mut hasher = Hasher::new();
        hasher.update(&compressed_data);
        let computed_checksum = hasher.finalize();

        if stored_checksum != computed_checksum {
            return Err(LoraDbError::StorageError(format!(
                "Checksum mismatch in SSTable {}",
                self.id
            ))
            .into());
        }

        // Decompress
        let mut decompressed = Vec::new();
        {
            let mut decoder = Decoder::new(&compressed_data[..])?;
            decoder.read_to_end(&mut decompressed)?;
        }

        // Deserialize frame
        let frame: Frame = bincode::deserialize(&decompressed)
            .context("Failed to deserialize frame from SSTable")?;

        Ok(frame)
    }

    /// Get the SSTable ID
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Get the file path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the maximum timestamp in this SSTable (for retention policy)
    pub fn max_timestamp(&self) -> Option<DateTime<Utc>> {
        // Convert microseconds timestamp to DateTime
        DateTime::from_timestamp_micros(self.metadata.max_key.timestamp)
    }

    /// Get all application IDs in this SSTable (for retention policy).
    /// Results are cached after the first scan so that repeated retention checks
    /// do not re-read all frames from disk.
    pub fn application_ids(&self) -> Result<HashSet<String>> {
        // Newly-written SSTables have application_ids populated at write time.
        if !self.metadata.application_ids.is_empty() {
            return Ok(self.metadata.application_ids.clone());
        }

        let mut cache = self.app_ids_cache.lock();
        if let Some(ref ids) = *cache {
            return Ok(ids.clone());
        }

        let mut app_ids = HashSet::new();
        for frame in self.iter_all()? {
            if let Some(app_id) = frame.application_id() {
                app_ids.insert(app_id.as_str().to_string());
            }
        }
        *cache = Some(app_ids.clone());
        Ok(app_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::frames::UplinkFrame;
    use crate::model::lorawan::*;
    use chrono::Utc;
    use tempfile::TempDir;

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

    #[test]
    fn test_sstable_write_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let dev_eui = DevEui::new("0123456789ABCDEF".to_string()).unwrap();

        let now = Utc::now();
        let one_hour_ago = now - chrono::Duration::hours(1);
        let two_hours_ago = now - chrono::Duration::hours(2);

        // Write SSTable
        let mut writer = SSTableWriter::new(1, temp_dir.path()).unwrap();

        let key1 = MemtableKey::new(&dev_eui, two_hours_ago, 0);
        let key2 = MemtableKey::new(&dev_eui, one_hour_ago, 1);
        let key3 = MemtableKey::new(&dev_eui, now, 2);

        writer
            .add(key1.clone(), create_test_frame("0123456789ABCDEF", two_hours_ago))
            .unwrap();
        writer
            .add(key2.clone(), create_test_frame("0123456789ABCDEF", one_hour_ago))
            .unwrap();
        writer
            .add(key3.clone(), create_test_frame("0123456789ABCDEF", now))
            .unwrap();

        let metadata = writer.finish().unwrap();
        assert_eq!(metadata.num_entries, 3);

        // Read SSTable
        let sstable_path = temp_dir.path().join("sstable-00000001.sst");
        let reader = SSTableReader::open(sstable_path).unwrap();

        assert_eq!(reader.metadata().num_entries, 3);
        assert!(reader.might_contain(&dev_eui));

        // Scan all entries
        let frames = reader.scan(&dev_eui, None, None).unwrap();
        assert_eq!(frames.len(), 3);

        // Scan with time range
        let recent_frames = reader.scan(&dev_eui, Some(one_hour_ago), None).unwrap();
        assert_eq!(recent_frames.len(), 2);
    }

    #[test]
    fn test_sstable_bloom_filter() {
        let temp_dir = TempDir::new().unwrap();
        let dev_eui1 = DevEui::new("0123456789ABCDEF".to_string()).unwrap();
        let dev_eui2 = DevEui::new("FEDCBA9876543210".to_string()).unwrap();

        let now = Utc::now();

        // Write SSTable with only dev_eui1
        let mut writer = SSTableWriter::new(1, temp_dir.path()).unwrap();
        let key = MemtableKey::new(&dev_eui1, now, 0);
        writer
            .add(key, create_test_frame("0123456789ABCDEF", now))
            .unwrap();
        writer.finish().unwrap();

        // Read and test bloom filter
        let sstable_path = temp_dir.path().join("sstable-00000001.sst");
        let reader = SSTableReader::open(sstable_path).unwrap();

        assert!(reader.might_contain(&dev_eui1));
        // dev_eui2 should not be present (but false positives are possible)
        // We can't assert !might_contain because of false positives
        let _ = dev_eui2;
    }

    #[test]
    fn test_sstable_sorted_order_enforcement() {
        let temp_dir = TempDir::new().unwrap();
        let dev_eui = DevEui::new("0123456789ABCDEF".to_string()).unwrap();

        let now = Utc::now();
        let one_hour_ago = now - chrono::Duration::hours(1);

        let mut writer = SSTableWriter::new(1, temp_dir.path()).unwrap();

        let key1 = MemtableKey::new(&dev_eui, now, 0);
        let key2 = MemtableKey::new(&dev_eui, one_hour_ago, 1); // Out of order!

        writer
            .add(key1, create_test_frame("0123456789ABCDEF", now))
            .unwrap();

        // This should fail
        let result = writer.add(key2, create_test_frame("0123456789ABCDEF", one_hour_ago));
        assert!(result.is_err());
    }
}
