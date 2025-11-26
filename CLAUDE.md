# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

LoRaDB is a specialized time-series database built in Rust for storing and querying LoRaWAN network traffic. It implements an LSM-tree storage engine with WAL, memtables, SSTables, and compaction, along with MQTT ingestion from ChirpStack and The Things Network, and a query DSL for data retrieval.

## Build & Development Commands

### Building
```bash
# Development build
cargo build

# Release build (with LTO, optimization level 3)
cargo build --release

# Build token generator utility
cargo build --release --bin generate-token
```

### Testing
```bash
# Run all tests
cargo test

# Run specific test module
cargo test --lib storage
cargo test --lib api
cargo test --lib query
cargo test --lib security

# Run tests with output
cargo test -- --nocapture

# Run single test
cargo test test_name -- --nocapture
```

### Benchmarking
```bash
# Run storage benchmarks
cargo bench --bench storage_bench

# Run query benchmarks
cargo bench --bench query_bench
```

### Docker
```bash
# Build and run with Docker Compose
docker-compose up -d

# View logs
docker-compose logs -f loradb

# Generate JWT token in container
docker compose exec loradb generate-token admin

# Stop container
docker-compose down
```

### Running the Server
```bash
# Load environment variables from .env file (see .env.example)
cp .env.example .env
# Edit .env with appropriate values

# Run in development
cargo run

# Run release binary
./target/release/loradb
```

## Architecture Overview

### Core Components

**Storage Engine** (`src/storage.rs`, `src/engine/`):
- **LSM-Tree Architecture**: Write-Ahead Log → Memtable → SSTables → Compaction
- **WAL** (`engine/wal.rs`): CRC32-checksummed entries with crash recovery
- **Memtable** (`engine/memtable.rs`): Lock-free `crossbeam-skiplist` for in-memory writes
- **SSTables** (`engine/sstable.rs`): Immutable sorted files with bloom filters and LZ4 compression
- **Compaction** (`engine/compaction.rs`): Background merging of SSTables

**Data Flow**:
1. MQTT message arrives → parsed into `Frame`
2. Frame sent via `mpsc::channel` to storage engine
3. Storage writes to WAL (durability), then memtable (speed)
4. When memtable reaches threshold, flushed to SSTable
5. Multiple SSTables trigger compaction to merge and deduplicate

**Device-First Indexing**: Composite key format `(DevEUI, timestamp, sequence)` enables efficient per-device queries.

### Key Modules

**MQTT Ingestion** (`src/ingest/`):
- `mqtt.rs`: TLS connection management and automatic reconnection
- `chirpstack.rs`: ChirpStack v4 JSON message parsing
- `ttn.rs`: The Things Network v3 message parsing
- All parsers convert to unified `Frame` enum

**Query System** (`src/query/`):
- `parser.rs`: Hand-written recursive descent parser for query DSL
- `dsl.rs`: AST representation (SELECT, FROM, WHERE, time ranges)
- `executor.rs`: Query execution against memtable + SSTables
- Query DSL syntax: `SELECT * FROM device 'DEV_EUI' WHERE LAST '1h'`

**API Layer** (`src/api/`):
- `http.rs`: Axum HTTP server with optional TLS (use reverse proxy in production)
- `handlers.rs`: REST endpoints (`/health`, `/query`, `/devices`, `/devices/:dev_eui`)
- `middleware.rs`: JWT authentication, security headers, CORS

**Security** (`src/security/`):
- `jwt.rs`: HS256 token generation/validation with 1-hour expiration
- `encryption.rs`: Optional AES-256-GCM data-at-rest encryption with key zeroization
- `tls.rs`: Rustls configuration for HTTPS

**Data Models** (`src/model/`):
- `frames.rs`: Unified `Frame` enum (Uplink, Downlink, Join, Status)
- `lorawan.rs`: DevEui, AppEui, LoRaWAN metadata types
- `device.rs`: `DeviceRegistry` using `DashMap` for concurrent device tracking
- `gateway.rs`: Gateway metadata structures

## Important Implementation Details

### Concurrency Model
- Storage engine uses `Arc<RwLock<T>>` for shared state
- Memtable uses lock-free `crossbeam-skiplist` internally
- Device registry uses `DashMap` for lock-free concurrent access
- Frame ingestion uses `mpsc::channel` for async message passing

