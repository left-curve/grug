# Architecture

Dango is a custom blockchain state machine that runs on top of CometBFT consensus.
It is inspired by CosmWasm but differs in several key ways: native Rust contract
execution, account abstraction at the protocol level, a dual-storage model (ADR-065
style), and simplified gas metering.

## 1. Database Layer

Dango separates storage into two independent stores following the Cosmos SDK ADR-065
pattern:

- **State Storage (SS):** Flat key-value store for raw, prehashed data. This is what
  contracts read and write.
- **State Commitment (SC):** Merkle-tree-backed store for cryptographic state
  proofs. Keys and values are hashed before insertion.

Both stores are backed by a single RocksDB instance using separate column families
(`dango/core/db/disk/src/db.rs`):

| Column Family      | Purpose                               |
| ------------------ | ------------------------------------- |
| `default`          | Metadata (latest committed version)   |
| `state_commitment` | JMT nodes (hashed key-value pairs)    |
| `state_storage`    | Chain-level state (non-contract keys) |
| `wasm_storage`     | Contract internal storage (see below) |

`state_storage` and `wasm_storage` together form the logical "state storage" layer.
They share the same `Batch` of pending writes; the DB routes each key to the correct
CF based on its prefix:

```rust
// dango/core/db/disk/src/db.rs
fn is_wasm_key(key: &[u8]) -> bool {
    key.starts_with(CONTRACT_NAMESPACE) && key.len() >= WASM_PREFIX_LEN
}
```

A contract key has the format `b"wasm" | address (20 bytes) | sub_key`, giving a
fixed 24-byte prefix (`WASM_PREFIX_LEN`). Everything else goes to `state_storage`.

The two CFs exist so that each can have **specialized RocksDB options**:

| Option           | `wasm_storage`                                          | `state_storage`                                                     |
| ---------------- | ------------------------------------------------------- | ------------------------------------------------------------------- |
| Memtable size    | 16 MiB (fewer flushes; contracts are less delete-heavy) | 2 MiB (frequent flushes; chain state is delete-heavy from cronjobs) |
| Prefix extractor | 24 bytes (`b"wasm"` + 20-byte address)                  | 4 bytes (namespace length)                                          |

Both CFs share a common base configuration: 256 MiB LRU block cache, bloom filters
(10 bits/key), L0 filter/index pinning, and level-style compaction.

During iteration, the DB detects whether the scan range falls entirely within the
wasm range, entirely outside it, or spans both. In the spanning case, it creates a
**merged iterator** over both CFs, preserving key ordering. When min/max share the
same 24-byte contract prefix, RocksDB's `prefix_same_as_start` mode is enabled for
faster prefix-scoped iteration.

### DiskDb

```rust
// dango/core/db/disk/src/db.rs
pub struct DiskDb<T> {
    data: Arc<RwLock<Data>>,          // RocksDB handle + priority data
    pending: Arc<RwLock<Option<PendingData>>>,  // Staged but uncommitted writes
    _commitment: PhantomData<T>,      // MerkleTree or SimpleCommitment
}
```

Key properties:

- **Two-phase commit.** `flush_but_not_commit()` stages a write batch in memory as
  `PendingData` and returns the new version + root hash. `commit()` atomically
  persists the staged batch to RocksDB. If the node crashes between these two calls,
  all changes are discarded on restart.
- **Versioning.** Each committed batch increments a monotonic version counter. The
  version must match the expected block height -- the `IncorrectVersion` error
  prevents out-of-order mutations.
- **Pruning.** Old versions can be pruned via `prune(up_to_version)` to reclaim disk
  space. Pruned versions can no longer produce Merkle proofs.

### MemDb (testing)

```rust
// dango/core/db/memory/src/db.rs
pub struct MemDb<T = SimpleCommitment> {
    inner: Shared<MemDbInner>,
    _commitment: PhantomData<T>,
}
```

An in-memory implementation using `BTreeMap`s. Only maintains the latest version.
Supports snapshot/recovery via `dump()` and `recover()` for mainnet forking in tests.

### Db trait

```rust
// dango/core/app/src/traits/db.rs
pub trait Db {
    type StateStorage: Storage + Clone + 'static;
    type StateCommitment: Storage + Clone + 'static;
    type Proof: BorshSerialize + BorshDeserialize;

    fn state_commitment(&self) -> Self::StateCommitment;
    fn state_storage_with_comment(&self, version: Option<u64>, comment: &'static str)
        -> Result<Self::StateStorage, Self::Error>;

    fn latest_version(&self) -> Option<u64>;
    fn root_hash(&self, version: Option<u64>) -> Result<Option<Hash256>, Self::Error>;
    fn prove(&self, key: &[u8], version: Option<u64>) -> Result<Self::Proof, Self::Error>;

    fn flush_but_not_commit(&self, batch: Batch) -> Result<(u64, Option<Hash256>), Self::Error>;
    fn commit(&self) -> Result<u64, Self::Error>;
    fn prune(&self, up_to_version: u64) -> Result<(), Self::Error>;
}
```

