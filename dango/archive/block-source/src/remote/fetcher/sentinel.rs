#[cfg(feature = "tracing")]
use tracing::instrument;
use {
    super::{AbortOnDrop, BlockFetcher, FetchStream},
    async_trait::async_trait,
    dango_archive_types::{AnyResult, BlockData, BlockDataExt},
    futures::future::join_all,
    std::{cmp::min, time::Duration},
    tokio::{sync::mpsc, time::sleep},
};

/// The sentinel's `/block/full/range` endpoint caps a single response at this
/// many blocks. Requesting more buys nothing — the run just continues on the
/// next request.
pub const MAX_BLOCK_RANGE: u64 = 20;

/// Fetches contiguous runs of full blocks from a node's indexer-httpd
/// `/block/full/range` endpoint (`?from=&to=`).
///
/// A trait so the [`SentinelBlockFetcher`] loop can be driven by a mock in
/// tests; the production impl is the crate's `HttpdClient`, which the fetcher
/// shares with the live subscriber.
#[async_trait]
pub trait BlockRangeClient: Send + Sync {
    /// Fetch the contiguous run of full blocks `[from, to]` (inclusive). The
    /// endpoint caps the run at [`MAX_BLOCK_RANGE`] and stops at the first height
    /// missing on disk, so the result is **ascending and contiguous from `from`**
    /// but may be **shorter** than requested — and empty if `from` itself is not
    /// yet available.
    async fn fetch_block_range(&self, from: u64, to: u64) -> AnyResult<Vec<BlockData>>;
}

/// Tuning for [`SentinelBlockFetcher`].
#[derive(Debug, Clone)]
pub struct SentinelFetcherConfig {
    /// Blocks requested per `/block/full/range` call, clamped to the blocks left
    /// in the gap and capped at [`MAX_BLOCK_RANGE`] (the endpoint's own limit).
    pub range_size: u64,
    /// Concurrent `/block/full/range` calls per batch. The fetcher issues this
    /// many contiguous strides at once — each as its own runtime task — and
    /// commits the responses in height order, so the stream stays strictly
    /// ascending; `1` restores the serial one-call-at-a-time pull. In
    /// block-dense height regions a call is payload-bound (hundreds of KB per
    /// block: the sentinel's disk read + JSON serialization on one end, this
    /// side's body assembly + JSON parse on the other), so this knob scales
    /// both the overlap of the HTTP waits and the CPU the parses can use —
    /// and, symmetrically, the load on the sentinel.
    pub parallelism: usize,
    /// Per-request timeout before retrying.
    pub timeout: Duration,
    /// Upper bound on blocks fetched-ahead but not yet consumed. The fetcher can
    /// outpace the store writer during a backfill; a bounded channel makes a
    /// full buffer block the fetcher (backpressure) instead of letting it race
    /// ahead and balloon RAM. No throughput cost — the consumer only needs a
    /// small lead. This is a **RAM** knob sized for the *dense* eras a backfill
    /// crosses (hundreds of KB per block, not the ~20 KB median): 256 is ~5 MB
    /// typical / ~75 MB at the heavy tail.
    pub channel_capacity: usize,
    /// Backoff after a failed, timed-out, empty, or contiguity-breaking batch
    /// before re-issuing.
    pub retry_backoff: Duration,
}

impl Default for SentinelFetcherConfig {
    fn default() -> Self {
        Self {
            range_size: MAX_BLOCK_RANGE,
            parallelism: 4,
            timeout: Duration::from_secs(30),
            channel_capacity: 256,
            retry_backoff: Duration::from_millis(500),
        }
    }
}

/// [`BlockFetcher`] backed by a sentinel's `/block/full/range` endpoint.
///
/// Fills a bounded gap `[from, to]` by issuing up to `parallelism` range calls
/// (contiguous strides of up to `range_size` blocks each) concurrently — each
/// spawned as its own runtime task — and committing the responses **in height
/// order** into the stream, so the output stays strictly ascending and
/// contiguous while the requests overlap. Spawning (rather than polling the
/// call futures inside this task) is what lets the CPU half of a call — body
/// assembly and the serde_json parse, the dominant cost in dense eras — run on
/// the runtime's worker threads in parallel; polled in-task, the parses
/// serialize on one thread and cap the whole backfill near one core of JSON
/// throughput regardless of `parallelism`. The batch shape is borrowed from
/// the bots `BlockFetcher` (see `design/remote-block-source.md`): `join_all`
/// preserves submission order, so no reorder buffer is needed, and the bounded
/// output channel is the only backpressure — a full channel stalls the commit
/// walk, which stalls the next batch.
///
/// Generic over the client so the crate depends only on the [`BlockRangeClient`]
/// trait, not a concrete HTTP client — and so it can be driven by a mock in
/// tests.
pub struct SentinelBlockFetcher<C> {
    client: C,
    config: SentinelFetcherConfig,
}