### Error Handling
- Custom error type: `LoraDbError` in `src/error.rs`
- Uses `thiserror` for error derivation
- All storage operations return `Result<T, LoraDbError>`

### Configuration
- Environment-based config using `dotenvy` crate
- See `.env.example` for all configuration options
- Config struct in `src/config.rs` with validation

### Security Notes
- JWT secret must be ≥32 characters (validated at startup)
- File permissions automatically set to 0700 for data directory on Unix
- TLS 1.2+ enforced for MQTT and optional HTTPS
- Production deployments should use reverse proxy (Caddy/nginx) for HTTPS

### Testing Strategy
- 75 total tests across all modules
- Unit tests embedded in module files with `#[cfg(test)]`
- Uses `tempfile` for filesystem test isolation
- Uses `tokio-test` for async test utilities

## Common Development Patterns

### Adding a New Query DSL Feature
1. Update AST in `query/dsl.rs`
2. Extend parser in `query/parser.rs` (recursive descent)
3. Implement execution logic in `query/executor.rs`
4. Add tests for parser and executor

### Adding a New MQTT Network Support
1. Create parser module in `src/ingest/your_network.rs`
2. Implement `parse_message()` to convert JSON to `Frame`
3. Register in `mqtt.rs` with broker config and topic pattern
4. Add configuration in `config.rs` and `.env.example`

### Adding API Endpoints
1. Define handler in `api/handlers.rs`
2. Register route in `api/http.rs` `serve()` method
3. Apply JWT middleware if authentication required
4. Add tests in `handlers.rs` test module

### Storage Engine Modifications
- WAL format changes require migration logic in `wal.rs`
- SSTable format changes require version handling in `sstable.rs`
- Compaction strategy changes go in `compaction.rs`

## Dependencies of Note
- **tokio**: Async runtime (full feature set)
- **axum 0.6**: HTTP server framework
- **rumqttc**: MQTT client with rustls TLS
- **crossbeam-skiplist**: Lock-free memtable
- **dashmap**: Concurrent device registry
- **jsonwebtoken**: JWT authentication
- **aes-gcm**: Optional encryption (feature flag `encryption-aes`)
- **lz4**: SSTable compression
- **rustls**: TLS implementation

## Project Structure
```
src/
├── main.rs              # Application entry point, component initialization
├── lib.rs               # Library exports
├── config.rs            # Environment-based configuration
├── error.rs             # Custom error types
├── storage.rs           # Storage engine orchestration
├── engine/              # LSM-tree components
│   ├── wal.rs          # Write-Ahead Log with CRC32
│   ├── memtable.rs     # In-memory skiplist
│   ├── sstable.rs      # Sorted string table files
│   └── compaction.rs   # Background compaction
├── ingest/              # MQTT message ingestion
│   ├── mqtt.rs         # TLS connection management
│   ├── chirpstack.rs   # ChirpStack v4 parser
│   └── ttn.rs          # TTN v3 parser
├── query/               # Query processing
│   ├── parser.rs       # DSL parser
│   ├── dsl.rs          # AST definitions
│   └── executor.rs     # Query execution
├── api/                 # HTTP API
│   ├── http.rs         # Axum server
│   ├── handlers.rs     # REST endpoints
│   └── middleware.rs   # Auth & security
├── security/            # Cryptography & auth
│   ├── jwt.rs          # JWT service
│   ├── encryption.rs   # AES-256-GCM
│   └── tls.rs          # Rustls config
├── model/               # Data models
│   ├── frames.rs       # Frame enum
│   ├── lorawan.rs      # LoRaWAN types
│   ├── device.rs       # Device registry
│   └── gateway.rs      # Gateway metadata
├── util/                # Utilities
│   ├── bloom.rs        # Bloom filter
│   ├── compression.rs  # LZ4 wrapper
│   ├── varint.rs       # Variable-length encoding
│   └── clock.rs        # Time utilities
└── bin/
    └── generate-token.rs # JWT token generator CLI

benches/                 # Criterion benchmarks
tests/                   # Integration tests (if any)
```