## 2. Jellyfish Merkle Tree (JMT)

State commitment uses a binary Jellyfish Merkle Tree adapted from Diem
(`dango/core/jellyfish-merkle/`). The tree provides:

- **Cryptographic state root** (SHA-256) included in the ABCI `app_hash` signed by
  validators.
- **Membership proofs** (a key exists with a given value) and **non-membership
  proofs** (a key does not exist).
- **Versioned nodes** enabling proofs at historical heights.

### Node types

```rust
// Internal node: branches left and right
pub struct InternalNode {
    left_hash: Option<Hash256>,
    right_hash: Option<Hash256>,
}

// Leaf node: actual key-value entry
pub struct LeafNode {
    key_hash: Hash256,
    value_hash: Hash256,
}
```

### Apply algorithm

1. Receive a `Batch` of prehash key-value operations.
2. Hash all keys and values with SHA-256.
3. Sort by key hash.
4. Recursively update tree nodes (only changed paths are rewritten).
5. Record orphaned nodes for future pruning.
6. Return new root hash.

### Proof verification

```rust
// dango/core/jellyfish-merkle/src/proof.rs
pub fn verify_membership_proof(
    root_hash: Hash256,
    key_hash: Hash256,
    value_hash: Hash256,
    proof: &MembershipProof,  // Vec of sibling hashes along the path
) -> Result<(), ProofError>;

pub fn verify_non_membership_proof(
    root_hash: Hash256,
    key_hash: Hash256,
    proof: &NonMembershipProof,
) -> Result<(), ProofError>;
```

### Commitment trait

```rust
// dango/core/app/src/traits/commitment.rs
pub trait Commitment {
    type Proof;
    fn root_hash(storage: &dyn Storage, version: u64) -> StdResult<Option<Hash256>>;
    fn apply(storage: &mut dyn Storage, old_version: u64, new_version: u64, batch: &Batch)
        -> StdResult<Option<Hash256>>;
    fn prove(storage: &dyn Storage, key_hash: Hash256, version: u64) -> StdResult<Self::Proof>;
    fn prune(storage: &mut dyn Storage, up_to_version: u64) -> StdResult<()>;
}
```

Two implementations: `MerkleTree` (production, full JMT) and `SimpleCommitment`
(testing fallback, SHA-256 of batch).

## 3. Storage Layer

`dango/core/storage/` provides type-safe, namespace-aware abstractions over raw key-value
storage.

### Abstractions

| Type                  | Purpose                          | Key file                   |
| --------------------- | -------------------------------- | -------------------------- |
| `Item<T>`             | Single value                     | `storage/src/item.rs`      |
| `Map<K, T>`           | Key-value mapping with iteration | `storage/src/map.rs`       |
| `Set<K>`              | Membership set                   | `storage/src/set.rs`       |
| `Counter<T>`          | Monotonic counter                | `storage/src/counter.rs`   |
| `IndexedMap<K, T, I>` | Map with secondary indexes       | `storage/src/index/map.rs` |

Usage example:

```rust
const CONFIG: Item<Config> = Item::new("config");
const BALANCES: Map<Addr, Uint128> = Map::new("balances");
const ADMINS: Set<Addr> = Set::new("admins");
```

### Key encoding

Keys implement the `PrimaryKey` trait which serializes composite keys with length
delimiters for unambiguous parsing. Tuple keys like `(Addr, u64)` are encoded as
`[len(Addr) | Addr bytes | u64 bytes]`. Values are serialized with Borsh by default.

### Contract storage isolation

Each contract's storage is wrapped in a `StorageProvider` (`dango/core/app/src/providers/storage.rs`):

```rust
pub struct StorageProvider {
    storage: Box<dyn Storage>,
    namespace: Vec<u8>,  // "wasm" + contract_address
}
```

Every read, write, scan, and remove operation is automatically prefixed with the
contract's namespace. Scans are bounded to `[namespace, namespace_increment)`.

**Security guarantee:** A contract cannot access another contract's storage through
any combination of key manipulation. The `StorageProvider` is opaque to contract code.

## 4. The App (ABCI Interface)

The `App` struct (`dango/core/app/src/app.rs`) is the state machine's entry point. It
connects the database, VM, indexer, and proposal preparer:

```rust
pub struct App<DB, VM, PP = NaiveProposalPreparer, ID = NullIndexer> {
    pub db: DB,
    vm: VM,
    pp: PP,
    pub indexer: ID,
    query_gas_limit: u64,
    upgrade_handler: Option<UpgradeHandler<VM>>,
    cargo_version: String,
}
```

