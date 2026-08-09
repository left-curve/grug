use {
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData},
    std::sync::Arc,
    tokio::sync::broadcast,
};

/// Abstract source of blocks for the archive.
///
/// Hides where blocks come from (live subscription, fetcher, on-disk cache,
/// ...) and where they are stored. The app owns a single `BlockSource` and
/// exposes it to all projections through `subscribe()` + `get()`.
#[async_trait]
pub trait BlockSource: Send + Sync {
    /// Start the source's internal tasks (subscribe, fetch, ...). Returns
    /// when the source has terminated (clean shutdown or unrecoverable error).
    async fn run(self: Arc<Self>) -> AnyResult<()>;

    /// Read one block by height. Used by projections during catch-up.
    async fn get(&self, height: u64) -> AnyResult<Option<BlockData>>;

    /// Subscribe to the live stream of newly-contiguous blocks. Multi-
    /// subscriber via tokio broadcast; payload is `Arc<BlockData>` so all
    /// projections share a single in-memory copy.
    fn subscribe(&self) -> broadcast::Receiver<Arc<BlockData>>;

    /// Highest H such that all heights in `[min..H]` are reachable through
    /// this source. `None` if the source has not been initialised yet.
    async fn contiguous_frontier(&self) -> AnyResult<Option<u64>>;
}
