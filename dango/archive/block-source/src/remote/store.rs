use {
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData},
};

mod disk;
mod memory;
mod ranges;

pub use {disk::RocksdbBlockStore, memory::MemoryBlockStore};

/// Genesis floor: block 0 does not exist, so the contiguous prefix and gap
/// detection start at height 1. Exported so the projection loop can clamp its
/// cursor to it — a cursor below the floor would never be served by `get`.
pub const GENESIS_HEIGHT: u64 = 1;

/// The source's durable store of raw blocks **and** the stored-height topology
/// the coordinator runs on.
///
/// The store is the single writer's storage layer: `put` persists a block and
/// folds its height into the topology (the contiguous frontier plus the gaps
/// above it), so the [`RemoteBlockSource`] coordinator stays a thin broadcast
/// driver and boot reads the frontier instead of scanning every stored height.
/// Keeping the topology here — next to the data it describes, updated on the
/// same write — is also what lets a persistent store checkpoint it for free.
///
/// Concrete in production (a local embedded KV; see
/// `design/remote-block-source.md`); behind a trait so the coordinator can be
/// built and tested against the in-memory [`MemoryBlockStore`].
///
/// [`RemoteBlockSource`]: crate::RemoteBlockSource
#[async_trait]
pub trait BlockStore: Send + Sync {
    /// Persist one block and fold its height into the topology. **Idempotent**:
    /// re-putting a height is a no-op, so the two writers (backfill + live tail)
    /// may overlap at a boundary without harm.
    ///
    /// Returns the new contiguous frontier **iff this put advanced it** — the
    /// caller broadcasts up to there. `None` means the block was a duplicate or
    /// an island sitting above a gap (nothing newly contiguous to broadcast).
    /// On a bulk-advance — a put that bridges the prefix to an already-stored
    /// run — the returned frontier can jump far past `height`; the caller
    /// broadcasts only that top and projections pull the skipped, now-durable
    /// heights via [`get`](Self::get).
    async fn put(&self, height: u64, data: &BlockData) -> AnyResult<Option<u64>>;

    /// Read one block by height, or `None` if not stored.
    async fn get(&self, height: u64) -> AnyResult<Option<BlockData>>;

    /// The contiguous frontier: highest `H` such that every height in
    /// `[GENESIS_HEIGHT, H]` is stored, or `None` if there is no prefix yet.
    /// Derived from the topology — no scan.
    async fn contiguous_frontier(&self) -> AnyResult<Option<u64>>;

    /// The lowest missing height range above the frontier (inclusive bounds,
    /// capped at the highest stored height), or `None` if the stored prefix is
    /// gap-free. Drives the healer, lowest-first; O(1) on the topology.
    async fn lowest_gap(&self) -> AnyResult<Option<(u64, u64)>>;

    /// Emit store-internal gauges (engine statistics), called periodically by the
    /// source's metric sampler. The default is a no-op — only the persistent
    /// [`RocksdbBlockStore`] has internals worth polling (memtable / SST / blob
    /// sizes, compaction backlog, write stalls), mirroring the dango node's
    /// RocksDB statistics. Synchronous and cheap (in-memory property reads).
    fn sample_metrics(&self) {}
}