### ABCI lifecycle

CometBFT drives the state machine through these ABCI methods:

```text
InitChain → [PrepareProposal → CheckTx* → FinalizeBlock → Commit]*
```

#### InitChain

Initializes genesis state: stores the chain config, deploys system contracts, executes
genesis messages. The first version is 0.

#### CheckTx

Lightweight mempool validation. Only runs:

1. Fee withholding -- Can the sender afford the gas fee?
2. `sender.authenticate()` -- Is the credential (signature, nonce) valid?

State changes from CheckTx are discarded. A failing CheckTx causes the transaction
to be rejected from the mempool.

#### FinalizeBlock

Full transaction processing:

1. **Upgrade check.** If the current height matches a scheduled upgrade and the
   binary version matches, run the upgrade handler. If the version mismatches,
   **halt the chain** intentionally.
2. **Process transactions** (see [Transaction lifecycle](../notes/transaction-lifecycle.md)):
   - Withhold the gas fee (`gas_limit * gas_fee_rate` of `gas_token`, credited to
     the owner) -- **must succeed**. Senders in `gas_exemptions` pay nothing.
   - `sender.authenticate()` -- If fails, the withheld fee is still charged.
   - Execute messages one-by-one, atomically.
   - Commit the withheld fee. There is no refund of unused gas.
3. **Run cronjobs.** Each scheduled cronjob runs in an isolated buffer; failures are
   silently discarded.
4. **Clean up orphaned codes.** Codes not referenced by any contract and older than
   `max_orphan_age` are removed.
5. **Flush.** `db.flush_but_not_commit(batch)` -- stages all changes, computes root
   hash, but does **not** persist to disk yet.
6. **Index.** The indexer receives the block and outcomes.

#### Commit

`db.commit()` atomically persists the staged changes to RocksDB. If this fails, the
chain panics (conservative: prevents state corruption).

### Buffer pattern (rollback)

State changes are accumulated in nested `Buffer<S>` layers:

```rust
// dango/core/types/src/buffer.rs
pub struct Buffer<S> {
    base: S,
    pending: Batch,  // BTreeMap<Vec<u8>, Op<Vec<u8>>>
}
```

- **Block-level buffer:** Wraps the DB's state storage.
- **Transaction-level buffer:** Wraps the block buffer. On tx success, merged up;
  on failure, discarded.
- **Submessage buffer:** Each submessage gets its own buffer for granular rollback.

Reads check `pending` first (most recent write wins), then fall through to `base`.

### Gas metering

```rust
// dango/core/app/src/gas/tracker.rs
pub struct GasTracker {
    inner: Shared<GasTrackerInner>,  // Shared<T> = Arc<RwLock<T>>
}

struct GasTrackerInner {
    limit: Option<u64>,  // None = unlimited (genesis, cronjobs)
    used: u64,
}
```

Gas is consumed on every operation. Exceeding the limit returns `StdError::OutOfGas`
and aborts execution (state changes discarded, fee still collected).

Gas costs (`dango/core/app/src/gas/costs.rs`):

| Operation                 | Cost                         |
| ------------------------- | ---------------------------- |
| `db_read`                 | 588 + 2/byte                 |
| `db_write`                | 1176 + 18/byte               |
| `db_scan` (setup)         | 588                          |
| `db_next` (per iteration) | 18                           |
| `secp256k1_verify`        | 770,000                      |
| `secp256r1_verify`        | 1,880,000                    |
| Hash functions            | 0 base + 5--28/byte (varies) |
| Wasmer operation          | 1 gas/op                     |

See [Gas](../notes/gas.md) for benchmark methodology.

## 5. Virtual Machine Layer

Two VM implementations share the same trait:

```rust
// dango/core/app/src/traits/vm.rs
pub trait Vm: Sized {
    type Instance: Instance;
    fn build_instance(
        &mut self,
        code: &[u8],
        code_hash: Hash256,
        storage: StorageProvider,
        state_mutable: bool,
        querier: Box<dyn QuerierProvider>,
        query_depth: usize,
        gas_tracker: GasTracker,
    ) -> Result<Self::Instance, Self::Error>;
}

pub trait Instance {
    fn call_in_0_out_1(self, name: &'static str, ctx: &Context) -> Result<Vec<u8>>;
    fn call_in_1_out_1<P>(self, name: &'static str, ctx: &Context, param: &P) -> Result<Vec<u8>>;
    fn call_in_2_out_1<P1, P2>(self, name: &'static str, ctx: &Context, p1: &P1, p2: &P2) -> Result<Vec<u8>>;
}
```

