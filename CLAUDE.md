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

**Storage Engine** (`src/storage/mod.rs`, `src/engine/`):
- **LSM-Tree Architecture**: Write-Ahead Log → Memtable → SSTables → Compaction
- **WAL** (`engine/wal.rs`): CRC32-checksummed entries with crash recovery
- **Memtable** (`engine/memtable.rs`): Lock-free `crossbeam-skiplist` for in-memory writes
- **SSTables** (`engine/sstable.rs`): Immutable sorted files with bloom filters and LZ4 compression
- **Compaction** (`engine/compaction.rs`): Background merging of SSTables
- **Retention Manager** (`storage/retention_manager.rs`): Dynamic retention policy management with JSON persistence

**Data Flow**:
1. Data arrives via **MQTT** or **HTTP ingestion**:
   - **MQTT**: Message parsed into `Frame` → sent via `mpsc::channel` to storage engine
   - **HTTP**: Webhook parsed into `Frame` → written directly to storage (no channel)
2. Storage writes to WAL (durability), then memtable (speed)
3. Memtable flushed to SSTable when:
   - Periodic flush timer triggers (default: 5 minutes, configurable via `LORADB_STORAGE_MEMTABLE_FLUSH_INTERVAL_SECS`)
   - Memtable reaches size threshold (default: 64MB, configurable via `LORADB_STORAGE_MEMTABLE_SIZE_MB`)
   - Graceful shutdown (SIGTERM/SIGINT)
4. Multiple SSTables trigger compaction to merge and deduplicate

**Device-First Indexing**: Composite key format `(DevEUI, timestamp, sequence)` enables efficient per-device queries.

### Key Modules

**MQTT Ingestion** (`src/ingest/`) - **OPTIONAL**:
- `mqtt.rs`: TLS connection management and automatic reconnection
- `chirpstack.rs`: ChirpStack v4 JSON message parsing
- `ttn.rs`: The Things Network v3 message parsing
- All parsers convert to unified `Frame` enum
- Can be disabled entirely - HTTP ingestion can be used instead

**HTTP Ingestion** (`src/api/handlers.rs::ingest_chirpstack`):
- Alternative to MQTT for environments without broker access (e.g., Helium, managed ChirpStack)
- Recommended for managed LoRaWAN services without direct MQTT access
- Accepts ChirpStack webhook events via `POST /ingest?event={type}`
- Supports event types: `up` (uplink), `join` (device join), `status` (battery/margin)
- Requires JWT or API token authentication
- Writes directly to storage (no mpsc channel buffering)
- Reuses ChirpStack parser methods: `parse_uplink()`, `parse_join()`, `parse_status()`
- See `docs/HTTP_INGESTION.md` for detailed configuration guide

**Query System** (`src/query/`):
- `parser.rs`: Hand-written recursive descent parser for query DSL
- `dsl.rs`: AST representation (SELECT, FROM, WHERE, time ranges)
- `executor.rs`: Query execution against memtable + SSTables with nested field projection
- Query DSL syntax examples:
  - `SELECT * FROM device 'DEV_EUI' WHERE LAST '1h'`
  - `SELECT decoded_payload.object.co2, decoded_payload.object.TempC_SHT FROM device 'DEV_EUI' WHERE LAST '24h'`
  - `SELECT f_port, f_cnt, decoded_payload.object.temperature FROM device 'DEV_EUI' WHERE SINCE '2025-01-01T00:00:00Z'`

**API Layer** (`src/api/`):
- `http.rs`: Axum HTTP server with optional TLS (use reverse proxy in production)
- `handlers.rs`: REST endpoints
  - `/health` - Health check
  - `/ingest?event={type}` - ChirpStack webhook ingestion (uplink, join, status events)
  - `/query` - Query DSL execution
  - `/devices`, `/devices/:dev_eui` - Device management
  - `/tokens` - API token management
  - `/retention/policies` - Retention policy management
  - `/retention/enforce` - Immediate enforcement trigger
- `middleware.rs`: Dual authentication (JWT + API tokens), security headers, CORS

**Security** (`src/security/`):
- `jwt.rs`: HS256 token generation/validation with configurable expiration (default: 1 hour)
- `api_token.rs`: Long-lived API token management with revocation, expiration, and usage tracking
- `encryption.rs`: Optional AES-256-GCM data-at-rest encryption with key zeroization
- `tls.rs`: Rustls configuration for HTTPS

**Data Models** (`src/model/`):
- `frames.rs`: Unified `Frame` enum (Uplink, Downlink, Join, Status)
- `lorawan.rs`: DevEui, AppEui, LoRaWAN metadata types
- `device.rs`: `DeviceRegistry` using `DashMap` for concurrent device tracking
- `gateway.rs`: Gateway metadata structures

