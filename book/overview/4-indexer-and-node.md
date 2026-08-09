# Indexer and Node Architecture

This chapter covers the indexer pipeline, SQL schema, GraphQL API, and the Dango CLI
that wires everything together.

## 1. Indexer Design

The indexer is a **read-only, non-consensus** component that observes state transitions
and writes structured data to external databases. It **cannot affect consensus** -- its
operations run after transaction execution and are never on the critical path for state
commitment.

### Indexer trait

```rust
// dango/core/app/src/traits/indexer.rs
#[async_trait]
pub trait Indexer: Send + Sync {
    async fn start(&mut self, storage: &dyn Storage) -> IndexerResult<()>;
    async fn shutdown(&mut self) -> IndexerResult<()>;
    async fn pre_indexing(&self, block_height: u64) -> IndexerResult<()>;
    async fn index_block(&self, block: &Block, outcome: &BlockOutcome) -> IndexerResult<()>;
    async fn post_indexing(&self, block_height: u64, cfg: Config, app_cfg: Json) -> IndexerResult<()>;
    async fn wait_for_finish(&self) -> IndexerResult<()>;
    async fn last_indexed_block_height(&self) -> IndexerResult<Option<u64>>;
}
```

### Call sequence in FinalizeBlock

```text
1. pre_indexing()     ← BEFORE transaction execution
2. Execute all txs
3. Execute cronjobs
4. Remove orphaned codes
5. db.flush_but_not_commit()  ← State root computed
6. index_block()      ← AFTER execution, BEFORE commit
7. [Commit happens separately in do_commit()]
8. post_indexing()    ← AFTER commit, spawned as async task
```

**Security properties:**

- Pre-indexing runs before any state mutation -- indexer cannot influence tx execution.
- State root is computed before `index_block()` -- indexer cannot affect `app_hash`.
- Post-indexing is async and non-blocking -- indexer errors don't halt the chain.
- Pre-indexing and index_block errors are fatal (halt block processing).

### HookedIndexer (composition)

`dango/indexer/hooked/` is the single `Indexer` impl that the chain wires into `App`. It owns the three production indexer components by value and orchestrates their per-block work:

```rust
pub struct HookedIndexer {
    pub file:       dango_indexer_cache::Cache,
    pub sql:        dango_indexer_sql::Indexer,
    pub clickhouse: dango_indexer_clickhouse::Indexer,
    // …plus an `is_running` flag and a per-block `post_indexing` task map.
}
```

The data flow is expressed through typed method arguments: `Cache::post_indexing` returns a `BlockAndBlockOutcomeWithHttpDetails` payload, which `HookedIndexer` then hands to `SqlIndexer::post_indexing` and `ClickhouseIndexer::post_indexing` in sequence. Each block's `post_indexing` runs on its own tokio task so SQL and Clickhouse writes do not block consensus; `wait_for_finish` drains the task map before shutdown.

The "Hooked" name is historical — earlier revisions held a dynamic `Arc<RwLock<Vec<Box<dyn Indexer>>>>` and passed data between entries through an opaque `http::Extensions`-based context. The current shape is the three concrete fields above, but the crate and struct name are kept so deploy scripts and imports do not need to churn.

## 2. SQL Indexer (`dango/indexer/sql/`)

### Schema

| Table          | Key Columns                                                   | Purpose         |
| -------------- | ------------------------------------------------------------- | --------------- |
| `blocks`       | `height` (unique), `hash`, `app_hash`                         | Block headers   |
| `transactions` | `hash` (unique), `block_height`, `sender`, `status`           | Tx metadata     |
| `messages`     | `transaction_id`, `contract_addr`, `method_name`              | Sub-tx messages |
| `events`       | `transaction_id`, `block_height`, `event_type`, `data` (JSON) | Emitted events  |

Indexes exist on `block_height`, `hash`, `sender`, `contract_addr`, and `events.data`
(JSON).

### HTTP request tracking

Each transaction records the HTTP peer that submitted it:

```rust
pub struct HttpRequestDetails {
    pub remote_ip: Option<String>,
    pub peer_ip: Option<String>,
    pub created_at: u64,
}
```

### Persistence properties

- **Idempotent:** `save_block()` checks if the block already exists before inserting
  (safe for crash recovery).
- **Atomic:** All table writes within a single database transaction.
- **Batch-safe:** Inserts batched to 2,048 rows to respect PostgreSQL argument limits.

### Event cache

An in-memory ring buffer of recent block events (`dango/indexer/sql/src/event_cache.rs`).
Configurable window size. Used for fast GraphQL lookups without DB round-trips.

## 3. Cache Indexer (`dango/indexer/cache/`)

Persists complete block + outcome data to disk for recovery:

```text
~/.dango/indexer/blocks/{height}.json  -- Serialized block data
~/.dango/indexer/last_block.json       -- Latest block height
```

## 4. Dango-Specific Writes (`dango/indexer/sql/src/write/`)

The SQL indexer crate also performs Dango-specific data extraction in the same `post_indexing` pass, after the generic block/tx/message/event rows have been written:

