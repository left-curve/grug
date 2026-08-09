#[cfg(feature = "tracing")]
use tracing::instrument;
use {
    crate::{BlockSource, httpd_client::HttpdClient},
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData, BlockDataExt},
    dango_indexer_cache::{IndexerPath, cache_file::CacheFile},
    futures::StreamExt,
    reqwest::IntoUrl,
    std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    },
    tokio::sync::broadcast,
};

/// Backoff between subscription / init reconnection attempts.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

/// V1 [`BlockSource`] implementation.
///
/// Runs on the same host as the dango node. The **live tail** comes from the
/// node's in-process `full_block` subscription (the same shared path the remote
/// source uses); **historical** blocks come from the cache files the node
/// already writes to disk, served by `get`. See `design/local-block-source.md`.
pub struct LocalBlockSource {
    cache_path: IndexerPath,
    httpd_client: HttpdClient,
    /// Highest block height the live feed has delivered — the broadcast
    /// frontier. Everything at or below it is reachable through `get` (the
    /// co-located node holds every finalized block on disk), so a jump in the
    /// feed simply advances it and the skipped heights are pulled from disk.
    ///
    /// Mutated only by the `run()` task; reads are lock-free. The atomic is
    /// here for cross-task visibility, not for write serialisation.
    frontier: AtomicU64,
    broadcast_tx: broadcast::Sender<Arc<BlockData>>,
}

impl LocalBlockSource {
    pub fn new<U>(
        cache_path: IndexerPath,
        httpd_base_url: U,
        pubsub_buffer_size: usize,
    ) -> AnyResult<Self>
    where
        U: IntoUrl,
    {
        let httpd_client = HttpdClient::new(httpd_base_url)?;
        let (broadcast_tx, _) = broadcast::channel(pubsub_buffer_size);
        Ok(Self {
            cache_path,
            httpd_client,
            frontier: AtomicU64::new(0),
            broadcast_tx,
        })
    }
}

#[async_trait]
impl BlockSource for LocalBlockSource {
    #[cfg_attr(feature = "tracing", instrument(skip_all, name = "bsource.local"))]
    async fn run(self: Arc<Self>) -> AnyResult<()> {
        #[cfg(feature = "tracing")]
        tracing::info!("LocalBlockSource starting");

        // Outer loop: keep the `full_block` subscription open across transient
        // disconnects, always (re)subscribing at the live tip. Unlike the remote
        // source there is no store to heal from — but there needs not be: the
        // co-located node has every finalized block on disk (it writes the cache
        // file at FinalizeBlock, before publishing the feed), so any height at or
        // below the frontier is always reachable through `get`. A reconnect
        // therefore just re-baselines the frontier at the new tip, and the
        // heights skipped during the downtime are pulled from disk by the
        // projection loop's `get` catch-up. Resuming at `frontier + 1` instead
        // would fail against the node's ~100-block ring ("resync required") after
        // any non-trivial downtime — a wedge with no recovery here.
        loop {
            let mut stream = match self.httpd_client.subscribe_full_blocks().await {
                Ok(s) => s,
                Err(_e) => {
                    #[cfg(feature = "metrics")]
                    metrics::counter!(crate::metrics::RECONNECTS, "reason" => "subscribe_failed")
                        .increment(1);
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        error = %_e,
                        backoff_secs = RECONNECT_BACKOFF.as_secs(),
                        "subscribe_full_blocks failed, retrying"
                    );
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                    continue;
                },
            };

            #[cfg(feature = "tracing")]
            tracing::info!("subscription open");

            loop {
                match stream.next().await {
                    Some(Ok(block)) => {
                        let height = block.height();
                        let current = self.frontier.load(Ordering::Acquire);

                        // Skip a re-delivered block at/under the frontier. The
                        // first block (frontier still 0) sets the baseline.
                        if height <= current {
                            continue;
                        }

                        // A jump beyond `current + 1` (a dropped live event, or a
                        // reconnect at a higher tip) is not a hole to replay: the
                        // skipped heights are already on the node's disk, so we
                        // advance the frontier straight to `height` and the
                        // projection loop pulls the gap via `get`. Record it for
                        // observability, but never break to "replay" — the node's
                        // ~100-block ring can't serve a large hole anyway.
                        #[cfg(any(feature = "metrics", feature = "tracing"))]
                        if current != 0 && height > current + 1 {
                            #[cfg(feature = "metrics")]
                            metrics::counter!(crate::metrics::DISCONTINUITIES).increment(1);
                            #[cfg(feature = "tracing")]
                            tracing::debug!(
                                current,
                                height,
                                "live feed skipped; advancing frontier (projections fill via disk)"
                            );
                        }

                        self.frontier.store(height, Ordering::Release);
                        let _ = self.broadcast_tx.send(Arc::new(block));

                        // Local is a contiguous passthrough: the frontier is the
                        // live tip. Refresh both, count the block, and surface the
                        // broadcast fan-out's backlog / subscriber count.
                        #[cfg(feature = "metrics")]
                        {
                            metrics::gauge!(crate::metrics::FRONTIER).set(height as f64);
                            metrics::gauge!(crate::metrics::LIVE_HEIGHT).set(height as f64);
                            metrics::counter!(crate::metrics::LIVE_BLOCKS).increment(1);
                            metrics::gauge!(crate::metrics::CHANNEL_DEPTH, "channel" => "broadcast")
                                .set(self.broadcast_tx.len() as f64);
                            metrics::gauge!(
                                crate::metrics::CHANNEL_RECEIVERS,
                                "channel" => "broadcast"
                            )
                            .set(self.broadcast_tx.receiver_count() as f64);
                        }
                    },
                    Some(Err(_e)) => {
                        #[cfg(feature = "metrics")]
                        metrics::counter!(crate::metrics::RECONNECTS, "reason" => "stream_error")
                            .increment(1);
                        #[cfg(feature = "tracing")]
                        tracing::warn!(error = %_e, "subscription error, reconnecting");
                        break;
                    },
                    None => {
                        #[cfg(feature = "metrics")]
                        metrics::counter!(crate::metrics::RECONNECTS, "reason" => "stream_ended")
                            .increment(1);
                        #[cfg(feature = "tracing")]
                        tracing::warn!("subscription stream ended, reconnecting");
                        break;
                    },
                }
            }

            tokio::time::sleep(RECONNECT_BACKOFF).await;
        }
    }

    async fn get(&self, height: u64) -> AnyResult<Option<BlockData>> {
        let path = self.cache_path.block_path(height);

        if !CacheFile::exists(path.clone()) {
            return Ok(None);
        }

        let cache_file = CacheFile::load_from_disk_async(path).await?;

        Ok(Some(BlockData {
            block: cache_file.data.block,
            outcome: cache_file.data.block_outcome,
        }))
    }

    fn subscribe(&self) -> broadcast::Receiver<Arc<BlockData>> {
        self.broadcast_tx.subscribe()
    }

    async fn contiguous_frontier(&self) -> AnyResult<Option<u64>> {
        let h = self.frontier.load(Ordering::Acquire);
        Ok(if h == 0 {
            None
        } else {
            Some(h)
        })
    }
}