## Querying Decoded Payload Measurements

The query system supports nested field projection using dot notation, making it easy to extract specific measurements from uplink frames:

### Basic Query Syntax

```sql
SELECT { * | uplink | downlink | join | status | field1, field2, ... }
FROM device 'DevEUI'
[ WHERE { BETWEEN 'start' AND 'end' | SINCE 'timestamp' | LAST 'duration' } ]
[ LIMIT integer ]
```

**Examples:**

```sql
-- Query all uplink frames
SELECT uplink FROM device '0123456789ABCDEF' WHERE LAST '1h'

-- Query status frames (battery, margin)
SELECT status FROM device '0123456789ABCDEF' WHERE LAST '7d'

-- Query specific status fields
SELECT margin, battery_level FROM device '0123456789ABCDEF' WHERE LAST '24h'

-- Query specific measurements using dot notation
SELECT decoded_payload.object.co2, decoded_payload.object.TempC_SHT FROM device '0123456789ABCDEF' WHERE LAST '24h'

-- Mix top-level frame fields with nested measurements
SELECT received_at, f_port, f_cnt, decoded_payload.object.temperature FROM device '0123456789ABCDEF' WHERE LAST '7d'

-- Query deeply nested fields
SELECT decoded_payload.object.sensor.voltage, decoded_payload.object.sensor.status FROM device '0123456789ABCDEF'

-- Limit results to 100 frames
SELECT * FROM device '0123456789ABCDEF' WHERE LAST '24h' LIMIT 100

-- Get only the last 10 uplink frames
SELECT uplink FROM device '0123456789ABCDEF' WHERE LAST '1h' LIMIT 10
```

### LIMIT Clause

The optional LIMIT clause restricts the number of results returned:

```sql
-- Get last 10 uplink frames
SELECT uplink FROM device '0123456789ABCDEF' WHERE LAST '1h' LIMIT 10

-- Get first 100 frames from today
SELECT * FROM device 'DEV_EUI' WHERE SINCE '2025-12-12T00:00:00Z' LIMIT 100
```

**Important:**
- LIMIT must be a positive integer (> 0)
- LIMIT values exceeding 10,000 are capped at MAX_QUERY_RESULTS (10,000) for security
- LIMIT is optional; queries without LIMIT default to MAX_QUERY_RESULTS (10,000)
- LIMIT clause must come after WHERE clause

### Field Path Structure

After deserialization, frames have the following structure (enum variant is unwrapped automatically):
```json
{
  "frame_type": "Uplink",
  "dev_eui": "0123456789ABCDEF",
  "f_port": 1,
  "f_cnt": 42,
  "received_at": "2025-01-26T12:00:00Z",
  "decoded_payload": {
    "object": {
      "co2": 450,
      "TempC_SHT": 22.5,
      "humidity": 65.0
    }
  }
}
```

To query specific measurements, use paths like:
- `decoded_payload.object.co2` - Direct measurement field
- `decoded_payload.object.sensor.voltage` - Nested sensor field
- `f_port` - Top-level frame metadata

### Query Result Format

When using field projection, results include only the requested fields:
```json
{
  "dev_eui": "0123456789ABCDEF",
  "total_frames": 10,
  "frames": [
    {
      "decoded_payload.object.co2": 450,
      "decoded_payload.object.TempC_SHT": 22.5,
      "received_at": "2025-01-26T12:00:00Z"
    }
  ]
}
```

### Implementation Notes

- Nested field extraction uses `query/executor.rs:get_nested_field()`
- Frame enum variants are automatically unwrapped for easier querying
- Non-existent fields are silently omitted from results
- The `DecodedPayload.object` field contains the arbitrary JSON from the network server decoder

## Authentication: JWT vs API Tokens

LoRaDB supports two authentication methods:

### JWT Tokens (Short-lived)
- **Use case**: Interactive sessions, testing, temporary access
- **Expiration**: Configurable (default: 1 hour via `LORADB_API_JWT_EXPIRATION_HOURS`)
- **Generation**: `cargo run --bin generate-token <username>`
- **Format**: Standard JWT (eyJ...)
- **Pros**: Stateless, self-contained claims
- **Cons**: Cannot be revoked, short-lived (requires re-authentication)

### API Tokens (Long-lived)
- **Use case**: Dashboards, automation, services, long-running applications
- **Expiration**: Optional (configurable per-token or never expires)
- **Generation**: Two methods available
  - **API (recommended for running servers)**: `POST /tokens` - instant, no restart needed
  - **CLI (requires restart if server running)**: `cargo run --bin generate-api-token <data_dir> <username> [name] [days]`