**Note:** The `Instance` is consumed (`self`, not `&self`) on each call, preventing
state leakage between invocations.

### RustVm (native execution)

```rust
// dango/core/vm/rust/src/vm.rs
pub struct RustVm;
```

Executes contracts compiled directly into the node binary. No sandboxing, no gas
metering overhead. Used for all first-party system contracts (bank, accounts,
perps, oracle, etc.).

**Security implication:** Code running in `RustVm` has the same trust level as the
node binary itself. A bug in a system contract is indistinguishable from a bug in
the state machine.

### WasmVm (sandboxed execution)

```rust
// dango/core/vm/wasm/src/vm.rs
pub struct WasmVm {
    cache: Option<Cache>,  // LRU cache of compiled Wasmer modules
}
```

Executes third-party WASM bytecode via the Wasmer runtime. Key protections:

**Gatekeeper middleware** (`dango/core/vm/wasm/src/gatekeeper.rs`): Validates WASM
modules at compilation time. Allowed/denied features:

| Feature            | Allowed | Rationale                         |
| ------------------ | ------- | --------------------------------- |
| Floats             | Yes     | Required for JSON deserialization |
| Bulk memory ops    | Yes     | Required by Rust 1.87+            |
| Reference types    | **No**  | Could enable memory leaks         |
| SIMD               | **No**  | Non-deterministic floats          |
| Threads            | **No**  | Non-deterministic                 |
| Exception handling | **No**  | Unstable WASM proposal            |

**Metering middleware:** Injects gas tracking into every WASM operation (1 gas per
Wasmer op).

**Memory limits:** 32 MiB per instance (512 WASM pages).

**Query depth limit:** Maximum 3 levels of nested cross-contract queries.

**Host functions** (`dango/core/vm/wasm/src/imports.rs`): The WASM guest can call these
host-provided functions:

- Storage: `db_read`, `db_write`, `db_remove`, `db_scan`, `db_next`
- Crypto: `secp256k1_verify`, `secp256r1_verify`, `secp256k1_pubkey_recover`
- Hashes: `sha2_256`, `sha2_512`, `sha3_256`, `sha3_512`, `keccak256`
- Cross-contract queries: `query_chain`
- Debug logging: `debug`

Each host function call:

1. Reads data from WASM linear memory.
2. Charges gas (based on operation + data size).
3. Enforces `state_mutable` -- writes rejected during query execution.
4. Invalidates all iterators on write (preventing use-after-mutation bugs).

## 6. Chain Upgrades

There are three dimensions in which a change can be breaking:

- **Consensus-breaking:** Given the same state and block, old and new software produce
  different results, causing a consensus failure.
- **State-breaking:** The format of data stored in the DB changes.
- **API-breaking:** The transaction or query API changes.

Any breaking change requires a **coordinated upgrade**: all validators halt at the
same block height, upgrade, and resume together.

### Upgrade procedure

1. The chain owner sends a `Message::Upgrade`:

   ```json
   {
     "upgrade": {
       "height": 12345,
       "cargo_version": "1.2.3",
       "git_tag": "v1.2.3",
       "url": "https://github.com/left-curve/left-curve/releases/v1.2.3"
     }
   }
   ```

   This signals the upgrade height and target version. Node operators should **not**
   upgrade yet.

2. The chain finalizes block 12344 normally. At block 12345, during `FinalizeBlock`,
   the App reads `NEXT_UPGRADE` from state and checks the binary's cargo version.

3. **Version mismatch → intentional halt.** The App returns an error in
   `FinalizeBlockResponse`. Block 12345 is not finalized; no state changes are
   committed. This is safer than risking a fork.

4. The node operator replaces the binary with version `1.2.3` and restarts.

5. CometBFT retries `FinalizeBlock` for block 12345. The App sees the version now
   matches, runs the **upgrade handler** (`App::upgrade_handler`) if one is registered,
   clears `NEXT_UPGRADE`, records the upgrade in `PAST_UPGRADES`, and resumes normal
   block processing.

### Upgrade handler

```rust
type UpgradeHandler<VM> = fn(Box<dyn Storage>, VM, BlockInfo) -> AppResult<()>;
```

The handler receives mutable storage access and can perform arbitrary state
migrations: adding fields to stored structs, rewriting storage layouts, deploying new
contracts, or updating configuration. It runs exactly once at the upgrade height.

### Security considerations

- The upgrade height and version are stored on-chain (`NEXT_UPGRADE` item in app
  state). Only the chain owner can schedule an upgrade.
- A mismatch between the running binary and the scheduled version causes an
  intentional halt rather than a silent fork -- this is the conservative choice.
- There is no automated upgrade tool (like Cosmos SDK's cosmovisor) yet; operators
  must manually replace the binary.
