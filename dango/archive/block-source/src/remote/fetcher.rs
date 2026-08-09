use {
    dango_archive_types::BlockData,
    tokio::{sync::mpsc, task::JoinHandle},
};

mod sentinel;

pub use sentinel::{
    BlockRangeClient, MAX_BLOCK_RANGE, SentinelBlockFetcher, SentinelFetcherConfig,
};

/// A spawned task that is aborted when this guard is dropped.
///
/// [`FetchStream`] holds one of these so the background fetch task's lifetime
/// is tied to the stream: drop the stream and the task stops, rather than
/// leaking and racing ahead of a consumer that is no longer reading. The
/// sentinel fetcher wraps its per-stride range-call tasks in the same guard,
/// so aborting the fetch task in turn aborts the strides it has in flight.
pub(crate) struct AbortOnDrop<T = ()>(pub(crate) JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

// ---- the trait ----

/// Pulls raw blocks from some backend (a sentinel node now; B2 + sentinel
/// later) and streams them, in strictly ascending height order, to the
/// `RemoteBlockSource` that owns it.
///
/// A fetcher does **bounded** backfill: it fetches one contiguous height
/// range and then terminates. The `RemoteBlockSource` invokes it once per gap
/// it needs to fill — see `design/remote-block-source.md`. The fetcher knows
/// nothing about where blocks are stored; storing is the source's job.
///
/// **The consumer validates, not trusts.** A fetcher *should* emit exactly the
/// ascending, contiguous range `[from, to]`, but the owning source does not
/// assume it: it checks each block's height against the one it expects, and
/// treats a mismatch — or a stream that ends ([`FetchStream::recv`] returns
/// `None`) before `to` is delivered — as a failure, never as "range complete".
/// This keeps a misbehaving backend (a wrong or stale block) or a dead task
/// from silently corrupting the store or leaving a hole, and it holds
/// uniformly across every fetcher impl. The fetcher therefore carries no
/// self-validation.
pub trait BlockFetcher: Send + Sync {
    /// Spawn the fetch task for the inclusive range `[from, to]`. The returned
    /// [`FetchStream`] yields the blocks ascending and ends after `to`.
    /// Dropping the stream aborts the task.
    fn spawn(&self, from: u64, to: u64) -> FetchStream;
}

// ---- the stream handle ----

/// Backend-agnostic handle to a running [`BlockFetcher`].
///
/// Blocks arrive in strictly ascending height order through a **bounded**
/// channel, so a slow consumer exerts backpressure on the fetcher rather than
/// letting it race ahead and balloon memory (each buffered block is hundreds
/// of KB). The handle owns the fetch task and aborts it on drop.
pub struct FetchStream {
    _abort: AbortOnDrop,
    rx: mpsc::Receiver<BlockData>,
}

impl FetchStream {
    /// Wrap the receiving half of a fetch channel and the task handle.
    /// Called by concrete [`BlockFetcher`] implementations from `spawn`.
    pub(crate) fn new(rx: mpsc::Receiver<BlockData>, handle: JoinHandle<()>) -> Self {
        Self {
            _abort: AbortOnDrop(handle),
            rx,
        }
    }

    /// Next block in ascending order, or `None` once the fetcher has delivered
    /// the whole requested range (or its task has terminated).
    pub async fn recv(&mut self) -> Option<BlockData> {
        self.rx.recv().await
    }

    /// Blocks fetched but not yet consumed (channel backlog).
    ///
    /// During a backfill this is the key signal for who is the bottleneck: a
    /// high/growing backlog means the consumer (the store writer) is the
    /// bottleneck; a backlog near zero means the fetcher is.
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.rx.len()
    }
}