- **Format**: `ldb_` prefix + 32 alphanumeric characters
- **Pros**: Revocable, named, tracked (last used), multiple per user
- **Cons**: Requires storage (JSON file in data directory)

### API Token Management
- **Create**: `POST /tokens` with `{"name": "Token Name", "expires_in_days": 365}` or `null` for no expiration
- **List**: `GET /tokens` (returns all tokens for authenticated user)
- **Revoke**: `DELETE /tokens/:token_id`
- **Storage**: `<data_dir>/api_tokens.json` (SHA256 hashed tokens)
- **Module**: `src/security/api_token.rs`
- **Important**: CLI-generated tokens require server restart to be loaded into memory. Use API method to avoid restart.

### Authentication Middleware
- **Module**: `src/api/middleware.rs`
- **Detection**: Automatically detects token type (JWT vs `ldb_` prefix)
- **Context**: Inserts `AuthContext` enum (Jwt or ApiToken) into request extensions
- **Backward compatibility**: JWT authentication also inserts `Claims` for existing handlers

See **API_TOKEN_GUIDE.md** for detailed usage examples and best practices.

## Retention Policy Management

LoRaDB supports flexible data retention policies that can be managed both via environment variables (legacy) and REST API (dynamic).

### Configuration Methods

**1. Environment Variables (Legacy, still supported):**
- `LORADB_STORAGE_RETENTION_DAYS` - Global default retention period in days
- `LORADB_STORAGE_RETENTION_APPS` - Per-application policies (format: `"app1:30,app2:365,app3:never"`)
- `LORADB_STORAGE_RETENTION_CHECK_INTERVAL_HOURS` - Enforcement frequency (default: 24)

**2. REST API (Dynamic, preferred):**
- Policies persisted in `<data_dir>/retention_policies.json`
- API takes precedence over environment variables
- No server restart required for policy changes
- Module: `src/storage/retention_manager.rs`

### API Endpoints

**List All Policies:**
```
GET /retention/policies
```
Returns global policy, check interval, and all application-specific policies.

**Global Policy Management:**
```
GET /retention/policies/global       # Get global retention days
PUT /retention/policies/global       # Set global policy: {"days": 90} or {"days": null} for "never"
```

**Application-Specific Policies:**
```
GET /retention/policies/:app_id      # Get policy for specific application
PUT /retention/policies/:app_id      # Set policy: {"days": 30} or {"days": null} for "never"
DELETE /retention/policies/:app_id   # Remove policy (falls back to global)
```

**Immediate Enforcement:**
```
POST /retention/enforce              # Trigger retention enforcement now (instead of waiting for scheduled run)
```

### Implementation Details

**Module**: `src/storage/retention_manager.rs`
- **RetentionPolicyManager**: Manages policies with JSON persistence
- **RetentionPolicies**: Struct containing global_days, applications map, check_interval_hours
- **RetentionPolicy**: Per-app policy with days, created_at, updated_at timestamps

**Integration**: `src/storage/mod.rs`
- Storage engine holds `Arc<RetentionPolicyManager>`
- `enforce_retention()` method (public, can be called via API)
- Background task runs periodically based on check_interval_hours
- Deletion logic unchanged (conservative approach using longest retention period)

**Backward Compatibility:**
- On first startup: reads env vars → writes to JSON file
- If JSON exists: JSON takes precedence
- If neither exists: no retention (keep forever)

**Example Usage:**
```rust
// Get retention manager from storage engine
let retention_manager = storage.retention_manager();

// Set global policy to 90 days
retention_manager.set_global(Some(90)).await?;

// Set application-specific policy
retention_manager.set_application("test-sensors".to_string(), Some(7)).await?;

// Trigger immediate enforcement
storage.enforce_retention().await?;
```

See **README.md** section "API-Based Retention Management" for curl examples.

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

### Versioning and Compatibility
- **WAL Versioning**: WAL_VERSION = 2 (v2: Fixed bincode compatibility for serde_json::Value)
  - Old WAL entries (v0/v1) are skipped during replay with warning
  - Module: `src/engine/wal.rs`
- **SSTable Versioning**: SSTABLE_VERSION = 2 (v2: Fixed bincode compatibility for Frame)
  - Old SSTables (v1) are skipped during open with warning
  - Incompatible SSTables preserved on disk but excluded from queries
  - Module: `src/engine/sstable.rs`
- **Format Change**: Version 2 introduced bincode compatibility fixes
  - Removed `skip_serializing_if` attributes from UplinkFrame fields
  - Custom serialization for DecodedPayload.object (JSON string wrapper)
  - See commit 09b3a73 for details

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
├── storage/             # Storage engine module
│   ├── mod.rs          # Storage engine orchestration
│   └── retention_manager.rs  # Retention policy management with JSON persistence
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