```rust
// Runs in SqlIndexer::post_indexing (async, non-blocking)
let (transfers, accounts, perps) = tokio::join!(
    crate::write::transfers::save_transfers(&self.context, block_height),
    crate::write::accounts::save_accounts(&self.context, block, app_cfg.clone()),
    crate::write::perps_events::save_perps_events(&self.context, block, app_cfg),
);
```

Two sea-orm migration tables are kept side by side in the same database (`grug_seaql_migrations` and `dango_seaql_migrations`) so existing prod data does not need to be migrated.

Extracts:

- **Account events:** `UserRegistered`, `AccountRegistered`, `KeyOwned`, `KeyDisowned`
  → accounts, users, public_keys tables.
- **Transfer events:** Bank transfer events → transfers table.
- **Perps events:** Trade execution, funding, liquidation → perps_events table.

Only processes committed events from successful transactions.

## 5. GraphQL / HTTP Server (`dango/indexer/httpd/`)

Actix-web HTTP server with async-graphql:

| Parameter            | Value  |
| -------------------- | ------ |
| Workers              | 8      |
| Max connections      | 10,000 |
| Backlog              | 8,192  |
| Max blocking threads | 16     |

### Query types

- `block(height)` / `blocks(first, after)` -- Block headers with nested transactions
  and events.
- `transaction(hash)` / `transactions(first, after)` -- Tx metadata with nested
  messages and events.
- `events(filter)` -- Event queries with JSON data filtering.

### Subscriptions

Real-time subscriptions via PostgreSQL LISTEN/NOTIFY:

- `blockMinted` -- New blocks.
- `transactionProcessed` -- New transactions.
- `eventEmitted` -- New events.

### Data loaders

N+1 query prevention via dataloaders:

- `BlockTransactionsDataLoader`, `BlockEventsDataLoader`
- `TransactionEventsDataLoader`, `TransactionMessagesDataLoader`
- `EventTransactionDataLoader`, `FileTransactionDataLoader`

## 6. Node Startup (`dango/cli/`)

The `dango start` command initializes and runs the full node:

```text
1.  Parse CLI args and config
2.  Initialize telemetry (Sentry + OpenTelemetry)
3.  Initialize metrics (Prometheus)
4.  Open DiskDb (RocksDB)
5.  Create RustVm
6.  Create base App
7.  Setup indexer stack (HookedIndexer with three components):
    ├── Cache (disk persistence)
    ├── SqlIndexer (PostgreSQL — generic + Dango-specific tables)
    └── ClickhouseIndexer (analytics)
8.  Run DB migrations + catch-up reindexing
9.  Spawn:
    ├── Dango HTTP server (GraphQL)
    ├── Metrics HTTP server (Prometheus)
    └── ABCI server (CometBFT connection)
10. Signal handlers (SIGINT, SIGTERM)
```

### ABCI server split

The app is split into four ABCI service components:

- **Consensus:** FinalizeBlock, Commit
- **Mempool:** CheckTx
- **Snapshot:** State sync
- **Info:** Query, simulation

### Graceful shutdown

1. Set HTTP shutdown flags (return 503 for new requests).
2. Wait 100ms for propagation.
3. Shutdown indexer (wait for async tasks to complete).
4. Flush telemetry (Sentry, OpenTelemetry).

## 7. Security Analysis

### Trust boundaries

```text
┌────────────────────────────────────────────────────────┐
│ Consensus-Critical (ABCI)                              │
│  App + DB + VM                                         │
│  Indexer trait called but read-only                    │
│  State root determined before indexer writes           │
└────────────────── ▼ ───────────────────────────────────┘
                    │ Block + Outcome
┌───────────────────┴────────────────────────────────────┐
│ Non-Consensus (Indexer Stack)                          │
│  Cache → disk                                          │
│  SqlIndexer → PostgreSQL (generic + Dango tables)      │
│  ClickhouseIndexer → analytics DB                      │
└────────────────── ▼ ───────────────────────────────────┘
                    │
┌───────────────────┴────────────────────────────────────┐
│ Public API (GraphQL/HTTP)                              │
│  Read-only queries over indexed data                   │
│  No state mutation capability                          │
└────────────────────────────────────────────────────────┘
```

### Network exposure

| Component     | Default Port | Exposure                        |
| ------------- | ------------ | ------------------------------- |
| CometBFT RPC  | 26657        | Public (read-only)              |
| ABCI          | 26658        | Localhost only (CometBFT ↔ App) |
| Dango GraphQL | 8000         | Configurable                    |
| Metrics       | 8001         | Internal                        |
| PostgreSQL    | 5432         | Private                         |

### Known gaps

- **No GraphQL query complexity limits.** Deeply nested or expensive queries could DoS
  the HTTP server.
- **No HTTP rate limiting.** Any client can issue unlimited queries.
- **Event JSON size unbounded.** Malicious contracts could emit large events,
  inflating the database.
- **IP logging without TTL.** Transaction origin IPs stored indefinitely.