impl<C> SentinelBlockFetcher<C> {
    pub fn new(client: C, config: SentinelFetcherConfig) -> Self {
        Self { client, config }
    }
}

impl<C> BlockFetcher for SentinelBlockFetcher<C>
where
    C: BlockRangeClient + Clone + 'static,
{
    fn spawn(&self, from: u64, to: u64) -> FetchStream {
        let (tx, rx) = mpsc::channel(self.config.channel_capacity);
        let handle = tokio::spawn(fetch_range(
            from,
            to,
            tx,
            self.client.clone(),
            self.config.clone(),
        ));
        FetchStream::new(rx, handle)
    }
}

/// Fetch `[from, to]` inclusive, sending each [`BlockData`] in ascending order
/// through `tx`. Returns when the range is done or the consumer drops the
/// receiver.
///
/// Each round issues one **batch**: up to `parallelism` range calls on fixed
/// contiguous strides of `range_size` blocks, each spawned as its own runtime
/// task so the response parses run on the worker threads, not serialized on
/// this task (see [`SentinelBlockFetcher`]). The tasks are guarded by
/// [`AbortOnDrop`], so when this task is itself aborted (the consumer dropped
/// the [`FetchStream`]) the in-flight strides die with it. `join_all` over the
/// handles preserves submission order, so the responses come back
/// height-ordered and are forwarded block by block while they stay contiguous
/// with `next_from` — the first height not yet delivered. The first anomaly —
/// a request error or timeout, a panicked stride task, an empty response, or
/// a run that breaks contiguity (which is also how a **short** run surfaces:
/// the next fixed stride no longer aligns) — stops the walk, discards the
/// rest of the batch, and the next round re-issues from `next_from` after a
/// backoff. Refetching a discarded tail trades a little bandwidth for never
/// buffering out-of-order blocks; every height in a gap exists below the live
/// tip, so anomalies are transient and rare, and are never surfaced to the
/// consumer.
#[cfg_attr(
    feature = "tracing",
    instrument(skip_all, name = "bsource.fetcher", fields(from, to))
)]
async fn fetch_range<C>(
    from: u64,
    to: u64,
    tx: mpsc::Sender<BlockData>,
    client: C,
    config: SentinelFetcherConfig,
) where
    C: BlockRangeClient + Clone + 'static,
{
    let range_size = config.range_size.clamp(1, MAX_BLOCK_RANGE);
    let parallelism = config.parallelism.max(1) as u64;
    let mut next_from = from;

    while next_from <= to {
        // One batch: up to `parallelism` contiguous strides, each spawned as
        // its own task so the CPU half of a call (body assembly + JSON parse)
        // lands on the runtime's workers instead of serializing here. Each
        // call is timed and its outcome labeled (ok / error / timeout / empty)
        // — the four mutually-exclusive ends of one request — exactly as when
        // the calls were serial.
        let mut strides = (0..parallelism)
            .map(|i| next_from + i * range_size)
            .take_while(|&chunk_from| chunk_from <= to)
            .map(|chunk_from| {
                let chunk_to = min(chunk_from + range_size - 1, to);
                let client = client.clone();
                let timeout = config.timeout;

                AbortOnDrop(tokio::spawn(async move {
                    #[cfg(feature = "metrics")]
                    let request_start = std::time::Instant::now();

                    let response = tokio::time::timeout(
                        timeout,
                        client.fetch_block_range(chunk_from, chunk_to),
                    )
                    .await;

                    #[cfg(feature = "metrics")]
                    {
                        metrics::histogram!(crate::metrics::FETCHER_REQUEST_DURATION)
                            .record(request_start.elapsed().as_secs_f64());

                        let outcome = match &response {
                            Ok(Ok(blocks)) if blocks.is_empty() => "empty",
                            Ok(Ok(_)) => "ok",
                            Ok(Err(_)) => "error",
                            Err(_) => "timeout",
                        };
                        metrics::counter!(crate::metrics::FETCHER_REQUESTS, "outcome" => outcome)
                            .increment(1);
                    }

                    response
                }))
            })
            .collect::<Vec<_>>();

        // Commit in submission (= height) order; stop at the first anomaly.
        // Awaiting `&mut handle` leaves the guards owning the tasks, so an
        // abort of this task mid-await still tears the strides down.
        let mut clean = true;

        'walk: for joined in join_all(strides.iter_mut().map(|stride| &mut stride.0)).await {
            let blocks = match joined {
                Ok(Ok(Ok(blocks))) => blocks,
                Ok(Ok(Err(_error))) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(error = %_error, next_from, "sentinel range fetch failed, retrying");
                    clean = false;
                    break 'walk;
                },
                Ok(Err(_timeout)) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(next_from, "sentinel range fetch timed out, retrying");
                    clean = false;
                    break 'walk;
                },
                Err(_join_error) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(error = %_join_error, next_from, "sentinel stride task failed, retrying");
                    clean = false;
                    break 'walk;
                },
            };

            // An empty response means the stride's first height is not on the
            // sentinel — retry from `next_from` after a backoff.
            if blocks.is_empty() {
                clean = false;
                break 'walk;
            }

            for block in blocks {
                let height = block.height();

                // The endpoint serves runs contiguous from the requested
                // `from`, so a mismatch means an *earlier* stride came back
                // short (this stride's fixed start no longer aligns) — or a
                // misbehaving backend. Either way: discard and refetch.
                if height != next_from {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        height,
                        expected = next_from,
                        "sentinel run broke contiguity, refetching"
                    );
                    clean = false;
                    break 'walk;
                }

                if tx.send(block).await.is_err() {
                    // The consumer dropped the `FetchStream`; stop.
                    return;
                }

                next_from += 1;
            }
        }

        if !clean {
            sleep(config.retry_backoff).await;
        }
    }
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use {
        super::*,
        dango_primitives::{Block, BlockInfo, BlockOutcome, Hash256, Timestamp},
        std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    fn block(height: u64) -> BlockData {
        BlockData {
            block: Block {
                info: BlockInfo {
                    height,
                    timestamp: Timestamp::from_nanos(0),
                    hash: Hash256::ZERO,
                },
                txs: vec![],
            },
            outcome: BlockOutcome {
                height,
                app_hash: Hash256::ZERO,
                cron_outcomes: vec![],
                tx_outcomes: vec![],
            },
        }
    }

    /// A range client that serves `[from, to]` but caps each response at
    /// `max_per_call` blocks — so it can legitimately return **fewer** than
    /// requested, exercising the "resume from the last received block" loop.
    #[derive(Clone)]
    struct MockRangeClient {
        max_per_call: u64,
    }

    #[async_trait]
    impl BlockRangeClient for MockRangeClient {
        async fn fetch_block_range(&self, from: u64, to: u64) -> AnyResult<Vec<BlockData>> {
            let end = to.min(from + self.max_per_call - 1);
            Ok((from..=end).map(block).collect())
        }
    }

    async fn collect(stream: &mut FetchStream, count: usize) -> Vec<u64> {
        let mut heights = Vec::new();
        for _ in 0..count {
            match stream.recv().await {
                Some(block) => heights.push(block.height()),
                None => break,
            }
        }
        heights
    }

    #[tokio::test]
    async fn fetches_full_range_in_order_then_ends() {
        let fetcher = SentinelBlockFetcher::new(
            MockRangeClient { max_per_call: 20 },
            SentinelFetcherConfig {
                range_size: 20,
                ..SentinelFetcherConfig::default()
            },
        );

        let mut stream = fetcher.spawn(1, 50);
        assert_eq!(collect(&mut stream, 50).await, (1..=50).collect::<Vec<_>>());
        // Stream ends after `to`.
        assert!(stream.recv().await.is_none());
    }

    #[tokio::test]
    async fn resumes_from_last_received_on_short_runs() {
        // The client returns at most 3 blocks per call, well under `range_size`,
        // so the loop must keep advancing via the committed frontier. With a
        // 10-block gap and `range_size` 20 every batch is a single stride, so a
        // short run resumes immediately (no misaligned follow-up stride).
        let fetcher = SentinelBlockFetcher::new(
            MockRangeClient { max_per_call: 3 },
            SentinelFetcherConfig {
                range_size: 20,
                ..SentinelFetcherConfig::default()
            },
        );

        let mut stream = fetcher.spawn(1, 10);
        assert_eq!(collect(&mut stream, 10).await, (1..=10).collect::<Vec<_>>());
        assert!(stream.recv().await.is_none());
    }

    #[tokio::test]
    async fn serial_mode_is_the_degenerate_batch() {
        // `parallelism: 1` = one stride per batch: the pre-parallelism loop.
        let fetcher = SentinelBlockFetcher::new(
            MockRangeClient { max_per_call: 3 },
            SentinelFetcherConfig {
                range_size: 20,
                parallelism: 1,
                ..SentinelFetcherConfig::default()
            },
        );

        let mut stream = fetcher.spawn(1, 10);
        assert_eq!(collect(&mut stream, 10).await, (1..=10).collect::<Vec<_>>());
        assert!(stream.recv().await.is_none());
    }

    #[tokio::test]
    async fn short_run_mid_batch_discards_the_tail_and_refetches() {
        // Strides of 5, three per batch, but the client caps every response at
        // 3 blocks: the first stride comes back short, so the second stride's
        // fixed start no longer aligns with the committed frontier and the
        // walk must discard the batch tail and re-issue — never skipping or
        // reordering a height. Zero backoff keeps the test instant.
        let fetcher = SentinelBlockFetcher::new(
            MockRangeClient { max_per_call: 3 },
            SentinelFetcherConfig {
                range_size: 5,
                parallelism: 3,
                retry_backoff: Duration::ZERO,
                ..SentinelFetcherConfig::default()
            },
        );

        let mut stream = fetcher.spawn(1, 30);
        assert_eq!(collect(&mut stream, 30).await, (1..=30).collect::<Vec<_>>());
        assert!(stream.recv().await.is_none());
    }

    /// Serves correct runs, except the *first* call for `from == 6`, which
    /// returns a run starting two heights high — a misbehaving backend.
    #[derive(Clone)]
    struct SkewedOnceClient {
        skewed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl BlockRangeClient for SkewedOnceClient {
        async fn fetch_block_range(&self, from: u64, to: u64) -> AnyResult<Vec<BlockData>> {
            if from == 6 && !self.skewed.swap(true, Ordering::SeqCst) {
                return Ok((from + 2..=to + 2).map(block).collect());
            }
            Ok((from..=to).map(block).collect())
        }
    }

    #[tokio::test]
    async fn non_contiguous_response_is_discarded_and_refetched() {
        // Batch of two strides [1, 5] and [6, 10]; the second comes back
        // skewed (8..=12). The consumer must never see the skewed run — the
        // walk discards it and refetches from 6.
        let fetcher = SentinelBlockFetcher::new(
            SkewedOnceClient {
                skewed: Arc::new(AtomicBool::new(false)),
            },
            SentinelFetcherConfig {
                range_size: 5,
                parallelism: 2,
                retry_backoff: Duration::ZERO,
                ..SentinelFetcherConfig::default()
            },
        );

        let mut stream = fetcher.spawn(1, 10);
        assert_eq!(collect(&mut stream, 10).await, (1..=10).collect::<Vec<_>>());
        assert!(stream.recv().await.is_none());
    }

    /// Counts in-flight calls and records the high-water mark, holding each
    /// call open briefly so a batch's calls demonstrably overlap.
    #[derive(Clone)]
    struct GaugedClient {
        inflight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BlockRangeClient for GaugedClient {
        async fn fetch_block_range(&self, from: u64, to: u64) -> AnyResult<Vec<BlockData>> {
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            sleep(Duration::from_millis(10)).await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok((from..=to).map(block).collect())
        }
    }

    #[tokio::test]
    async fn batch_calls_run_concurrently_and_stay_bounded() {
        let peak = Arc::new(AtomicUsize::new(0));
        let fetcher = SentinelBlockFetcher::new(
            GaugedClient {
                inflight: Arc::new(AtomicUsize::new(0)),
                peak: peak.clone(),
            },
            SentinelFetcherConfig {
                range_size: 5,
                parallelism: 4,
                ..SentinelFetcherConfig::default()
            },
        );

        // 40 blocks = 8 strides = 2 batches of 4.
        let mut stream = fetcher.spawn(1, 40);
        assert_eq!(collect(&mut stream, 40).await, (1..=40).collect::<Vec<_>>());
        assert!(stream.recv().await.is_none());

        // Every call of a batch parks on the mock's sleep before any
        // completes, so the high-water mark is exactly the batch size — proof
        // the calls overlap and never exceed `parallelism`.
        assert_eq!(peak.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn saturated_channel_stalls_the_fetcher_then_resumes() {
        // A 1-slot channel forces the commit walk to block on nearly every
        // send while a whole batch of responses is in hand; draining the
        // stream must still yield every height exactly once, in order.
        let fetcher = SentinelBlockFetcher::new(
            MockRangeClient { max_per_call: 20 },
            SentinelFetcherConfig {
                range_size: 5,
                parallelism: 4,
                channel_capacity: 1,
                ..SentinelFetcherConfig::default()
            },
        );

        let mut stream = fetcher.spawn(1, 40);
        assert_eq!(collect(&mut stream, 40).await, (1..=40).collect::<Vec<_>>());
        assert!(stream.recv().await.is_none());
    }
}
